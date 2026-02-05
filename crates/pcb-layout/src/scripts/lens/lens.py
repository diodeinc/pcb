"""
Core lens operations for layout synchronization.

This module implements the core lens operations:

1. get(source) -> BoardView
   Extract view from source netlist.

2. extract(dest) -> (BoardView, BoardComplement)
   Extract view and complement from destination board.

3. adapt_complement(v_new, c_old, v_old) -> BoardComplement
   Adapt complement to new view structure.

These operations enable SOURCE-driven synchronization where view data
comes from the netlist and complement (placement) data is preserved
from the destination.

Note: Renames (moved() paths) are now handled in Rust preprocessing before
the Python sync runs. Paths are already in their final form.
"""

from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple, TYPE_CHECKING
import logging
import uuid as uuid_module

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

    fp_id_by_ref: Dict[str, EntityId] = {
        fp_view.reference: fp_id for fp_id, fp_view in footprints.items()
    }

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
        net_kind = getattr(net, "kind", "Net")

        # Normalize nodes to strings. Each node is (refdes, pad_num, pin_name).
        nodes: List[Tuple[str, str, str]] = [
            (str(ref), str(pad_num), str(pin_name))
            for ref, pad_num, pin_name in net.nodes
        ]

        # Compute logical ports (component refdes + pin/port name), independent of pad fanout.
        # The third element in the node tuple is a logical pin/port name.
        logical_ports_set: set[tuple[str, str]] = set()
        pad_nums_by_logical_port: Dict[Tuple[str, str], set[str]] = {}
        for ref, pad_num, pin_name in nodes:
            port = (ref, pin_name)
            logical_ports_set.add(port)
            pad_nums_by_logical_port.setdefault(port, set()).add(pad_num)
        logical_ports = tuple(sorted(logical_ports_set))

        # Special-case: NotConnected net connected to exactly one logical port, but that
        # port fans out to multiple pads. In this case, create a distinct net per pad so
        # those pads do not get electrically tied together.
        if net_kind == "NotConnected" and len(logical_ports) == 1:
            (ref, pin_name) = logical_ports[0]
            pad_nums = sorted(
                pad_nums_by_logical_port.get((ref, pin_name), set()),
                key=_pad_sort_key,
            )
            fp_id = fp_id_by_ref.get(ref)
            if fp_id and len(pad_nums) > 1:
                for pad_num in pad_nums:
                    unconnected_name = _unique_net_name(
                        _unconnected_net_name(fp_id.path, ref, pad_num),
                        nets,
                    )
                    nets[unconnected_name] = NetView(
                        name=unconnected_name,
                        connections=((fp_id, pad_num),),
                        kind=net_kind,
                        logical_ports=logical_ports,
                    )
                continue

        connections_list: List[Tuple[EntityId, str]] = []
        seen_connections: set[Tuple[EntityId, str]] = set()
        for ref, pad_num, _pin_name in nodes:
            fp_id = fp_id_by_ref.get(ref)
            if not fp_id:
                continue
            conn = (fp_id, pad_num)
            if conn in seen_connections:
                continue
            seen_connections.add(conn)
            connections_list.append(conn)

        # Treat NotConnected nets as normal nets for connectivity purposes.
        # Any "no connect" behavior is expressed via pad pin type (see kicad_adapter).
        nets[net.name] = NetView(
            name=net.name,
            connections=tuple(connections_list),
            kind=net_kind,
            logical_ports=logical_ports,
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


def _unconnected_net_name(path: EntityPath, ref: str, pad_num: str) -> str:
    """Generate KiCad-style unconnected net name for a single pad."""
    path_str = str(path) or str(ref)
    return f"unconnected-({path_str}:{pad_num})"


def _unique_net_name(base: str, existing: Dict[str, Any]) -> str:
    """Return a name that's not already in `existing` (dict keyed by net name)."""
    if base not in existing:
        return base
    i = 2
    while True:
        candidate = f"{base}__{i}"
        if candidate not in existing:
            return candidate
        i += 1


def _pad_sort_key(pad_num: str) -> tuple[int, object]:
    """Sort pad numbers naturally when possible (e.g. 2 before 10)."""
    s = str(pad_num)
    if s.isdigit():
        return (0, int(s))
    return (1, s)


def extract(
    board: Any,
    pcbnew: Any,
    kiid_to_path: Optional[Dict[str, str]] = None,
    diagnostics: Optional[List[Dict[str, Any]]] = None,
) -> Tuple[BoardView, BoardComplement]:
    """
    Extract both View (π_V) and Complement (π_C) from a KiCad board in a single pass.

    This implements the decomposition D ≅ V ⊕ C from the formal spec.

    Args:
        board: KiCad board object
        pcbnew: KiCad pcbnew module
        kiid_to_path: Optional map from KIID UUID to path string (for old boards without Path field)
        diagnostics: Optional list to append warning diagnostics for unmanaged footprints

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

    if kiid_to_path is None:
        kiid_to_path = {}

    # ─────────────────────────────────────────────────────────────────────────
    # Extract footprints: both view and complement in one pass
    # ─────────────────────────────────────────────────────────────────────────
    for fp in board.GetFootprints():
        # Try Path field first (new boards)
        path_field = fp.GetFieldByName("Path")
        path_str = path_field.GetText() if path_field else ""

        # Fallback: use KIID->path map for old boards without Path field
        if not path_str:
            kiid_path = fp.GetPath().AsString()
            if kiid_path:
                parts = kiid_path.strip("/").split("/")
                if parts:
                    path_str = kiid_to_path.get(parts[-1], "")

        if not path_str:
            continue

        # ─────────────────────────────────────────────────────────────────────
        # Validate KIID_PATH matches expected value from Path field
        # This detects duplicates/extras that have Path field but wrong KIID_PATH
        # ─────────────────────────────────────────────────────────────────────
        expected_uuid = str(uuid_module.uuid5(uuid_module.NAMESPACE_URL, path_str))
        expected_kiid_path = f"/{expected_uuid}/{expected_uuid}"
        actual_kiid_path = fp.GetPath().AsString()

        if actual_kiid_path != expected_kiid_path:
            if diagnostics is not None:
                reference = fp.GetReference()
                fpid = fp.GetFPIDAsString()
                diagnostics.append(
                    {
                        "kind": "layout.sync.unmanaged_footprint",
                        "severity": "warning",
                        "body": f"Footprint {reference} ({path_str}:{fpid}) is not managed by sync",
                        "path": path_str,
                        "reference": reference,
                    }
                )
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
    # Skip KiCad internal groups (e.g., group-boardCharacteristics, group-boardStackUp)
    # ─────────────────────────────────────────────────────────────────────────
    for group in board.Groups():
        group_name = group.GetName()
        if not group_name:
            continue

        # Skip KiCad internal groups (board stackup, characteristics, etc.)
        if group_name.startswith("group-board"):
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
                        diameter=item.GetWidth(pcbnew.F_Cu),
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


def _remap_routing_nets(
    items: tuple,
    net_remap: Dict[str, str],
    valid_nets: set,
    context: str,
) -> tuple:
    """Remap fragment net names to board nets. Unmapped nets become no-net."""
    from dataclasses import replace

    result = []
    orphan_nets: List[str] = []

    for item in items:
        net = item.net_name
        mapped = net_remap.get(net, net)

        if mapped == "" or mapped in valid_nets:
            if mapped != net:
                result.append(replace(item, net_name=mapped))
            else:
                result.append(item)
        else:
            orphan_nets.append(net)
            result.append(replace(item, net_name=""))

    if orphan_nets:
        logger.warning(
            f"{context}: {len(orphan_nets)} items converted to no-net "
            f"(unmapped nets: {sorted(set(orphan_nets))[:5]})"
        )

    return tuple(result)


def adapt_complement(
    new_view: BoardView,
    old_complement: BoardComplement,
) -> BoardComplement:
    """
    Adapt old Complement to match the structure of new View.

    This is the core lens operation: α(v_new, c_old) → c_new

    EntityId now includes fpid, so FPID changes naturally become
    remove (old path+fpid) + add (new path+fpid) operations.

    Note: Renames (moved() paths) are handled in Rust preprocessing.
    Paths in new_view and old_complement are already in their final form.

    Fragment loading is now handled at HierPlace time in kicad_adapter.py,
    so this function no longer needs a fragment_loader parameter.

    Parameters:
        new_view: V_new from get(source)
        old_complement: C_old from extract(dest)[1]

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
            # New footprint - start at origin, HierPlace will position it
            # (fragment positions are applied at HierPlace time if fragment exists)
            new_footprints[entity_id] = default_footprint_complement()

    # ═══════════════════════════════════════════════════════════════════════════
    # Adapt group complements
    # ═══════════════════════════════════════════════════════════════════════════
    for entity_id, group_view in new_view.groups.items():
        existing = old_complement.groups.get(entity_id)

        if existing:
            new_groups[entity_id] = existing
        else:
            # New group - start with empty complement
            # (fragment routing is applied at HierPlace time if fragment exists)
            new_groups[entity_id] = default_group_complement()

    new_complement = BoardComplement(
        footprints=new_footprints,
        groups=new_groups,
    )

    check_lens_invariants(new_view, new_complement)

    return new_complement


def check_lens_invariants(
    view: BoardView,
    complement: BoardComplement,
    diagnostics: Optional[List[Dict[str, Any]]] = None,
) -> None:
    """
    Verify the four lens laws hold for a view/complement pair.

    Violations are appended to diagnostics if provided.
    """

    def _add_diagnostic(kind: str, severity: str, body: str) -> None:
        if diagnostics is not None:
            diagnostics.append({"kind": kind, "severity": severity, "body": body})

    view_fp_keys = set(view.footprints.keys())
    complement_fp_keys = set(complement.footprints.keys())
    if complement_fp_keys != view_fp_keys:
        missing = view_fp_keys - complement_fp_keys
        extra = complement_fp_keys - view_fp_keys
        if missing:
            _add_diagnostic(
                "layout.sync.domain_mismatch",
                "error",
                f"Footprints missing in complement: {missing}",
            )
        if extra:
            _add_diagnostic(
                "layout.sync.domain_mismatch",
                "error",
                f"Extra footprints in complement: {extra}",
            )

    view_group_keys = set(view.groups.keys())
    complement_group_keys = set(complement.groups.keys())
    if complement_group_keys != view_group_keys:
        missing = view_group_keys - complement_group_keys
        extra = complement_group_keys - view_group_keys
        if missing:
            _add_diagnostic(
                "layout.sync.domain_mismatch",
                "error",
                f"Groups missing in complement: {missing}",
            )
        if extra:
            _add_diagnostic(
                "layout.sync.domain_mismatch",
                "error",
                f"Extra groups in complement: {extra}",
            )

    fp_paths = {fp_id.path for fp_id in view.footprints.keys()}
    for group_id in view.groups.keys():
        if group_id.path in fp_paths:
            _add_diagnostic(
                "layout.sync.no_leaf_groups",
                "error",
                f"Group path {group_id.path} equals a footprint path",
            )

    for group_id, group_view in view.groups.items():
        for member_id in group_view.member_ids:
            if member_id not in view.footprints:
                _add_diagnostic(
                    "layout.sync.invalid_group_member",
                    "error",
                    f"Group {group_id.path} has member {member_id.path} which is not a footprint",
                )
            elif not group_id.path.is_ancestor_of(member_id.path):
                _add_diagnostic(
                    "layout.sync.invalid_group_member",
                    "error",
                    f"Member {member_id.path} is not a descendant of group {group_id.path}",
                )

    for group_id, group_view in view.groups.items():
        has_members = len(group_view.member_ids) > 0
        has_layout = bool(group_view.layout_path)
        group_comp = complement.groups.get(group_id)
        has_complement = group_comp is not None and not group_comp.is_empty

        if not (has_members or has_layout or has_complement):
            _add_diagnostic(
                "layout.sync.empty_group",
                "warning",
                f"Group {group_id.path} has no members, no layout_path, and empty complement",
            )

    valid_nets = set(view.nets.keys()) | {""}
    unknown_nets: set = set()

    for group_id, group_comp in complement.groups.items():
        for item in group_comp.tracks + group_comp.vias + group_comp.zones:
            if item.net_name and item.net_name not in valid_nets:
                unknown_nets.add(item.net_name)

    if unknown_nets:
        _add_diagnostic(
            "layout.sync.unknown_nets",
            "warning",
            f"Routing references {len(unknown_nets)} unknown net(s): {sorted(unknown_nets)[:5]}...",
        )


# =============================================================================
# Sync Pipeline (main entry point)
# =============================================================================


@dataclass
class SyncResult:
    """Result of a sync operation."""

    changeset: "SyncChangeset"
    diagnostics: List[Dict[str, Any]] = field(default_factory=list)
    applied: bool = False
    oplog: Optional["OpLog"] = None


def run_lens_sync(
    netlist: Any,
    kicad_board: Any,
    pcbnew: Any,
    board_path: Path,
    footprint_lib_map: Dict[str, str],
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

    diagnostics: List[Dict[str, Any]] = []

    new_view = get(netlist)

    # Build KIID->path map for old boards without Path field
    # This allows extract() to identify footprints by their KIID_PATH UUID
    kiid_to_path: Dict[str, str] = {}
    for entity_id in new_view.footprints.keys():
        kiid_to_path[entity_id.kiid_uuid] = str(entity_id.path)

    dest_view, old_complement = extract(kicad_board, pcbnew, kiid_to_path, diagnostics)

    logger.info(
        f"Source: {len(new_view.footprints)} footprints, "
        f"{len(new_view.groups)} groups, {len(new_view.nets)} nets"
    )

    # Log OLD state (destination before sync)
    log_lens_state("OLD", dest_view, old_complement, logger)

    # Fragment loading is now handled at HierPlace time in kicad_adapter.py
    new_complement = adapt_complement(
        new_view,
        old_complement,
    )

    check_lens_invariants(new_view, new_complement, diagnostics)

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

    # Log all diagnostics
    if diagnostics:
        logger.info(f"Diagnostics ({len(diagnostics)}):")
        for d in diagnostics:
            level = d.get("severity", "info").upper()
            kind = d.get("kind", "unknown")
            path = d.get("path", "")
            body = d.get("body", "")
            logger.info(f"  [{level}] {kind} @ {path}: {body}")

    if dry_run:
        # Only emit add/remove diagnostics in dry-run mode
        diagnostics.extend(changeset.to_diagnostics())
        return SyncResult(
            changeset=changeset,
            diagnostics=diagnostics,
            applied=False,
        )

    changeset_text = changeset.to_plaintext()
    changeset = SyncChangeset.from_plaintext(
        changeset_text, changeset.view, changeset.complement
    )

    oplog = apply_changeset(
        changeset,
        kicad_board,
        pcbnew,
        footprint_lib_map,
        board_path,
    )

    # Log oplog
    oplog.log_to(logger)

    logger.info(f"Sync completed in {time.time() - start_time:.3f}s")

    return SyncResult(
        changeset=changeset,
        diagnostics=diagnostics,
        applied=True,
        oplog=oplog,
    )
