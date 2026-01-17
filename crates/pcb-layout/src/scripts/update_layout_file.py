# With inspiration from
# - https://github.com/devbisme/kinet2pcb
# - https://github.com/atopile/atopile/tree/main/src/atopile/kicad_plugin
# - https://github.com/devbisme/HierPlace

"""
update_layout_file.py - Diode JSON netlist ⇆ KiCad layout synchronisation
=========================================================================

Pipeline Overview
-----------------
0. SetupBoard
   • Cosmetic tweaks (e.g. hide Fab layers) so the board opens cleanly in KiCad.

1. ImportNetlist
   • CRUD-sync footprints and nets from the JSON netlist file.

2. SyncLayouts
   • Copy tracks / graphics / zones from reusable *layout fragments* (captured as
     .kicad_pcb files + UUID maps) into the matching groups on this board.

3. PlaceComponents (first-pass placement for *new* items only)
   • Recursively pack newly-created groups bottom-up so each behaves as a rigid block.
   • Run the HierPlace heuristic (largest-first, TL/BR candidate points) with
     collision checks based on courtyard bounding boxes.

4. FinalizeBoard
   • Fill all copper zones.
   • Emit a deterministic JSON snapshot (for regression tests).
   • Save the updated *.kicad_pcb*.
"""

import argparse
import logging
import os
import os.path
import re
import time
from abc import ABC, abstractmethod
from typing import Optional, Set
from pathlib import Path
import json
import sys
import uuid
from dataclasses import dataclass
from typing import List, Dict, Tuple
from enum import Enum
from typing import Any

# Global logger.
logger = logging.getLogger("pcb")


def natural_sort_key(text: str) -> List:
    """
    Generate a sort key for natural (human-friendly) sorting.

    Splits a string into numeric and non-numeric parts, converting numeric
    parts to integers so that "C2" sorts before "C10".

    Example:
        "C10" -> ['C', 10]
        "C2"  -> ['C', 2]
        "IC1.R5" -> ['IC', 1, '.R', 5]
    """

    def convert(part):
        return int(part) if part.isdigit() else part.lower()

    return [convert(c) for c in re.split("([0-9]+)", text)]


def parse_bool(value: str) -> bool:
    """Safely parse a boolean value from a property string."""
    if value is None:
        return False
    return str(value).lower() == "true"


def export_diagnostics(diagnostics: List[Dict[str, Any]], path: Path) -> None:
    """Export diagnostics to JSON file."""
    output = {"diagnostics": diagnostics}
    with open(path, "w", encoding="utf-8") as f:
        json.dump(output, f, indent=2)
    count = len(diagnostics)
    if count > 0:
        logger.info(f"Saved {count} diagnostic(s) to {path}")


####################################################################################################
# Two-Phase Change Plan Data Structures
#
# Phase 1 (pure) computes changes without mutation. Phase 2 applies or reports them.
####################################################################################################


@dataclass
class Change:
    """An auto-fixable mutation to apply to a footprint."""

    field: str  # e.g., "reference", "value", "field:Description"
    old: Any
    new: Any


@dataclass
class ChangeSet:
    """Container for detected changes."""

    changes: List[Change]
    needs_replacement: bool = (
        False  # True if FPID changed and footprint needs full replacement
    )


# Properties that shouldn't be added as PCB fields
SKIP_PROPERTIES = {"value", "reference", "symbol_name", "symbol_path"}


def compute_footprint_changes(
    fp: Any,  # pcbnew.FOOTPRINT
    part: Any,  # JsonNetlistParser.Part
    is_existing: bool,
) -> ChangeSet:
    """Pure function: detect all changes without mutation."""
    changes: List[Change] = []

    context = part.sheetpath.names.split(":")[-1]
    ref = part.ref

    def add_change(field: str, old: Any, new: Any) -> None:
        if old != new:
            changes.append(Change(field=field, old=old, new=new))

    # FPID mismatch (non-fixable for existing footprints)
    old_fpid = fp.GetFPIDAsString()
    new_fpid = part.footprint

    if old_fpid != new_fpid and is_existing and old_fpid:
        # FPID mismatch - needs full footprint replacement
        return ChangeSet(changes=[], needs_replacement=True)

    # Basic fields
    add_change("fpid", old_fpid, new_fpid)
    add_change("reference", fp.GetReference(), ref)
    add_change("value", fp.GetValue(), part.value)

    # KiCad path
    new_kiid_path = f"{part.sheetpath.tstamps}/{part.sheetpath.tstamps}"
    add_change("kiid_path", fp.GetPath().AsString().lstrip("/"), new_kiid_path)

    # Process properties to extract flags and fields
    desired_fields: Dict[str, str] = {
        "Datasheet": "",
        "Description": "",
        "Path": context,
    }
    new_dnp, new_skip_bom, new_skip_pos = False, False, False

    for prop in part.properties:
        name_lower = prop.name.lower()
        if name_lower == "dnp":
            new_dnp = parse_bool(prop.value)
        elif name_lower == "skip_bom":
            new_skip_bom = parse_bool(prop.value)
        elif name_lower == "skip_pos":
            new_skip_pos = parse_bool(prop.value)
        elif name_lower == "datasheet":
            desired_fields["Datasheet"] = prop.value
        elif name_lower == "description":
            desired_fields["Description"] = prop.value
        elif name_lower not in SKIP_PROPERTIES and not prop.name.startswith("_"):
            desired_fields[prop.name.replace("_", " ").title()] = prop.value

    # Flags
    add_change("dnp", fp.IsDNP(), new_dnp)
    add_change("exclude_from_bom", fp.IsExcludedFromBOM(), new_skip_bom)
    add_change("exclude_from_pos", fp.IsExcludedFromPosFiles(), new_skip_pos)

    # Custom fields
    existing_fields: Dict[str, str] = {}
    for field in fp.GetFields():
        if not field.IsValue() and not field.IsReference():
            existing_fields[field.GetName()] = field.GetText()

    # Fields to remove
    for name in existing_fields:
        if name not in desired_fields:
            add_change(f"field:{name}", existing_fields[name], None)

    # Fields to add or update
    for name, desired in desired_fields.items():
        add_change(f"field:{name}", existing_fields.get(name), desired)

    # Field visibility (Path, Value should be invisible)
    for name in ["Path", "Value"]:
        field = fp.GetFieldByName(name)
        if field and field.IsVisible():
            add_change(f"field:{name}:visible", True, False)

    return ChangeSet(changes=changes)


def apply_footprint_changes(fp: Any, changes: List[Change], pcbnew_module: Any) -> None:
    """Apply computed changes to a footprint."""
    for c in changes:
        if c.field == "reference":
            fp.SetReference(c.new)
        elif c.field == "value":
            fp.SetValue(c.new)
        elif c.field == "fpid":
            fp.SetFPIDAsString(c.new)
        elif c.field == "kiid_path":
            fp.SetPath(pcbnew_module.KIID_PATH(c.new))
        elif c.field == "dnp":
            fp.SetDNP(c.new)
        elif c.field == "exclude_from_bom":
            fp.SetExcludedFromBOM(c.new)
        elif c.field == "exclude_from_pos":
            fp.SetExcludedFromPosFiles(c.new)
        elif c.field.startswith("field:"):
            parts = c.field.split(":")
            field_name = parts[1]
            if len(parts) == 3 and parts[2] == "visible":
                field = fp.GetFieldByName(field_name)
                if field:
                    field.SetVisible(c.new)
            elif c.new is None:
                fp.RemoveField(field_name)
            else:
                fp.SetField(field_name, c.new)
                field = fp.GetFieldByName(field_name)
                if field:
                    field.SetVisible(False)


def replace_footprint(
    board: Any,  # pcbnew.BOARD
    old_fp: Any,  # pcbnew.FOOTPRINT
    part: Any,  # JsonNetlistParser.Part
    lib_uri: str,
    pcbnew_module: Any,
) -> Any:
    """
    Replace a footprint with a new one from the library, preserving position, nets, and KIIDs.

    This mirrors KiCad's ExchangeFootprint behavior - we can't just update the FPID string,
    we need to load the new footprint geometry from the library.

    Returns the new footprint.
    """
    fp_lib, fp_name = part.footprint.split(":")

    # Load the new footprint from library
    new_fp = pcbnew_module.FootprintLoad(lib_uri, fp_name)
    if new_fp is None:
        raise ValueError(f"Footprint '{fp_name}' not found in library")

    new_fp.SetParent(board)

    # Copy position and orientation from old footprint
    new_fp.SetPosition(old_fp.GetPosition())
    new_fp.SetOrientation(old_fp.GetOrientation())

    # Handle layer/flip - check if we need to flip the new footprint
    old_layer = old_fp.GetLayer()
    new_layer = new_fp.GetLayer()
    old_is_back = old_layer == pcbnew_module.B_Cu
    new_is_back = new_layer == pcbnew_module.B_Cu

    if old_is_back != new_is_back:
        # Need to flip to match the old footprint's side
        pos = new_fp.GetPosition()
        new_fp.Flip(pos, True)
        logger.debug(
            f"Flipped replacement footprint to {'back' if old_is_back else 'front'} side"
        )

    # Copy pad net assignments by matching pad names (KiCad uses similarity-based matching,
    # but pad name/number matching is weighted heavily with a +2.0 bonus, so we use that)
    old_pads = {pad.GetPadName(): pad for pad in old_fp.Pads()}
    for new_pad in new_fp.Pads():
        pad_name = new_pad.GetPadName()
        if pad_name in old_pads:
            old_pad = old_pads[pad_name]
            net_code = old_pad.GetNetCode()
            if net_code > 0:
                new_pad.SetNetCode(net_code)

    # Copy schematic linkage properties (critical for maintaining PCB-schematic sync)
    # These are set by our netlist import, but we preserve them from old footprint
    # in case there are any runtime modifications
    new_fp.SetPath(old_fp.GetPath())

    # Now apply metadata changes (reference, value, fields, etc.)
    changeset = compute_footprint_changes(new_fp, part, is_existing=False)
    apply_footprint_changes(new_fp, changeset.changes, pcbnew_module)

    # Remove old footprint and add new one
    board.Delete(old_fp)
    board.Add(new_fp)

    return new_fp


def canonicalize_json(obj: Any) -> Any:
    """
    Recursively canonicalize a JSON-serializable object for deterministic output.

    - Dicts are converted to sorted dicts (by key)
    - Lists are sorted by their JSON string representation
    - Primitives are returned as-is
    """
    if isinstance(obj, dict):
        return {k: canonicalize_json(v) for k, v in sorted(obj.items())}
    elif isinstance(obj, list):
        canonicalized = [canonicalize_json(item) for item in obj]
        # Sort by JSON representation for stable ordering
        return sorted(canonicalized, key=lambda x: json.dumps(x, sort_keys=True))
    else:
        return obj


# Read PYTHONPATH environment variable and add all folders to the search path
python_path = os.environ.get("PYTHONPATH", "")
path_separator = (
    os.pathsep
)  # Use OS-specific path separator (: on Unix/Mac, ; on Windows)
if python_path:
    for path in python_path.split(path_separator):
        if path and path not in sys.path:
            sys.path.append(path)
            logger.info(f"Added {path} to Python search path")

# Available in KiCad's Python environment.
import pcbnew  # type: ignore[unresolved-import]  # noqa: E402


####################################################################################################
# JSON Netlist Parser
#
# This class parses the JSON netlist format from diode-sch.
####################################################################################################


class JsonNetlistParser:
    """Parse JSON netlist from diode-sch format."""

    class Part:
        """Represents a component part from the netlist."""

        def __init__(self, ref, value, footprint, sheetpath):
            self.ref = ref
            self.value = value
            self.footprint = footprint
            self.sheetpath = sheetpath
            self.properties = []

    class Module:
        """Represents a module instance from the netlist."""

        def __init__(self, path, layout_path=None):
            self.path = path  # Hierarchical path like "BMI270" or "Power.Regulator"
            self.layout_path = layout_path  # Path to .kicad_pcb file if it has a layout

    class SheetPath:
        """Represents the hierarchical sheet path."""

        def __init__(self, names, tstamps):
            self.names = names
            self.tstamps = tstamps

    class Net:
        """Represents an electrical net."""

        def __init__(self, name, nodes):
            self.name = name
            self.nodes = nodes

    class Property:
        """Represents a component property."""

        def __init__(self, name, value):
            self.name = name
            self.value = value

    def __init__(self):
        self.parts = []
        self.nets = []
        self.modules = {}  # Dict of module path -> Module instance

    @staticmethod
    def parse_netlist(json_path):
        """Parse a JSON netlist file and return a netlist object compatible with kinparse."""
        with open(json_path, "r") as f:
            data = json.load(f)

        parser = JsonNetlistParser()

        # Parse modules first
        for instance_ref, instance in data["instances"].items():
            if instance["kind"] != "Module":
                continue

            # Extract module path (remove file path and <root> prefix)
            if ":" in instance_ref:
                _, instance_path = instance_ref.rsplit(":", 1)
            else:
                instance_path = instance_ref

            # Remove <root> prefix if present
            path_parts = instance_path.split(".")
            if path_parts[0] == "<root>":
                path_parts = path_parts[1:]

            # Skip the root module itself
            if not path_parts:
                continue

            module_path = ".".join(path_parts)

            # Get layout_path attribute if present
            layout_path = None
            if "layout_path" in instance.get("attributes", {}):
                layout_path_attr = instance["attributes"]["layout_path"]
                if isinstance(layout_path_attr, dict) and "String" in layout_path_attr:
                    layout_path = layout_path_attr["String"]

            # Create and store module
            module = JsonNetlistParser.Module(module_path, layout_path)
            parser.modules[module_path] = module

            logger.info(f"Found module {module_path} with layout_path: {layout_path}")

        # Parse components (only Component kind)
        for instance_ref, instance in data["instances"].items():
            if instance["kind"] != "Component":
                continue

            # Get reference designator
            ref = instance.get("reference_designator", "U?")

            # Get value - follow the same precedence as Rust: mpn > value > Value > "?"
            value = "?"
            for key in ["mpn", "value", "Value"]:
                if (
                    key in instance["attributes"]
                    and "String" in instance["attributes"][key]
                ):
                    value = instance["attributes"][key]["String"]
                    break

            # Get footprint
            footprint_path = (
                instance["attributes"].get("footprint", {}).get("String", "")
            )
            if footprint_path:
                # Use the format_footprint function to handle both file paths and lib:fp format
                footprint = format_footprint(footprint_path)
            else:
                footprint = "unknown:unknown"

            # Build hierarchical path - this needs to match the Rust implementation
            # Extract the instance path after the root module
            # Format: "/path/to/file.star:<root>.BMI270.IC"
            # We need to extract "BMI270.IC" as the hierarchical name

            # Split by ':' to separate file path from instance path
            if ":" in instance_ref:
                _, instance_path = instance_ref.rsplit(":", 1)
            else:
                instance_path = instance_ref

            # Remove <root> prefix if present
            path_parts = instance_path.split(".")
            if path_parts[0] == "<root>":
                path_parts = path_parts[1:]

            # The hierarchical name is the dot-separated path (matching comp.hier_name in Rust)
            hier_name = ".".join(path_parts)

            # Generate UUID v5 using the same namespace and input as Rust
            # UUID_NAMESPACE_URL = uuid.UUID('6ba7b811-9dad-11d1-80b4-00c04fd430c8')
            ts_uuid = str(uuid.uuid5(uuid.NAMESPACE_URL, hier_name))

            sheetpath = JsonNetlistParser.SheetPath(hier_name, ts_uuid)

            # Create part
            part = JsonNetlistParser.Part(ref, value, footprint, sheetpath)

            # Add properties from attributes
            for attr_name, attr_value in instance["attributes"].items():
                if attr_name not in ["footprint", "value", "Value"]:
                    if isinstance(attr_value, dict):
                        if "String" in attr_value:
                            prop = JsonNetlistParser.Property(
                                attr_name, attr_value["String"]
                            )
                            part.properties.append(prop)
                        elif "Boolean" in attr_value:
                            # Convert boolean to string for consistency
                            prop = JsonNetlistParser.Property(
                                attr_name, "true" if attr_value["Boolean"] else "false"
                            )
                            part.properties.append(prop)
                        elif "Number" in attr_value:
                            prop = JsonNetlistParser.Property(
                                attr_name, str(attr_value["Number"])
                            )
                            part.properties.append(prop)
                        elif "Array" in attr_value:
                            # Arrays are formatted as CSV strings
                            # Convert array elements to CSV format
                            array_items = []
                            for item in attr_value["Array"]:
                                if isinstance(item, dict):
                                    if "String" in item:
                                        array_items.append(item["String"])
                                    elif "Number" in item:
                                        array_items.append(str(item["Number"]))
                                    elif "Boolean" in item:
                                        array_items.append(
                                            "true" if item["Boolean"] else "false"
                                        )
                                    else:
                                        # For other types, use string representation
                                        array_items.append(str(item))
                            prop = JsonNetlistParser.Property(
                                attr_name, ",".join(array_items)
                            )
                            part.properties.append(prop)

            parser.parts.append(part)

        # Parse nets
        for net_name, net_data in data["nets"].items():
            nodes = []

            # For each port in the net
            for port_ref in net_data["ports"]:
                # Find the component and pad
                port_parts = port_ref.split(".")

                # Find parent component by walking up the hierarchy
                parent_ref = None
                for i in range(len(port_parts) - 1, 0, -1):
                    test_ref = ".".join(port_parts[:i])
                    if (
                        test_ref in data["instances"]
                        and data["instances"][test_ref]["kind"] == "Component"
                    ):
                        parent_ref = test_ref
                        break

                if parent_ref:
                    parent = data["instances"][parent_ref]
                    ref_des = parent.get("reference_designator", "U?")

                    # Get the pad number from the port
                    port_instance = data["instances"].get(port_ref, {})
                    pad_nums = [
                        pad.get("String", "1")
                        for pad in (
                            port_instance.get("attributes", {})
                            .get("pads", {})
                            .get("Array", [])
                        )
                    ]

                    for pad_num in pad_nums:
                        nodes.append((ref_des, pad_num, net_name))

            if nodes:
                net = JsonNetlistParser.Net(net_name, nodes)
                parser.nets.append(net)

        return parser

    def get_component_module(
        self, component_path: str
    ) -> Optional["JsonNetlistParser.Module"]:
        """Find which module a component belongs to based on its hierarchical path.

        For example, if component_path is "Power.Regulator.C1", this will check:
        - "Power.Regulator" (if it exists as a module)
        - "Power" (if it exists as a module)

        Returns the deepest (most specific) module that contains this component.
        """
        if not component_path:
            return None

        path_parts = component_path.split(".")

        # Try from most specific to least specific
        for i in range(len(path_parts) - 1, 0, -1):
            module_path = ".".join(path_parts[:i])
            if module_path in self.modules:
                return self.modules[module_path]

        return None


####################################################################################################
# "Virtual DOM" for KiCad Board Items
#
# This provides a hierarchical representation of KiCad board items (footprints, tracks, zones, etc)
# that allows for easier manipulation while maintaining links back to the actual KiCad objects.
####################################################################################################


class VirtualItemType(Enum):
    """Types of items that can exist in the virtual DOM."""

    EDA_ITEM = (
        "eda_item"  # Generic EDA item (footprint, track, via, zone, drawing, etc.)
    )
    GROUP = "group"  # Hierarchical grouping (KiCad groups, modules, etc.)


@dataclass
class VirtualBoundingBox:
    """Bounding box for virtual DOM items."""

    x: int
    y: int
    width: int
    height: int

    @property
    def left(self) -> int:
        return self.x

    @property
    def right(self) -> int:
        return self.x + self.width

    @property
    def top(self) -> int:
        return self.y

    @property
    def bottom(self) -> int:
        return self.y + self.height

    @property
    def center_x(self) -> int:
        return self.x + self.width // 2

    @property
    def center_y(self) -> int:
        return self.y + self.height // 2

    @property
    def area(self) -> int:
        return self.width * self.height

    def contains_point(self, x: int, y: int) -> bool:
        """Check if a point is within this bounding box."""
        return self.left <= x <= self.right and self.top <= y <= self.bottom

    def intersects(self, other: "VirtualBoundingBox") -> bool:
        """Check if this bounding box intersects with another.

        Note: Boxes that are exactly touching (sharing an edge) are NOT considered intersecting.
        """
        return not (
            self.right <= other.left
            or self.left >= other.right
            or self.bottom <= other.top
            or self.top >= other.bottom
        )

    def merge(self, other: "VirtualBoundingBox") -> "VirtualBoundingBox":
        """Return a bounding box that encompasses both bounding boxes."""
        left = min(self.left, other.left)
        top = min(self.top, other.top)
        right = max(self.right, other.right)
        bottom = max(self.bottom, other.bottom)
        return VirtualBoundingBox(left, top, right - left, bottom - top)

    def inflate(self, amount: int) -> "VirtualBoundingBox":
        """Return a new bounding box expanded by amount on all sides."""
        return VirtualBoundingBox(
            self.x - amount,
            self.y - amount,
            self.width + 2 * amount,
            self.height + 2 * amount,
        )

    def __str__(self):
        return f"VirtualBoundingBox(x={self.x}, y={self.y}, width={self.width}, height={self.height})"


# Base class for all virtual items
class VirtualItem:
    """Base class for virtual DOM items."""

    def __init__(self, item_type: VirtualItemType, item_id: str, name: str):
        self.type = item_type
        self.id = item_id
        self.name = name
        self.parent: Optional["VirtualItem"] = None
        self._added = False  # Private attribute for storing added state

    @property
    def added(self) -> bool:
        """Whether this item was newly added during sync."""
        return self._added

    @added.setter
    def added(self, value: bool) -> None:
        """Set the added state."""
        self._added = value

    @property
    def bbox(self) -> Optional[VirtualBoundingBox]:
        """Get the bounding box of this item."""
        raise NotImplementedError("Subclasses must implement bbox property")

    def move_by(self, dx: int, dy: int) -> None:
        """Move this item by a relative offset."""
        raise NotImplementedError("Subclasses must implement move_by")

    def move_to(self, x: int, y: int) -> None:
        """Move this item to a specific position."""
        if self.bbox:
            dx = x - self.bbox.x
            dy = y - self.bbox.y
            self.move_by(dx, dy)

    def get_position(self) -> Optional[Tuple[int, int]]:
        """Get the position of this item."""
        if self.bbox:
            return (self.bbox.x, self.bbox.y)
        return None

    def get_center(self) -> Optional[Tuple[int, int]]:
        """Get the center position of this item."""
        if self.bbox:
            return (self.bbox.center_x, self.bbox.center_y)
        return None

    def intersects_with(self, other: "VirtualItem", margin: int = 0) -> bool:
        """Check if this item's bounding box intersects with another's."""
        if not self.bbox or not other.bbox:
            return False

        if margin:
            self_inflated = self.bbox.inflate(margin)
            other_inflated = other.bbox.inflate(margin)
            return self_inflated.intersects(other_inflated)
        else:
            return self.bbox.intersects(other.bbox)

    def render_tree(self, indent: int = 0) -> str:
        """Render this item and its subtree as a string."""
        raise NotImplementedError("Subclasses must implement render_tree")


class VirtualElement(VirtualItem, ABC):
    """Abstract base class for all PCB elements (footprints, zones, graphics)."""

    def __init__(self, element_id: str, name: str, kicad_item: Any):
        super().__init__(VirtualItemType.EDA_ITEM, element_id, name)
        self.kicad_item = kicad_item
        self.attributes: Dict[str, Any] = {}

    @property
    def bbox(self) -> Optional[VirtualBoundingBox]:
        """Get bounding box from KiCad object."""
        return get_kicad_bbox(self.kicad_item) if self.kicad_item else None

    def move_by(self, dx: int, dy: int) -> None:
        """Move element using KiCad's built-in Move method."""
        if self.kicad_item:
            self.kicad_item.Move(pcbnew.VECTOR2I(dx, dy))

    def clone_to_board(self, target_board: pcbnew.BOARD) -> Any:
        """Clone this element to a target board.

        Since all KiCad EDA_ITEMs have Clone(), we can use a single implementation.
        The target_board parameter is kept for API compatibility but not used.
        """
        return self.kicad_item.Clone()


class VirtualFootprint(VirtualElement):
    """Represents a footprint with position and orientation."""

    def __init__(
        self,
        fp_id: str,
        name: str,
        kicad_footprint: Any,  # pcbnew.FOOTPRINT
        bbox: VirtualBoundingBox,  # Kept for API compatibility but not used
    ):
        super().__init__(fp_id, name, kicad_footprint)
        self.kicad_footprint = kicad_footprint  # Keep for compatibility

    def replace_with(self, source_footprint: "VirtualFootprint") -> None:
        """Copy position and properties from another footprint."""
        if source_footprint.kicad_footprint and self.kicad_footprint:
            self._copy_kicad_properties(
                source_footprint.kicad_footprint, self.kicad_footprint
            )

    def _copy_kicad_properties(self, source: Any, target: Any) -> None:
        """Copy position, orientation, etc from source to target KiCad object."""
        if hasattr(source, "GetPosition") and hasattr(target, "SetPosition"):
            # Handle layer and flipping
            source_layer = source.GetLayer()
            target_layer = target.GetLayer()

            # Check if we need to flip the footprint
            source_is_back = source_layer == pcbnew.B_Cu
            target_is_back = target_layer == pcbnew.B_Cu

            if source_is_back != target_is_back:
                # Need to flip the footprint
                # When flipping, we need to maintain the position
                pos = target.GetPosition()
                target.Flip(pos, True)  # True means flip around Y axis
                logger.debug(
                    f"Flipped footprint from {'back' if target_is_back else 'front'} to {'back' if source_is_back else 'front'}"
                )

            # Set the layer after flipping (flipping changes the layer)
            target.SetLayer(source_layer)
            target.SetLayerSet(source.GetLayerSet())

            target.SetPosition(source.GetPosition())
            target.SetOrientation(source.GetOrientation())

            # Copy reference designator position/attributes
            if hasattr(source, "Reference") and hasattr(target, "Reference"):
                source_ref = source.Reference()
                target_ref = target.Reference()
                target_ref.SetPosition(source_ref.GetPosition())
                target_ref.SetAttributes(source_ref.GetAttributes())

            # Copy value field position/attributes
            if hasattr(source, "Value") and hasattr(target, "Value"):
                source_val = source.Value()
                target_val = target.Value()
                target_val.SetPosition(source_val.GetPosition())
                target_val.SetAttributes(source_val.GetAttributes())

    def render_tree(self, indent: int = 0) -> str:
        """Render this footprint as a string."""
        prefix = "  " * indent
        status_markers = []
        if self.added:
            status_markers.append("NEW")
        status_str = f" [{', '.join(status_markers)}]" if status_markers else ""

        fpid_info = ""
        if "fpid" in self.attributes:
            fpid_info = f" ({self.attributes['fpid']})"

        return f"{prefix}{self.name}{fpid_info}{status_str} {self.bbox}"


class VirtualConnectedItem(VirtualElement):
    """Represents any BOARD_CONNECTED_ITEM (track, via, zone, etc.) with net connectivity."""

    def __init__(self, item_id: str, name: str, kicad_item: Any):
        super().__init__(item_id, name, kicad_item)
        self._source_net_code = kicad_item.GetNetCode()
        # Determine the item type for display purposes
        item_class = kicad_item.GetClass()
        if "VIA" in item_class.upper():
            self._item_type = "Via"
        elif "TRACK" in item_class.upper():
            self._item_type = "Track"
        elif "ZONE" in item_class.upper():
            self._item_type = "Zone"
        else:
            # Fallback for any other BOARD_CONNECTED_ITEM types
            self._item_type = item_class

    def apply_net_code_mapping(
        self, net_code_map: Dict[int, int], target_board: pcbnew.BOARD
    ) -> bool:
        """Apply net code mapping to this connected item."""
        if self._source_net_code == 0:
            return True  # No net connection

        target_net_code = net_code_map.get(self._source_net_code, 0)
        if target_net_code == 0:
            logger.warning(
                f"{self._item_type} {self.name}: No mapping for net code {self._source_net_code}"
            )
            self.kicad_item.SetNetCode(0)
            return False

        target_net = target_board.FindNet(target_net_code)
        if target_net:
            self.kicad_item.SetNet(target_net)
            return True
        else:
            logger.error(
                f"{self._item_type} {self.name}: Target net code {target_net_code} not found"
            )
            self.kicad_item.SetNetCode(0)
            return False

    def render_tree(self, indent: int = 0) -> str:
        """Render this connected item as a string."""
        prefix = "  " * indent
        status_markers = []
        if self.added:
            status_markers.append("NEW")
        status_str = f" [{', '.join(status_markers)}]" if status_markers else ""

        net_info = f" (net:{self._source_net_code})"
        return (
            f"{prefix}{self._item_type}:{self.name}{net_info}{status_str} {self.bbox}"
        )


class VirtualGraphic(VirtualElement):
    """Represents a graphic element (line, arc, text, etc)."""

    def __init__(self, graphic_id: str, name: str, kicad_graphic: Any):
        super().__init__(graphic_id, name, kicad_graphic)
        self._type = kicad_graphic.GetClass()

    def render_tree(self, indent: int = 0) -> str:
        """Render this graphic as a string."""
        prefix = "  " * indent
        status_markers = []
        if self.added:
            status_markers.append("NEW")
        status_str = f" [{', '.join(status_markers)}]" if status_markers else ""

        return f"{prefix}Graphic:{self._type}{status_str} {self.bbox}"


class VirtualGroup(VirtualItem):
    """Lightweight group/container for organizing items hierarchically."""

    def __init__(self, group_id: str, name: str):
        super().__init__(VirtualItemType.GROUP, group_id, name)
        self.children: List[VirtualItem] = []
        self.synced = False  # Whether this group has been synced from a layout file
        self._cached_bbox: Optional[VirtualBoundingBox] = None

    @property
    def added(self) -> bool:
        """A group is considered added if all of its children are added.

        Empty groups are not considered added.
        """
        if not self.children:
            return False
        return all(child.added for child in self.children)

    @property
    def bbox(self) -> Optional[VirtualBoundingBox]:
        """Compute bounding box from children on-the-fly."""
        # Return cached bbox if available and valid
        if self._cached_bbox is not None:
            return self._cached_bbox

        if not self.children:
            return None

        child_bboxes = [child.bbox for child in self.children if child.bbox]
        if not child_bboxes:
            return None

        min_x = min(bbox.left for bbox in child_bboxes)
        min_y = min(bbox.top for bbox in child_bboxes)
        max_x = max(bbox.right for bbox in child_bboxes)
        max_y = max(bbox.bottom for bbox in child_bboxes)

        self._cached_bbox = VirtualBoundingBox(
            min_x, min_y, max_x - min_x, max_y - min_y
        )
        return self._cached_bbox

    def add_child(self, child: VirtualItem) -> None:
        """Add a child item to this group."""
        child.parent = self
        self.children.append(child)
        self._cached_bbox = None  # Invalidate cache

    def remove_child(self, child: VirtualItem) -> None:
        """Remove a child item from this group."""
        if child in self.children:
            child.parent = None
            self.children.remove(child)
            self._cached_bbox = None  # Invalidate cache

    def move_by(self, dx: int, dy: int) -> None:
        """Move all children by a relative offset."""
        # Move all children
        for child in self.children:
            child.move_by(dx, dy)

        # Invalidate our cached bbox
        self._cached_bbox = None

    def find_by_id(self, item_id: str) -> Optional[VirtualItem]:
        """Find an item by ID in this subtree."""
        if self.id == item_id:
            return self
        for child in self.children:
            if isinstance(child, VirtualGroup):
                result = child.find_by_id(item_id)
                if result:
                    return result
            elif child.id == item_id:
                return child
        return None

    def find_all_footprints(self) -> List[VirtualFootprint]:
        """Find all footprints in this subtree."""
        results = []
        for child in self.children:
            if isinstance(child, VirtualFootprint):
                results.append(child)
            elif isinstance(child, VirtualGroup):
                results.extend(child.find_all_footprints())
        return results

    def render_tree(self, indent: int = 0) -> str:
        """Render this group and its subtree as a string."""
        lines = []
        prefix = "  " * indent

        status_markers = []
        if self.added:
            status_markers.append("NEW")
        if self.synced:
            status_markers.append("SYNCED")
        status_str = f" [{', '.join(status_markers)}]" if status_markers else ""

        lines.append(f"{prefix}{self.name}{status_str} {self.bbox}")

        # Render children
        for child in self.children:
            lines.append(child.render_tree(indent + 1))

        return "\n".join(lines)

    def update_bbox_from_children(self) -> None:
        """Force update of cached bbox from children."""
        self._cached_bbox = None
        _ = self.bbox  # Force recomputation


class VirtualBoard:
    """Root of the virtual DOM tree representing a KiCad board."""

    def __init__(self):
        self.root = VirtualGroup("board", "Board")
        # Keep a registry of all footprints by UUID for quick lookup
        self.footprints_by_id: Dict[str, VirtualFootprint] = {}

    def register_footprint(self, footprint: VirtualFootprint) -> None:
        """Register a footprint in the board's registry."""
        self.footprints_by_id[footprint.id] = footprint

    def get_footprint_by_id(self, fp_id: str) -> Optional[VirtualFootprint]:
        """Get a footprint by ID from the registry."""
        return self.footprints_by_id.get(fp_id)

    def render(self) -> str:
        """Render the entire virtual DOM tree as a string."""
        return self.root.render_tree()

    def get_kicad_object(self, item_id: str) -> Optional[Any]:
        """Get the KiCad object for a virtual item by ID.

        Args:
            item_id: The ID of the virtual item

        Returns:
            The KiCad object or None if not found
        """
        footprint = self.get_footprint_by_id(item_id)
        if footprint:
            return footprint.kicad_footprint
        return None


def build_groups_registry(board: pcbnew.BOARD) -> Dict[str, Any]:
    """Build a groups registry from a board.

    Returns a dict mapping group name -> PCB_GROUP. Anonymous groups are excluded.

    Note: We use board.Groups() to enumerate groups. The stale SWIG wrapper issue
    (which motivated the previous GetDrawings() approach) is handled by ensuring
    the registry is updated after any group deletion in _sync_groups().
    """
    registry = {}
    for group in board.Groups():
        name = group.GetName()
        if name:
            registry[name] = group
    return registry


def build_virtual_dom_from_board(
    board: pcbnew.BOARD,
    include_zones: bool = True,
    include_graphics: bool = True,
    include_tracks: bool = True,
) -> VirtualBoard:
    """Build a virtual DOM from a KiCad board including all element types.

    The virtual DOM hierarchy is built from footprint Path fields, not from
    KiCad's PCB_GROUP objects. This makes it independent of the groups registry.

    Args:
        board: The KiCad board to build from
        include_zones: Whether to include zones in the DOM
        include_graphics: Whether to include graphics in the DOM
        include_tracks: Whether to include tracks and vias in the DOM

    Returns:
        A VirtualBoard with the complete hierarchy
    """
    vboard = VirtualBoard()

    # First pass: collect all unique paths to determine the hierarchy
    all_paths = set()
    for fp in board.GetFootprints():
        path_field = fp.GetFieldByName("Path")
        if path_field:
            path = path_field.GetText()
            if path:
                # Add this path and all parent paths
                parts = path.split(".")
                for i in range(1, len(parts) + 1):
                    all_paths.add(".".join(parts[:i]))

    # Sort paths for deterministic processing
    sorted_paths = sorted(all_paths)

    # Create groups for all paths that have children
    module_groups = {}

    for path in sorted_paths:
        # Check if this path has any children
        has_children = any(p != path and p.startswith(path + ".") for p in all_paths)

        if has_children:
            # Create virtual group
            group = VirtualGroup(path, path)
            module_groups[path] = group

    # Build hierarchy of module groups - sort by path for deterministic order
    for path in sorted(module_groups.keys()):
        group = module_groups[path]
        parts = path.split(".")
        if len(parts) > 1:
            # Find parent module
            parent_path = ".".join(parts[:-1])
            if parent_path in module_groups:
                module_groups[parent_path].add_child(group)
            else:
                # No parent module, add to root
                vboard.root.add_child(group)
        else:
            # Top-level module, add to root
            vboard.root.add_child(group)

    # Sort footprints by UUID for deterministic processing
    sorted_footprints = sorted(
        board.GetFootprints(), key=lambda fp: get_footprint_uuid(fp)
    )

    # Add footprints to their respective groups or root
    for fp in sorted_footprints:
        fp_uuid = get_footprint_uuid(fp)
        fp_bbox = get_kicad_bbox(fp)

        # Get hierarchical path from the board
        path_field = fp.GetFieldByName("Path")
        fp_path = path_field.GetText() if path_field else ""

        # The footprint's display name
        name = fp_path if fp_path else fp.GetReference()

        # Create virtual footprint with direct object reference
        vfp = VirtualFootprint(
            fp_uuid,
            name,
            fp,
            fp_bbox,
        )
        vfp.attributes["fpid"] = fp.GetFPIDAsString()

        # Register the footprint in the board's registry
        vboard.register_footprint(vfp)

        if fp_path:
            # Find which module this footprint belongs to
            placed = False
            # Check from most specific to least specific
            parts = fp_path.split(".")
            for i in range(len(parts), 0, -1):
                parent_path = ".".join(parts[:i])
                if parent_path in module_groups:
                    module_groups[parent_path].add_child(vfp)
                    placed = True
                    break

            if not placed:
                # No matching module, add to root
                vboard.root.add_child(vfp)
        else:
            # No path, add to root
            vboard.root.add_child(vfp)

    # Add zones to virtual DOM
    if include_zones:
        # Convert to list first to avoid iterator issues on Windows
        zones_list = list(board.Zones())
        for zone in zones_list:
            zone_uuid = (
                str(zone.m_Uuid) if hasattr(zone, "m_Uuid") else str(uuid.uuid4())
            )
            zone_name = zone.GetZoneName() or f"Zone_{zone_uuid[:8]}"

            vzone = VirtualConnectedItem(zone_uuid, zone_name, zone)

            # Determine which group this zone belongs to by checking parent group
            parent_group = (
                zone.GetParentGroup() if hasattr(zone, "GetParentGroup") else None
            )
            if parent_group and parent_group.GetName() in module_groups:
                module_groups[parent_group.GetName()].add_child(vzone)
            else:
                # Add to root if no parent group
                vboard.root.add_child(vzone)

    # Add graphics to virtual DOM
    if include_graphics:
        # Convert to list first to avoid iterator issues on Windows
        drawings_list = list(board.GetDrawings())
        for drawing in drawings_list:
            # Skip items that belong to footprints
            if drawing.GetParent() and isinstance(
                drawing.GetParent(), pcbnew.FOOTPRINT
            ):
                continue

            drawing_uuid = (
                str(drawing.m_Uuid) if hasattr(drawing, "m_Uuid") else str(uuid.uuid4())
            )
            drawing_name = f"{drawing.GetClass()}_{drawing_uuid[:8]}"

            vgraphic = VirtualGraphic(drawing_uuid, drawing_name, drawing)

            # Determine which group this graphic belongs to
            parent_group = (
                drawing.GetParentGroup() if hasattr(drawing, "GetParentGroup") else None
            )
            if parent_group and parent_group.GetName() in module_groups:
                module_groups[parent_group.GetName()].add_child(vgraphic)
            else:
                # Add to root if no parent group
                vboard.root.add_child(vgraphic)

    # Add tracks and vias to virtual DOM
    if include_tracks:
        # Convert to list first to avoid iterator issues on Windows
        tracks_list = list(board.GetTracks())
        for item in tracks_list:
            item_uuid = (
                str(item.m_Uuid) if hasattr(item, "m_Uuid") else str(uuid.uuid4())
            )

            # Check if it's a track or via based on class name
            item_class = item.GetClass()
            if "VIA" in item_class.upper():
                # It's a via
                item_name = f"Via_{item_uuid[:8]}"
            else:
                # It's a track
                item_name = f"Track_{item_uuid[:8]}"

            vitem = VirtualConnectedItem(item_uuid, item_name, item)

            # Determine which group this track/via belongs to
            parent_group = (
                item.GetParentGroup() if hasattr(item, "GetParentGroup") else None
            )
            if parent_group and parent_group.GetName() in module_groups:
                module_groups[parent_group.GetName()].add_child(vitem)
            else:
                # Add to root if no parent group
                vboard.root.add_child(vitem)

    return vboard


def build_net_code_mapping(
    source_board: pcbnew.BOARD,
    target_board: pcbnew.BOARD,
    matched_footprints: List[Tuple[VirtualFootprint, VirtualFootprint]],
) -> Dict[int, int]:
    """Build mapping of source net codes to target net codes via matched footprint pads.

    Args:
        source_board: The source layout board
        target_board: The target board
        matched_footprints: List of (source_fp, target_fp) pairs

    Returns:
        Dict mapping source_net_code -> target_net_code
    """
    net_code_map = {}

    for source_vfp, target_vfp in matched_footprints:
        source_fp = (
            source_vfp.kicad_footprint
            if hasattr(source_vfp, "kicad_footprint")
            else source_vfp.kicad_item
        )
        target_fp = (
            target_vfp.kicad_footprint
            if hasattr(target_vfp, "kicad_footprint")
            else target_vfp.kicad_item
        )

        if not source_fp or not target_fp:
            continue

        # Build pad mapping by pad name/number
        source_pads = {pad.GetPadName(): pad for pad in source_fp.Pads()}
        target_pads = {pad.GetPadName(): pad for pad in target_fp.Pads()}

        for pad_name, source_pad in source_pads.items():
            if pad_name not in target_pads:
                continue

            target_pad = target_pads[pad_name]
            source_net_code = source_pad.GetNetCode()
            target_net_code = target_pad.GetNetCode()

            # Only map connected pads (net code 0 means no connection)
            if source_net_code > 0 and target_net_code > 0:
                if source_net_code in net_code_map:
                    # Verify consistency
                    if net_code_map[source_net_code] != target_net_code:
                        source_net = source_board.FindNet(source_net_code)
                        target_net = target_board.FindNet(target_net_code)
                        source_name = (
                            source_net.GetNetname() if source_net else "unknown"
                        )
                        target_name = (
                            target_net.GetNetname() if target_net else "unknown"
                        )
                        logger.warning(
                            f"Net code mapping conflict: {source_net_code} ({source_name}) "
                            f"maps to both {net_code_map[source_net_code]} and {target_net_code} ({target_name})"
                        )
                else:
                    net_code_map[source_net_code] = target_net_code

                    # Log mapping for debugging
                    if logger.isEnabledFor(logging.DEBUG):
                        source_net = source_board.FindNet(source_net_code)
                        target_net = target_board.FindNet(target_net_code)
                        source_name = (
                            source_net.GetNetname() if source_net else "unknown"
                        )
                        target_name = (
                            target_net.GetNetname() if target_net else "unknown"
                        )
                        logger.debug(
                            f"Mapped net {source_net_code} ({source_name}) -> "
                            f"{target_net_code} ({target_name})"
                        )

    return net_code_map


def get_kicad_bbox(item: Any) -> VirtualBoundingBox:
    """Get bounding box from any KiCad item."""
    if isinstance(item, pcbnew.FOOTPRINT):
        # Exclude fab layers from bbox calculation
        lset = pcbnew.LSET.AllLayersMask()
        lset.RemoveLayer(pcbnew.F_Fab)
        lset.RemoveLayer(pcbnew.B_Fab)
        bb = item.GetLayerBoundingBox(lset)
    else:
        bb = item.GetBoundingBox()

    return VirtualBoundingBox(bb.GetLeft(), bb.GetTop(), bb.GetWidth(), bb.GetHeight())


####################################################################################################
# Footprint formatting helper
####################################################################################################


def is_kicad_lib_fp(s):
    """Determine whether a given string is a KiCad lib:footprint reference rather than a file path."""
    if ":" not in s:
        return False

    lib, fp = s.split(":", 1)

    # Filter out Windows drive prefixes like "C:"
    if len(lib) == 1 and lib.isalpha():
        return False

    # Any path separator indicates this is still a filesystem path
    if "/" in lib or "\\" in lib or "/" in fp or "\\" in fp:
        return False

    return True


def format_footprint(fp_str):
    """Convert footprint strings that may point to a .kicad_mod file into a KiCad lib:fp identifier.

    This matches the Rust implementation in kicad_netlist.rs
    """
    if is_kicad_lib_fp(fp_str):
        return fp_str

    # Extract the footprint name from the file path
    fp_path = Path(fp_str)
    stem = fp_path.stem
    if not stem:
        return "UNKNOWN:UNKNOWN"

    return f"{stem}:{stem}"


####################################################################################################
# Data Structures + Utility Functions
#
# Here we define some data structures that represent the footprints and layouts we'll be working
# with.
####################################################################################################


def rmv_quotes(s):
    """Remove starting and ending quotes from a string."""
    if not isinstance(s, str):
        return s

    mtch = re.match(r'^\s*"(.*)"\s*$', s)
    if mtch:
        try:
            s = s.decode(mtch.group(1))
        except (AttributeError, LookupError):
            s = mtch.group(1)

    return s


def to_list(x):
    """
    Return x if it is already a list, return a list containing x if x is a scalar unless
    x is None in which case return an empty list.
    """
    if x is None:
        # Return empty list if x is None.
        return []
    if isinstance(x, (list, tuple)):
        return x  # Already a list, so just return it.
    return [x]  # Wasn't a list, so make it into one.


def get_group_items(group: pcbnew.PCB_GROUP) -> list[pcbnew.BOARD_ITEM]:
    return [
        item.Cast()
        for item in group.GetItemsDeque()
        if item.GetClass() not in ["PCB_GENERATOR"]
    ]


def get_footprint_uuid(fp: pcbnew.FOOTPRINT) -> str:
    """Return the UUID of a footprint."""
    path = fp.GetPath().AsString()
    return path.split("/")[-1]


def footprints_by_uuid(board: pcbnew.BOARD) -> dict[str, pcbnew.FOOTPRINT]:
    """Return a dict of footprints by UUID."""
    return {get_footprint_uuid(fp): fp for fp in board.GetFootprints()}


def flip_dict(d: dict) -> dict:
    """Return a dict with keys and values swapped."""
    return {v: k for k, v in d.items()}


class Step(ABC):
    """A step in the layout sync process."""

    @abstractmethod
    def run(self):
        pass

    def run_with_timing(self):
        """Run the step with timing information."""
        step_name = self.__class__.__name__
        logger.info(f"Starting {step_name}...")
        start_time = time.time()

        try:
            self.run()
            elapsed = time.time() - start_time
            logger.info(f"Completed {step_name} in {elapsed:.3f} seconds")
        except Exception as e:
            logger.error(f"Failed {step_name}: {e}")
            raise


@dataclass
class FootprintInfo:
    """Minimal footprint information for tracking without KiCad type references."""

    uuid: str
    path: str  # Hierarchical path like "Power.Regulator.C1"


@dataclass
class GroupInfo:
    """Information about a group without KiCad type references."""

    name: str
    is_locked: bool = False
    is_synced: bool = False  # True if synced from a layout file
    bbox: Optional[Tuple[int, int, int, int]] = None  # (x, y, width, height)


class SyncState:
    """Shared state for the sync process."""

    def __init__(self):
        # All footprints currently on the board by UUID
        self.footprints: Dict[str, FootprintInfo] = {}

        # Track changes during sync
        self.removed_footprint_uuids: Set[str] = set()
        self.added_footprint_uuids: Set[str] = set()
        self.updated_footprint_uuids: Set[str] = set()

        # Track orphaned footprints: group_name -> list of footprint UUIDs
        self.orphaned_footprints_by_group: Dict[str, List[str]] = {}

        # Track module paths that have been synced from layout files
        self.synced_module_paths: Set[str] = set()

        # Virtual DOM representation of the board
        self.virtual_board = VirtualBoard()

        # Diagnostics collected during sync (e.g., FPID mismatches)
        self.layout_diagnostics: List[Dict[str, Any]] = []

        # Groups registry: Python-side source of truth for KiCad groups.
        # Maps group name -> PCB_GROUP. This avoids querying board.Groups()
        # which can return stale SWIG wrappers after groups are removed.
        self.groups_registry: Dict[str, pcbnew.PCB_GROUP] = {}

    def add_diagnostic(
        self,
        kind: str,
        severity: str,
        body: str,
        path: str = "",
        reference: Optional[str] = None,
    ):
        """Add a diagnostic to be reported after sync completes."""
        self.layout_diagnostics.append(
            {
                "kind": kind,
                "severity": severity,
                "body": body,
                "path": path,
                "reference": reference,
            }
        )

    def track_footprint_removed(self, fp: pcbnew.FOOTPRINT):
        """Track that a footprint was removed from the board."""
        uuid = get_footprint_uuid(fp)
        self.removed_footprint_uuids.add(uuid)
        if uuid in self.footprints:
            del self.footprints[uuid]

    def track_footprint_added(self, fp: pcbnew.FOOTPRINT):
        """Track that a footprint was added to the board."""
        uuid = get_footprint_uuid(fp)
        self.added_footprint_uuids.add(uuid)

        # Extract path from field
        path = ""
        field = fp.GetFieldByName("Path")
        if field:
            path = field.GetText()

        self.footprints[uuid] = FootprintInfo(uuid=uuid, path=path)

    def track_footprint_updated(self, fp: pcbnew.FOOTPRINT):
        """Track that a footprint was updated on the board."""
        uuid = get_footprint_uuid(fp)
        self.updated_footprint_uuids.add(uuid)

        # Update stored info
        path = ""
        field = fp.GetFieldByName("Path")
        if field:
            path = field.GetText()

        self.footprints[uuid] = FootprintInfo(uuid=uuid, path=path)

    def track_orphaned_footprint(self, group_name: str, target_fp: pcbnew.FOOTPRINT):
        """Track a footprint from the target board that has no corresponding footprint in the source layout."""
        uuid = get_footprint_uuid(target_fp)
        if group_name not in self.orphaned_footprints_by_group:
            self.orphaned_footprints_by_group[group_name] = []
        self.orphaned_footprints_by_group[group_name].append(uuid)

        # Also ensure this footprint is tracked in our footprints dict
        if uuid not in self.footprints:
            self.track_footprint_updated(target_fp)

    def get_footprints_by_path_prefix(self, prefix: str) -> List[FootprintInfo]:
        """Get all footprints whose path starts with the given prefix."""
        return [fp for fp in self.footprints.values() if fp.path.startswith(prefix)]

    def get_newly_added_footprints(self) -> List[FootprintInfo]:
        """Get all footprints that were added in this sync."""
        return [
            self.footprints[uuid]
            for uuid in self.added_footprint_uuids
            if uuid in self.footprints
        ]

    def get_footprint_by_uuid(self, uuid: str) -> Optional[FootprintInfo]:
        """Get footprint info by UUID."""
        return self.footprints.get(uuid)


####################################################################################################
# Step 0. Setup Board
####################################################################################################


class SetupBoard(Step):
    """Set up the board for the sync process."""

    def __init__(
        self,
        state: SyncState,
        board: pcbnew.BOARD,
        board_config_path: Optional[str] = None,
        sync_board_config: bool = True,
    ):
        self.state = state
        self.board = board
        self.board_config_path = board_config_path
        self.sync_board_config = sync_board_config

    # Configuration table: (json_path, ds_attribute, display_name, [custom_setter])
    CONFIG_MAPPINGS = [
        # Copper constraints
        (
            ["design_rules", "constraints", "copper", "minimum_clearance"],
            "m_MinClearance",
            "minimum clearance",
        ),
        (
            ["design_rules", "constraints", "copper", "minimum_track_width"],
            "m_TrackMinWidth",
            "minimum track width",
        ),
        (
            ["design_rules", "constraints", "copper", "minimum_connection_width"],
            "m_MinConn",
            "minimum connection width",
        ),
        (
            ["design_rules", "constraints", "copper", "minimum_annular_width"],
            "m_ViasMinAnnularWidth",
            "minimum annular width",
        ),
        (
            ["design_rules", "constraints", "copper", "minimum_via_diameter"],
            "m_ViasMinSize",
            "minimum via diameter",
        ),
        (
            ["design_rules", "constraints", "copper", "copper_to_hole_clearance"],
            "m_HoleClearance",
            "copper to hole clearance",
        ),
        (
            ["design_rules", "constraints", "copper", "copper_to_edge_clearance"],
            "m_CopperEdgeClearance",
            "copper to edge clearance",
        ),
        # Hole constraints
        (
            ["design_rules", "constraints", "holes", "minimum_through_hole"],
            "m_MinThroughDrill",
            "minimum through hole",
        ),
        (
            ["design_rules", "constraints", "holes", "hole_to_hole_clearance"],
            "m_HoleToHoleMin",
            "hole to hole clearance",
        ),
        # Micro via constraints
        (
            ["design_rules", "constraints", "uvias", "minimum_uvia_diameter"],
            "m_MicroViasMinSize",
            "minimum uvia diameter",
        ),
        (
            ["design_rules", "constraints", "uvias", "minimum_uvia_hole"],
            "m_MicroViasMinDrill",
            "minimum uvia hole",
        ),
        # Silkscreen constraints
        (
            ["design_rules", "constraints", "silkscreen", "minimum_item_clearance"],
            "m_SilkClearance",
            "silkscreen minimum item clearance",
        ),
        # Special case for text size (needs VECTOR2I)
        (
            ["design_rules", "constraints", "silkscreen", "minimum_text_height"],
            "m_TextSize",
            "silkscreen minimum text height",
            lambda ds, val: setattr(
                ds,
                "m_TextSize",
                pcbnew.VECTOR2I(pcbnew.FromMM(val), pcbnew.FromMM(val)),
            ),
        ),
        # Note: m_TextThickness requires a complex array setup, skipping for now
        # Pre-defined sizes (track widths and via dimensions only - diff pairs not supported)
        (
            ["design_rules", "predefined_sizes", "track_widths"],
            "m_TrackWidthList",
            "pre-defined track widths",
            "track_widths",
        ),
        (
            ["design_rules", "predefined_sizes", "via_dimensions"],
            "m_ViasDimensionsList",
            "pre-defined via dimensions",
            "via_dimensions",
        ),
        # Netclasses
        (["design_rules", "netclasses"], None, "netclasses", "netclasses"),
    ]

    def _get_nested_value(self, data, path):
        """Get a nested value from a dictionary using a path list."""
        for key in path:
            if not isinstance(data, dict) or key not in data:
                return None
            data = data[key]
        return data

    def _set_track_width_list(self, ds, values):
        """Set track width list from list of width values in mm."""
        track_list = ds.m_TrackWidthList

        # Check if list already matches desired values
        # Note: The placeholder at index 0 may not persist in saved files,
        # so we compare with and without it
        actual_size = track_list.size()
        has_placeholder = actual_size == len(values) + 1
        exact_match = actual_size == len(values)

        if has_placeholder or exact_match:
            matches = True
            start_idx = 1 if has_placeholder else 0

            # Check placeholder if present
            if has_placeholder and track_list[0] != 0:
                matches = False

            # Check all values
            if matches:
                for i, width in enumerate(values):
                    expected_val = pcbnew.FromMM(width)
                    actual_val = track_list[start_idx + i]
                    if actual_val != expected_val:
                        matches = False
                        break

            if matches:
                logger.debug(f"Track width list unchanged ({len(values)} widths)")
                return

        # List differs, rebuild it
        logger.info(f"Updating track width list: {len(values)} widths")
        track_list.clear()

        # Index 0 is reserved to "make room for the netclass value" (from KiCad source)
        # This placeholder allows the GUI to use netclass defaults when no specific size is selected
        # See: pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr_parser.cpp line 2178-2180
        track_list.push_back(0)

        for width in values:
            track_list.push_back(pcbnew.FromMM(width))

    def _set_via_dimensions_list(self, ds, values):
        """Set via dimensions list from list of {diameter, drill} objects."""
        via_list = ds.m_ViasDimensionsList

        # Check if list already matches desired values
        expected_size = len(values) + 1  # +1 for placeholder at index 0
        if via_list.size() == expected_size:
            # Verify placeholder and all values match
            matches = via_list[0].m_Diameter == 0 and via_list[0].m_Drill == 0
            for i, via_def in enumerate(values):
                if (
                    isinstance(via_def, dict)
                    and "diameter" in via_def
                    and "drill" in via_def
                ):
                    via_dim = via_list[i + 1]
                    matches = matches and (
                        via_dim.m_Diameter == pcbnew.FromMM(via_def["diameter"])
                        and via_dim.m_Drill == pcbnew.FromMM(via_def["drill"])
                    )

            if matches:
                logger.debug(
                    f"Via dimensions list unchanged ({len(values)} dimensions)"
                )
                return

        # List differs, rebuild it
        logger.info(f"Updating via dimensions list: {len(values)} dimensions")
        via_list.clear()

        # Index 0 is reserved to "make room for the netclass value" (from KiCad source)
        # This placeholder allows the GUI to use netclass defaults when no specific size is selected
        # See: pcbnew/pcb_io/kicad_sexpr/pcb_io_kicad_sexpr_parser.cpp (VIA_DIMENSION case)
        via_list.push_back(pcbnew.VIA_DIMENSION(0, 0))

        for via_def in values:
            if (
                not isinstance(via_def, dict)
                or "diameter" not in via_def
                or "drill" not in via_def
            ):
                logger.warning(
                    f"Via dimension must have 'diameter' and 'drill' keys: {via_def}"
                )
                continue
            diameter = pcbnew.FromMM(via_def["diameter"])
            drill = pcbnew.FromMM(via_def["drill"])
            via_dim = pcbnew.VIA_DIMENSION(diameter, drill)
            via_list.push_back(via_dim)

    def _set_netclasses(self, ds, values):
        """Set netclasses from list of netclass definitions."""
        if not isinstance(values, list):
            logger.warning("Netclasses must be a list")
            return

        netSettings = ds.m_NetSettings

        def netclass_matches(netclass, nc_def):
            """Check if a netclass matches the definition."""
            checks = []
            if "clearance" in nc_def and nc_def["clearance"] is not None:
                checks.append(
                    netclass.GetClearance() == pcbnew.FromMM(nc_def["clearance"])
                )
            if "track_width" in nc_def and nc_def["track_width"] is not None:
                checks.append(
                    netclass.GetTrackWidth() == pcbnew.FromMM(nc_def["track_width"])
                )
            if "via_diameter" in nc_def and nc_def["via_diameter"] is not None:
                checks.append(
                    netclass.GetViaDiameter() == pcbnew.FromMM(nc_def["via_diameter"])
                )
            if "via_drill" in nc_def and nc_def["via_drill"] is not None:
                checks.append(
                    netclass.GetViaDrill() == pcbnew.FromMM(nc_def["via_drill"])
                )
            if (
                "microvia_diameter" in nc_def
                and nc_def["microvia_diameter"] is not None
            ):
                checks.append(
                    netclass.GetuViaDiameter()
                    == pcbnew.FromMM(nc_def["microvia_diameter"])
                )
            if "microvia_drill" in nc_def and nc_def["microvia_drill"] is not None:
                checks.append(
                    netclass.GetuViaDrill() == pcbnew.FromMM(nc_def["microvia_drill"])
                )
            if "diff_pair_width" in nc_def and nc_def["diff_pair_width"] is not None:
                checks.append(
                    netclass.GetDiffPairWidth()
                    == pcbnew.FromMM(nc_def["diff_pair_width"])
                )
            if "diff_pair_gap" in nc_def and nc_def["diff_pair_gap"] is not None:
                checks.append(
                    netclass.GetDiffPairGap() == pcbnew.FromMM(nc_def["diff_pair_gap"])
                )
            if (
                "diff_pair_via_gap" in nc_def
                and nc_def["diff_pair_via_gap"] is not None
            ):
                checks.append(
                    netclass.GetDiffPairViaGap()
                    == pcbnew.FromMM(nc_def["diff_pair_via_gap"])
                )
            if "priority" in nc_def and nc_def["priority"] is not None:
                checks.append(netclass.GetPriority() == int(nc_def["priority"]))
            return all(checks) if checks else True

        def apply_netclass_properties(netclass, nc_def):
            """Apply properties from definition dict to a netclass object."""
            if "clearance" in nc_def and nc_def["clearance"] is not None:
                netclass.SetClearance(pcbnew.FromMM(nc_def["clearance"]))
            if "track_width" in nc_def and nc_def["track_width"] is not None:
                netclass.SetTrackWidth(pcbnew.FromMM(nc_def["track_width"]))
            if "via_diameter" in nc_def and nc_def["via_diameter"] is not None:
                netclass.SetViaDiameter(pcbnew.FromMM(nc_def["via_diameter"]))
            if "via_drill" in nc_def and nc_def["via_drill"] is not None:
                netclass.SetViaDrill(pcbnew.FromMM(nc_def["via_drill"]))
            if (
                "microvia_diameter" in nc_def
                and nc_def["microvia_diameter"] is not None
            ):
                netclass.SetuViaDiameter(pcbnew.FromMM(nc_def["microvia_diameter"]))
            if "microvia_drill" in nc_def and nc_def["microvia_drill"] is not None:
                netclass.SetuViaDrill(pcbnew.FromMM(nc_def["microvia_drill"]))
            if "diff_pair_width" in nc_def and nc_def["diff_pair_width"] is not None:
                netclass.SetDiffPairWidth(pcbnew.FromMM(nc_def["diff_pair_width"]))
            if "diff_pair_gap" in nc_def and nc_def["diff_pair_gap"] is not None:
                netclass.SetDiffPairGap(pcbnew.FromMM(nc_def["diff_pair_gap"]))
            if (
                "diff_pair_via_gap" in nc_def
                and nc_def["diff_pair_via_gap"] is not None
            ):
                netclass.SetDiffPairViaGap(pcbnew.FromMM(nc_def["diff_pair_via_gap"]))
            if "priority" in nc_def and nc_def["priority"] is not None:
                netclass.SetPriority(int(nc_def["priority"]))
            # Set color if provided
            if "color" in nc_def and nc_def["color"] is not None:
                try:
                    color = pcbnew.COLOR4D(nc_def["color"])
                    netclass.SetPcbColor(color)
                except Exception as e:
                    logger.warning(
                        f"Failed to parse color '{nc_def['color']}' for netclass '{name}': {e}"
                    )

        # Check if netclasses need updating
        needs_update = False
        existing_netclasses = {}

        # Get existing netclasses - GetNetclasses() returns a map-like object
        netclasses_map = netSettings.GetNetclasses()
        for name, nc in netclasses_map.items():
            existing_netclasses[name] = nc

        # Check for differences
        desired_names = {
            nc_def["name"]
            for nc_def in values
            if isinstance(nc_def, dict) and "name" in nc_def
        }
        existing_names = set(existing_netclasses.keys())

        # Check if sets of names differ
        if desired_names != existing_names:
            needs_update = True
            logger.debug(
                f"Netclass names differ - desired: {desired_names}, existing: {existing_names}"
            )
        else:
            # Check if any netclass properties differ
            for nc_def in values:
                if not isinstance(nc_def, dict) or "name" not in nc_def:
                    continue
                name = nc_def["name"]
                if name in existing_netclasses:
                    if not netclass_matches(existing_netclasses[name], nc_def):
                        needs_update = True
                        logger.debug(f"Netclass '{name}' properties differ")
                        break

        if not needs_update:
            logger.debug(f"Netclasses unchanged ({len(values)} netclasses)")
            return

        # Netclasses differ, rebuild them
        logger.info(f"Updating netclasses: {len(values)} netclasses")
        netSettings.ClearNetclasses()

        for nc_def in values:
            if not isinstance(nc_def, dict) or "name" not in nc_def:
                logger.warning(f"Netclass definition must have 'name' key: {nc_def}")
                continue

            name = nc_def["name"]
            if not name:
                logger.warning("Netclass name cannot be empty")
                continue

            # Create new netclass (False = don't init with defaults)
            netclass = pcbnew.NETCLASS(name, False)
            apply_netclass_properties(netclass, nc_def)

            # Special handling for "Default" netclass
            if name == "Default":
                # Update the actual internal default netclass (m_DefaultNetclass)
                default_nc = netSettings.GetDefaultNetclass()
                apply_netclass_properties(default_nc, nc_def)
            else:
                # Add netclass to the collection (skip for Default since it's already there)
                netSettings.SetNetclass(netclass.GetName(), netclass)

    def _apply_board_config(self):
        """Apply board configuration from JSON file to design settings."""
        if not self.board_config_path:
            return

        logger.info(f"Applying board configuration from {self.board_config_path}")

        try:
            with open(self.board_config_path, "r") as f:
                config = json.load(f)
        except (FileNotFoundError, json.JSONDecodeError) as e:
            logger.error(f"Failed to load board config: {e}")
            return

        ds = self.board.GetDesignSettings()

        # Apply all configuration mappings
        for mapping in self.CONFIG_MAPPINGS:
            json_path, ds_attr, display_name = mapping[:3]
            custom_setter = mapping[3] if len(mapping) > 3 else None  # type: ignore[misc]

            value = self._get_nested_value(config, json_path)
            if value is not None:
                if custom_setter:
                    if callable(custom_setter):
                        # Lambda function
                        custom_setter(ds, value)
                    elif custom_setter == "track_widths":
                        self._set_track_width_list(ds, value)
                    elif custom_setter == "via_dimensions":
                        self._set_via_dimensions_list(ds, value)
                    elif custom_setter == "netclasses":
                        self._set_netclasses(ds, value)
                    else:
                        logger.warning(f"Unknown custom setter: {custom_setter}")
                else:
                    # Compare before setting to avoid unnecessary modifications
                    assert ds_attr is not None
                    current_value = getattr(ds, ds_attr)
                    new_value = pcbnew.FromMM(value)

                    if current_value != new_value:
                        setattr(ds, ds_attr, new_value)
                        logger.info(
                            f"Updated {display_name}: {pcbnew.ToMM(current_value):.3f}mm -> {value}mm"
                        )
                    else:
                        logger.debug(f"{display_name} unchanged: {value}mm")

    def _setup_title_block(self):
        """Configure the title block with variable placeholders."""
        title_block = self.board.GetTitleBlock()
        title_block.SetTitle("${PCB_NAME}")
        title_block.SetDate("${CURRENT_DATE}")
        title_block.SetRevision("${PCB_VERSION}")
        logger.info("Configured title block with variable placeholders")

    def run(self):
        # Setup title block with variable placeholders
        self._setup_title_block()

        # Apply board config logic
        should_apply_config = self.board_config_path and self.sync_board_config

        if should_apply_config:
            self._apply_board_config()


####################################################################################################
# Step 1. Import Netlist
#
# The first step is to import the netlist, which means matching the set of footprints in the
# netlist to the footprints on the board.
####################################################################################################


class ImportNetlist(Step):
    """Import the netlist into the board by syncing footprints and nets."""

    def __init__(
        self,
        state: SyncState,
        board: pcbnew.BOARD,
        board_path: Path,
        netlist: JsonNetlistParser,
        dry_run: bool = False,
    ):
        self.state = state
        self.board = board
        self.board_path = board_path
        self.netlist = netlist
        self.dry_run = dry_run

        # Map from footprint library name to library path.
        self.footprint_lib_map = {}

    def _setup_env(self):
        """Set up environment variables for footprint resolution."""
        if "KIPRJMOD" not in os.environ.keys():
            os.environ["KIPRJMOD"] = os.path.dirname(self.board_path)

        if "KICAD9_FOOTPRINT_DIR" not in os.environ.keys():
            if os.name == "nt":
                os.environ["KICAD9_FOOTPRINT_DIR"] = (
                    "C:/Program Files/KiCad/9.0/share/kicad/footprints/"
                )
            elif sys.platform == "darwin":
                os.environ["KICAD9_FOOTPRINT_DIR"] = (
                    "/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints/"
                )
            else:
                os.environ["KICAD9_FOOTPRINT_DIR"] = "/usr/share/kicad/footprints"

        if "KISYSMOD" not in os.environ.keys():
            if os.name == "nt":
                os.environ["KISYSMOD"] = (
                    "C:/Program Files/KiCad/9.0/share/kicad/modules"
                )
            else:
                os.environ["KISYSMOD"] = "/usr/share/kicad/modules"

    def _load_footprint_lib_map(self):
        """Populate self.footprint_lib_map with the global and local fp-lib-table paths."""

        def _load_fp_lib_table(path: str):
            """Load the fp-lib-table from the given path and return the path if found."""
            # Read contents of footprint library file into a single string.
            try:
                with open(path) as fp:
                    tbl = fp.read()
            except IOError:
                return

            # Get individual "(lib ...)" entries from the string.
            libs = re.findall(
                r"\(\s*lib\s* .*? \)\)",
                tbl,
                flags=re.IGNORECASE | re.VERBOSE | re.DOTALL,
            )

            # Add the footprint modules found in each enabled KiCad library.
            for lib in libs:
                # Skip disabled libraries.
                disabled = re.findall(
                    r"\(\s*disabled\s*\)", lib, flags=re.IGNORECASE | re.VERBOSE
                )
                if disabled:
                    continue

                # Skip non-KiCad libraries (primarily git repos).
                type_ = re.findall(
                    r'(?:\(\s*type\s*) ("[^"]*?"|[^)]*?) (?:\s*\))',
                    lib,
                    flags=re.IGNORECASE | re.VERBOSE,
                )[0]
                if "kicad" not in type_.lower():
                    continue

                # Get the library directory and nickname.
                uri = re.findall(
                    r'(?:\(\s*uri\s*) ("[^"]*?"|[^)]*?) (?:\s*\))',
                    lib,
                    flags=re.IGNORECASE | re.VERBOSE,
                )[0]
                nickname = re.findall(
                    r'(?:\(\s*name\s*) ("[^"]*?"|[^)]*?) (?:\s*\))',
                    lib,
                    flags=re.IGNORECASE | re.VERBOSE,
                )[0]

                # Remove any quotes around the URI or nickname.
                uri = rmv_quotes(uri)
                nickname = rmv_quotes(nickname)

                # Expand variables and ~ in the URI.
                uri = os.path.expandvars(os.path.expanduser(uri))

                if nickname in self.footprint_lib_map:
                    logger.info(
                        f"Overwriting {nickname}:{self.footprint_lib_map[nickname]} with {nickname}:{uri}"
                    )
                self.footprint_lib_map[nickname] = uri

        # Find and load the global fp-lib-table.
        paths = (
            "$HOME/.config/kicad",
            "~/.config/kicad",
            "%APPDATA%/kicad",
            "$HOME/Library/Preferences/kicad",
            "~/Library/Preferences/kicad",
            "%ProgramFiles%/KiCad/share/kicad/template",
            "/usr/share/kicad/template",
            "/Applications/KiCad/Kicad.app/Contents/SharedSupport/template",
            "C:/Program Files/KiCad/9.0/share/kicad/template",
        )

        for path in paths:
            path = os.path.normpath(os.path.expanduser(os.path.expandvars(path)))
            fp_lib_table_path = os.path.join(path, "fp-lib-table")
            if os.path.exists(fp_lib_table_path):
                _load_fp_lib_table(fp_lib_table_path)

        # Load the local fp-lib-table.
        local_fp_lib_table_path = os.path.join(
            os.path.dirname(self.board_path), "fp-lib-table"
        )

        if os.path.exists(local_fp_lib_table_path):
            _load_fp_lib_table(local_fp_lib_table_path)

    def _emit_diagnostic(
        self, kind: str, severity: str, body: str, part: Any = None, fp: Any = None
    ):
        """Emit a diagnostic with optional reference info from footprint or part."""
        diag = {
            "kind": kind,
            "severity": severity,
            "body": body,
            "path": "",
            "reference": None,
        }
        if part:
            diag["path"] = part.sheetpath.names.split(":")[-1]
            diag["reference"] = part.ref
        if fp and not diag["reference"]:
            diag["reference"] = fp.GetReference()
        self.state.layout_diagnostics.append(diag)

    def _sync_footprints(self):
        """Remove footprints from the board that are not in the netlist, and add new ones that are missing from the board."""
        netlist_footprint_ids = set(
            part.sheetpath.tstamps for part in self.netlist.parts
        )

        board_fps_by_uuid = footprints_by_uuid(self.board)
        board_footprint_ids = set(board_fps_by_uuid.keys())

        for fp_id in board_footprint_ids - netlist_footprint_ids:
            # Footprint on board but not in netlist - should be removed
            fp = board_fps_by_uuid[fp_id]
            if self.dry_run:
                # Get module path if available
                path_field = fp.GetFieldByName("Path")
                path_info = (
                    f" at {path_field.GetText()}"
                    if path_field and path_field.GetText()
                    else ""
                )
                fpid = fp.GetFPIDAsString()
                self._emit_diagnostic(
                    "layout.sync.extra_footprint",
                    "warning",
                    f"Footprint '{fp.GetReference()}' ({fpid}){path_info} exists on board but not in netlist. "
                    f"Run 'pcb layout' to remove it.",
                    fp=fp,
                )
            else:
                logger.info(f"{fp_id} ({fp.GetReference()}): Removing from board")
                self.state.track_footprint_removed(fp)
                self.board.Delete(fp)

        # Handle footprints in netlist but not on board - should be added
        for fp_id in netlist_footprint_ids - board_footprint_ids:
            part = next(
                part for part in self.netlist.parts if part.sheetpath.tstamps == fp_id
            )

            if self.dry_run:
                context = part.sheetpath.names.split(":")[-1]
                path_info = f" at {context}" if context else ""
                self._emit_diagnostic(
                    "layout.sync.missing_footprint",
                    "error",
                    f"Footprint '{part.ref}' ({part.footprint}){path_info} is missing from board. "
                    f"Run 'pcb layout' to add it.",
                    part=part,
                )
                continue

            logger.info(f"{fp_id} ({part.ref}): Adding to board")

            # Load footprint from library
            fp_lib, fp_name = part.footprint.split(":")
            lib_uri = self.footprint_lib_map[fp_lib]

            # (Deal with Windows extended path prefix)
            lib_uri = lib_uri.replace("\\\\?\\", "")

            logger.info(f"Loading footprint {fp_name} from {lib_uri}")

            try:
                fp = pcbnew.FootprintLoad(lib_uri, fp_name)
            except Exception as e:
                logger.error(
                    f"Unable to find footprint '{fp_name}' in library '{fp_lib}'. "
                    f"Please check that the footprint library is installed and the footprint name is correct."
                )
                raise e

            if fp is None:
                logger.error(
                    f"Unable to find footprint '{fp_name}' in library '{fp_lib}'. "
                    f"Please check that the footprint library is installed and the footprint name is correct."
                )
                raise ValueError(
                    f"Footprint '{fp_name}' not found in library '{fp_lib}'"
                )

            fp.SetParent(self.board)

            # Phase 1: Detect changes (for new footprints)
            changeset = compute_footprint_changes(fp, part, is_existing=False)

            # Phase 2: Apply changes (always apply for new footprints)
            apply_footprint_changes(fp, changeset.changes, pcbnew)

            self.board.Add(fp)
            self.state.track_footprint_added(fp)

        # Handle footprints that exist in both netlist and board
        for fp_id in netlist_footprint_ids & board_footprint_ids:
            fp = self.board.FindFootprintByPath(pcbnew.KIID_PATH(f"{fp_id}/{fp_id}"))
            part = next(
                part for part in self.netlist.parts if part.sheetpath.tstamps == fp_id
            )

            # Phase 1: Detect changes
            changeset = compute_footprint_changes(fp, part, is_existing=True)

            # Phase 2: Apply or Report
            if self.dry_run:
                # In dry_run mode, report changes as diagnostics
                if changeset.needs_replacement:
                    old_fpid = fp.GetFPIDAsString()
                    context = part.sheetpath.names.split(":")[-1]
                    path_info = f" at {context}" if context else ""
                    self._emit_diagnostic(
                        "layout.sync.fpid_mismatch",
                        "error",
                        f"Footprint '{old_fpid}'{path_info} should be '{part.footprint}'. "
                        f"Run 'pcb layout' to replace it.",
                        part=part,
                    )
                elif changeset.changes:

                    def format_change(c: Change) -> str:
                        # Extract readable field name from field identifier
                        if c.field.startswith("field:"):
                            field_name = c.field.split(":")[1]
                        else:
                            field_name = c.field
                        return f"{field_name}: '{c.old}' -> '{c.new}'"

                    changes_desc = ", ".join(
                        format_change(c) for c in changeset.changes
                    )
                    self._emit_diagnostic(
                        "layout.sync.metadata_mismatch",
                        "warning",
                        f"Metadata out of sync: {changes_desc}. "
                        f"Run 'pcb layout' to update.",
                        part=part,
                    )
            else:
                # Normal mode: apply changes or replace footprint
                if changeset.needs_replacement:
                    # FPID changed - need to replace entire footprint
                    old_fpid = fp.GetFPIDAsString()
                    fp_lib, fp_name = part.footprint.split(":")
                    lib_uri = self.footprint_lib_map[fp_lib]
                    lib_uri = lib_uri.replace("\\\\?\\", "")

                    logger.info(
                        f"{fp_id} ({part.ref}): Replacing footprint '{old_fpid}' -> '{part.footprint}'"
                    )

                    new_fp = replace_footprint(self.board, fp, part, lib_uri, pcbnew)
                    self.state.track_footprint_updated(new_fp)
                elif changeset.changes:
                    apply_footprint_changes(fp, changeset.changes, pcbnew)
                    self.state.track_footprint_updated(fp)
                    logger.info(
                        f"{fp_id} ({part.ref}): Updated {len(changeset.changes)} field(s)"
                    )
                    for change in changeset.changes:
                        logger.debug(
                            f"  - {change.field}: {change.old} -> {change.new}"
                        )
                else:
                    logger.debug(f"{part.ref}: No metadata changes detected")

    def _sync_nets(self):
        """Sync the nets in the netlist to the board."""
        for net in self.netlist.nets:
            pcb_net = pcbnew.NETINFO_ITEM(self.board, net.name)
            self.board.Add(pcb_net)

            logger.info(f"Adding net {net.name}")

            pins = net.nodes

            # Connect the part pins on the netlist net to the PCB net.
            for pin in pins:
                pin_ref, pin_num, _ = pin
                module = self.board.FindFootprintByReference(pin_ref)
                if not module:
                    continue

                pad = None
                while True:
                    pad = module.FindPadByNumber(pin_num, pad)
                    if pad:
                        logger.info(
                            f"Connecting pad {module.GetReference()}/{pad.GetPadName()} to net {net.name}"
                        )
                        pad.SetNet(pcb_net)
                    else:
                        break  # Done with all pads for this pin number.

    def _sync_groups(self):
        """Create or update KiCad groups based on the module hierarchy in the netlist."""
        # First, collect all unique hierarchical paths from the netlist
        all_paths = set()
        path_to_parts = {}  # Map from path to list of parts at that path

        for part in self.netlist.parts:
            # Get the hierarchical path from the part
            path = part.sheetpath.names.split(":")[-1]  # Get the last component
            if path:
                # Store this part under its path
                if path not in path_to_parts:
                    path_to_parts[path] = []
                path_to_parts[path].append(part)

                # Add this path and all parent paths to our set
                parts = path.split(".")
                for i in range(1, len(parts) + 1):
                    all_paths.add(".".join(parts[:i]))

        # Use the groups registry as the source of truth
        existing_groups = dict(self.state.groups_registry)

        # Determine which groups to create based on child count
        groups_to_create = {}
        for path in all_paths:
            # Count all items that would be direct children of this group
            direct_child_count = 0

            # Count footprints directly at this path
            if path in path_to_parts:
                direct_child_count += len(path_to_parts[path])

            # Count child groups (paths that are direct children)
            for other_path in all_paths:
                if other_path != path and other_path.startswith(path + "."):
                    # Check if this is a direct child (no additional dots)
                    remainder = other_path[len(path) + 1 :]
                    if "." not in remainder:
                        direct_child_count += 1

            # Check if this module has a layout_path (will have graphics/zones)
            has_layout = False
            if path in self.netlist.modules:
                module = self.netlist.modules[path]
                if module.layout_path:
                    has_layout = True

            # Create group if it has children OR if it has a layout (which will have graphics/zones)
            if direct_child_count > 1 or has_layout:
                groups_to_create[path] = True
                logger.debug(
                    f"Will create group {path} with {direct_child_count} children"
                    + (" and layout" if has_layout else "")
                )

        # Create or update groups, ensuring hierarchical structure
        created_groups = {}
        for path in sorted(
            groups_to_create.keys()
        ):  # Sort to ensure parents are created first
            if path in existing_groups:
                # Group already exists, just track it
                group = existing_groups[path]
                created_groups[path] = group
                logger.info(f"Using existing group: {path}")
            else:
                # Create new group
                group = pcbnew.PCB_GROUP(self.board)
                group.SetName(path)
                self.board.Add(group)
                created_groups[path] = group
                # Update the registry with the new group
                self.state.groups_registry[path] = group
                logger.info(f"Created new group: {path}")

                # Find parent group and add this group as a child
                parts = path.split(".")
                if len(parts) > 1:
                    # Look for parent group
                    for i in range(len(parts) - 1, 0, -1):
                        parent_path = ".".join(parts[:i])
                        if parent_path in created_groups:
                            parent_group = created_groups[parent_path]
                            parent_group.AddItem(group)
                            logger.debug(
                                f"Added group {path} as child of {parent_path}"
                            )
                            break

        # Now assign footprints to their groups
        footprints_by_uuid_dict = footprints_by_uuid(self.board)

        for part in self.netlist.parts:
            fp_uuid = part.sheetpath.tstamps
            if fp_uuid not in footprints_by_uuid_dict:
                continue  # Footprint not on board yet

            fp = footprints_by_uuid_dict[fp_uuid]
            path = part.sheetpath.names.split(":")[-1]

            if not path:
                continue  # No hierarchical path

            # Find the most specific group this footprint belongs to
            best_group = None
            best_path = ""

            # Check all possible parent paths, from most specific to least
            parts = path.split(".")
            for i in range(len(parts), 0, -1):
                parent_path = ".".join(parts[:i])
                if parent_path in created_groups:
                    best_group = created_groups[parent_path]
                    best_path = parent_path
                    break

            if best_group:
                # Remove from any existing group first
                if fp.GetParentGroup():
                    fp.GetParentGroup().RemoveItem(fp)

                # Add to the new group
                best_group.AddItem(fp)
                logger.debug(f"Added {fp.GetReference()} to group {best_path}")

        # Remove empty groups and update registry
        for group_name, group in existing_groups.items():
            if group_name and len(get_group_items(group)) == 0:
                logger.info(f"Removing empty group: {group_name}")
                self.board.Remove(group)
                # Remove from registry
                if group_name in self.state.groups_registry:
                    del self.state.groups_registry[group_name]

    def _refresh_board(self):
        self.board.BuildListOfNets()
        pcbnew.Refresh()

    def _build_virtual_dom(self):
        """Build the virtual DOM from the current board state."""
        self.state.virtual_board = build_virtual_dom_from_board(self.board)

        # Now mark items as added based on our tracking
        for fp_uuid in self.state.added_footprint_uuids:
            item = self.state.virtual_board.root.find_by_id(fp_uuid)
            if item:
                item.added = True

    def _log_virtual_dom(self):
        """Log the contents of the virtual DOM."""
        logger.info("Virtual DOM structure:")
        logger.info(self.state.virtual_board.render())

    def run(self):
        """Run the import process."""
        # Setup environment
        setup_start = time.time()
        self._setup_env()
        logger.debug(f"Environment setup took {time.time() - setup_start:.3f} seconds")

        # Load footprint library map (needed even in dry-run to detect missing libs)
        lib_start = time.time()
        self._load_footprint_lib_map()
        logger.debug(
            f"Footprint library map loading took {time.time() - lib_start:.3f} seconds"
        )

        # Sync footprints (in dry-run mode, emits diagnostics instead of making changes)
        sync_start = time.time()
        self._sync_footprints()
        logger.info(
            f"Footprint synchronization took {time.time() - sync_start:.3f} seconds"
        )

        # In dry-run mode, we only detect issues - don't modify the board further
        if self.dry_run:
            logger.info("Dry-run mode: skipping remaining sync steps")
            return

        # Sync nets
        nets_start = time.time()
        self._sync_nets()
        logger.info(f"Net synchronization took {time.time() - nets_start:.3f} seconds")

        # Sync groups
        groups_start = time.time()
        self._sync_groups()
        logger.info(
            f"Group synchronization took {time.time() - groups_start:.3f} seconds"
        )

        # Refresh board
        refresh_start = time.time()
        self._refresh_board()
        logger.debug(f"Board refresh took {time.time() - refresh_start:.3f} seconds")

        # Build virtual DOM
        vdom_start = time.time()
        self._build_virtual_dom()
        logger.info(f"Virtual DOM building took {time.time() - vdom_start:.3f} seconds")

        # Log virtual DOM contents
        self._log_virtual_dom()


####################################################################################################
# Step 2. Sync Layouts
####################################################################################################


class SyncLayouts(Step):
    """Sync layouts from layout files to groups marked as newly added."""

    def __init__(
        self, state: SyncState, board: pcbnew.BOARD, netlist: JsonNetlistParser
    ):
        self.state = state
        self.board = board
        self.netlist = netlist

    def _sync_connected_item(
        self,
        item: Any,
        net_code_map: Dict[int, int],
        group: VirtualGroup,
        item_name: Optional[str] = None,
    ) -> VirtualConnectedItem:
        """Sync a single connected item (zone, track, or via) to the target board.

        Args:
            item: The source item to sync
            net_code_map: Mapping of source to target net codes
            group: The group to add the item to
            item_name: Optional name for the item

        Returns:
            The created VirtualConnectedItem
        """
        # Use Duplicate() to copy all properties automatically
        new_item = item.Duplicate()

        # Apply net code mapping (this is the only thing we need to update)
        source_net_code = item.GetNetCode()
        if source_net_code in net_code_map:
            target_net_code = net_code_map[source_net_code]
            target_net = self.board.FindNet(target_net_code)
            if target_net:
                new_item.SetNet(target_net)
        else:
            new_item.SetNetCode(0)

        # Add to board
        self.board.Add(new_item)

        # Create virtual connected item and add to group
        item_uuid = str(uuid.uuid4())

        # Determine item name if not provided
        if item_name is None:
            item_class = item.GetClass()
            if "VIA" in item_class.upper():
                item_name = f"Via_{item_uuid[:8]}"
            elif "TRACK" in item_class.upper():
                item_name = f"Track_{item_uuid[:8]}"
            elif "ZONE" in item_class.upper():
                item_name = item.GetZoneName() or f"Zone_{item_uuid[:8]}"
            else:
                item_name = f"{item_class}_{item_uuid[:8]}"

        vitem = VirtualConnectedItem(item_uuid, item_name, new_item)
        vitem.added = True
        group.add_child(vitem)

        return vitem

    def _sync_group_layout(self, group: VirtualGroup, layout_file: Path):
        """Sync all elements (footprints, zones, graphics) in a group from a layout file."""
        # Load the layout file into a virtual board
        layout_board = pcbnew.LoadBoard(str(layout_file))
        layout_vboard = build_virtual_dom_from_board(layout_board)

        # Get all footprints in the target group (recursively)
        target_footprints = self._get_footprints_in_group(group)

        # Get all footprints from the layout
        source_footprints = self._get_all_footprints(layout_vboard.root)

        # Build maps for matching footprints
        target_by_path = {}  # relative_path -> VirtualFootprint
        for fp in target_footprints:
            # Get the footprint's path relative to the group
            full_path = fp.name  # This is the full hierarchical path
            if full_path.startswith(group.id + "."):
                relative_path = full_path[len(group.id) + 1 :]
            elif full_path == group.id:
                relative_path = ""  # The group itself as a footprint
            else:
                continue  # Skip if not in this group

            target_by_path[relative_path] = fp

        source_by_path = {}  # path -> VirtualFootprint
        for fp in source_footprints:
            source_by_path[fp.name] = fp

        # Match footprints and build matched pairs
        matched_pairs = []
        unmatched_target = []
        unmatched_source = []

        # Try to match each target footprint by path only
        for rel_path, target_fp in target_by_path.items():
            if rel_path in source_by_path:
                # Found a match - sync the footprint
                source_fp = source_by_path[rel_path]
                target_fp.replace_with(source_fp)
                matched_pairs.append((source_fp, target_fp))
                logger.debug(f"  Matched and synced: {target_fp.name}")
            else:
                unmatched_target.append(target_fp.name)
                logger.debug(f"  No match found for: {target_fp.name}")

        # Find source footprints that weren't matched
        matched_sources = set()
        for rel_path in target_by_path:
            if rel_path in source_by_path:
                matched_sources.add(rel_path)

        for src_path in source_by_path:
            if src_path not in matched_sources:
                unmatched_source.append(src_path)

        # Log footprint results
        logger.info(f"  Synced {len(matched_pairs)} footprints")
        if unmatched_target:
            logger.warning(
                f"  {len(unmatched_target)} footprints in group had no match in layout:"
            )
            for fp in unmatched_target:
                logger.warning(f"    - {fp}")

        if unmatched_source:
            logger.info(
                f"  {len(unmatched_source)} footprints in layout had no match in group:"
            )
            for fp in unmatched_source:
                logger.info(f"    - {fp}")

        # Only sync zones and graphics if we matched at least one footprint
        if matched_pairs:
            # Build net code mapping from matched footprints
            net_code_map = build_net_code_mapping(
                layout_board, self.board, matched_pairs
            )

            # Sync all connected items (zones and tracks/vias)
            zones_synced = 0
            tracks_synced = 0
            vias_synced = 0

            # Sync zones
            # Convert to list first to avoid iterator issues on Windows
            zones_list = list(layout_board.Zones())
            for zone in zones_list:
                self._sync_connected_item(zone, net_code_map, group)
                zones_synced += 1

            # Sync tracks and vias
            # Convert to list first to avoid iterator issues on Windows
            tracks_list = list(layout_board.GetTracks())
            for item in tracks_list:
                self._sync_connected_item(item, net_code_map, group)
                # Count tracks vs vias for logging
                item_class = item.GetClass()
                if "VIA" in item_class.upper():
                    vias_synced += 1
                else:
                    tracks_synced += 1

            # Get all graphics from source layout
            graphics_synced = 0
            # Convert to list first to avoid iterator issues on Windows
            drawings_list = list(layout_board.GetDrawings())
            for drawing in drawings_list:
                # Skip footprint graphics
                if drawing.GetParent() and isinstance(
                    drawing.GetParent(), pcbnew.FOOTPRINT
                ):
                    continue

                # Use Duplicate() to copy all graphic properties automatically
                new_drawing = drawing.Duplicate()

                # Add to board
                self.board.Add(new_drawing)

                # Create virtual graphic and add to group
                item_type = drawing.GetClass()
                graphic_uuid = str(uuid.uuid4())
                vgraphic = VirtualGraphic(
                    graphic_uuid, f"{item_type}_{graphic_uuid[:8]}", new_drawing
                )
                vgraphic.added = True
                group.add_child(vgraphic)
                graphics_synced += 1

            logger.info(
                f"  Synced {zones_synced} zones, {graphics_synced} graphics, {tracks_synced} tracks, and {vias_synced} vias"
            )

            # Mark the group as synced
            group.synced = True

            # Find and link to KiCad group if it exists (use registry)
            kicad_group = self.state.groups_registry.get(group.id)
            if kicad_group:
                # Add zones, graphics, tracks and vias to the KiCad group
                for child in group.children:
                    if isinstance(
                        child,
                        (VirtualConnectedItem, VirtualGraphic),
                    ):
                        kicad_group.AddItem(child.kicad_item)

            logger.info(f"  Marked group {group.id} as synced")

        # Explicitly delete the layout board to release resources (important for Windows)
        del layout_board

    def _get_footprints_in_group(self, group: VirtualGroup) -> List[VirtualFootprint]:
        """Get all footprints within a group (recursively)."""
        return group.find_all_footprints()

    def _get_all_footprints(self, root: VirtualGroup) -> List[VirtualFootprint]:
        """Get all footprints in a virtual board."""
        return root.find_all_footprints()

    def run(self):
        """Find groups that are marked as 'added' and have layout_path, then sync them."""
        # Use BFS to traverse the virtual DOM and sync only the top-most layouts
        # Once we sync a group, we don't process its children

        from collections import deque

        # Start BFS from the root
        queue = deque([self.state.virtual_board.root])
        synced_count = 0

        while queue:
            current = queue.popleft()

            # Skip non-group items
            if not isinstance(current, VirtualGroup):
                continue

            # Skip the root board group
            if current.id == "board":
                # Add children to queue for processing
                queue.extend(current.children)
                continue

            # Check if this group should be synced
            should_sync = False
            layout_file = None

            if current.added:
                # Check if this group has a corresponding module with layout_path
                module = self.netlist.modules.get(current.id)
                if module and module.layout_path:
                    # Resolve the layout path
                    layout_path = Path(module.layout_path)
                    if not layout_path.is_absolute():
                        layout_path = (
                            Path(self.board.GetFileName()).parent / layout_path
                        )

                    layout_file = layout_path / "layout.kicad_pcb"

                    # Check if layout file exists
                    if layout_file.exists():
                        should_sync = True
                    else:
                        logger.warning(
                            f"Layout file not found for {current.id} at {layout_file}. "
                            f"Skipping layout sync for this module."
                        )

            if should_sync:
                assert layout_file is not None
                # Sync this group
                logger.info(f"Syncing layout for group {current.id} from {layout_file}")
                self._sync_group_layout(current, layout_file)
                synced_count += 1

                # Don't process children of synced groups - they're handled by the layout
                logger.debug(
                    f"Skipping children of {current.id} as it was synced from layout"
                )
            else:
                # Only add children to queue if we didn't sync this group
                queue.extend(current.children)

                if not current.added:
                    logger.debug(f"Skipping group {current.id} - not newly added")
                else:
                    logger.debug(f"Skipping group {current.id} - no layout_path")

        logger.info(f"Completed layout sync: synced {synced_count} groups")


####################################################################################################
# Step 3. Place new footprints and groups
####################################################################################################


class PlaceComponents(Step):
    """Place new footprints and groups on the board using hierarchical placement.

    This uses a depth-first search (DFS) to traverse the virtual DOM and place
    components bottom-up. Synced groups are treated as atomic units, while NEW
    siblings are packed together using the HierPlace algorithm.
    """

    def __init__(
        self, state: SyncState, board: pcbnew.BOARD, netlist: JsonNetlistParser
    ):
        self.state = state
        self.board = board
        self.netlist = netlist
        self.MODULE_SPACING = 350000  # 0.35mm spacing between footprints
        self.GROUP_SPACING = 5 * 350000  # 1.75mm spacing between groups

    def _hierplace_pack(self, items: List[VirtualItem]) -> None:
        """Pack items using the HierPlace algorithm (corner-based placement).

        This algorithm places items by considering top-left and bottom-right
        corners as potential placement points, choosing positions that minimize
        the overall bounding box while avoiding overlaps.

        Args:
            items: List of VirtualItems to pack (modifies their positions in-place)
        """
        if not items:
            return

        # Filter out items without bounding boxes
        items_with_bbox = [item for item in items if item.bbox]
        if not items_with_bbox:
            return

        # Sort by area (largest first) for better packing, then by name for determinism
        # Use natural sort for names so "C2" comes before "C10"
        items_with_bbox.sort(
            key=lambda item: (-item.bbox.area, natural_sort_key(item.name), item.id)
        )

        # Storage for potential placement points (as (x, y) tuples)
        # These are points where we can place the bottom-left corner of an item
        # Use a list to maintain insertion order for determinism
        placement_pts = []

        # Track placed items for collision detection
        placed_items = []

        for i, item in enumerate(items_with_bbox):
            logger.info(f"Placing {item.name}...")

            if i == 0:
                # First item serves as anchor at origin
                # For synced groups, use rigid body transformation
                # Move item to origin
                item.move_to(0, 0)
                placed_items.append(item)
                # Add its corners as placement points
                placement_pts.extend(
                    [
                        (item.bbox.left, item.bbox.top),  # Top-left
                        (item.bbox.right, item.bbox.bottom),  # Bottom-right
                    ]
                )

                logger.info(f"Placed {item.name} at {item.bbox}")
            else:
                # Store original position to restore if needed
                original_pos = item.get_position()

                # Find best placement point for this item
                best_pt = None
                smallest_size = float("inf")

                for pt_idx, (pt_x, pt_y) in enumerate(placement_pts):
                    logger.debug(f"Trying placement point {pt_x}, {pt_y}")

                    # Move item's bottom-left corner to this placement point
                    # Since move_to uses top-left, we need to adjust
                    item.move_to(pt_x, pt_y - item.bbox.height)

                    # Check for collisions with placed items
                    collision = False

                    for placed in placed_items:
                        if item.intersects_with(placed):
                            logger.debug(f"Collision detected with {placed.name}")
                            collision = True
                            break

                    if not collision:
                        # Calculate the size metric for this placement
                        # Get bounding box of all placed items plus current item
                        all_bbox = item.bbox
                        for placed in placed_items:
                            all_bbox = all_bbox.merge(placed.bbox)

                        # Size metric: sum of dimensions plus aspect ratio penalty
                        size = (
                            all_bbox.width
                            + all_bbox.height
                            + abs(all_bbox.width - all_bbox.height)
                        )

                        # If size is equal, use placement point index as tiebreaker for determinism
                        if size < smallest_size or (
                            size == smallest_size and best_pt is None
                        ):
                            smallest_size = size
                            best_pt = (
                                item.bbox.left,
                                item.bbox.top,
                            )  # Store current top-left

                if best_pt:
                    # Move to the best position found
                    # For synced groups, use rigid body transformation
                    # Move item to best position
                    item.move_to(best_pt[0], best_pt[1])
                    placed_items.append(item)

                    logger.info(f"Placed {item.name} at {best_pt}")

                    # Remove the used placement point
                    # The placement point that was used is the bottom-left corner
                    used_pt = (item.bbox.left, item.bbox.bottom)
                    # Create new list without the used point to maintain order
                    placement_pts = [pt for pt in placement_pts if pt != used_pt]

                    # Add new placement points from this item
                    placement_pts.extend(
                        [
                            (item.bbox.left, item.bbox.top),  # Top-left
                            (item.bbox.right, item.bbox.bottom),  # Bottom-right
                        ]
                    )
                else:
                    # Restore original position if we couldn't find a placement
                    if original_pos:
                        item.move_to(original_pos[0], original_pos[1])

                    raise RuntimeError(f"Could not find placement for item {item.name}")

    def _process_group_dfs(self, group: VirtualItem) -> Optional[VirtualItem]:
        """Process a group and its children using depth-first search.

        This implements the core placement logic:
        1. Recursively process child groups/footprints first (bottom-up)
        2. Collect sparse subtrees representing only placed items
        3. If any items were placed, arrange them and return a sparse group
        4. The returned VirtualItem tree contains only items that were placed

        Args:
            group: The VirtualItem to process (can be GROUP or EDA_ITEM)

        Returns:
            A sparse VirtualItem containing only placed items, or None if nothing was placed
        """
        # Handle footprints (leaf nodes)
        if isinstance(group, VirtualFootprint):
            if group.added:
                # This footprint needs placement, return it
                return group
            else:
                # This footprint doesn't need placement
                return None

        # Handle groups
        if not isinstance(group, VirtualGroup):
            return None

        # If this group was synced from a layout file, it's already positioned
        # as a unit - return it as-is if it was added
        if group.synced:
            if group.added:
                return group
            else:
                return None

        # Sort children by name and id for deterministic processing order
        # Use natural sort for names so "C2" comes before "C10"
        sorted_children = sorted(
            group.children, key=lambda child: (natural_sort_key(child.name), child.id)
        )

        # Recursively process all children and collect sparse subtrees
        placed_children = []
        for child in sorted_children:
            placed_subtree = self._process_group_dfs(child)
            if placed_subtree:
                placed_children.append(placed_subtree)

        if not placed_children:
            # Nothing was placed in this subtree
            return None

        # Create a sparse group containing only placed children
        sparse_group = VirtualGroup(
            group.id,
            group.name,
        )

        # Add placed children to the sparse group
        for child in placed_children:
            sparse_group.add_child(child)

        logger.info(f"Placing {len(placed_children)} items in group {group.name}")
        for child in placed_children:
            logger.info(f"\n{child.render_tree()}")

        # Pack the items using HierPlace algorithm
        self._hierplace_pack(placed_children)

        # Update the sparse group's bounding box based on placed children
        sparse_group.update_bbox_from_children()

        return sparse_group

    def _position_relative_to_existing(
        self, sparse_tree: Optional[VirtualItem]
    ) -> None:
        """Position all newly placed content relative to existing content on the board.

        Args:
            sparse_tree: The sparse VirtualItem tree containing only placed items
        """
        if not sparse_tree:
            logger.info("No items were placed")
            return

        # Find top-level items in the sparse tree
        top_level_added = []
        if isinstance(sparse_tree, VirtualGroup):
            # If root is a group, use its children as top-level items
            # Sort them for deterministic ordering using natural sort
            top_level_added = sorted(
                sparse_tree.children,
                key=lambda item: (natural_sort_key(item.name), item.id),
            )
        else:
            # If root is a single item, use it
            top_level_added = [sparse_tree]

        if not top_level_added:
            logger.info("No added items to position")
            return

        # Calculate bounding box of all added content
        added_bbox = None
        for item in top_level_added:
            if item.bbox:
                if added_bbox is None:
                    added_bbox = item.bbox
                else:
                    added_bbox = added_bbox.merge(item.bbox)

        if not added_bbox:
            logger.info("No bounding boxes found for added items")
            return

        # Calculate bounding box of all existing (non-added) footprints
        existing_bbox = None

        def collect_existing_bbox(item: VirtualItem):
            nonlocal existing_bbox
            if not item.added and item.bbox and isinstance(item, VirtualFootprint):
                if existing_bbox is None:
                    existing_bbox = item.bbox
                else:
                    existing_bbox = existing_bbox.merge(item.bbox)
            # Don't recurse into added groups
            if not item.added and isinstance(item, VirtualGroup):
                # Sort children for deterministic traversal using natural sort
                sorted_children = sorted(
                    item.children,
                    key=lambda child: (natural_sort_key(child.name), child.id),
                )
                for child in sorted_children:
                    collect_existing_bbox(child)

        collect_existing_bbox(self.state.virtual_board.root)

        # Calculate offset to position new content
        if existing_bbox:
            # Position to the right of existing content
            margin = 10000000  # 10mm
            # Use center-to-center alignment for better positioning
            target_x = existing_bbox.right + margin + added_bbox.width // 2
            target_y = existing_bbox.center_y
            offset_x = target_x - added_bbox.center_x
            offset_y = target_y - added_bbox.center_y
        else:
            # Center on A4 sheet if no existing content
            sheet_width = 297000000  # 297mm
            sheet_height = 210000000  # 210mm
            # Center the added content on the sheet
            target_x = sheet_width // 2
            target_y = sheet_height // 2
            offset_x = target_x - added_bbox.center_x
            offset_y = target_y - added_bbox.center_y

        # Move all items in the sparse tree
        # Move all items by the calculated offset
        for item in top_level_added:
            item.move_by(offset_x, offset_y)

        logger.info(f"Positioned new content with offset ({offset_x}, {offset_y})")

    def run(self):
        """Run the hierarchical placement algorithm using the virtual DOM."""
        logger.info("Starting hierarchical component placement")

        # Process the entire tree starting from root using DFS
        # This returns a sparse tree containing only placed items
        sparse_tree = self._process_group_dfs(self.state.virtual_board.root)

        # Position all newly placed content relative to existing content
        self._position_relative_to_existing(sparse_tree)

        logger.info("Completed hierarchical component placement")


####################################################################################################
# Step 4. Clear orphaned net assignments
####################################################################################################


class ClearOrphanedNets(Step):
    """Clear net assignments for zones/vias referencing nets not in our netlist."""

    def __init__(
        self,
        state: SyncState,
        board: pcbnew.BOARD,
        netlist: JsonNetlistParser,
        dry_run: bool = False,
    ):
        self.state = state
        self.board = board
        self.netlist = netlist
        self.dry_run = dry_run

    def run(self):
        valid_nets = {net.name for net in self.netlist.nets}
        action = "Found" if self.dry_run else "Cleared"

        for zone in self.board.Zones():
            net_name = zone.GetNetname()
            if net_name and net_name not in valid_nets:
                if not self.dry_run:
                    zone.SetNetCode(0)
                logger.warning(f"{action} zone on unknown net '{net_name}'")
                self.state.add_diagnostic(
                    kind="layout.orphaned_zone",
                    severity="warning",
                    body=f"Zone on unknown net '{net_name}'",
                )

        for track in self.board.GetTracks():
            if isinstance(track, pcbnew.PCB_VIA):
                net_name = track.GetNetname()
                if net_name and net_name not in valid_nets:
                    if not self.dry_run:
                        track.SetNetCode(0)
                    logger.warning(f"{action} via on unknown net '{net_name}'")
                    self.state.add_diagnostic(
                        kind="layout.orphaned_via",
                        severity="warning",
                        body=f"Via on unknown net '{net_name}'",
                    )


####################################################################################################
# Step 5. Finalize board
####################################################################################################


class FinalizeBoard(Step):
    """Finalize the board by filling zones, saving a layout snapshot, and saving the board."""

    def __init__(
        self,
        state: SyncState,
        board: pcbnew.BOARD,
        snapshot_path: Optional[Path],
        diagnostics_path: Optional[Path] = None,
    ):
        self.state = state
        self.board = board
        self.snapshot_path = snapshot_path
        self.diagnostics_path = diagnostics_path

    def _get_footprint_data(self, fp: pcbnew.FOOTPRINT) -> dict:
        """Extract relevant data from a footprint."""
        # Return a sorted dictionary to ensure consistent ordering
        return {
            "footprint": fp.GetFPIDAsString(),
            "group": fp.GetParentGroup().GetName() if fp.GetParentGroup() else None,
            "layer": fp.GetLayerName(),
            "locked": fp.IsLocked(),
            "orientation": fp.GetOrientation().AsDegrees(),
            "position": {"x": fp.GetPosition().x, "y": fp.GetPosition().y},
            "reference": fp.GetReference(),
            "uuid": get_footprint_uuid(fp),
            # Getting cross-platform unicode normalization to work is a headache, so let's just
            # strip any non-ASCII characters.
            "value": "".join(c for c in str(fp.GetValue()) if ord(c) < 128),
            "dnp": fp.IsDNP(),
            "exclude_from_bom": (
                fp.IsExcludeFromBOM()
                if hasattr(fp, "IsExcludeFromBOM")
                else (
                    fp.GetFieldByName("exclude_from_bom").GetText() == "true"
                    if fp.GetFieldByName("exclude_from_bom")
                    else False
                )
            ),
            "exclude_from_pos_files": (
                fp.IsExcludeFromPosFiles()
                if hasattr(fp, "IsExcludeFromPosFiles")
                else (
                    fp.GetFieldByName("exclude_from_pos_files").GetText() == "true"
                    if fp.GetFieldByName("exclude_from_pos_files")
                    else False
                )
            ),
            "pads": [
                {
                    "name": pad.GetName(),
                    "position": {"x": pad.GetPosition().x, "y": pad.GetPosition().y},
                    "layer": pad.GetLayerName(),
                }
                for pad in fp.Pads()
            ],
            "graphical_items": [
                {
                    "type": item.GetClass(),
                    "layer": item.GetLayerName(),
                    "position": {
                        "x": item.GetPosition().x,
                        "y": item.GetPosition().y,
                    },
                    "start": (
                        {"x": item.GetStart().x, "y": item.GetStart().y}
                        if hasattr(item, "GetStart")
                        else None
                    ),
                    "end": (
                        {"x": item.GetEnd().x, "y": item.GetEnd().y}
                        if hasattr(item, "GetEnd")
                        else None
                    ),
                    "angle": (item.GetAngle() if hasattr(item, "GetAngle") else None),
                    "text": item.GetText() if hasattr(item, "GetText") else None,
                    "shape": item.GetShape() if hasattr(item, "GetShape") else None,
                    "width": item.GetWidth() if hasattr(item, "GetWidth") else None,
                }
                for item in fp.GraphicalItems()
            ],
        }

    def _get_group_data(self, group: pcbnew.PCB_GROUP) -> dict:
        """Extract relevant data from a group."""
        bbox = group.GetBoundingBox()
        # Return a sorted dictionary to ensure consistent ordering
        return {
            "bounding_box": {
                "bottom": bbox.GetBottom(),
                "left": bbox.GetLeft(),
                "right": bbox.GetRight(),
                "top": bbox.GetTop(),
            },
            "footprints": sorted(
                get_footprint_uuid(item)
                for item in get_group_items(group)
                if isinstance(item, pcbnew.FOOTPRINT)
            ),
            "drawings": sorted(
                [
                    {
                        "type": item.GetClass(),
                        "layer": item.GetLayerName(),
                        "position": {
                            "x": item.GetPosition().x,
                            "y": item.GetPosition().y,
                        },
                        "start": (
                            {"x": item.GetStart().x, "y": item.GetStart().y}
                            if hasattr(item, "GetStart")
                            else None
                        ),
                        "end": (
                            {"x": item.GetEnd().x, "y": item.GetEnd().y}
                            if hasattr(item, "GetEnd")
                            else None
                        ),
                        "angle": (
                            item.GetAngle() if hasattr(item, "GetAngle") else None
                        ),
                        "text": item.GetText() if hasattr(item, "GetText") else None,
                        "shape": item.GetShape() if hasattr(item, "GetShape") else None,
                        "width": item.GetWidth() if hasattr(item, "GetWidth") else None,
                    }
                    for item in get_group_items(group)
                    if isinstance(item, (pcbnew.PCB_SHAPE, pcbnew.PCB_TEXT))
                ],
                # Use a comprehensive sort key to ensure deterministic ordering even
                # when multiple drawings share the same position. This prevents the
                # output snapshot from changing across runs.
                key=lambda g: (
                    g["position"]["x"],
                    g["position"]["y"],
                    g.get("type") or "",
                    g.get("layer") or "",
                    # Start/end coordinates provide deterministic tie-breakers for shapes
                    (g.get("start", {}).get("x") if g.get("start") else None) or -1,
                    (g.get("start", {}).get("y") if g.get("start") else None) or -1,
                    (g.get("end", {}).get("x") if g.get("end") else None) or -1,
                    (g.get("end", {}).get("y") if g.get("end") else None) or -1,
                    # Numeric attributes
                    (g.get("angle") if g.get("angle") is not None else -1),
                    (g.get("shape") if g.get("shape") is not None else -1),
                    (g.get("width") if g.get("width") is not None else -1),
                    # Text last to avoid impacting geometry-first ordering
                    g.get("text") or "",
                ),
            ),
            "locked": group.IsLocked(),
            "name": group.GetName(),
        }

    def _get_zone_data(self, zone: pcbnew.ZONE) -> dict:
        """Extract relevant data from a zone."""
        # Return a sorted dictionary to ensure consistent ordering
        return {
            "name": zone.GetZoneName(),
            "net_name": zone.GetNetname(),
            "layer": zone.GetLayerName(),
            "locked": zone.IsLocked(),
            "filled": zone.IsFilled(),
            "hatch_style": zone.GetHatchStyle(),
            "min_thickness": zone.GetMinThickness(),
            "points": [
                {"x": point.x, "y": point.y}
                for point in zone.Outline().COutline(0).CPoints()
            ],
        }

    def _get_track_data(self, track: Any) -> dict:
        """Extract relevant data from a track."""
        # Return a sorted dictionary to ensure consistent ordering
        start = track.GetStart()
        end = track.GetEnd()
        return {
            "net_name": track.GetNetname(),
            "layer": track.GetLayerName(),
            "width": track.GetWidth(),
            "locked": track.IsLocked(),
            "start": {"x": start.x, "y": start.y},
            "end": {"x": end.x, "y": end.y},
        }

    def _get_via_data(self, via: Any) -> dict:
        """Extract relevant data from a via."""
        # Return a sorted dictionary to ensure consistent ordering
        pos = via.GetPosition()
        return {
            "net_name": via.GetNetname(),
            "position": {"x": pos.x, "y": pos.y},
            "drill": via.GetDrillValue(),
            # Use GetFrontWidth() to avoid KiCad 9 warning about layer-less GetWidth()
            "diameter": via.GetFrontWidth(),
            "locked": via.IsLocked(),
            "via_type": via.GetViaType(),
        }

    def _export_layout_snapshot(self):
        """Export a JSON snapshot of the board layout."""
        if self.snapshot_path is None:
            return

        # Separate tracks and vias
        tracks = []
        vias = []
        for item in self.board.GetTracks():
            item_class = item.GetClass()
            if "VIA" in item_class.upper():
                vias.append(item)
            else:
                tracks.append(item)

        # Sort footprints by UUID and groups by name for deterministic ordering
        snapshot = {
            "footprints": [
                self._get_footprint_data(fp)
                for fp in sorted(
                    self.board.GetFootprints(), key=lambda fp: get_footprint_uuid(fp)
                )
            ],
            "groups": [
                self._get_group_data(group)
                for group in sorted(
                    self.state.groups_registry.values(), key=lambda g: g.GetName() or ""
                )
            ],
            "zones": [self._get_zone_data(zone) for zone in self.board.Zones()],
            "tracks": [self._get_track_data(track) for track in tracks],
            "vias": [self._get_via_data(via) for via in vias],
        }

        with self.snapshot_path.open("w", encoding="utf-8") as f:
            json.dump(
                canonicalize_json(snapshot),
                f,
                indent=2,
                ensure_ascii=False,
            )

        logger.info(f"Saved layout snapshot to {self.snapshot_path}")

    def run(self):
        # Fill zones
        # zone_start = time.time()
        # filler = pcbnew.ZONE_FILLER(self.board)
        # filler.Fill(self.board.Zones())
        # logger.info(f"Zone filling took {time.time() - zone_start:.3f} seconds")

        # Export layout snapshot
        snapshot_start = time.time()
        self._export_layout_snapshot()
        logger.info(f"Snapshot export took {time.time() - snapshot_start:.3f} seconds")

        # Export diagnostics
        self._export_diagnostics()

        # Trigger KiCad's connectivity updates
        try:
            self.board.GetConnectivity().Build(self.board)
        except Exception:
            pass

        # Save board only once at the very end
        save_start = time.time()
        pcbnew.SaveBoard(self.board.GetFileName(), self.board)
        logger.info(f"Board saving took {time.time() - save_start:.3f} seconds")

    def _export_diagnostics(self):
        """Export collected diagnostics to JSON file."""
        if self.diagnostics_path:
            export_diagnostics(self.state.layout_diagnostics, self.diagnostics_path)


####################################################################################################
# Command-line interface
####################################################################################################


def main():
    parser = argparse.ArgumentParser(
        description="""Convert JSON netlist into a PCBNEW .kicad_pcb file."""
    )
    parser.add_argument(
        "--json-input",
        "-j",
        type=str,
        metavar="file",
        required=True,
        help="""Input file containing JSON netlist from diode-sch.""",
    )
    parser.add_argument(
        "--output",
        "-o",
        nargs="?",
        type=str,
        metavar="file",
        help="""Output file for storing KiCad board.""",
    )
    parser.add_argument(
        "--snapshot",
        "-s",
        type=str,
        metavar="file",
        help="""Output file for storing layout snapshot.""",
    )
    parser.add_argument(
        "--only-snapshot",
        action="store_true",
        help="""Generate a snapshot and exit.""",
    )
    parser.add_argument(
        "--board-config",
        type=str,
        metavar="file",
        help="""JSON file containing board setup configuration.""",
    )
    parser.add_argument(
        "--sync-board-config",
        type=bool,
        default=True,
        help="""Apply board config (default: true).""",
    )
    parser.add_argument(
        "--diagnostics",
        "-d",
        type=str,
        metavar="file",
        help="""Output file for storing sync diagnostics JSON.""",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="""Detect issues without modifying the board (read-only mode).""",
    )
    args = parser.parse_args()

    logger.setLevel(logging.DEBUG)

    handler = logging.StreamHandler()
    formatter = logging.Formatter("%(levelname)s: %(message)s")
    handler.setFormatter(formatter)
    logger.addHandler(handler)

    state = SyncState()

    # Check if output file exists, if not create a new board
    if not os.path.exists(args.output):
        logger.info(f"Creating new board file at {args.output}")
        board = pcbnew.NewBoard(args.output)
        pcbnew.SaveBoard(args.output, board)
    else:
        board = pcbnew.LoadBoard(args.output)

    # Initialize groups registry from board. This is the source of truth for
    # KiCad groups throughout the pipeline.
    state.groups_registry = build_groups_registry(board)

    # Parse JSON netlist
    logger.info(f"Parsing JSON netlist from {args.json_input}")
    netlist = JsonNetlistParser.parse_netlist(args.json_input)

    snapshot_path = Path(args.snapshot) if args.snapshot else None
    diagnostics_path = Path(args.diagnostics) if args.diagnostics else None

    if args.dry_run:
        # Dry-run mode: detect issues without modifying the board (read-only)
        # ImportNetlist with dry_run=True emits diagnostics instead of making changes
        steps = [
            ImportNetlist(state, board, args.output, netlist, dry_run=True),
            ClearOrphanedNets(state, board, netlist, dry_run=True),
        ]
        save_board = False
    elif args.only_snapshot:
        steps = [
            FinalizeBoard(state, board, snapshot_path, diagnostics_path),
        ]
        save_board = True
    else:
        steps = [
            SetupBoard(state, board, args.board_config, args.sync_board_config),
            ImportNetlist(state, board, args.output, netlist),
            SyncLayouts(state, board, netlist),
            PlaceComponents(state, board, netlist),
            ClearOrphanedNets(state, board, netlist),
            FinalizeBoard(state, board, snapshot_path, diagnostics_path),
        ]
        save_board = True

    for step in steps:
        logger.info("-" * 80)
        logger.info(f"Running step: {step.__class__.__name__}")
        logger.info("-" * 80)
        step.run_with_timing()

    # Export diagnostics in dry-run mode (FinalizeBoard handles this in normal mode)
    if args.dry_run and diagnostics_path:
        export_diagnostics(state.layout_diagnostics, diagnostics_path)

    if save_board:
        pcbnew.SaveBoard(args.output, board)

    # Explicitly delete the board to release resources (important for Windows)
    del board


###############################################################################
# Main entrypoint.
###############################################################################
if __name__ == "__main__":
    main()
