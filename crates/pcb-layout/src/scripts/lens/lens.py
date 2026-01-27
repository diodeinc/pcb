"""
Core lens operations for layout synchronization.

This module implements the core lens operations:

1. get(source) -> BoardView
   Extract view from source netlist (V_new).

2. extract(dest) -> (BoardView, BoardComplement)
   Extract view and complement from destination board in one pass.

3. adapt_complement(v_new, c_old, v_old) -> BoardComplement
   Adapt complement to new view structure.

4. join(v, c) -> Board
   Combine view and complement into destination.

The sync formula:
    sync(s, d) = join(get(s), adapt_complement(get(s), *extract(d)))

Note: Renames (moved() paths) are now handled in Rust preprocessing before
the Python sync runs. Paths are already in their final form.
"""

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional, Set, Tuple, TYPE_CHECKING
import logging

if TYPE_CHECKING:
    from .changeset import SyncChangeset
    from .oplog import OpLog

from .types import (
    EntityPath,
    EntityId,
    Position,
    FootprintView,
    FootprintComplement,
    GroupView,
    GroupComplement,
    NetView,
    BoardView,
    BoardComplement,
    TrackComplement,
    ViaComplement,
    ZoneComplement,
    default_footprint_complement,
    default_group_complement,
)
from .kicad_adapter import extract_zone_outline_positions

logger = logging.getLogger("pcb.lens")


@dataclass
class FragmentData:
    """Data extracted from a layout fragment for lens logic.

    Contains pure Python dataclasses only - no KiCad C++ objects.
    The apply phase loads the fragment board fresh to duplicate items.

    Fields:
    - group_complement: Routing/graphics with fragment-local net names
    - footprint_complements: Positions keyed by reference and path
    - pad_net_map: Maps (fp_path, pad_name) -> fragment_net_name for net remapping
    """

    group_complement: GroupComplement
    footprint_complements: Dict[str, FootprintComplement]
    pad_net_map: Dict[Tuple[str, str], str] = field(default_factory=dict)


def get(netlist: Any) -> BoardView:
    """
    Derive a BoardView from the SOURCE netlist.

    This is a pure function that extracts all SOURCE-authoritative data
    and structures it for the lens.
    """
    footprints: Dict[EntityId, FootprintView] = {}
    groups: Dict[EntityId, GroupView] = {}
    nets: Dict[str, NetView] = {}

    for part in netlist.parts:
        path_str = part.sheetpath.names.split(":")[-1] if part.sheetpath.names else ""
        entity_path = EntityPath.from_string(path_str)
        # Include fpid in entity identity - FPID change = delete + create
        entity_id = EntityId(path=entity_path, fpid=part.footprint)

        fields: Dict[str, str] = {
            "Datasheet": "",
            "Description": "",
            "Path": path_str,
        }
        dnp = False
        exclude_from_bom = False
        exclude_from_pos = False

        for prop in part.properties:
            name_lower = prop.name.lower()
            if name_lower == "dnp":
                dnp = _parse_bool(prop.value)
            elif name_lower == "skip_bom":
                exclude_from_bom = _parse_bool(prop.value)
            elif name_lower == "skip_pos":
                exclude_from_pos = _parse_bool(prop.value)
            elif name_lower == "datasheet":
                fields["Datasheet"] = prop.value
            elif name_lower == "description":
                fields["Description"] = prop.value
            elif name_lower not in {
                "value",
                "reference",
                "symbol_name",
                "symbol_path",
            } and not prop.name.startswith("_"):
                field_name = prop.name.replace("_", " ").title()
                fields[field_name] = prop.value

        footprints[entity_id] = FootprintView(
            entity_id=entity_id,
            reference=part.ref,
            value=part.value,
            fpid=part.footprint,
            dnp=dnp,
            exclude_from_bom=exclude_from_bom,
            exclude_from_pos=exclude_from_pos,
            fields=fields,
        )

    if hasattr(netlist, "modules"):
        # First pass: collect all modules that qualify as meaningful groups.
        # A module qualifies if it has a layout_path OR contains multiple children.
        # Single-child modules (e.g., generic component wrappers) are not groups.
        module_paths_set = set(netlist.modules.keys())

        for module_path, module in netlist.modules.items():
            entity_path = EntityPath.from_string(module_path)
            entity_id = EntityId(path=entity_path)

            # Count direct children: footprints + submodules
            direct_footprints = [
                fp_id
                for fp_id in footprints.keys()
                if fp_id.path.parent() == entity_path
            ]
            direct_submodules = [
                sub_path
                for sub_path in module_paths_set
                if EntityPath.from_string(sub_path).parent() == entity_path
            ]

            layout_path = getattr(module, "layout_path", None)

            # Skip modules that are single-child wrappers (e.g., Resistor generic)
            # unless they have a layout_path
            total_children = len(direct_footprints) + len(direct_submodules)
            if total_children <= 1 and not layout_path:
                continue

            # Build member list - include all descendant footprints
            # (not just direct children, since nested component wrappers are skipped)
            member_ids: List[EntityId] = [
                fp_id
                for fp_id in footprints.keys()
                if entity_path.is_ancestor_of(fp_id.path)
            ]

            groups[entity_id] = GroupView(
                entity_id=entity_id,
                member_ids=tuple(member_ids),
                layout_path=layout_path,
            )

    for net in netlist.nets:
        connections: List[Tuple[EntityId, str]] = []
        for node in net.nodes:
            ref, pin_num, _pin_name = node
            for fp_id, fp_view in footprints.items():
                if fp_view.reference == ref:
                    connections.append((fp_id, pin_num))
                    break

        nets[net.name] = NetView(
            name=net.name,
            connections=tuple(connections),
        )

    return BoardView(
        footprints=footprints,
        groups=groups,
        nets=nets,
    )


def _parse_bool(value: Any) -> bool:
    """Safely parse a boolean value from a property string."""
    if value is None:
        return False
    return str(value).lower() == "true"


def extract(board: Any, pcbnew: Any) -> Tuple[BoardView, BoardComplement]:
    """
    Extract both View (π_V) and Complement (π_C) from a KiCad board in a single pass.

    This implements the decomposition D ≅ V ⊕ C from the formal spec.

    Returns:
        (view, complement) tuple where:
        - view: BoardView with footprint metadata (reference, value, fpid) for FPID comparison
        - complement: BoardComplement with placement data (position, orientation, routing)
    """
    # Footprint data
    footprint_views: Dict[EntityId, FootprintView] = {}
    footprint_complements: Dict[EntityId, FootprintComplement] = {}

    # Group data
    group_views: Dict[EntityId, GroupView] = {}
    group_complements: Dict[EntityId, GroupComplement] = {}

    # Net data (view only)
    net_connections: Dict[str, List[Tuple[EntityId, str]]] = {}

    # ─────────────────────────────────────────────────────────────────────────
    # Extract footprints: both view and complement in one pass
    # ─────────────────────────────────────────────────────────────────────────
    for fp in board.GetFootprints():
        path_field = fp.GetFieldByName("Path")
        if not path_field:
            continue

        path_str = path_field.GetText()
        if not path_str:
            continue

        entity_path = EntityPath.from_string(path_str)
        fpid = fp.GetFPIDAsString()
        # Include fpid in entity identity - matches get() behavior
        entity_id = EntityId(path=entity_path, fpid=fpid)

        # ─────────────────────────────────────────────────────────────────────
        # Extract View (SOURCE-authoritative metadata)
        # ─────────────────────────────────────────────────────────────────────
        fields: Dict[str, str] = {}
        for field_obj in fp.GetFields():
            field_name = field_obj.GetName()
            if field_name not in {"Reference", "Value", "Footprint"}:
                fields[field_name] = field_obj.GetText()

        footprint_views[entity_id] = FootprintView(
            entity_id=entity_id,
            reference=fp.GetReference(),
            value=fp.GetValue(),
            fpid=fpid,
            dnp=fp.IsDNP(),
            exclude_from_bom=fp.IsExcludedFromBOM(),
            exclude_from_pos=fp.IsExcludedFromPosFiles(),
            fields=fields,
        )

        # ─────────────────────────────────────────────────────────────────────
        # Extract Complement (user-authored placement data)
        # ─────────────────────────────────────────────────────────────────────
        pos = fp.GetPosition()
        ref_field = fp.Reference()
        val_field = fp.Value()

        ref_pos = ref_field.GetPosition()
        val_pos = val_field.GetPosition()

        layer = "B.Cu" if fp.GetLayer() == pcbnew.B_Cu else "F.Cu"

        footprint_complements[entity_id] = FootprintComplement(
            position=Position(x=pos.x, y=pos.y),
            orientation=fp.GetOrientation().AsDegrees(),
            layer=layer,
            locked=fp.IsLocked(),
            reference_position=Position(x=ref_pos.x, y=ref_pos.y),
            reference_visible=ref_field.IsVisible(),
            value_position=Position(x=val_pos.x, y=val_pos.y),
            value_visible=val_field.IsVisible(),
        )

        # ─────────────────────────────────────────────────────────────────────
        # Extract pad connections for nets
        # ─────────────────────────────────────────────────────────────────────
        for pad in fp.Pads():
            net = pad.GetNet()
            if net:
                net_name = net.GetNetname()
                if net_name:
                    pad_name = pad.GetPadName()
                    if net_name not in net_connections:
                        net_connections[net_name] = []
                    net_connections[net_name].append((entity_id, pad_name))

    # ─────────────────────────────────────────────────────────────────────────
    # Extract groups: view + complement
    # ─────────────────────────────────────────────────────────────────────────
    for group in board.Groups():
        group_name = group.GetName()
        if not group_name:
            continue

        entity_path = EntityPath.from_string(group_name)
        entity_id = EntityId(path=entity_path)

        # Find members (footprints whose path is a descendant)
        member_ids: List[EntityId] = []
        for fp_id in footprint_views.keys():
            if entity_path.is_ancestor_of(fp_id.path):
                member_ids.append(fp_id)

        group_views[entity_id] = GroupView(
            entity_id=entity_id,
            member_ids=tuple(member_ids),
            layout_path=None,  # Not stored in KiCad
        )

        # Extract group complement (routing within the group)
        tracks: List[TrackComplement] = []
        vias: List[ViaComplement] = []
        zones: List[ZoneComplement] = []

        for item in group.GetItems():
            item_class = item.GetClass().upper()
            item_uuid = str(item.m_Uuid.AsString())

            if "VIA" in item_class:
                net = item.GetNet()
                net_name = net.GetNetname() if net else ""
                pos = item.GetPosition()
                vias.append(
                    ViaComplement(
                        uuid=item_uuid,
                        position=Position(x=pos.x, y=pos.y),
                        diameter=item.GetWidth(),
                        drill=item.GetDrill(),
                        via_type="through",
                        net_name=net_name,
                    )
                )
            elif "TRACK" in item_class or "ARC" in item_class:
                net = item.GetNet()
                net_name = net.GetNetname() if net else ""
                start = item.GetStart()
                end = item.GetEnd()
                tracks.append(
                    TrackComplement(
                        uuid=item_uuid,
                        start=Position(x=start.x, y=start.y),
                        end=Position(x=end.x, y=end.y),
                        width=item.GetWidth(),
                        layer=board.GetLayerName(item.GetLayer()),
                        net_name=net_name,
                    )
                )
            elif "ZONE" in item_class:
                net = item.GetNet()
                net_name = net.GetNetname() if net else ""
                positions = extract_zone_outline_positions(item)
                zones.append(
                    ZoneComplement(
                        uuid=item_uuid,
                        name=item.GetZoneName() or "",
                        outline=tuple(positions),
                        layer=board.GetLayerName(item.GetLayer()),
                        priority=item.GetAssignedPriority(),
                        net_name=net_name,
                    )
                )

        group_complements[entity_id] = GroupComplement(
            tracks=tuple(tracks),
            vias=tuple(vias),
            zones=tuple(zones),
        )

    # Build net views
    net_views: Dict[str, NetView] = {}
    for net_name, connections in net_connections.items():
        net_views[net_name] = NetView(
            name=net_name,
            connections=tuple(connections),
        )

    view = BoardView(
        footprints=footprint_views,
        groups=group_views,
        nets=net_views,
    )

    complement = BoardComplement(
        footprints=footprint_complements,
        groups=group_complements,
    )

    return view, complement


def build_fragment_net_remap(
    group_path: EntityPath,
    member_paths: List[EntityPath],
    fragment_pad_net_map: Dict[Tuple[str, str], str],
    board_pad_net_map: Dict[Tuple[EntityId, str], str],
) -> Tuple[Dict[str, str], List[str]]:
    """Build a net remapping from fragment-local nets to board nets.

    For each footprint in the group, find what net each pad connects to in the board,
    and create a mapping from the fragment's local net name to the board's net name.

    Returns (net_remap, warnings) tuple.
    """
    net_remap: Dict[str, str] = {}
    warnings: List[str] = []
    group_path_str = str(group_path)

    for member_path in member_paths:
        member_path_str = str(member_path)

        # Get relative path within the group (for matching with fragment footprints)
        if member_path_str.startswith(group_path_str + "."):
            relative_path = member_path_str[len(group_path_str) + 1 :]
        else:
            relative_path = member_path_str

        # Find what nets this footprint connects to in the board via board_pad_net_map
        # We need to find all (entity_id, pad_name) entries where entity_id.path == member_path
        for (entity_id, pad_name), board_net_name in board_pad_net_map.items():
            if entity_id.path != member_path:
                continue

            # Look up the corresponding local net in the fragment
            fragment_net = fragment_pad_net_map.get((relative_path, pad_name))
            if fragment_net:
                if (
                    fragment_net in net_remap
                    and net_remap[fragment_net] != board_net_name
                ):
                    warnings.append(
                        f"Net remap conflict: {fragment_net} -> {net_remap[fragment_net]} vs {board_net_name}"
                    )
                else:
                    net_remap[fragment_net] = board_net_name

    return net_remap, warnings


def _remap_routing_nets(items: tuple, net_remap: Dict[str, str]) -> tuple:
    """Remap net names in routing items using dataclasses.replace()."""
    from dataclasses import replace

    result = []
    for item in items:
        new_net = net_remap.get(item.net_name, item.net_name)
        if new_net != item.net_name:
            result.append(replace(item, net_name=new_net))
        else:
            result.append(item)
    return tuple(result)


def adapt_complement(
    new_view: BoardView,
    old_complement: BoardComplement,
    fragment_loader: Optional[Callable[[str], FragmentData]] = None,
) -> BoardComplement:
    """
    Adapt old Complement to match the structure of new View.

    This is the core lens operation: α(v_new, c_old) → c_new

    EntityId now includes fpid, so FPID changes naturally become
    remove (old path+fpid) + add (new path+fpid) operations.

    Note: Renames (moved() paths) are handled in Rust preprocessing.
    Paths in new_view and old_complement are already in their final form.

    Parameters:
        new_view: V_new from get(source)
        old_complement: C_old from extract(dest)[1]
        fragment_loader: Optional callable to load FragmentData for layout fragments

    Returns the adapted BoardComplement.
    """
    new_footprints: Dict[EntityId, FootprintComplement] = {}
    new_groups: Dict[EntityId, GroupComplement] = {}

    # ═══════════════════════════════════════════════════════════════════════════
    # Adapt footprint complements
    # EntityId includes fpid, so exact match means same path AND same fpid
    # ═══════════════════════════════════════════════════════════════════════════
    for entity_id, fp_view in new_view.footprints.items():
        existing = old_complement.footprints.get(entity_id)

        if existing:
            # Exact match (same path + same fpid) - preserve complement
            new_footprints[entity_id] = existing
        else:
            # New footprint (or FPID changed - which is a new identity)
            fragment_complement = _get_fragment_footprint_complement(
                entity_id, new_view, fragment_loader
            )
            new_footprints[entity_id] = (
                fragment_complement or default_footprint_complement()
            )

    # ═══════════════════════════════════════════════════════════════════════════
    # Adapt group complements
    # ═══════════════════════════════════════════════════════════════════════════
    for entity_id, group_view in new_view.groups.items():
        existing = old_complement.groups.get(entity_id)

        if existing:
            new_groups[entity_id] = existing
        else:
            group_complement = _get_fragment_group_complement(
                entity_id, group_view, new_view, fragment_loader
            )
            new_groups[entity_id] = group_complement

    new_complement = BoardComplement(
        footprints=new_footprints,
        groups=new_groups,
    )

    check_lens_invariants(new_view, new_complement)

    return new_complement


def _get_fragment_footprint_complement(
    entity_id: EntityId,
    view: BoardView,
    fragment_loader: Optional[Callable[[str], FragmentData]],
) -> Optional[FootprintComplement]:
    """Get footprint complement from parent module's layout fragment.

    Walks up the hierarchy and finds the OUTERMOST layout that contains this footprint.
    Outer layouts take precedence (e.g., parent can override submodule positioning).
    """
    if not fragment_loader:
        return None

    # Collect all parent groups with layout_paths
    candidates: List[Tuple[EntityPath, GroupView]] = []
    parent_path = entity_id.path.parent()
    while parent_path:
        parent_id = EntityId(path=parent_path)
        group_view = view.groups.get(parent_id)
        if group_view and group_view.layout_path:
            candidates.append((parent_path, group_view))
        parent_path = parent_path.parent()

    # Try outermost first (reverse order)
    for parent_path, group_view in reversed(candidates):
        try:
            fragment_data = fragment_loader(group_view.layout_path)
        except Exception as e:
            logger.warning(f"Failed to load fragment {group_view.layout_path}: {e}")
            continue

        # Look up by relative path first, then by name
        relative_path = entity_id.path.relative_to(parent_path)
        if relative_path:
            path_key = str(relative_path)
            if path_key in fragment_data.footprint_complements:
                return fragment_data.footprint_complements[path_key]
        name_key = entity_id.path.name
        if name_key in fragment_data.footprint_complements:
            return fragment_data.footprint_complements[name_key]

    return None


def _get_fragment_group_complement(
    entity_id: EntityId,
    group_view: GroupView,
    board_view: BoardView,
    fragment_loader: Optional[Callable[[str], FragmentData]],
) -> GroupComplement:
    """Get group complement from layout fragment, with net remapping.

    If group has no layout_path or fragment fails to load, returns default.
    """
    if not group_view.layout_path or not fragment_loader:
        return default_group_complement()

    try:
        fragment_data = fragment_loader(group_view.layout_path)
    except Exception as e:
        logger.warning(
            f"Failed to load fragment {group_view.layout_path} for {entity_id}: {e}"
        )
        return default_group_complement()

    # Build net remapping from fragment nets to board nets
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

    gc = fragment_data.group_complement
    return GroupComplement(
        tracks=_remap_routing_nets(gc.tracks, net_remap),
        vias=_remap_routing_nets(gc.vias, net_remap),
        zones=_remap_routing_nets(gc.zones, net_remap),
        graphics=gc.graphics,
    )


def check_lens_invariants(view: BoardView, complement: BoardComplement) -> None:
    """
    Verify the four lens laws hold for a view/complement pair.

    Raises AssertionError with descriptive message if any invariant fails.
    """
    view_fp_keys = set(view.footprints.keys())
    complement_fp_keys = set(complement.footprints.keys())
    if complement_fp_keys != view_fp_keys:
        missing_in_complement = view_fp_keys - complement_fp_keys
        extra_in_complement = complement_fp_keys - view_fp_keys
        msg = "Law 1 & 4 - Domain Alignment (footprints): complement keys must match view keys."
        if missing_in_complement:
            msg += f" Missing in complement: {missing_in_complement}."
        if extra_in_complement:
            msg += f" Extra in complement: {extra_in_complement}."
        raise AssertionError(msg)

    view_group_keys = set(view.groups.keys())
    complement_group_keys = set(complement.groups.keys())
    if complement_group_keys != view_group_keys:
        missing_in_complement = view_group_keys - complement_group_keys
        extra_in_complement = complement_group_keys - view_group_keys
        msg = "Law 1 & 4 - Domain Alignment (groups): complement keys must match view keys."
        if missing_in_complement:
            msg += f" Missing in complement: {missing_in_complement}."
        if extra_in_complement:
            msg += f" Extra in complement: {extra_in_complement}."
        raise AssertionError(msg)

    # ─────────────────────────────────────────────────────────────────────────
    # Inv_NoLeafGroups: Groups represent modules, not individual footprints
    # ─────────────────────────────────────────────────────────────────────────
    fp_paths = {fp_id.path for fp_id in view.footprints.keys()}
    for group_id in view.groups.keys():
        if group_id.path in fp_paths:
            raise AssertionError(
                f"NoLeafGroups: Group path {group_id.path} equals a footprint path. "
                f"Groups should represent modules, not individual footprints."
            )

    # ─────────────────────────────────────────────────────────────────────────
    # Inv_GroupMembersAreFootprints: All members must be footprints
    # Inv_GroupMembersAreDescendants: All members must be descendants of group
    # ─────────────────────────────────────────────────────────────────────────
    for group_id, group_view in view.groups.items():
        for member_id in group_view.member_ids:
            if member_id not in view.footprints:
                raise AssertionError(
                    f"GroupMembersAreFootprints: Group {group_id.path} has member "
                    f"{member_id.path} which is not a footprint."
                )
            if not group_id.path.is_ancestor_of(member_id.path):
                raise AssertionError(
                    f"GroupMembersAreDescendants: Member {member_id.path} is not a "
                    f"descendant of group {group_id.path}."
                )

    # ─────────────────────────────────────────────────────────────────────────
    # Inv_GroupHasPurpose: Groups must have members, layout_path, or complement
    # ─────────────────────────────────────────────────────────────────────────
    for group_id, group_view in view.groups.items():
        has_members = len(group_view.member_ids) > 0
        has_layout = bool(group_view.layout_path)
        group_comp = complement.groups.get(group_id)
        has_complement = group_comp is not None and not group_comp.is_empty

        if not (has_members or has_layout or has_complement):
            raise AssertionError(
                f"GroupHasPurpose: Group {group_id.path} has no members, "
                f"no layout_path, and empty complement."
            )

    valid_nets = set(view.nets.keys()) | {""}

    def _check_routing_nets(items, context: str) -> None:
        """Check that all routing items have valid net names."""
        for item in items:
            if item.net_name not in valid_nets:
                raise AssertionError(
                    f"Law 4 - No Routing on Unknown Nets: {context} has unknown net '{item.net_name}'"
                )

    for group_id, group_comp in complement.groups.items():
        prefix = f"group {group_id.path}"
        _check_routing_nets(group_comp.tracks, f"{prefix} track")
        _check_routing_nets(group_comp.vias, f"{prefix} via")
        _check_routing_nets(group_comp.zones, f"{prefix} zone")


# =============================================================================
# Sync Pipeline (main entry point)
# =============================================================================


def make_fragment_loader(board_dir: Path, pcbnew: Any) -> Callable[[str], FragmentData]:
    """Create a fragment loader with internal caching."""
    from .kicad_adapter import load_layout_fragment_with_footprints

    cache: Dict[str, FragmentData] = {}

    def load_fragment(layout_path: str) -> FragmentData:
        if layout_path in cache:
            return cache[layout_path]
        data = load_layout_fragment_with_footprints(layout_path, board_dir, pcbnew)
        cache[layout_path] = data
        return data

    return load_fragment


@dataclass
class SyncResult:
    """Result of a sync operation."""

    changeset: "SyncChangeset"
    tracking: Dict[str, Set[str]] = field(default_factory=dict)
    diagnostics: List[Dict[str, Any]] = field(default_factory=list)
    applied: bool = False
    oplog: Optional["OpLog"] = None


def run_lens_sync(
    netlist: Any,
    kicad_board: Any,
    pcbnew: Any,
    board_path: Path,
    footprint_lib_map: Dict[str, str],
    groups_registry: Dict[str, Any],
    dry_run: bool = False,
) -> SyncResult:
    """Run the lens-based sync pipeline.

    This is the main entry point called by ImportNetlist in update_layout_file.py.
    """
    import time
    from .kicad_adapter import apply_changeset
    from .changeset import (
        SyncChangeset,
        build_sync_changeset,
        log_lens_state,
        log_changeset,
    )

    start_time = time.time()
    logger.info("Starting lens-based layout sync")

    new_view = get(netlist)
    dest_view, old_complement = extract(kicad_board, pcbnew)

    logger.info(
        f"Source: {len(new_view.footprints)} footprints, "
        f"{len(new_view.groups)} groups, {len(new_view.nets)} nets"
    )

    # Log OLD state (destination before sync)
    log_lens_state("OLD", dest_view, old_complement, logger)

    fragment_loader = make_fragment_loader(board_path.parent, pcbnew)

    new_complement = adapt_complement(
        new_view,
        old_complement,
        fragment_loader=fragment_loader,
    )

    check_lens_invariants(new_view, new_complement)

    changeset = build_sync_changeset(
        new_view=new_view,
        new_complement=new_complement,
        old_complement=old_complement,
    )

    logger.info(
        f"Changes: +{len(changeset.added_footprints)} -{len(changeset.removed_footprints)} footprints"
    )

    # Log NEW state (after lens computation)
    log_lens_state("NEW", new_view, new_complement, logger)

    # Log changeset
    log_changeset(changeset, logger)

    if dry_run:
        return SyncResult(
            changeset=changeset,
            diagnostics=changeset.to_diagnostics(),
            applied=False,
        )

    changeset_text = changeset.to_plaintext()
    changeset = SyncChangeset.from_plaintext(
        changeset_text, changeset.view, changeset.complement
    )

    apply_tracking, oplog = apply_changeset(
        changeset,
        kicad_board,
        pcbnew,
        footprint_lib_map,
        groups_registry,
        board_path,
    )

    # Log oplog
    oplog.log_to(logger)

    logger.info(f"Sync completed in {time.time() - start_time:.3f}s")

    return SyncResult(
        changeset=changeset,
        tracking=apply_tracking,
        diagnostics=[],
        applied=True,
        oplog=oplog,
    )
