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
    TrackComplement,
    ViaComplement,
    ZoneComplement,
    GraphicComplement,
    default_footprint_complement,
)
from .hierplace import (
    PlacementRect,
    Rect,
    hierplace,
    pack_at_origin,
    translate_layout,
    compute_cluster_bbox,
)
from .oplog import OpLog
from typing import TYPE_CHECKING
from dataclasses import dataclass

if TYPE_CHECKING:
    from .changeset import SyncChangeset
    from .lens import FragmentData as FragmentDataType

logger = logging.getLogger("pcb.lens.kicad")


@dataclass(frozen=True)
class FragmentPlan:
    """Intermediate structure for Rule A (Top-Most Fragment Wins).

    Centralizes all fragment-related logic: which fragments are authoritative,
    which entities they cover, and which footprints belong to each fragment.
    """

    loaded: Dict[EntityId, "FragmentDataType"]  # authoritative fragments only
    owner: Dict[EntityId, EntityId]  # entity -> owning authoritative fragment
    descendant_ids: frozenset  # all entities covered by any fragment
    descendant_footprints: Dict[
        EntityId, List[EntityId]
    ]  # fragment -> covered footprints

    def is_covered(self, eid: EntityId) -> bool:
        """Check if entity is covered by an authoritative fragment."""
        return eid in self.descendant_ids

    def is_authoritative(self, eid: EntityId) -> bool:
        """Check if entity is an authoritative fragment group."""
        return eid in self.loaded


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


# =============================================================================
# Unified geometry helpers (bbox computation + item movement)
# =============================================================================


def _compute_items_bbox(items: List[Any], pcbnew: Any) -> Optional[Rect]:
    """Compute bounding box of a list of KiCad items.

    This is the single source of truth for bbox computation from KiCad objects.
    Returns None if no valid items.
    """
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
        return None
    return (int(min_x), int(min_y), int(max_x - min_x), int(max_y - min_y))


def _move_footprint_to(
    fp: Any, target_x: int, target_y: int, pcbnew: Any
) -> Optional[Tuple[int, int]]:
    """Move footprint so its bbox top-left is at (target_x, target_y).

    Returns (width, height) of the footprint, or None if move failed.
    """
    bbox = _get_item_bbox(fp, pcbnew)
    if not bbox:
        return None

    dx = target_x - int(bbox.GetLeft())
    dy = target_y - int(bbox.GetTop())
    pos = fp.GetPosition()
    fp.SetPosition(pcbnew.VECTOR2I(pos.x + dx, pos.y + dy))

    return (int(bbox.GetWidth()), int(bbox.GetHeight()))


def _move_group_to(
    group: Any, target_x: int, target_y: int, pcbnew: Any
) -> Optional[Tuple[int, int]]:
    """Move all items in a group so the group's bbox top-left is at (target_x, target_y).

    Returns (dx, dy) applied to all items, or None if move failed.
    """
    items = list(group.GetItems())
    bbox = _compute_items_bbox(items, pcbnew)
    if not bbox:
        return None

    dx = target_x - bbox[0]
    dy = target_y - bbox[1]

    for item in items:
        if hasattr(item, "Move"):
            item.Move(pcbnew.VECTOR2I(dx, dy))

    return (dx, dy)


def _build_rects_from_footprints(
    entity_ids: List[EntityId],
    fps_by_entity_id: Dict[EntityId, Any],
    pcbnew: Any,
) -> List[PlacementRect]:
    """Build PlacementRects from KiCad footprints by reading their bboxes.

    This is the single source of truth for building rects from live KiCad objects.
    """
    rects = []
    for eid in entity_ids:
        fp = fps_by_entity_id.get(eid)
        if not fp:
            continue
        bbox = _get_item_bbox(fp, pcbnew)
        if bbox and bbox.GetWidth() > 0 and bbox.GetHeight() > 0:
            rects.append(
                PlacementRect(
                    entity_id=eid,
                    width=int(bbox.GetWidth()),
                    height=int(bbox.GetHeight()),
                )
            )
    return rects


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


def _build_fragment_plan(
    changeset: "SyncChangeset",
    board_view: BoardView,
    board_path: Path,
    pcbnew: Any,
    oplog: OpLog,
) -> FragmentPlan:
    """Build a FragmentPlan implementing Rule A (Top-Most Fragment Wins).

    A group is an authoritative fragment iff:
    - It has a successfully loaded fragment, AND
    - No ancestor has a successfully loaded fragment

    Returns a FragmentPlan that centralizes all fragment coverage information.
    """
    from .lens import FragmentData

    # Sort groups by depth (shallowest first) for top-down traversal
    groups_with_layout = [
        (gid, board_view.groups[gid])
        for gid in changeset.added_groups
        if gid in board_view.groups and board_view.groups[gid].layout_path
    ]
    groups_with_layout.sort(key=lambda x: (x[0].path.depth, str(x[0].path)))

    loaded: Dict[EntityId, FragmentData] = {}
    authoritative: Set[EntityId] = set()

    # Phase 1: Determine authoritative fragments (Rule A)
    for gid, gv in groups_with_layout:
        # Walk parents to check for authoritative ancestor (faster than scanning)
        auth_ancestor = _find_authoritative_ancestor(gid, authoritative)
        if auth_ancestor:
            logger.warning(
                f"Fragment at `{gid.path}` ignored because ancestor "
                f"`{auth_ancestor.path}` is authoritative."
            )
            oplog.frag_ignored(str(gid.path), str(auth_ancestor.path))
            continue

        # Try to load the fragment
        try:
            data = load_layout_fragment_with_footprints(
                gv.layout_path, board_path.parent, pcbnew
            )
            loaded[gid] = data
            authoritative.add(gid)
        except Exception as e:
            logger.warning(f"Fragment {gv.layout_path} not found, using HierPlace: {e}")

    # Phase 2: Compute coverage maps in one pass
    owner: Dict[EntityId, EntityId] = {}
    descendant_footprints: Dict[EntityId, List[EntityId]] = {
        gid: [] for gid in authoritative
    }

    # Find owner for each entity by walking up parent chain
    all_entities = set(changeset.added_groups) | set(changeset.added_footprints)
    for eid in all_entities:
        auth = _find_authoritative_ancestor(eid, authoritative)
        if auth and eid != auth:
            owner[eid] = auth

    # Group footprints by their owning fragment
    for fid in changeset.added_footprints:
        auth = owner.get(fid)
        if auth:
            descendant_footprints[auth].append(fid)

    # Sort for determinism
    for gid in descendant_footprints:
        descendant_footprints[gid].sort(key=lambda e: str(e.path))

    # Log authoritative fragments for debugging
    if authoritative:
        auth_paths = sorted(str(gid.path) for gid in authoritative)
        logger.info(f"Authoritative fragments: {auth_paths}")

    return FragmentPlan(
        loaded=loaded,
        owner=owner,
        descendant_ids=frozenset(owner.keys()),
        descendant_footprints=descendant_footprints,
    )


def _find_authoritative_ancestor(
    eid: EntityId, authoritative: Set[EntityId]
) -> Optional[EntityId]:
    """Walk up parent chain to find authoritative fragment ancestor."""
    p = eid.path.parent()
    while p:
        cand = EntityId(path=p)
        if cand in authoritative:
            return cand
        p = p.parent()
    return None


def _collect_item_sizes(
    changeset: "SyncChangeset",
    fps_by_entity_id: Dict[EntityId, Any],
    groups_by_name: Dict[str, Any],
    pcbnew: Any,
    plan: FragmentPlan,
    exclude_footprints: Optional[Set[EntityId]] = None,
) -> Dict[EntityId, Tuple[int, int]]:
    """Collect width/height for all newly-added items from KiCad bboxes.

    This is the ONLY place we read geometry from KiCad for layout.
    Uses FragmentPlan to determine which items to skip (covered by fragments).
    """
    sizes: Dict[EntityId, Tuple[int, int]] = {}
    exclude = exclude_footprints or set()

    # Footprints (excluding inherited ones and fragment-covered ones)
    for fid in changeset.added_footprints:
        if fid in exclude or plan.is_covered(fid):
            continue
        fp = fps_by_entity_id.get(fid)
        if not fp:
            continue
        bbox = _get_item_bbox(fp, pcbnew)
        sizes[fid] = (bbox.GetWidth(), bbox.GetHeight()) if bbox else (0, 0)

    # Authoritative fragment groups: use their KiCad bbox as rigid block size
    for gid in plan.loaded:
        sizes[gid] = _get_group_bbox_size(groups_by_name.get(str(gid.path)), pcbnew)

    # Non-fragment group sizes computed bottom-up in _compute_hierarchical_layout
    return sizes


def _get_group_bbox_size(group: Any, pcbnew: Any) -> Tuple[int, int]:
    """Get the bounding box size of a KiCad group."""
    if not group:
        return (0, 0)
    bbox = _compute_items_bbox(list(group.GetItems()), pcbnew)
    return (bbox[2], bbox[3]) if bbox else (0, 0)


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
    """Pure hierarchical layout: bottom-up packing, then top-down position propagation.

    Uses pack_at_origin for local group packing, hierplace for root placement.
    """
    child_local_pos: Dict[EntityId, Dict[EntityId, Tuple[int, int]]] = {}
    sizes = dict(sizes)  # Make mutable copy

    # Bottom-up: pack children within each non-fragment group
    # pack_at_origin handles 0/1/many cases uniformly
    for gid in sorted(group_ids, key=lambda g: (-g.path.depth, str(g.path))):
        if gid in fragment_group_ids:
            continue

        rects = _build_rects(tree.get(gid, []), sizes)
        layout = pack_at_origin(rects)
        child_local_pos[gid] = layout

        # Compute group size from packed children
        if not layout:
            sizes.setdefault(gid, (0, 0))
        else:
            placed = [r.move_to(*layout[r.entity_id]) for r in rects if r.entity_id in layout]
            cluster = compute_cluster_bbox(placed)
            sizes[gid] = (cluster[2], cluster[3]) if cluster else (0, 0)

    # Root-level placement using unified hierplace
    root_rects = _build_rects(tree.get(None, []), sizes)
    if not root_rects:
        return {}

    global_pos = hierplace(root_rects, anchor=existing_bbox)

    # Top-down: propagate local positions to global
    queue = [eid for eid in global_pos if eid in group_ids and eid not in fragment_group_ids]
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
    group_move_deltas: Optional[Dict[EntityId, Tuple[int, int]]] = None,
) -> int:
    """Apply computed layout to KiCad objects using unified move helpers."""
    placed_count = 0
    exclude = exclude_footprints or set()
    if group_move_deltas is None:
        group_move_deltas = {}

    # Pre-compute footprints that belong to fragment groups (moved with their group)
    fragment_footprints: Set[EntityId] = set()
    for gid in fragment_group_ids:
        gv = board_view.groups.get(gid)
        if gv:
            fragment_footprints.update(gv.member_ids)

    # Move fragment groups as rigid blocks
    for gid in sorted(fragment_group_ids, key=lambda e: str(e.path)):
        if gid not in layout:
            continue
        group = groups_by_name.get(str(gid.path))
        if not group:
            continue

        target_x, target_y = layout[gid]
        delta = _move_group_to(group, target_x, target_y, pcbnew)
        if delta:
            group_move_deltas[gid] = delta
            placed_count += 1
            oplog.place_gr(str(gid.path), target_x, target_y, w=0, h=0)

    # Move non-fragment footprints individually
    for fid in sorted(changeset.added_footprints, key=lambda e: str(e.path)):
        if fid in exclude or fid not in layout or fid in fragment_footprints:
            continue
        fp = fps_by_entity_id.get(fid)
        if not fp:
            continue

        target_x, target_y = layout[fid]
        size = _move_footprint_to(fp, target_x, target_y, pcbnew)
        if size:
            placed_count += 1
            oplog.place_fp(str(fid.path), target_x, target_y, w=size[0], h=size[1])

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

    # Build initial indices
    fps_by_entity_id: Dict[EntityId, Any] = {}
    for fp in kicad_board.GetFootprints():
        entity_id = _get_entity_id_from_footprint(fp)
        if entity_id:
            fps_by_entity_id[entity_id] = fp

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
    # All new footprints start at origin; HierPlace will position them
    # (including applying fragment positions if the fragment loads successfully)
    for entity_id in sorted(changeset.added_footprints, key=lambda e: str(e.path)):
        fp_view = view.footprints[entity_id]
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

        # Add child groups as members
        for child_group_id in sorted(view.groups.keys(), key=lambda g: str(g.path)):
            child_path = str(child_group_id.path)
            if child_path.startswith(group_name + "."):
                suffix = child_path[len(group_name) + 1 :]
                if "." not in suffix and child_path in groups_by_name:
                    group.AddItem(groups_by_name[child_path])

    # ==========================================================================
    # Phase 5: Pad-to-net assignments (creates nets on-demand)
    # ==========================================================================

    for entity_id in view.footprints:
        fp = fps_by_entity_id.get(entity_id)
        if not fp:
            continue

        for net_name, net in view.nets.items():
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
    # Fragment loading: tries to load layout fragments; falls back to HierPlace on failure

    placed_count = _run_hierarchical_placement(
        changeset,
        view,
        fps_by_entity_id,
        groups_by_name,
        kicad_board,
        pcbnew,
        oplog,
        board_path,
    )
    if placed_count > 0:
        logger.info(f"HierPlace: placed {placed_count} items")

    return oplog


def _apply_fragment_routing(
    group: Any,
    group_view: GroupView,
    entity_id: EntityId,
    fragment_data: "FragmentDataType",
    board_view: BoardView,
    kicad_board: Any,
    pcbnew: Any,
    board_path: Path,
    oplog: OpLog,
    move_delta: Tuple[int, int] = (0, 0),
) -> None:
    """Apply routing from a layout fragment to a new group.

    Uses pre-loaded FragmentData for net remapping, but still loads the fragment
    board to duplicate KiCad objects (tracks, vias, zones, graphics).

    Args:
        move_delta: (dx, dy) offset to apply to duplicated items (from group move)
    """
    if not group_view.layout_path:
        return

    group_complement = fragment_data.group_complement
    if group_complement.is_empty:
        return

    group_name = str(entity_id.path)

    # Load fragment board to get actual KiCad objects to duplicate
    layout_path = Path(group_view.layout_path)
    if not layout_path.is_absolute():
        layout_path = board_path.parent / layout_path
    layout_file = layout_path / "layout.kicad_pcb"

    if not layout_file.exists():
        return

    fragment_board = pcbnew.LoadBoard(str(layout_file))

    # Build net remapping from fragment nets to board nets
    from .lens import build_fragment_net_remap

    board_pad_net_map: Dict[Tuple[EntityId, str], str] = {}
    for net_name, net_view in board_view.nets.items():
        for conn_entity_id, pad_name in net_view.connections:
            board_pad_net_map[(conn_entity_id, pad_name)] = net_name

    member_paths = [m.path for m in group_view.member_ids]
    net_remap, warnings = build_fragment_net_remap(
        entity_id.path, member_paths, fragment_data.pad_net_map, board_pad_net_map
    )
    for warning in warnings:
        logger.warning(warning)

    valid_nets = set(board_view.nets.keys()) | {""}

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

    def _set_net(item: Any, fragment_net_name: str) -> None:
        """Set net on item, remapping from fragment net to board net."""
        if not fragment_net_name:
            return
        board_net_name = net_remap.get(fragment_net_name, fragment_net_name)
        if board_net_name not in valid_nets:
            board_net_name = ""
        if not board_net_name:
            return
        net_info = kicad_board.FindNet(board_net_name)
        if not net_info:
            net_info = pcbnew.NETINFO_ITEM(kicad_board, board_net_name)
            kicad_board.Add(net_info)
            oplog.net_add(board_net_name)
        item.SetNet(net_info)

    # Build move offset vector
    offset = pcbnew.VECTOR2I(move_delta[0], move_delta[1])

    def _dup_and_add(src: Any, net_name: str = "") -> Any:
        """Duplicate item, offset it, set net, and add to board+group."""
        item = src.Duplicate()
        if net_name:
            _set_net(item, net_name)
        kicad_board.Add(item)
        item.Move(offset)
        group.AddItem(item)
        return item

    # Duplicate tracks
    tracks_created = 0
    for track_comp in group_complement.tracks:
        src = fragment_tracks.get(track_comp.uuid)
        if src:
            track = _dup_and_add(src, track_comp.net_name)
            start = track.GetStart()
            end = track.GetEnd()
            width = track.GetWidth() if hasattr(track, "GetWidth") else 0
            remapped_net = net_remap.get(track_comp.net_name, track_comp.net_name)
            oplog.frag_track(
                group_name,
                remapped_net or "(no-net)",
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
            via = _dup_and_add(src, via_comp.net_name)
            pos = via.GetPosition()
            drill = via.GetDrill() if hasattr(via, "GetDrill") else 0
            remapped_net = net_remap.get(via_comp.net_name, via_comp.net_name)
            oplog.frag_via(
                group_name, remapped_net or "(no-net)", pos.x, pos.y, drill=drill
            )
            vias_created += 1

    # Duplicate zones
    zones_created = 0
    for zone_comp in group_complement.zones:
        src = fragment_zones.get(zone_comp.uuid)
        if src:
            zone = _dup_and_add(src, zone_comp.net_name)
            remapped_net = net_remap.get(zone_comp.net_name, zone_comp.net_name)
            oplog.frag_zone(
                group_name,
                remapped_net or "(no-net)",
                fragment_board.GetLayerName(zone.GetLayer()),
                zone.GetZoneName() or "",
            )
            zones_created += 1

    # Duplicate graphics
    graphics_created = 0
    for gr_comp in group_complement.graphics:
        src = fragment_graphics.get(gr_comp.uuid)
        if src:
            graphic = _dup_and_add(src)
            oplog.frag_graphic(
                group_name,
                graphic.GetClass(),
                fragment_board.GetLayerName(graphic.GetLayer()),
            )
            graphics_created += 1

    if tracks_created or vias_created or zones_created or graphics_created:
        logger.info(
            f"Applied fragment routing to {entity_id}: "
            f"{tracks_created} tracks, {vias_created} vias, {zones_created} zones, "
            f"{graphics_created} graphics"
        )


def _build_group_tree(
    changeset: "SyncChangeset",
    plan: FragmentPlan,
    exclude_footprints: Optional[Set[EntityId]] = None,
) -> Dict[Optional[EntityId], List[EntityId]]:
    """Build a tree of groups and footprints by parent EntityId.

    Returns a dict mapping parent_entity_id -> list of child EntityIds.
    Root-level items have parent = None.

    Uses FragmentPlan to exclude covered descendants - they're handled
    entirely at the authoritative fragment level.
    """
    from collections import defaultdict

    tree: Dict[Optional[EntityId], List[EntityId]] = defaultdict(list)
    added_group_ids: Set[EntityId] = set(changeset.added_groups)
    exclude = exclude_footprints or set()

    def parent_for(entity_id: EntityId) -> Optional[EntityId]:
        """Walk up ancestry to find nearest added group parent."""
        p = entity_id.path.parent()
        while p:
            pid = EntityId(path=p)
            # Don't attach under authoritative fragment groups - they're rigid blocks
            if plan.is_authoritative(pid):
                return None
            # First ancestor that is an added group wins
            if pid in added_group_ids:
                return pid
            p = p.parent()
        return None

    # Add groups (excluding covered descendants)
    for gid in changeset.added_groups:
        if plan.is_covered(gid):
            continue
        tree[parent_for(gid)].append(gid)

    # Add footprints (excluding inherited ones and covered descendants)
    for fid in changeset.added_footprints:
        if fid in exclude or plan.is_covered(fid):
            continue
        tree[parent_for(fid)].append(fid)

    # Sort children for determinism
    for children in tree.values():
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


def _apply_fragment_positions(
    plan: FragmentPlan,
    inherited: Set[EntityId],
    fps_by_entity_id: Dict[EntityId, Any],
    pcbnew: Any,
    oplog: OpLog,
) -> Tuple[int, Set[EntityId]]:
    """Apply positions from fragments to ALL descendant footprints (Rule B).

    For each authoritative fragment:
    1. Position footprints that ARE in the fragment
    2. Pack orphans (descendants NOT in fragment) near the fragment bbox

    Uses pre-computed descendant_footprints from FragmentPlan.
    Returns (count, positioned_ids).
    """
    placed = 0
    positioned: Set[EntityId] = set()

    for gid in sorted(plan.loaded.keys(), key=lambda e: str(e.path)):
        fragment_data = plan.loaded[gid]
        descendant_fps = [
            fid for fid in plan.descendant_footprints[gid] if fid not in inherited
        ]

        # Separate into in-fragment (have positions) and orphans (don't)
        in_fragment: List[Tuple[EntityId, FootprintComplement]] = []
        orphans: List[EntityId] = []

        for fid in descendant_fps:
            fp = fps_by_entity_id.get(fid)
            if not fp:
                continue
            comp = _lookup_fragment_complement(fid, gid, fragment_data)
            if comp:
                in_fragment.append((fid, comp))
            else:
                orphans.append(fid)

        # Apply positions to footprints that ARE in the fragment
        for fid, comp in in_fragment:
            fp = fps_by_entity_id.get(fid)
            if fp:
                apply_footprint_placement(fp, comp, pcbnew)
                positioned.add(fid)
                placed += 1
                oplog.place_fp_fragment(
                    str(fid.path), comp.position.x, comp.position.y, str(gid.path)
                )

        # Pack orphans near the fragment bbox (Rule B step 2)
        if orphans:
            count, orphan_positioned = _pack_orphans(
                orphans, in_fragment, gid, fps_by_entity_id, pcbnew, oplog
            )
            placed += count
            positioned.update(orphan_positioned)

    return placed, positioned


def _lookup_fragment_complement(
    fid: EntityId, gid: EntityId, fragment_data: "FragmentDataType"
) -> Optional[FootprintComplement]:
    """Look up footprint complement from fragment by relative path or name."""
    rel = fid.path.relative_to(gid.path)
    comp = fragment_data.footprint_complements.get(str(rel)) if rel else None
    return comp or fragment_data.footprint_complements.get(fid.path.name)


def _pack_orphans(
    orphans: List[EntityId],
    in_fragment: List[Tuple[EntityId, FootprintComplement]],
    gid: EntityId,
    fps_by_entity_id: Dict[EntityId, Any],
    pcbnew: Any,
    oplog: OpLog,
) -> Tuple[int, Set[EntityId]]:
    """Pack orphan footprints to the right of the fragment bbox.

    Uses unified helpers: _build_rects_from_footprints, hierplace, _move_footprint_to.
    """
    orphan_rects = _build_rects_from_footprints(orphans, fps_by_entity_id, pcbnew)
    if not orphan_rects:
        return 0, set()

    # Fragment bbox as anchor (footprints that ARE in the fragment)
    fragment_fps = [fps_by_entity_id[fid] for fid, _ in in_fragment if fid in fps_by_entity_id]
    fragment_bbox = _compute_items_bbox(fragment_fps, pcbnew)

    orphan_layout = hierplace(orphan_rects, anchor=fragment_bbox, margin=5_000_000)

    placed = 0
    positioned: Set[EntityId] = set()
    for fid, (target_x, target_y) in orphan_layout.items():
        fp = fps_by_entity_id.get(fid)
        if fp and _move_footprint_to(fp, target_x, target_y, pcbnew):
            positioned.add(fid)
            placed += 1
            oplog.place_fp_orphan(str(fid.path), target_x, target_y, str(gid.path))

    return placed, positioned


def _run_hierarchical_placement(
    changeset: "SyncChangeset",
    board_view: BoardView,
    fps_by_entity_id: Dict[EntityId, Any],
    groups_by_name: Dict[str, Any],
    kicad_board: Any,
    pcbnew: Any,
    oplog: OpLog,
    board_path: Path,
) -> int:
    """Position new items using HierPlace rules.

    Rule A: Top-most fragment wins (authoritative fragments)
    Rule B: Authoritative fragments handle all descendants
    Rule C: Non-fragment groups use pure bottom-up HierPlace
    Rule D: Root integration with existing content
    """
    placed, inherited = _apply_position_inheritance(
        changeset, fps_by_entity_id, pcbnew, oplog
    )

    if not (changeset.added_footprints - inherited) and not changeset.added_groups:
        return placed

    # Build fragment plan (centralizes Rule A logic)
    plan = _build_fragment_plan(changeset, board_view, board_path, pcbnew, oplog)

    # Rule B: Apply fragment positions to all descendants (including orphan packing)
    frag_placed, fragment_fps = _apply_fragment_positions(
        plan, inherited, fps_by_entity_id, pcbnew, oplog
    )
    placed += frag_placed
    exclude = inherited | fragment_fps

    # Rule C & D: Build tree excluding fragment descendants
    tree = _build_group_tree(changeset, plan, exclude_footprints=exclude)
    if not tree:
        return placed

    # Collect sizes (excluding fragment descendants)
    sizes = _collect_item_sizes(
        changeset, fps_by_entity_id, groups_by_name, pcbnew, plan, exclude
    )
    existing_bbox = _compute_existing_bbox(
        kicad_board, set(changeset.added_footprints), pcbnew
    )
    layout = _compute_hierarchical_layout(
        tree, sizes, set(changeset.added_groups), set(plan.loaded.keys()), existing_bbox
    )

    if not layout:
        return placed

    group_move_deltas: Dict[EntityId, Tuple[int, int]] = {}
    placed += _apply_hierarchical_layout(
        layout,
        changeset,
        fps_by_entity_id,
        groups_by_name,
        pcbnew,
        set(plan.loaded.keys()),
        board_view,
        oplog,
        exclude,
        group_move_deltas,
    )

    # Apply fragment routing (tracks, vias, zones) to groups
    for gid in sorted(plan.loaded.keys(), key=lambda e: str(e.path)):
        fragment_data = plan.loaded[gid]
        gv = board_view.groups.get(gid)
        group = groups_by_name.get(str(gid.path)) if gv else None
        if gv and group:
            _apply_fragment_routing(
                group,
                gv,
                gid,
                fragment_data,
                board_view,
                kicad_board,
                pcbnew,
                board_path,
                oplog,
                group_move_deltas.get(gid, (0, 0)),
            )

    return placed


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

    # Hide Value field (matches old sync behavior - Value text is on Fab layer)
    value_field = fp.GetFieldByName("Value")
    if value_field:
        value_field.SetVisible(False)

    path_str = str(view.entity_id.path)
    new_uuid = str(uuid_module.uuid5(uuid_module.NAMESPACE_URL, path_str))
    fp.SetPath(pcbnew.KIID_PATH(f"/{new_uuid}/{new_uuid}"))

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
                    diameter=track.GetWidth(pcbnew.F_Cu),
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
