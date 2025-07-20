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
import pcbnew


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

            # Get value - follow the same precedence as Rust: mpn > Value > Val > type > "?"
            value = None
            if (
                "mpn" in instance["attributes"]
                and "String" in instance["attributes"]["mpn"]
            ):
                value = instance["attributes"]["mpn"]["String"]
            elif (
                "Value" in instance["attributes"]
                and "String" in instance["attributes"]["Value"]
            ):
                value = instance["attributes"]["Value"]["String"]
            elif (
                "Val" in instance["attributes"]
                and "String" in instance["attributes"]["Val"]
            ):
                value = instance["attributes"]["Val"]["String"]
            elif (
                "type" in instance["attributes"]
                and "String" in instance["attributes"]["type"]
            ):
                value = instance["attributes"]["type"]["String"]
            if not value:
                value = "?"

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
                    if isinstance(attr_value, dict) and "String" in attr_value:
                        prop = JsonNetlistParser.Property(
                            attr_name, attr_value["String"]
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


class VirtualFootprint(VirtualItem):
    """Represents a real footprint with 1:1 mapping to KiCad footprint."""

    def __init__(
        self,
        fp_id: str,
        name: str,
        kicad_footprint: Any,  # pcbnew.FOOTPRINT
        bbox: VirtualBoundingBox,
    ):
        super().__init__(VirtualItemType.EDA_ITEM, fp_id, name)
        self.kicad_footprint = kicad_footprint
        self._bbox = bbox
        self.attributes: Dict[str, Any] = {}

    @property
    def bbox(self) -> Optional[VirtualBoundingBox]:
        """Get the current bounding box from the KiCad footprint."""
        # Always return the current bbox from the actual footprint position
        if self.kicad_footprint and hasattr(self.kicad_footprint, "GetBoundingBox"):
            bb = get_kicad_bbox(self.kicad_footprint)
            self._bbox = bb
        return self._bbox

    def move_by(self, dx: int, dy: int) -> None:
        """Move the footprint by a relative offset."""
        # Update the actual KiCad object
        if self.kicad_footprint and hasattr(self.kicad_footprint, "SetPosition"):
            pos = self.kicad_footprint.GetPosition()
            self.kicad_footprint.SetPosition(pcbnew.VECTOR2I(pos.x + dx, pos.y + dy))

        # Update cached bbox
        if self._bbox:
            self._bbox = VirtualBoundingBox(
                self._bbox.x + dx,
                self._bbox.y + dy,
                self._bbox.width,
                self._bbox.height,
            )

        # Notify parent groups to update their bboxes
        parent = self.parent
        while parent and isinstance(parent, VirtualGroup):
            parent._cached_bbox = None  # Invalidate cache
            parent = parent.parent

    def move_to(self, x: int, y: int) -> None:
        """Move the footprint to a specific position."""
        if self.bbox:
            dx = x - self.bbox.x
            dy = y - self.bbox.y
            self.move_by(dx, dy)

    def get_position(self) -> Optional[Tuple[int, int]]:
        """Get the position of this footprint."""
        if self.kicad_footprint and hasattr(self.kicad_footprint, "GetPosition"):
            pos = self.kicad_footprint.GetPosition()
            return (pos.x, pos.y)
        elif self._bbox:
            return (self._bbox.x, self._bbox.y)
        return None

    def get_center(self) -> Optional[Tuple[int, int]]:
        """Get the center position of this footprint."""
        if self.bbox:
            return (self.bbox.center_x, self.bbox.center_y)
        return None

    def replace_with(self, source_footprint: "VirtualFootprint") -> None:
        """Copy position and properties from another footprint."""
        if source_footprint.kicad_footprint and self.kicad_footprint:
            self._copy_kicad_properties(
                source_footprint.kicad_footprint, self.kicad_footprint
            )
        # Update bbox after copying properties
        self._bbox = source_footprint._bbox

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

        # Invalidate our cached bbox and parent caches
        self._cached_bbox = None
        parent = self.parent
        while parent and isinstance(parent, VirtualGroup):
            parent._cached_bbox = None
            parent = parent.parent

    def move_to(self, x: int, y: int) -> None:
        """Move this group to a specific position."""
        if self.bbox:
            dx = x - self.bbox.x
            dy = y - self.bbox.y
            self.move_by(dx, dy)

    def get_center(self) -> Optional[Tuple[int, int]]:
        """Get the center position of this group."""
        if self.bbox:
            return (self.bbox.center_x, self.bbox.center_y)
        return None

    def get_position(self) -> Optional[Tuple[int, int]]:
        """Get the position (top-left) of this group."""
        if self.bbox:
            return (self.bbox.x, self.bbox.y)
        return None

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


def build_virtual_dom_from_board(board: pcbnew.BOARD) -> VirtualBoard:
    """Build a virtual DOM from a KiCad board based on footprint paths.

    Args:
        board: The KiCad board to build from

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
            # Groups don't have a direct KiCad object
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

    return vboard


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

    def __init__(self, state: SyncState, board: pcbnew.BOARD):
        self.state = state
        self.board = board

    def run(self):
        lset = pcbnew.LSET.AllLayersMask()
        lset.RemoveLayer(pcbnew.F_Fab)
        lset.RemoveLayer(pcbnew.B_Fab)
        self.board.SetVisibleLayers(lset)


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
    ):
        self.state = state
        self.board = board
        self.board_path = board_path
        self.netlist = netlist

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

    def _sync_footprints(self):
        """Remove footprints from the board that are not in the netlist, and add new ones that are missing from the board."""
        netlist_footprint_ids = set(
            part.sheetpath.tstamps for part in self.netlist.parts
        )

        board_footprint_ids = set(
            get_footprint_uuid(fp) for fp in self.board.GetFootprints()
        )

        for fp_id in board_footprint_ids - netlist_footprint_ids:
            # Delete the footprint from the board.
            fp = self.board.FindFootprintByPath(pcbnew.KIID_PATH(f"{fp_id}/{fp_id}"))
            if fp:
                logger.info(f"{fp_id} ({fp.GetReference()}): Removing from board")
                self.state.track_footprint_removed(fp)
                self.board.Delete(fp)

        def _configure_footprint(fp: pcbnew.FOOTPRINT, part: any):
            for field in fp.GetFields():
                if (
                    not field.IsValue()
                    and not field.IsReference()
                    and not field.IsDatasheet()
                ):
                    fp.RemoveField(field.GetName())

            fp.SetReference(part.ref)
            fp.SetValue(part.value)
            fp.SetField("Path", part.sheetpath.names.split(":")[-1])
            fp.SetFPIDAsString(part.footprint)
            fp.SetPath(
                pcbnew.KIID_PATH(f"{part.sheetpath.tstamps}/{part.sheetpath.tstamps}")
            )
            fp.SetDNP(any(x.name == "dnp" for x in part.properties))

            fp.GetFieldByName("Value").SetVisible(False)
            fp.GetFieldByName("Path").SetVisible(False)

            for prop in part.properties:
                # Skip value, Reference, and reference properties - these are handled separately
                if prop.name.lower() not in ["value", "reference"]:
                    fp.SetField(prop.name, prop.value)
                    fp.GetFieldByName(prop.name).SetVisible(False)

        for fp_id in netlist_footprint_ids - board_footprint_ids:
            # Create a new footprint from the netlist.
            part = next(
                part for part in self.netlist.parts if part.sheetpath.tstamps == fp_id
            )
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
            _configure_footprint(fp, part)

            self.board.Add(fp)
            self.state.track_footprint_added(fp)

        for fp_id in netlist_footprint_ids & board_footprint_ids:
            # Update metadata for footprints that are already on the board.
            fp = self.board.FindFootprintByPath(pcbnew.KIID_PATH(f"{fp_id}/{fp_id}"))
            self.state.track_footprint_updated(fp)
            part = next(
                part for part in self.netlist.parts if part.sheetpath.tstamps == fp_id
            )

            logger.info(f"{fp_id} ({part.ref}): Updating metadata")
            _configure_footprint(fp, part)

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

        # Get existing groups on the board
        existing_groups = {g.GetName(): g for g in self.board.Groups()}

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

            # Only create group if it has more than one child
            if direct_child_count > 1:
                groups_to_create[path] = True
                logger.debug(
                    f"Will create group {path} with {direct_child_count} children"
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

        # Remove empty groups
        for group_name, group in existing_groups.items():
            if group_name and len(get_group_items(group)) == 0:
                logger.info(f"Removing empty group: {group_name}")
                self.board.Remove(group)

    def _refresh_board(self):
        """Refresh the board to update the UI."""
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

        # Load footprint library map
        lib_start = time.time()
        self._load_footprint_lib_map()
        logger.debug(
            f"Footprint library map loading took {time.time() - lib_start:.3f} seconds"
        )

        # Sync footprints
        sync_start = time.time()
        self._sync_footprints()
        logger.info(
            f"Footprint synchronization took {time.time() - sync_start:.3f} seconds"
        )

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

    def _sync_group_layout(self, group: VirtualGroup, layout_file: Path):
        """Sync footprints in a group from a layout file."""
        # Load the layout file into a virtual board
        layout_board = pcbnew.LoadBoard(str(layout_file))
        layout_vboard = build_virtual_dom_from_board(layout_board)

        # Get all footprints in the target group (recursively)
        target_footprints = self._get_footprints_in_group(group)

        # Get all footprints from the layout
        source_footprints = self._get_all_footprints(layout_vboard.root)

        # Build maps for matching
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

        # Match footprints and sync
        matched = 0
        unmatched_target = []
        unmatched_source = []

        # Try to match each target footprint by path only
        for rel_path, target_fp in target_by_path.items():
            if rel_path in source_by_path:
                # Found a match - sync the footprint
                source_fp = source_by_path[rel_path]
                target_fp.replace_with(source_fp)
                matched += 1
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

                # Log results
        logger.info(f"  Synced {matched} footprints")
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

        # Mark the group as synced if we matched at least one footprint
        if matched > 0:
            group.synced = True
            logger.info(f"  Marked group {group.id} as synced")

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
        items_with_bbox.sort(key=lambda item: (-item.bbox.area, item.name, item.id))

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
                best_bbox = None

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
                            best_bbox = all_bbox

                if best_pt:
                    # Move to the best position found
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
        sorted_children = sorted(
            group.children, key=lambda child: (child.name, child.id)
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
            # Sort them for deterministic ordering
            top_level_added = sorted(
                sparse_tree.children, key=lambda item: (item.name, item.id)
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
                # Sort children for deterministic traversal
                sorted_children = sorted(
                    item.children, key=lambda child: (child.name, child.id)
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
        # move_by will recursively move children and update parent bboxes
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
# Step 4. Finalize board
####################################################################################################


class FinalizeBoard(Step):
    """Finalize the board by filling zones, saving a layout snapshot, and saving the board."""

    def __init__(self, state: SyncState, board: pcbnew.BOARD, snapshot_path: Path):
        self.state = state
        self.board = board
        self.snapshot_path = snapshot_path

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
            "pads": [
                {
                    "name": pad.GetName(),
                    "position": {"x": pad.GetPosition().x, "y": pad.GetPosition().y},
                    "layer": pad.GetLayerName(),
                }
                for pad in fp.Pads()
            ],
            "graphical_items": sorted(
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
                    for item in fp.GraphicalItems()
                ],
                key=lambda g: (g["position"]["x"], g["position"]["y"]),
            ),
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
                key=lambda g: (g["position"]["x"], g["position"]["y"]),
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

    def _export_layout_snapshot(self):
        """Export a JSON snapshot of the board layout."""
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
                    self.board.Groups(), key=lambda g: g.GetName() or ""
                )
            ],
            "zones": [
                self._get_zone_data(zone)
                for zone in sorted(
                    self.board.Zones(), key=lambda z: z.GetZoneName() or ""
                )
            ],
        }

        with self.snapshot_path.open("w", encoding="utf-8") as f:
            json.dump(
                snapshot,
                f,
                indent=2,
                sort_keys=True,  # Ensure all dictionaries are sorted by key
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

        # Save board
        save_start = time.time()
        pcbnew.SaveBoard(self.board.GetFileName(), self.board)
        logger.info(f"Board saving took {time.time() - save_start:.3f} seconds")


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
    args = parser.parse_args()

    # Respect RUST_LOG environment variable for log level
    rust_log = os.environ.get("RUST_LOG", "error").lower()
    log_level_map = {
        "trace": logging.DEBUG,  # Python doesn't have TRACE, map to DEBUG
        "debug": logging.DEBUG,
        "info": logging.INFO,
        "warn": logging.WARNING,
        "warning": logging.WARNING,
        "error": logging.ERROR,
        "off": logging.CRITICAL + 1,  # Effectively disable logging
    }
    log_level = log_level_map.get(rust_log, logging.INFO)

    logger.setLevel(log_level)

    handler = logging.StreamHandler()
    handler.setLevel(log_level)
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

    # Parse JSON netlist
    logger.info(f"Parsing JSON netlist from {args.json_input}")
    netlist = JsonNetlistParser.parse_netlist(args.json_input)

    if args.only_snapshot:
        steps = [
            FinalizeBoard(state, board, Path(args.snapshot) if args.snapshot else None),
        ]
    else:
        steps = [
            SetupBoard(state, board),
            ImportNetlist(state, board, args.output, netlist),
            SyncLayouts(state, board, netlist),
            PlaceComponents(state, board, netlist),
            FinalizeBoard(state, board, Path(args.snapshot) if args.snapshot else None),
        ]

    for step in steps:
        logger.info("-" * 80)
        logger.info(f"Running step: {step.__class__.__name__}")
        logger.info("-" * 80)
        step.run_with_timing()

    pcbnew.SaveBoard(args.output, board)


###############################################################################
# Main entrypoint.
###############################################################################
if __name__ == "__main__":
    main()
