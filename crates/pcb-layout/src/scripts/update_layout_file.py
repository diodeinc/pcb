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

1. ImportNetlist (via lens sync)
   • CRUD-sync footprints and nets from the JSON netlist file.
   • Apply layout fragments (tracks/zones/graphics) to groups with layout_path.
   • Run hierarchical HierPlace algorithm for newly added items.

2. FinalizeBoard
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
from collections import defaultdict
from typing import Optional, Set
from pathlib import Path
import json
import sys
import uuid
from typing import List, Dict
from typing import Any

# Global logger.
logger = logging.getLogger("pcb")


def export_diagnostics(diagnostics: List[Dict[str, Any]], path: Path) -> None:
    """Export diagnostics to JSON file."""
    output = {"diagnostics": diagnostics}
    with open(path, "w", encoding="utf-8") as f:
        json.dump(output, f, indent=2)
    count = len(diagnostics)
    if count > 0:
        logger.info(f"Saved {count} diagnostic(s) to {path}")


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
import pcbnew  # noqa: E402

# Suppress wxWidgets debug messages (e.g., "Adding duplicate image handler")
# These are noisy and non-deterministic, interfering with test snapshots.
try:
    import wx

    wx.Log.EnableLogging(False)
except Exception:
    pass  # wx may not be available in all environments


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
            self.layout_path = (
                layout_path  # Path to layout directory (not a specific file)
            )

    class SheetPath:
        """Represents the hierarchical sheet path."""

        def __init__(self, names, tstamps):
            self.names = names
            self.tstamps = tstamps

    class Net:
        """Represents an electrical net."""

        def __init__(self, name, nodes, kind="Net"):
            self.name = name
            self.nodes = nodes
            self.kind = (
                kind  # Net type kind (e.g., "Net", "Power", "Ground", "NotConnected")
            )

    class Property:
        """Represents a component property."""

        def __init__(self, name, value):
            self.name = name
            self.value = value

    def __init__(self):
        self.parts = []
        self.nets = []
        self.modules = {}  # Dict of module path -> Module instance
        self.package_roots = {}  # Dict of package URL -> absolute filesystem path

    @staticmethod
    def parse_netlist(json_path):
        """Parse a JSON netlist file and return a netlist object compatible with kinparse."""
        with open(json_path, "r") as f:
            data = json.load(f)

        parser = JsonNetlistParser()
        parser.package_roots = data.get("package_roots", {})

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

            logger.debug(f"Found module {module_path} with layout_path: {layout_path}")

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
                        # Preserve the logical port identity (component pin) separately
                        # from the physical pad number. A single logical pin can map to
                        # multiple pads (e.g. SW pins, thermal pads, stitched pads).
                        #
                        # The node tuple is (ref_des, pad_num, pin_name). The third
                        # field is ignored for net connectivity, but is used for
                        # pin-vs-pad aware behavior (e.g. NotConnected handling).
                        pin_name = port_parts[-1] if port_parts else ""
                        nodes.append((ref_des, pad_num, pin_name))

            if nodes:
                # Extract net kind (defaults to "Net" if not specified)
                net_kind = net_data.get("kind", "Net")
                net = JsonNetlistParser.Net(net_name, nodes, net_kind)
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


class SyncState:
    """Shared state for the sync process."""

    def __init__(self):
        # Net rename mapping and old net codes for orphan cleanup
        self.net_rename_mapping: Optional[Dict[str, Dict[str, int]]] = None
        self.old_net_codes: Dict[str, int] = {}

        # Diagnostics collected during sync (e.g., FPID mismatches)
        self.layout_diagnostics: List[Dict[str, Any]] = []

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
        # Note: Keys are wxString objects, must convert to str for comparison
        netclasses_map = netSettings.GetNetclasses()
        for name, nc in netclasses_map.items():
            existing_netclasses[str(name)] = nc

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
                f"Netclass names differ - desired: {sorted(desired_names)}, existing: {sorted(existing_names)}"
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
# Net Rename Mapping System
#
# This system tracks net name changes by comparing old and new pad assignments for existing
# footprints, enabling analysis and eventual automatic updating of zones.
####################################################################################################


class NetRenameMapper:
    def __init__(self):
        self.old_pad_nets: Dict[str, Dict[str, str]] = {}
        self.new_pad_nets: Dict[str, Dict[str, str]] = {}

    def capture_current_state(self, board: pcbnew.BOARD, existing_footprints: Set[str]):
        for fp in board.GetFootprints():
            fp_uuid = get_footprint_uuid(fp)
            if fp_uuid not in existing_footprints:
                continue

            pad_nets = {}
            for pad in fp.Pads():
                net = pad.GetNet()
                if net and net.GetNetname():
                    pad_nets[pad.GetPadName()] = net.GetNetname()

            if pad_nets:
                self.old_pad_nets[fp_uuid] = pad_nets

    def capture_new_state(self, board: pcbnew.BOARD):
        for fp in board.GetFootprints():
            fp_uuid = get_footprint_uuid(fp)
            if fp_uuid not in self.old_pad_nets:
                continue

            pad_nets = {}
            for pad in fp.Pads():
                net = pad.GetNet()
                if net and net.GetNetname():
                    pad_nets[pad.GetPadName()] = net.GetNetname()

            self.new_pad_nets[fp_uuid] = pad_nets

    def build_net_rename_mapping(self) -> Dict[str, Dict[str, int]]:
        net_mapping = defaultdict(lambda: defaultdict(int))

        for fp_uuid in self.old_pad_nets:
            if fp_uuid not in self.new_pad_nets:
                continue

            old_pads = self.old_pad_nets[fp_uuid]
            new_pads = self.new_pad_nets[fp_uuid]

            for pad_name in old_pads:
                old_net = old_pads[pad_name]
                new_net = new_pads.get(pad_name)
                if new_net:
                    net_mapping[old_net][new_net] += 1

        return {
            old_net: dict(new_net_counts)
            for old_net, new_net_counts in net_mapping.items()
        }


####################################################################################################
# Step 1. Import Netlist
#
# Imports the netlist using the lens-based sync architecture for provably correct operations.
# The lens module is extracted to a temp directory and added to PYTHONPATH by Rust.
####################################################################################################

# Import the lens module (extracted to temp dir by Rust and added to PYTHONPATH)
from lens import run_lens_sync  # noqa: E402


class ImportNetlist(Step):
    """
    Import the netlist using lens-based synchronization.

    This is a thin wrapper around run_lens_sync() that handles:
    - Environment setup (KiCad paths, footprint library map)
    - Transferring diagnostics to SyncState
    """

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
        self.board_path = Path(board_path)
        self.netlist = netlist
        self.dry_run = dry_run
        self.package_roots = netlist.package_roots
        self.footprint_lib_map: Dict[str, str] = {}

    def _setup_env(self):
        """Set up environment variables for footprint resolution."""
        if "KIPRJMOD" not in os.environ.keys():
            os.environ["KIPRJMOD"] = str(self.board_path.parent)

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
            str(self.board_path.parent), "fp-lib-table"
        )

        if os.path.exists(local_fp_lib_table_path):
            _load_fp_lib_table(local_fp_lib_table_path)

    def run(self):
        """Run the lens-based import process."""
        self._setup_env()
        self._load_footprint_lib_map()

        logger.info("Running lens-based netlist sync")

        result = run_lens_sync(
            netlist=self.netlist,
            kicad_board=self.board,
            pcbnew=pcbnew,
            board_path=self.board_path,
            footprint_lib_map=self.footprint_lib_map,
            dry_run=self.dry_run,
            package_roots=self.package_roots,
        )

        # Transfer diagnostics
        self.state.layout_diagnostics.extend(result.diagnostics)

        if not self.dry_run:
            # Refresh board
            self.board.BuildListOfNets()
            pcbnew.Refresh()

        # Log summary
        changeset = result.changeset
        added_count = len(changeset.added_footprints)
        removed_count = len(changeset.removed_footprints)
        logger.info(f"Lens sync complete: +{added_count} -{removed_count} footprints")


####################################################################################################
# Step 2. Finalize board
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
            "diameter": via.GetWidth(pcbnew.F_Cu),
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
                    [g for g in self.board.Groups() if g.GetName()],
                    key=lambda g: g.GetName() or "",
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

        # Trigger KiCad's connectivity updates and fix orphaned items
        try:
            self.board.GetConnectivity().Build(self.board)
        except Exception:
            pass
        self._fix_remaining_orphaned_items()

        # Save board only once at the very end
        save_start = time.time()
        pcbnew.SaveBoard(self.board.GetFileName(), self.board)
        logger.info(f"Board saving took {time.time() - save_start:.3f} seconds")

    def _export_diagnostics(self):
        """Export collected diagnostics to JSON file."""
        if self.diagnostics_path:
            export_diagnostics(self.state.layout_diagnostics, self.diagnostics_path)

    def _fix_remaining_orphaned_items(self):
        if (
            not hasattr(self.state, "net_rename_mapping")
            or not self.state.net_rename_mapping
        ):
            return

        # Get current nets
        new_nets = {}
        for fp in self.board.GetFootprints():
            for pad in fp.Pads():
                net = pad.GetNet()
                if net and net.GetNetname() and net.GetNetname() not in new_nets:
                    new_nets[net.GetNetname()] = net

        zones_updated = vias_updated = polygons_updated = 0

        for old_net, new_net_counts in self.state.net_rename_mapping.items():
            if not new_net_counts:
                continue

            new_net_name, count = max(new_net_counts.items(), key=lambda x: x[1])
            confidence = count / sum(new_net_counts.values())

            if confidence < 0.8 or old_net == new_net_name:
                continue

            old_net_code = getattr(self.state, "old_net_codes", {}).get(old_net)
            new_pcb_net = new_nets.get(new_net_name)

            if not old_net_code or not new_pcb_net:
                continue

            zones_count = vias_count = polygons_count = 0

            for zone in self.board.Zones():
                if zone.GetNetCode() == old_net_code:
                    zone.SetNet(new_pcb_net)
                    zones_count += 1

            for track in self.board.GetTracks():
                if (
                    isinstance(track, pcbnew.PCB_VIA)
                    and track.GetNetCode() == old_net_code
                ):
                    track.SetNet(new_pcb_net)
                    vias_count += 1

            for drawing in self.board.GetDrawings():
                if hasattr(drawing, "GetNetCode") and hasattr(drawing, "SetNet"):
                    if drawing.GetNetCode() == old_net_code:
                        drawing.SetNet(new_pcb_net)
                        polygons_count += 1

            zones_updated += zones_count
            vias_updated += vias_count
            polygons_updated += polygons_count

            if zones_count > 0:
                print(
                    f"ZONES: {zones_count} orphaned items fixed '{old_net}' -> '{new_net_name}' ({confidence:.1%} confidence)"
                )
            if vias_count > 0:
                print(
                    f"VIAS: {vias_count} orphaned items fixed '{old_net}' -> '{new_net_name}' ({confidence:.1%} confidence)"
                )
            if polygons_count > 0:
                print(
                    f"POLYGONS: {polygons_count} orphaned items fixed '{old_net}' -> '{new_net_name}' ({confidence:.1%} confidence)"
                )

        if zones_updated + vias_updated + polygons_updated > 0:
            summary = []
            if zones_updated > 0:
                summary.append(f"{zones_updated} zones")
            if vias_updated > 0:
                summary.append(f"{vias_updated} vias")
            if polygons_updated > 0:
                summary.append(f"{polygons_updated} polygons")
            print(f"FINAL CLEANUP: {', '.join(summary)} orphaned items fixed")


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
