"""
KiCad adapter for lens-based layout synchronization.

Bridges abstract lens types and the concrete pcbnew API.

This module implements the consolidated apply_changeset function that handles:
1. Deletions (GR-REMOVE with contents, then FP-REMOVE)
2. Additions (FP-ADD, GR-ADD with fragment content)
3. View updates for existing footprints
4. Group membership rebuild
5. Pad-to-net assignments (creates nets on-demand)
6. HierPlace for positioning new items

Note: Renames (moved() paths) are handled in Rust preprocessing before
the Python sync runs. FPID changes are now handled as delete + add operations
since EntityId includes fpid.
"""

from typing import Any, Dict, List, Optional, Set, Tuple
import logging
import uuid as uuid_module
from pathlib import Path

from .types import (
    EntityId,
    Position,
    FootprintView,
    FootprintComplement,
    GroupView,
    GroupComplement,
    BoardView,
    BoardComplement,
    TrackComplement,
    ViaComplement,
    ZoneComplement,
    GraphicComplement,
    default_footprint_complement,
)
from .hierplace import (
    PlacementRect,
    Rect,
    hierplace_layout,
    pack_at_origin,
    compute_cluster_bbox,
)
from .oplog import OpLog
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .changeset import SyncChangeset
    from .lens import FragmentData as FragmentDataType

logger = logging.getLogger("pcb.lens.kicad")


def extract_zone_outline_positions(zone: Any) -> List[Position]:
    """Extract the zone's main outline as a list of Positions.

    Uses the correct KiCad API: SHAPE_POLY_SET.COutline(0).CPoints()
    """
    positions: List[Position] = []
    outline = zone.Outline()
    if not outline or outline.OutlineCount() <= 0:
        return positions
    for point in outline.COutline(0).CPoints():
        positions.append(Position(x=point.x, y=point.y))
    return positions


def _get_entity_id_from_footprint(fp: Any) -> Optional[EntityId]:
    """Extract EntityId from a KiCad footprint.

    The canonical EntityId includes both path and fpid.
    Returns None if the footprint has no Path field.
    """
    path_field = fp.GetFieldByName("Path")
    if not path_field:
        return None
    path_str = path_field.GetText()
    if not path_str:
        return None
    fpid = fp.GetFPIDAsString()
    return EntityId.from_string(path_str, fpid=fpid)


def _get_item_bbox(item: Any, pcbnew: Any) -> Any:
    """Get bounding box for a KiCad item.

    For footprints, excludes F.Fab and B.Fab layers to match old sync behavior.
    This gives a tighter bbox based on actual copper/silkscreen content.
    """
    if hasattr(pcbnew, "FOOTPRINT") and isinstance(item, pcbnew.FOOTPRINT):
        # Exclude fab layers from bbox calculation
        lset = pcbnew.LSET.AllLayersMask()
        lset.RemoveLayer(pcbnew.F_Fab)
        lset.RemoveLayer(pcbnew.B_Fab)
        return item.GetLayerBoundingBox(lset)
    elif hasattr(item, "GetBoundingBox"):
        return item.GetBoundingBox()
    return None


def _build_pad_net_map(
    entity_id: EntityId,
    view: BoardView,
    kicad_board: Any,
) -> Dict[str, Any]:
    """Build a mapping from pad/pin name to KiCad NETINFO for a footprint.

    Looks up net assignments from SOURCE (BoardView.nets) rather than copying
    from existing pads, ensuring SOURCE-authoritative connectivity.

    Args:
        entity_id: The footprint's entity ID
        view: The BoardView containing SOURCE net definitions
        kicad_board: KiCad board to look up NETINFO objects

    Returns:
        Dict mapping pin_name -> NETINFO object (or None if net not found)
    """
    pad_net_map: Dict[str, Any] = {}

    for net_view in view.nets.values():
        for conn_entity_id, pin_name in net_view.connections:
            if conn_entity_id == entity_id:
                net_info = kicad_board.FindNet(net_view.name)
                if net_info:
                    pad_net_map[pin_name] = net_info

    return pad_net_map


# =============================================================================
# Pure hierarchical layout computation
# =============================================================================


def _identify_fragment_groups(
    changeset: "SyncChangeset",
    board_view: BoardView,
) -> Set[EntityId]:
    """Identify groups that have layout fragments (already positioned internally)."""
    fragment_groups: Set[EntityId] = set()
    for gid in changeset.added_groups:
        gv = board_view.groups.get(gid)
        if gv and gv.layout_path:
            fragment_groups.add(gid)
    return fragment_groups


def _collect_item_sizes(
    changeset: "SyncChangeset",
    fps_by_entity_id: Dict[EntityId, Any],
    groups_by_name: Dict[str, Any],
    pcbnew: Any,
    fragment_group_ids: Set[EntityId],
    exclude_footprints: Optional[Set[EntityId]] = None,
) -> Dict[EntityId, Tuple[int, int]]:
    """Collect width/height for all newly-added items from KiCad bboxes.

    This is the ONLY place we read geometry from KiCad for layout.

    Args:
        exclude_footprints: Footprints to skip (e.g., those that inherited position)
    """
    sizes: Dict[EntityId, Tuple[int, int]] = {}
    exclude = exclude_footprints or set()

    # Footprints (excluding inherited ones)
    for fid in changeset.added_footprints:
        if fid in exclude:
            continue
        fp = fps_by_entity_id.get(fid)
        if not fp:
            continue
        bbox = _get_item_bbox(fp, pcbnew)
        if bbox:
            sizes[fid] = (bbox.GetWidth(), bbox.GetHeight())
        else:
            sizes[fid] = (0, 0)

    # Fragment groups: use their KiCad bbox as rigid block size
    for gid in fragment_group_ids:
        group = groups_by_name.get(str(gid.path))
        if not group:
            continue
        items = list(group.GetItems())
        if not items:
            sizes[gid] = (0, 0)
            continue

        min_x = min_y = float("inf")
        max_x = max_y = float("-inf")

        for item in items:
            bbox = _get_item_bbox(item, pcbnew)
            if not bbox:
                continue
            min_x = min(min_x, bbox.GetLeft())
            min_y = min(min_y, bbox.GetTop())
            max_x = max(max_x, bbox.GetRight())
            max_y = max(max_y, bbox.GetBottom())

        if min_x == float("inf"):
            sizes[gid] = (0, 0)
        else:
            sizes[gid] = (int(max_x - min_x), int(max_y - min_y))

    # Non-fragment group sizes computed bottom-up in _compute_hierarchical_layout
    return sizes


def _build_rects(
    entity_ids: List[EntityId],
    sizes: Dict[EntityId, Tuple[int, int]],
) -> List[PlacementRect]:
    """Build PlacementRects for entities with non-zero sizes."""
    rects = []
    for eid in entity_ids:
        wh = sizes.get(eid)
        if wh and wh[0] > 0 and wh[1] > 0:
            rects.append(PlacementRect(entity_id=eid, width=wh[0], height=wh[1]))
    return rects


def _compute_hierarchical_layout(
    tree: Dict[Optional[EntityId], List[EntityId]],
    sizes: Dict[EntityId, Tuple[int, int]],
    group_ids: Set[EntityId],
    fragment_group_ids: Set[EntityId],
    existing_bbox: Optional[Rect],
) -> Dict[EntityId, Tuple[int, int]]:
    """Pure hierarchical layout: bottom-up packing, then top-down position propagation."""
    child_local_pos: Dict[EntityId, Dict[EntityId, Tuple[int, int]]] = {}
    sizes = dict(sizes)  # Make mutable copy

    # Bottom-up: pack children within each non-fragment group
    for gid in sorted(group_ids, key=lambda g: (-g.path.depth, str(g.path))):
        if gid in fragment_group_ids:
            continue

        children = tree.get(gid, [])
        rects = _build_rects(children, sizes)

        if not rects:
            sizes.setdefault(gid, (0, 0))
            child_local_pos[gid] = {}
        elif len(rects) == 1:
            child_local_pos[gid] = {rects[0].entity_id: (0, 0)}
            sizes[gid] = (rects[0].width, rects[0].height)
        else:
            layout = pack_at_origin(rects)
            child_local_pos[gid] = layout
            placed = [
                r.move_to(*layout[r.entity_id]) for r in rects if r.entity_id in layout
            ]
            cluster = compute_cluster_bbox(placed)
            sizes[gid] = (cluster[2], cluster[3]) if cluster else (0, 0)

    # Root-level placement
    root_rects = _build_rects(tree.get(None, []), sizes)
    if not root_rects:
        return {}

    global_pos = hierplace_layout(root_rects, existing_bbox)

    # Top-down: propagate local positions to global
    queue = [
        eid for eid in global_pos if eid in group_ids and eid not in fragment_group_ids
    ]
    while queue:
        gid = queue.pop()
        origin = global_pos[gid]
        for cid, (lx, ly) in child_local_pos.get(gid, {}).items():
            global_pos[cid] = (origin[0] + lx, origin[1] + ly)
            if cid in group_ids and cid not in fragment_group_ids:
                queue.append(cid)

    return global_pos


def _apply_hierarchical_layout(
    layout: Dict[EntityId, Tuple[int, int]],
    changeset: "SyncChangeset",
    fps_by_entity_id: Dict[EntityId, Any],
    groups_by_name: Dict[str, Any],
    pcbnew: Any,
    fragment_group_ids: Set[EntityId],
    board_view: BoardView,
    oplog: OpLog,
    exclude_footprints: Optional[Set[EntityId]] = None,
) -> int:
    """Apply computed layout to KiCad objects in a single pass.

    Args:
        exclude_footprints: Footprints to skip (e.g., those that inherited position)
    """
    placed_count = 0
    exclude = exclude_footprints or set()

    # Pre-compute footprints that belong to fragment groups (moved with their group)
    fragment_footprints: Set[EntityId] = set()
    for gid in fragment_group_ids:
        gv = board_view.groups.get(gid)
        if gv:
            fragment_footprints.update(gv.member_ids)

    # Move fragment groups as rigid blocks (sorted for deterministic output)
    for gid in sorted(fragment_group_ids, key=lambda e: str(e.path)):
        if gid not in layout:
            continue
        group = groups_by_name.get(str(gid.path))
        if not group:
            continue
        items = list(group.GetItems())
        if not items:
            continue

        # Get current top-left
        min_x = min_y = float("inf")
        for item in items:
            bbox = _get_item_bbox(item, pcbnew)
            if bbox:
                min_x = min(min_x, bbox.GetLeft())
                min_y = min(min_y, bbox.GetTop())
        if min_x == float("inf"):
            continue

        target_x, target_y = layout[gid]
        dx, dy = target_x - int(min_x), target_y - int(min_y)

        for item in group.GetItems():
            if hasattr(item, "Move"):
                item.Move(pcbnew.VECTOR2I(dx, dy))

        placed_count += 1
        oplog.place_gr(str(gid.path), target_x, target_y, w=0, h=0)

    # Move non-fragment footprints individually (sorted for deterministic output)
    # Skip inherited footprints - they're already positioned
    for fid in sorted(changeset.added_footprints, key=lambda e: str(e.path)):
        if fid in exclude or fid not in layout or fid in fragment_footprints:
            continue
        fp = fps_by_entity_id.get(fid)
        if not fp:
            continue

        bbox = _get_item_bbox(fp, pcbnew)
        if not bbox:
            continue

        target_x, target_y = layout[fid]
        dx, dy = target_x - bbox.GetLeft(), target_y - bbox.GetTop()

        pos = fp.GetPosition()
        fp.SetPosition(pcbnew.VECTOR2I(pos.x + dx, pos.y + dy))

        placed_count += 1
        oplog.place_fp(
            str(fid.path), target_x, target_y, w=bbox.GetWidth(), h=bbox.GetHeight()
        )

    return placed_count


def _build_groups_index(kicad_board: Any) -> Dict[str, Any]:
    """Build index of groups by name from the board."""
    groups = {}
    for group in list(kicad_board.Groups()):
        name = group.GetName()
        if name:
            groups[name] = group
    return groups


def apply_changeset(
    changeset: "SyncChangeset",
    kicad_board: Any,
    pcbnew: Any,
    footprint_lib_map: Dict[str, str],
    board_path: Path,
) -> OpLog:
    """Apply a SyncChangeset to a KiCad board.

    This is the unified apply function that handles both structural changes
    and physical layout (positioning) in a single pass.

    Args:
        changeset: The computed changeset from lens sync
        kicad_board: KiCad BOARD object
        pcbnew: pcbnew module
        footprint_lib_map: Mapping of library nicknames to paths
        board_path: Path to the board file (for resolving fragment paths)

    Returns:
        OpLog with all operations performed
    """
    oplog = OpLog()

    # Build local groups index
    groups_by_name = _build_groups_index(kicad_board)

    view = changeset.view
    complement = changeset.complement

    # Build initial indices
    fps_by_entity_id: Dict[EntityId, Any] = {}
    for fp in kicad_board.GetFootprints():
        entity_id = _get_entity_id_from_footprint(fp)
        if entity_id:
            fps_by_entity_id[entity_id] = fp

    # Identify fragment groups and their child footprints (for position handling)
    # Fragment groups have layout_path and their children get positions from the fragment
    fragment_groups: Set[EntityId] = set()
    fragment_footprints: Set[EntityId] = set()

    for entity_id in changeset.added_groups:
        group_view = view.groups.get(entity_id)
        if group_view and group_view.layout_path:
            fragment_groups.add(entity_id)
            for member_id in group_view.member_ids:
                if member_id in changeset.added_footprints:
                    fragment_footprints.add(member_id)

    # ==========================================================================
    # Phase 1: Deletions (groups with contents, then footprints)
    # ==========================================================================

    # 1a. GR-REMOVE - delete groups AND all their contents
    for entity_id in sorted(changeset.removed_groups, key=lambda e: str(e.path)):
        group_name = str(entity_id.path)
        if group_name in groups_by_name:
            group = groups_by_name[group_name]

            # Delete all items in the group (footprints, tracks, vias, zones, graphics)
            items_deleted = 0
            for item in list(group.GetItems()):
                # Track removed footprints (items with GetFPIDAsString are footprints)
                if hasattr(item, "GetFPIDAsString"):
                    removed_entity_id = _get_entity_id_from_footprint(item)
                    if removed_entity_id and removed_entity_id in fps_by_entity_id:
                        del fps_by_entity_id[removed_entity_id]

                kicad_board.Delete(item)
                items_deleted += 1

            kicad_board.Remove(group)
            del groups_by_name[group_name]
            oplog.gr_remove(group_name, items_deleted)
            logger.info(f"Removed group with contents: {entity_id}")

    # 1b. FP-REMOVE - delete remaining standalone footprints
    for entity_id in sorted(
        changeset.removed_footprints.keys(), key=lambda e: str(e.path)
    ):
        if entity_id in fps_by_entity_id:
            fp = fps_by_entity_id[entity_id]
            kicad_board.Delete(fp)
            del fps_by_entity_id[entity_id]
            oplog.fp_remove(str(entity_id.path))
            logger.info(f"Removed footprint: {entity_id}")

    # Rebuild groups index after deletions to avoid stale SWIG wrappers
    groups_by_name = _build_groups_index(kicad_board)

    # ==========================================================================
    # Phase 2: Additions (footprints and groups)
    # ==========================================================================

    # 2a. FP-ADD - create footprints at origin (0,0)
    for entity_id in sorted(changeset.added_footprints, key=lambda e: str(e.path)):
        fp_view = view.footprints[entity_id]

        # For fragment children, get position from fragment; for others, use default
        if entity_id in fragment_footprints:
            fp_complement = complement.footprints.get(
                entity_id, default_footprint_complement()
            )
        else:
            # Non-fragment footprints start at origin for HierPlace
            fp_complement = default_footprint_complement()

        try:
            fp = _create_footprint(
                fp_view, fp_complement, kicad_board, pcbnew, footprint_lib_map
            )
            if fp:
                # Assign pad nets from SOURCE (BoardView.nets)
                pad_net_map = _build_pad_net_map(entity_id, view, kicad_board)
                for pad in fp.Pads():
                    pad_name = pad.GetPadName()
                    if pad_name in pad_net_map:
                        pad.SetNet(pad_net_map[pad_name])

                kicad_board.Add(fp)
                fps_by_entity_id[entity_id] = fp
                pos = fp.GetPosition()
                layer_name = kicad_board.GetLayerName(fp.GetLayer())
                pad_count = len(list(fp.Pads()))
                oplog.fp_add(
                    str(entity_id.path),
                    fp_view.reference,
                    fp_view.fpid,
                    fp_view.value,
                    pos.x,
                    pos.y,
                    layer=layer_name,
                    pad_count=pad_count,
                )
                logger.info(f"Added footprint: {entity_id}")
        except Exception as e:
            logger.error(f"Failed to add footprint {entity_id}: {e}")

    # 2b. GR-ADD - create groups (routing applied in Phase 5b after membership rebuild)
    for entity_id in sorted(changeset.added_groups, key=lambda e: str(e.path)):
        group_view = view.groups[entity_id]
        group_name = str(entity_id.path)

        group = pcbnew.PCB_GROUP(kicad_board)
        group.SetName(group_name)
        kicad_board.Add(group)
        groups_by_name[group_name] = group
        oplog.gr_add(group_name)
        logger.info(f"Added group: {entity_id}")

    # ==========================================================================
    # Phase 3: View updates for existing footprints (don't touch position)
    # ==========================================================================

    for entity_id, fp_view in sorted(
        view.footprints.items(), key=lambda kv: str(kv[0].path)
    ):
        if entity_id in changeset.added_footprints:
            continue
        if entity_id not in fps_by_entity_id:
            continue

        fp = fps_by_entity_id[entity_id]
        _update_footprint_view(fp, fp_view, pcbnew)

    # ==========================================================================
    # Phase 4: Group membership rebuild
    # ==========================================================================

    all_group_paths = {str(gid.path) for gid in view.groups.keys()}

    for entity_id, group_view in sorted(
        view.groups.items(), key=lambda kv: str(kv[0].path)
    ):
        group_name = str(entity_id.path)
        if group_name not in groups_by_name:
            continue

        group = groups_by_name[group_name]

        # Clear only lens-owned membership (footprints and child groups)
        # Routing items (tracks, vias, zones, graphics) are board-authored and preserved
        for item in list(group.GetItems()):
            is_footprint = hasattr(pcbnew, "FOOTPRINT") and isinstance(
                item, pcbnew.FOOTPRINT
            )
            is_group = hasattr(pcbnew, "PCB_GROUP") and isinstance(
                item, pcbnew.PCB_GROUP
            )
            if is_footprint or is_group:
                group.RemoveItem(item)

        # Track added members for oplog
        added_members: List[str] = []

        # Add member footprints (only direct members, not those in child groups)
        for member_id in sorted(group_view.member_ids, key=lambda m: str(m.path)):
            member_path = str(member_id.path)

            # Check if this member has a more specific child group
            has_child_group = any(
                child_path != group_name
                and child_path.startswith(group_name + ".")
                and member_path.startswith(child_path + ".")
                for child_path in all_group_paths
            )

            if has_child_group:
                continue

            if member_id in fps_by_entity_id:
                group.AddItem(fps_by_entity_id[member_id])
                added_members.append(member_path)

        # Add child groups as members
        for child_group_id in sorted(view.groups.keys(), key=lambda g: str(g.path)):
            child_path = str(child_group_id.path)
            if child_path.startswith(group_name + "."):
                suffix = child_path[len(group_name) + 1 :]
                if "." not in suffix and child_path in groups_by_name:
                    group.AddItem(groups_by_name[child_path])
                    added_members.append(child_path)

        oplog.gr_member(group_name, added_members)

    # ==========================================================================
    # Phase 4b: Apply fragment routing to NEW groups (AFTER membership rebuild)
    # This must happen after Phase 4 because Phase 4 clears group membership
    # ==========================================================================

    for entity_id in sorted(changeset.added_groups, key=lambda e: str(e.path)):
        if entity_id not in fragment_groups:
            continue

        group_view = view.groups.get(entity_id)
        if not group_view or not group_view.layout_path:
            continue

        group_name = str(entity_id.path)
        group = groups_by_name.get(group_name)
        if not group:
            continue

        _apply_fragment_routing(
            group=group,
            group_view=group_view,
            entity_id=entity_id,
            complement=complement,
            kicad_board=kicad_board,
            pcbnew=pcbnew,
            board_path=board_path,
            oplog=oplog,
        )

    # ==========================================================================
    # Phase 5: Pad-to-net assignments (creates nets on-demand)
    # ==========================================================================

    for entity_id, fp_view in view.footprints.items():
        if entity_id not in fps_by_entity_id:
            continue

        fp = fps_by_entity_id[entity_id]
        net_view = view.nets

        for net_name, net in net_view.items():
            for conn_entity_id, pin_num in net.connections:
                if conn_entity_id == entity_id:
                    for pad in fp.Pads():
                        if pad.GetPadName() == pin_num:
                            net_info = kicad_board.FindNet(net_name)
                            if not net_info:
                                net_info = pcbnew.NETINFO_ITEM(kicad_board, net_name)
                                kicad_board.Add(net_info)
                                oplog.net_add(net_name)
                            pad.SetNet(net_info)
                            break

    # ==========================================================================
    # Phase 6: HierPlace - position new items hierarchically
    # ==========================================================================
    # Pack children within groups first (bottom-up), then pack groups at root level
    # Position inheritance: FPID changes inherit position from removed_footprints

    placed_count = _run_hierarchical_placement(
        changeset, view, fps_by_entity_id, groups_by_name, kicad_board, pcbnew, oplog
    )
    if placed_count > 0:
        logger.info(f"HierPlace: placed {placed_count} items")

    return oplog


def _apply_fragment_routing(
    group: Any,
    group_view: GroupView,
    entity_id: EntityId,
    complement: BoardComplement,
    kicad_board: Any,
    pcbnew: Any,
    board_path: Path,
    oplog: OpLog,
) -> None:
    """Apply routing from a layout fragment to a new group.

    Loads the fragment board fresh and duplicates items directly.
    Net names in the GroupComplement are already remapped by adapt_complement.
    """
    if not group_view.layout_path:
        return

    group_complement = complement.groups.get(entity_id)
    if not group_complement or group_complement.is_empty:
        return

    group_name = str(entity_id.path)

    # Load fragment board fresh
    layout_path = Path(group_view.layout_path)
    if not layout_path.is_absolute():
        layout_path = board_path.parent / layout_path
    layout_file = layout_path / "layout.kicad_pcb"

    if not layout_file.exists():
        logger.warning(f"Layout fragment not found: {layout_file}")
        return

    fragment_board = pcbnew.LoadBoard(str(layout_file))

    # Build UUID lookup tables from the fragment board
    fragment_tracks: Dict[str, Any] = {}
    fragment_vias: Dict[str, Any] = {}
    for track in fragment_board.GetTracks():
        item_uuid = str(track.m_Uuid.AsString())
        if "VIA" in track.GetClass().upper():
            fragment_vias[item_uuid] = track
        else:
            fragment_tracks[item_uuid] = track

    fragment_zones: Dict[str, Any] = {}
    for zone in fragment_board.Zones():
        fragment_zones[str(zone.m_Uuid.AsString())] = zone

    fragment_graphics: Dict[str, Any] = {}
    for drawing in fragment_board.GetDrawings():
        parent = drawing.GetParent()
        if (
            parent
            and hasattr(pcbnew, "FOOTPRINT")
            and isinstance(parent, pcbnew.FOOTPRINT)
        ):
            continue
        fragment_graphics[str(drawing.m_Uuid.AsString())] = drawing

    created_nets: Set[str] = set()

    def _set_net(item: Any, net_name: str) -> None:
        """Set net on item, creating it on-demand if needed."""
        if not net_name:
            return
        net_info = kicad_board.FindNet(net_name)
        if not net_info:
            net_info = pcbnew.NETINFO_ITEM(kicad_board, net_name)
            kicad_board.Add(net_info)
            created_nets.add(net_name)
            oplog.net_add(net_name)
        item.SetNet(net_info)

    # Duplicate tracks
    tracks_created = 0
    for track_comp in group_complement.tracks:
        src = fragment_tracks.get(track_comp.uuid)
        if src:
            track = src.Duplicate()
            _set_net(track, track_comp.net_name)
            kicad_board.Add(track)
            group.AddItem(track)
            start = track.GetStart()
            end = track.GetEnd()
            width = track.GetWidth() if hasattr(track, "GetWidth") else 0
            oplog.frag_track(
                group_name,
                track_comp.net_name or "(no-net)",
                fragment_board.GetLayerName(track.GetLayer()),
                start.x,
                start.y,
                end.x,
                end.y,
                width=width,
            )
            tracks_created += 1

    # Duplicate vias
    vias_created = 0
    for via_comp in group_complement.vias:
        src = fragment_vias.get(via_comp.uuid)
        if src:
            via = src.Duplicate()
            _set_net(via, via_comp.net_name)
            kicad_board.Add(via)
            group.AddItem(via)
            pos = via.GetPosition()
            drill = via.GetDrill() if hasattr(via, "GetDrill") else 0
            oplog.frag_via(
                group_name, via_comp.net_name or "(no-net)", pos.x, pos.y, drill=drill
            )
            vias_created += 1

    # Duplicate zones (preserves fill status)
    zones_created = 0
    for zone_comp in group_complement.zones:
        src = fragment_zones.get(zone_comp.uuid)
        if src:
            zone = src.Duplicate()
            _set_net(zone, zone_comp.net_name)
            kicad_board.Add(zone)
            group.AddItem(zone)
            oplog.frag_zone(
                group_name,
                zone_comp.net_name or "(no-net)",
                fragment_board.GetLayerName(zone.GetLayer()),
                zone.GetZoneName() or "",
            )
            zones_created += 1

    # Duplicate graphics
    graphics_created = 0
    for gr_comp in group_complement.graphics:
        src = fragment_graphics.get(gr_comp.uuid)
        if src:
            graphic = src.Duplicate()
            kicad_board.Add(graphic)
            group.AddItem(graphic)
            oplog.frag_graphic(
                group_name,
                graphic.GetClass(),
                fragment_board.GetLayerName(graphic.GetLayer()),
            )
            graphics_created += 1

    logger.info(
        f"Applied fragment routing to {entity_id}: "
        f"{tracks_created} tracks, {vias_created} vias, {zones_created} zones, "
        f"{graphics_created} graphics"
    )


def _build_group_tree(
    changeset: "SyncChangeset",
    exclude_footprints: Optional[Set[EntityId]] = None,
) -> Dict[Optional[EntityId], List[EntityId]]:
    """Build a tree of groups and footprints by parent EntityId.

    Returns a dict mapping parent_entity_id -> list of child EntityIds.
    Root-level items have parent = None.

    Only parents that are added groups participate in the tree;
    otherwise items are attached to the root.

    Args:
        changeset: The sync changeset
        exclude_footprints: Footprints to exclude (e.g., those that inherited position)
    """
    from collections import defaultdict

    tree: Dict[Optional[EntityId], List[EntityId]] = defaultdict(list)
    added_group_ids: Set[EntityId] = set(changeset.added_groups)
    exclude = exclude_footprints or set()

    def parent_for(entity_id: EntityId) -> Optional[EntityId]:
        parent_path = entity_id.path.parent()
        if not parent_path:
            return None
        parent_id = EntityId(path=parent_path)
        return parent_id if parent_id in added_group_ids else None

    # Add groups to tree
    for gid in changeset.added_groups:
        parent = parent_for(gid)
        tree[parent].append(gid)

    # Add footprints to tree (excluding inherited ones)
    for fid in changeset.added_footprints:
        if fid in exclude:
            continue
        parent = parent_for(fid)
        tree[parent].append(fid)

    # Sort children for determinism
    for parent, children in tree.items():
        children.sort(key=lambda eid: str(eid.path))

    return dict(tree)


def _apply_position_inheritance(
    changeset: "SyncChangeset",
    fps_by_entity_id: Dict[EntityId, Any],
    pcbnew: Any,
    oplog: OpLog,
) -> Tuple[int, Set[EntityId]]:
    """Apply position inheritance for FPID changes.

    When a footprint's FPID changes, it appears as removed (old fpid) + added (new fpid)
    with the same path. We inherit position from the removed complement.

    Returns (placed_count, inherited_footprint_ids).
    """
    placed_count = 0
    inherited: Set[EntityId] = set()

    # Map path -> (old EntityId, old complement)
    removed_by_path = {
        eid.path: (eid, comp) for eid, comp in changeset.removed_footprints.items()
    }

    for added_id in changeset.added_footprints:
        removed_info = removed_by_path.get(added_id.path)
        if not removed_info:
            continue
        old_id, old_comp = removed_info
        fp = fps_by_entity_id.get(added_id)
        if not fp:
            continue

        apply_footprint_placement(fp, old_comp, pcbnew)
        inherited.add(added_id)
        placed_count += 1
        oplog.place_fp_inherit(
            str(added_id.path),
            old_comp.position.x,
            old_comp.position.y,
            old_id.fpid,
            added_id.fpid,
        )

    return placed_count, inherited


def _run_hierarchical_placement(
    changeset: "SyncChangeset",
    board_view: BoardView,
    fps_by_entity_id: Dict[EntityId, Any],
    groups_by_name: Dict[str, Any],
    kicad_board: Any,
    pcbnew: Any,
    oplog: OpLog,
) -> int:
    """Run hierarchical HierPlace algorithm. Returns number of items placed."""
    # Phase 0: Position inheritance for FPID changes
    placed_count, inherited = _apply_position_inheritance(
        changeset, fps_by_entity_id, pcbnew, oplog
    )

    # Phase 1: Build group tree (excluding inherited footprints)
    if not (changeset.added_footprints - inherited) and not changeset.added_groups:
        return placed_count

    tree = _build_group_tree(changeset, exclude_footprints=inherited)
    if not tree:
        return placed_count

    # Phase 2: Identify fragment groups
    fragment_group_ids = _identify_fragment_groups(changeset, board_view)

    # Phase 3: Collect item sizes
    sizes = _collect_item_sizes(
        changeset=changeset,
        fps_by_entity_id=fps_by_entity_id,
        groups_by_name=groups_by_name,
        pcbnew=pcbnew,
        fragment_group_ids=fragment_group_ids,
        exclude_footprints=inherited,
    )

    # Phase 4: Existing content bbox (KiCad read-only)
    existing_bbox = _compute_existing_bbox(
        kicad_board=kicad_board,
        exclude_entity_ids=set(changeset.added_footprints),
        pcbnew=pcbnew,
    )

    # Phase 5: Pure hierarchical layout computation
    group_ids = set(changeset.added_groups)
    layout = _compute_hierarchical_layout(
        tree=tree,
        sizes=sizes,
        group_ids=group_ids,
        fragment_group_ids=fragment_group_ids,
        existing_bbox=existing_bbox,
    )

    if not layout:
        return placed_count

    # Phase 6: Apply to KiCad
    placed_count += _apply_hierarchical_layout(
        layout,
        changeset,
        fps_by_entity_id,
        groups_by_name,
        pcbnew,
        fragment_group_ids,
        board_view,
        oplog,
        inherited,
    )

    return placed_count


def _compute_existing_bbox(
    kicad_board: Any,
    exclude_entity_ids: Set[EntityId],
    pcbnew: Any = None,
) -> Optional[Rect]:
    """Compute bounding box of existing (non-new) footprints.

    Note: We compare by path only for exclusion, since the same path with
    different fpid (FPID change) should still be excluded as "new".
    """
    min_x = min_y = float("inf")
    max_x = max_y = float("-inf")

    exclude_paths = {eid.path for eid in exclude_entity_ids}

    for fp in kicad_board.GetFootprints():
        entity_id = _get_entity_id_from_footprint(fp)
        if entity_id and entity_id.path in exclude_paths:
            continue

        if pcbnew:
            bbox = _get_item_bbox(fp, pcbnew)
        else:
            bbox = fp.GetBoundingBox()
        if not bbox:
            continue
        min_x = min(min_x, bbox.GetLeft())
        min_y = min(min_y, bbox.GetTop())
        max_x = max(max_x, bbox.GetRight())
        max_y = max(max_y, bbox.GetBottom())

    if min_x == float("inf"):
        return None

    return (int(min_x), int(min_y), int(max_x - min_x), int(max_y - min_y))


def apply_footprint_placement(
    fp: Any,
    complement: FootprintComplement,
    pcbnew: Any,
) -> None:
    """Apply position, layer, and orientation to a footprint.

    IMPORTANT: Flip() negates orientation, so we must:
    1. Set position first
    2. Flip if target layer is B.Cu
    3. Set orientation AFTER flip

    This helper encapsulates this ordering to prevent bugs.
    """
    fp.SetPosition(pcbnew.VECTOR2I(complement.position.x, complement.position.y))

    target_on_back = complement.layer == "B.Cu"
    current_on_back = fp.IsFlipped()

    if target_on_back and not current_on_back:
        fp.Flip(fp.GetPosition(), True)
    elif not target_on_back and current_on_back:
        fp.Flip(fp.GetPosition(), True)

    fp.SetOrientation(pcbnew.EDA_ANGLE(complement.orientation, pcbnew.DEGREES_T))
    fp.SetLocked(complement.locked)


def _create_footprint(
    view: FootprintView,
    complement: FootprintComplement,
    board: Any,
    pcbnew: Any,
    footprint_lib_map: Dict[str, str],
) -> Any:
    """Create a new KiCad footprint from view and complement."""
    if ":" not in view.fpid:
        raise ValueError(f"Invalid FPID format: {view.fpid}")

    fp_lib, fp_name = view.fpid.split(":", 1)

    if fp_lib not in footprint_lib_map:
        raise ValueError(f"Unknown footprint library: {fp_lib}")

    lib_uri = footprint_lib_map[fp_lib]
    lib_uri = lib_uri.replace("\\\\?\\", "")  # Windows path fix

    fp = pcbnew.FootprintLoad(lib_uri, fp_name)
    if fp is None:
        raise ValueError(f"Footprint '{fp_name}' not found in library '{fp_lib}'")

    fp.SetParent(board)

    fp.SetReference(view.reference)
    fp.SetValue(view.value)
    fp.SetFPIDAsString(view.fpid)
    fp.SetDNP(view.dnp)
    fp.SetExcludedFromBOM(view.exclude_from_bom)
    fp.SetExcludedFromPosFiles(view.exclude_from_pos)

    for name, value in view.fields.items():
        fp.SetField(name, value)
        field = fp.GetFieldByName(name)
        if field:
            field.SetVisible(False)

    path_str = str(view.entity_id.path)
    new_uuid = str(uuid_module.uuid5(uuid_module.NAMESPACE_URL, path_str))
    fp.SetPath(pcbnew.KIID_PATH(f"{new_uuid}/{new_uuid}"))

    apply_footprint_placement(fp, complement, pcbnew)

    return fp


def _update_footprint_view(fp: Any, view: FootprintView, pcbnew: Any) -> None:
    """Update footprint view properties unconditionally from SOURCE."""
    fp.SetReference(view.reference)
    fp.SetValue(view.value)
    fp.SetDNP(view.dnp)
    fp.SetExcludedFromBOM(view.exclude_from_bom)
    fp.SetExcludedFromPosFiles(view.exclude_from_pos)

    for field_name, field_value in view.fields.items():
        fp.SetField(field_name, field_value)
        field = fp.GetFieldByName(field_name)
        if field:
            field.SetVisible(False)


def load_layout_fragment_with_footprints(
    layout_path: str,
    base_dir: Path,
    pcbnew: Any,
) -> "FragmentDataType":
    """Load a layout fragment including footprint positions.

    Returns a FragmentData with pure Python dataclasses only (no KiCad C++ objects).
    """
    from .lens import FragmentData

    path = Path(layout_path)
    if not path.is_absolute():
        path = base_dir / path

    layout_file = path / "layout.kicad_pcb"
    if not layout_file.exists():
        raise FileNotFoundError(f"Layout fragment not found: {layout_file}")

    layout_board = pcbnew.LoadBoard(str(layout_file))

    footprint_complements: Dict[str, FootprintComplement] = {}
    pad_net_map: Dict[Tuple[str, str], str] = {}

    for fp in layout_board.GetFootprints():
        reference = fp.GetReference()
        if not reference:
            continue

        path_field = fp.GetFieldByName("Path")
        path_str = path_field.GetText() if path_field else ""

        pos = fp.GetPosition()
        position = Position(x=pos.x, y=pos.y)
        orientation = fp.GetOrientation().AsDegrees()
        layer = "B.Cu" if fp.GetLayer() == pcbnew.B_Cu else "F.Cu"

        ref_field = fp.Reference()
        ref_pos = ref_field.GetPosition()
        reference_position = Position(x=ref_pos.x, y=ref_pos.y)
        reference_visible = ref_field.IsVisible()

        val_field = fp.Value()
        val_pos = val_field.GetPosition()
        value_position = Position(x=val_pos.x, y=val_pos.y)
        value_visible = val_field.IsVisible()

        fp_complement = FootprintComplement(
            position=position,
            orientation=orientation,
            layer=layer,
            locked=fp.IsLocked(),
            reference_position=reference_position,
            reference_visible=reference_visible,
            value_position=value_position,
            value_visible=value_visible,
        )

        footprint_complements[reference] = fp_complement
        if path_str:
            footprint_complements[path_str] = fp_complement

        for pad in fp.Pads():
            pad_name = pad.GetPadName()
            net = pad.GetNet()
            if net and net.GetNetname():
                fp_key = path_str if path_str else reference
                pad_net_map[(fp_key, pad_name)] = net.GetNetname()

    tracks: List[TrackComplement] = []
    vias: List[ViaComplement] = []

    for track in layout_board.GetTracks():
        item_class = track.GetClass().upper()
        net = track.GetNet()
        net_name = net.GetNetname() if net else ""
        item_uuid = str(track.m_Uuid.AsString())

        if "VIA" in item_class:
            pos = track.GetPosition()
            vias.append(
                ViaComplement(
                    uuid=item_uuid,
                    position=Position(x=pos.x, y=pos.y),
                    diameter=track.GetWidth(),
                    drill=track.GetDrill(),
                    via_type="through",
                    net_name=net_name,
                )
            )
        else:
            start = track.GetStart()
            end = track.GetEnd()
            tracks.append(
                TrackComplement(
                    uuid=item_uuid,
                    start=Position(x=start.x, y=start.y),
                    end=Position(x=end.x, y=end.y),
                    width=track.GetWidth(),
                    layer=layout_board.GetLayerName(track.GetLayer()),
                    net_name=net_name,
                )
            )

    zones: List[ZoneComplement] = []
    for zone in layout_board.Zones():
        item_uuid = str(zone.m_Uuid.AsString())
        net = zone.GetNet()
        net_name = net.GetNetname() if net else ""

        positions = extract_zone_outline_positions(zone)

        zones.append(
            ZoneComplement(
                uuid=item_uuid,
                name=zone.GetZoneName() or "",
                outline=tuple(positions),
                layer=layout_board.GetLayerName(zone.GetLayer()),
                priority=zone.GetAssignedPriority(),
                net_name=net_name,
            )
        )

    graphics: List[GraphicComplement] = []
    for drawing in layout_board.GetDrawings():
        parent = drawing.GetParent()
        if (
            parent
            and hasattr(pcbnew, "FOOTPRINT")
            and isinstance(parent, pcbnew.FOOTPRINT)
        ):
            continue

        item_uuid = str(drawing.m_Uuid.AsString())
        graphic_type = drawing.GetClass()
        layer = layout_board.GetLayerName(drawing.GetLayer())

        geometry: Dict[str, Any] = {}
        if hasattr(drawing, "GetStart"):
            start = drawing.GetStart()
            geometry["start"] = {"x": start.x, "y": start.y}
        if hasattr(drawing, "GetEnd"):
            end = drawing.GetEnd()
            geometry["end"] = {"x": end.x, "y": end.y}

        graphics.append(
            GraphicComplement(
                uuid=item_uuid,
                graphic_type=graphic_type,
                layer=layer,
                geometry=geometry,
            )
        )

    group_complement = GroupComplement(
        tracks=tuple(tracks),
        vias=tuple(vias),
        zones=tuple(zones),
        graphics=tuple(graphics),
    )

    return FragmentData(
        group_complement=group_complement,
        footprint_complements=footprint_complements,
        pad_net_map=pad_net_map,
    )
