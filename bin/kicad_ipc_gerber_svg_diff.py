#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pillow>=10", "numpy>=1.26", "scipy>=1.11"]
# ///
"""Validate the IPC-2581 -> Gerber/drill conversion against KiCad as an oracle.

For a .kicad_pcb file this script:

1. exports Gerbers and Excellon drills directly from KiCad (the oracle),
2. exports IPC-2581 from KiCad and converts it to Gerbers/XNC with pcbc,
3. renders each Gerber pair to SVG with pcbc, rasterizes both into a shared
   viewport, and fails on significant raster XOR area, and
4. parses both drill file sets and fails on unmatched holes or slots.

By default every layer present in the pcbc export is compared (copper,
mask, paste, silkscreen, edge cuts) plus the drill files.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import zipfile
from collections.abc import Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import NoReturn
from xml.etree import ElementTree as ET

import numpy as np
from PIL import Image, ImageDraw
from scipy import ndimage

Image.MAX_IMAGE_PIXELS = None  # our own renders; sizes are capped below

REPO_ROOT = Path(__file__).resolve().parents[1]
SVG_NAMESPACE = "http://www.w3.org/2000/svg"
DEFAULT_PX_PER_MM = 100
MAX_RASTER_PIXELS = 150_000_000
DEFAULT_TOTAL_TOLERANCE_MM2 = 5.0
DEFAULT_COMPONENT_TOLERANCE_MM2 = 0.5
DRILL_POSITION_TOLERANCE_MM = 0.01
DRILL_DIAMETER_TOLERANCE_MM = 0.01

# pcbc export filename -> KiCad layer name, for the fixed-name layers.
KICAD_LAYER_BY_GERBER = {
    "F_Cu.gtl": "F.Cu",
    "B_Cu.gbl": "B.Cu",
    "F_Mask.gts": "F.Mask",
    "B_Mask.gbs": "B.Mask",
    "F_Paste.gtp": "F.Paste",
    "B_Paste.gbp": "B.Paste",
    "F_SilkS.gto": "F.SilkS",
    "B_SilkS.gbo": "B.SilkS",
    "Edge_Cuts.gm1": "Edge.Cuts",
}
INNER_COPPER_RE = re.compile(r"In(\d+)_Cu\.gbr$")


def main() -> int:
    args = parse_args()
    layout = args.layout.resolve()
    if not layout.is_file() or layout.suffix != ".kicad_pcb":
        fail(f"expected a .kicad_pcb file, got {layout}")

    kicad_cli = resolve_command(args.kicad_cli, "kicad-cli")
    kicad_python = (
        resolve_kicad_python(args.kicad_python, kicad_cli)
        if args.refill_zones
        else None
    )
    rsvg_convert = resolve_command(args.rsvg_convert, "rsvg-convert")

    out_dir = (
        args.output_dir
        or Path.cwd() / "build" / "kicad-ipc-gerber-svg-diff" / layout.stem
    ).resolve()
    if out_dir.exists() and args.clean:
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    prepared_layout = prepare_layout_for_exports(
        layout, out_dir / "prepared-layout.kicad_pcb", kicad_python
    )

    # pcbc pipeline: KiCad IPC-2581 -> Gerber/XNC package.
    ipc_xml = out_dir / "layout.ipc2581.xml"
    run([kicad_cli, "pcb", "export", "ipc2581", "--output", str(ipc_xml), str(prepared_layout)])
    gerber_zip = out_dir / "ipc-gerbers.zip"
    run_pcbc(
        args,
        [
            "ipc2581",
            "gerber",
            "--layout-target",
            args.layout_target,
            "--output",
            str(gerber_zip),
            str(ipc_xml),
        ],
    )
    ipc_gerber_dir = out_dir / "ipc-gerbers"
    unzip_to(gerber_zip, ipc_gerber_dir)

    comparisons = select_layers(ipc_gerber_dir, args.layers)
    if not comparisons:
        fail(f"no comparable Gerber layers found in {ipc_gerber_dir}")

    # KiCad oracle exports, one invocation for all layers.
    kicad_gerber_dir = out_dir / "kicad-gerbers"
    run_kicad_gerbers(
        kicad_cli,
        prepared_layout,
        [kicad_layer for kicad_layer, _ in comparisons],
        kicad_gerber_dir,
    )

    results: list[LayerResult] = []
    for kicad_layer, gerber_name in comparisons:
        results.append(
            compare_layer(
                args,
                rsvg_convert,
                out_dir,
                kicad_layer,
                kicad_gerber_file(kicad_gerber_dir, kicad_layer),
                ipc_gerber_dir / gerber_name,
            )
        )

    drill_result = None
    if args.drills:
        drill_result = compare_drills(kicad_cli, prepared_layout, out_dir, ipc_gerber_dir)

    print()
    print(f"Artifacts: {out_dir}")
    failed = print_summary(results, drill_result, args)
    if failed:
        print("FAIL: KiCad oracle comparison exceeds tolerance", file=sys.stderr)
        return 1
    print("PASS: all compared layers and drills match KiCad within tolerance")
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Compare pcbc's IPC-2581 -> Gerber/drill conversion against "
            "KiCad's directly exported artifacts for a .kicad_pcb file."
        )
    )
    parser.add_argument("layout", type=Path, help="Path to a .kicad_pcb file")
    parser.add_argument(
        "--layers",
        default="all",
        help=(
            "Comma-separated KiCad layer names to compare (e.g. F.Cu,B.Mask), "
            "or 'all' for every layer present in the pcbc export"
        ),
    )
    parser.add_argument(
        "--no-drills",
        dest="drills",
        action="store_false",
        help="Skip the Excellon/XNC drill comparison",
    )
    parser.add_argument(
        "--layout-target",
        default="board",
        choices=["board", "board-array"],
        help="IPC Gerber export target",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        help="Directory for generated IPC, Gerber, SVG, PNG, and diff artifacts",
    )
    parser.add_argument(
        "--px-per-mm",
        type=int,
        default=DEFAULT_PX_PER_MM,
        help=(
            "Rasterization resolution; automatically reduced when a layer "
            f"would exceed {MAX_RASTER_PIXELS:,} pixels"
        ),
    )
    parser.add_argument(
        "--max-total-diff-mm2",
        type=float,
        default=DEFAULT_TOTAL_TOLERANCE_MM2,
        help="Fail when a layer's total XOR area exceeds this value",
    )
    parser.add_argument(
        "--max-component-diff-mm2",
        type=float,
        default=DEFAULT_COMPONENT_TOLERANCE_MM2,
        help="Fail when a layer's largest connected XOR component exceeds this value",
    )
    parser.add_argument(
        "--alpha-threshold",
        type=int,
        default=8,
        help="Alpha value above which a raster pixel counts as painted",
    )
    parser.add_argument(
        "--kicad-cli",
        default=os.environ.get("KICAD_CLI"),
        help="Path to kicad-cli; defaults to KICAD_CLI or PATH lookup",
    )
    parser.add_argument(
        "--kicad-python",
        default=os.environ.get("KICAD_PYTHON"),
        help=(
            "Path to a Python interpreter with pcbnew; defaults to KICAD_PYTHON "
            "or the Python bundled with the KiCad app"
        ),
    )
    parser.add_argument(
        "--no-refill-zones",
        dest="refill_zones",
        action="store_false",
        help="Export from a copied board without refilling zones first",
    )
    parser.add_argument(
        "--rsvg-convert",
        default=os.environ.get("RSVG_CONVERT"),
        help="Path to rsvg-convert; defaults to RSVG_CONVERT or PATH lookup",
    )
    parser.add_argument(
        "--pcbc-bin",
        type=Path,
        help="Use an existing pcbc binary instead of cargo run -p pcbc --",
    )
    parser.add_argument(
        "--release",
        action="store_true",
        help="Use cargo run --release when --pcbc-bin is not set",
    )
    parser.add_argument(
        "--keep-output",
        dest="clean",
        action="store_false",
        help="Do not clear the output directory before running",
    )
    parser.set_defaults(clean=True, refill_zones=True, drills=True)
    return parser.parse_args()


# --- Layer selection -------------------------------------------------------


def select_layers(ipc_gerber_dir: Path, layers_arg: str) -> list[tuple[str, str]]:
    """Return `(kicad_layer, pcbc_gerber_name)` pairs to compare."""
    available: dict[str, str] = {}
    for path in sorted(ipc_gerber_dir.iterdir()):
        kicad_layer = KICAD_LAYER_BY_GERBER.get(path.name)
        if kicad_layer is None:
            inner = INNER_COPPER_RE.fullmatch(path.name)
            if inner is None:
                continue
            # pcbc numbers inner copper by absolute stack position (top = 1);
            # KiCad numbers inner layers from 1.
            kicad_layer = f"In{int(inner.group(1)) - 1}.Cu"
        available[kicad_layer] = path.name

    if layers_arg == "all":
        return sorted(available.items(), key=lambda item: layer_sort_key(item[0]))

    selected = []
    for name in layers_arg.split(","):
        name = name.strip()
        if not name:
            continue
        if name not in available:
            fail(
                f"layer {name!r} has no matching file in {ipc_gerber_dir}; "
                f"available: {', '.join(sorted(available))}"
            )
        selected.append((name, available[name]))
    return selected


def layer_sort_key(kicad_layer: str) -> tuple[int, int, str]:
    order = ["F.Cu", "In", "B.Cu", "F.Mask", "B.Mask", "F.Paste", "B.Paste",
             "F.SilkS", "B.SilkS", "Edge.Cuts"]
    inner = re.fullmatch(r"In(\d+)\.Cu", kicad_layer)
    if inner:
        return (order.index("In"), int(inner.group(1)), kicad_layer)
    return (order.index(kicad_layer) if kicad_layer in order else len(order), 0, kicad_layer)


# --- Per-layer raster comparison -------------------------------------------


@dataclass
class LayerResult:
    layer: str
    px_per_mm: int
    kicad_area_mm2: float
    ipc_area_mm2: float
    diff_mm2: float
    largest_component_mm2: float
    component_count: int
    panel: Path

    def failed(self, args: argparse.Namespace) -> bool:
        return (
            self.diff_mm2 > args.max_total_diff_mm2
            or self.largest_component_mm2 > args.max_component_diff_mm2
        )


def compare_layer(
    args: argparse.Namespace,
    rsvg_convert: str,
    out_dir: Path,
    kicad_layer: str,
    kicad_gerber: Path,
    ipc_gerber: Path,
) -> LayerResult:
    safe = kicad_layer.replace(".", "_")
    kicad_svg = out_dir / f"kicad-gerber-{safe}.svg"
    ipc_svg = out_dir / f"ipc-gerber-{safe}.svg"
    run_pcbc(args, ["gerber", "render", "--output", str(kicad_svg), str(kicad_gerber)])
    run_pcbc(args, ["gerber", "render", "--output", str(ipc_svg), str(ipc_gerber)])

    width_mm, height_mm = unify_svg_viewports(kicad_svg, ipc_svg)
    px_per_mm = effective_px_per_mm(args.px_per_mm, width_mm, height_mm)

    kicad_png = out_dir / f"kicad-{safe}.png"
    ipc_png = out_dir / f"ipc-gerber-{safe}.png"
    rasterize_svg(rsvg_convert, kicad_svg, kicad_png, px_per_mm)
    rasterize_svg(rsvg_convert, ipc_svg, ipc_png, px_per_mm)

    reference = alpha_mask(kicad_png, args.alpha_threshold)
    candidate = alpha_mask(ipc_png, args.alpha_threshold)
    if reference.shape != candidate.shape:
        # Shared viewBox + identical DPI should always agree; tolerate a
        # single-pixel rounding edge by cropping to the common extent.
        rows = min(reference.shape[0], candidate.shape[0])
        cols = min(reference.shape[1], candidate.shape[1])
        reference = reference[:rows, :cols]
        candidate = candidate[:rows, :cols]

    xor = reference ^ candidate
    labels, component_count = ndimage.label(xor)
    largest_px = 0
    if component_count:
        sizes = ndimage.sum_labels(xor, labels, index=range(1, component_count + 1))
        largest_px = int(sizes.max())

    write_diff_panel(
        out_dir / f"kicad-vs-ipc-gerber-{safe}.panel.png",
        kicad_layer,
        reference,
        candidate,
        px_per_mm,
    )

    px_per_mm2 = px_per_mm * px_per_mm
    return LayerResult(
        layer=kicad_layer,
        px_per_mm=px_per_mm,
        kicad_area_mm2=int(reference.sum()) / px_per_mm2,
        ipc_area_mm2=int(candidate.sum()) / px_per_mm2,
        diff_mm2=int(xor.sum()) / px_per_mm2,
        largest_component_mm2=largest_px / px_per_mm2,
        component_count=int(component_count),
        panel=out_dir / f"kicad-vs-ipc-gerber-{safe}.panel.png",
    )


def alpha_mask(png: Path, threshold: int) -> np.ndarray:
    image = np.asarray(Image.open(png).convert("RGBA"))
    return image[:, :, 3] > threshold


def write_diff_panel(
    output: Path,
    layer: str,
    reference: np.ndarray,
    candidate: np.ndarray,
    px_per_mm: int,
) -> None:
    height, width = reference.shape
    rgb = np.full((height, width, 3), 255, dtype=np.uint8)
    rgb[reference & candidate] = (18, 18, 18)
    rgb[reference & ~candidate] = (220, 38, 38)
    rgb[~reference & candidate] = (37, 99, 235)
    diff = Image.fromarray(rgb)

    max_width = 1600
    scale = min(1.0, max_width / diff.size[0])
    image_width = max(1, int(diff.size[0] * scale))
    image_height = max(1, int(diff.size[1] * scale))
    header_height = 64
    legend_height = 48
    panel = Image.new(
        "RGB", (image_width, header_height + image_height + legend_height), "white"
    )
    draw = ImageDraw.Draw(panel)
    draw.rectangle([0, 0, image_width, header_height], fill=(245, 245, 245))
    diff_mm2 = int((reference ^ candidate).sum()) / (px_per_mm * px_per_mm)
    draw.text((18, 12), f"KiCad vs IPC->Gerber: {layer}", fill=(20, 20, 20))
    draw.text((18, 36), f"XOR {diff_mm2:.4f} mm^2 at {px_per_mm} px/mm", fill=(40, 40, 40))
    panel.paste(
        diff.resize((image_width, image_height), Image.Resampling.LANCZOS),
        (0, header_height),
    )
    legend_y = header_height + image_height
    legend = [
        ((18, 18, 18), "common"),
        ((220, 38, 38), "KiCad only (missing)"),
        ((37, 99, 235), "candidate only (extra)"),
    ]
    x = 18
    for color, label in legend:
        draw.rectangle([x, legend_y + 14, x + 24, legend_y + 38], fill=color)
        draw.text((x + 34, legend_y + 14), label, fill=(30, 30, 30))
        x += 300
    panel.save(output)


# --- SVG plumbing -----------------------------------------------------------


def unify_svg_viewports(svg_a: Path, svg_b: Path) -> tuple[float, float]:
    """Rewrite both SVGs to share the union viewBox; returns its size in mm."""
    box_a = read_viewbox(svg_a)
    box_b = read_viewbox(svg_b)
    min_x = min(box_a[0], box_b[0])
    min_y = min(box_a[1], box_b[1])
    max_x = max(box_a[0] + box_a[2], box_b[0] + box_b[2])
    max_y = max(box_a[1] + box_a[3], box_b[1] + box_b[3])
    width = max_x - min_x
    height = max_y - min_y
    if width <= 0 or height <= 0:
        fail(f"degenerate union viewBox for {svg_a} and {svg_b}")
    for svg in (svg_a, svg_b):
        set_viewbox(svg, (min_x, min_y, width, height))
    return width, height


def read_viewbox(svg: Path) -> tuple[float, float, float, float]:
    root = parse_svg(svg)
    viewbox = root.attrib.get("viewBox")
    if viewbox is None:
        fail(f"SVG has no viewBox: {svg}")
    values = [float(value) for value in viewbox.replace(",", " ").split()]
    if len(values) != 4 or values[2] <= 0 or values[3] <= 0:
        fail(f"invalid SVG viewBox in {svg}: {viewbox!r}")
    return values[0], values[1], values[2], values[3]


def set_viewbox(svg: Path, box: tuple[float, float, float, float]) -> None:
    root = parse_svg(svg)
    root.attrib["viewBox"] = f"{box[0]} {box[1]} {box[2]} {box[3]}"
    root.attrib["width"] = f"{box[2]}mm"
    root.attrib["height"] = f"{box[3]}mm"
    ET.register_namespace("", SVG_NAMESPACE)
    svg.write_text(ET.tostring(root, encoding="unicode") + "\n")


def parse_svg(svg: Path) -> ET.Element:
    try:
        root = ET.fromstring(svg.read_text())
    except ET.ParseError as error:
        fail(f"invalid SVG XML in {svg}: {error}")
    if root.tag.rsplit("}", 1)[-1] != "svg":
        fail(f"expected SVG root in {svg}, got {root.tag!r}")
    return root


def effective_px_per_mm(requested: int, width_mm: float, height_mm: float) -> int:
    px_per_mm = requested
    while px_per_mm > 1 and width_mm * height_mm * px_per_mm * px_per_mm > MAX_RASTER_PIXELS:
        px_per_mm = int(px_per_mm * 0.8)
    if px_per_mm != requested:
        print(
            f"note: reducing rasterization to {px_per_mm} px/mm for a "
            f"{width_mm:.0f}x{height_mm:.0f} mm board"
        )
    return max(px_per_mm, 1)


def rasterize_svg(
    rsvg_convert: str, input_svg: Path, output_png: Path, px_per_mm: int
) -> None:
    dpi = px_per_mm * 25.4
    run(
        [
            rsvg_convert,
            "--dpi-x",
            f"{dpi}",
            "--dpi-y",
            f"{dpi}",
            str(input_svg),
            "--output",
            str(output_png),
        ]
    )


# --- Drill comparison -------------------------------------------------------


@dataclass
class DrillFile:
    holes: list[tuple[float, float, float]] = field(default_factory=list)
    slots: list[tuple[float, float, float, float, float]] = field(default_factory=list)


@dataclass
class DrillResult:
    kicad_holes: int
    ipc_holes: int
    kicad_slots: int
    ipc_slots: int
    missing: list[str]
    extra: list[str]

    def failed(self) -> bool:
        return bool(self.missing or self.extra)


def compare_drills(
    kicad_cli: str, layout: Path, out_dir: Path, ipc_gerber_dir: Path
) -> DrillResult:
    kicad_drill_dir = out_dir / "kicad-drills"
    if kicad_drill_dir.exists():
        shutil.rmtree(kicad_drill_dir)
    kicad_drill_dir.mkdir(parents=True)
    run(
        [
            kicad_cli,
            "pcb",
            "export",
            "drill",
            "--format",
            "excellon",
            "--excellon-units",
            "mm",
            "--excellon-zeros-format",
            "decimal",
            "--excellon-oval-format",
            "route",
            "--excellon-separate-th",
            "--output",
            str(kicad_drill_dir) + os.sep,
            str(layout),
        ]
    )

    kicad = DrillFile()
    for path in sorted(kicad_drill_dir.glob("*.drl")):
        parse_excellon(path, kicad)
    ipc = DrillFile()
    for path in sorted(ipc_gerber_dir.glob("*.drl")):
        parse_excellon(path, ipc)

    missing: list[str] = []
    extra: list[str] = []
    match_entries(kicad.holes, ipc.holes, format_hole, missing, extra)
    match_entries(kicad.slots, ipc.slots, format_slot, missing, extra)
    return DrillResult(
        kicad_holes=len(kicad.holes),
        ipc_holes=len(ipc.holes),
        kicad_slots=len(kicad.slots),
        ipc_slots=len(ipc.slots),
        missing=missing,
        extra=extra,
    )


COORD_RE = re.compile(r"([XY])(-?\d*\.?\d+)")


def parse_excellon(path: Path, out: DrillFile) -> None:
    """Parse the decimal-format Excellon/XNC subset KiCad and pcbc emit."""
    tools: dict[str, float] = {}
    current: float | None = None
    in_header = True
    route_start: tuple[float, float] | None = None
    route_points: list[tuple[float, float]] = []
    plunged = False
    scale = 1.0

    for raw_line in path.read_text().splitlines():
        line = raw_line.strip()
        if not line or line.startswith(";"):
            continue
        if line == "M48":
            in_header = True
            continue
        if line == "%" or line == "M95":
            in_header = False
            continue
        if line in ("METRIC", "M71"):
            scale = 1.0
            continue
        if line in ("INCH", "M72"):
            scale = 25.4
            continue
        tool_def = re.fullmatch(r"T(\d+)C([\d.]+)", line)
        if tool_def and in_header:
            tools[tool_def.group(1).lstrip("0") or "0"] = float(tool_def.group(2)) * scale
            continue
        tool_select = re.fullmatch(r"T(\d+)", line)
        if tool_select and not in_header:
            number = tool_select.group(1).lstrip("0") or "0"
            current = tools.get(number)
            continue
        if line == "M15":
            plunged = True
            route_points = []
            continue
        if line in ("M16", "M17"):
            if route_start is not None and route_points and current is not None:
                start = route_start
                for end in route_points:
                    out.slots.append((start[0], start[1], end[0], end[1], current))
                    start = end
            route_start = None
            route_points = []
            plunged = False
            continue
        if line.startswith(("G00", "G0X", "G0Y")):
            coords = parse_coords(line, scale)
            if coords is not None:
                route_start = coords
            continue
        if line.startswith("G01") and not in_header:
            coords = parse_coords(line, scale)
            if coords is not None and plunged:
                route_points.append(coords)
            continue
        if line.startswith(("X", "Y")) and current is not None:
            if "G85" in line:
                first, _, second = line.partition("G85")
                start = parse_coords(first, scale)
                end = parse_coords(second, scale)
                if start is not None and end is not None:
                    out.slots.append((start[0], start[1], end[0], end[1], current))
                continue
            coords = parse_coords(line, scale)
            if coords is not None:
                out.holes.append((coords[0], coords[1], current))


def parse_coords(text: str, scale: float) -> tuple[float, float] | None:
    values = dict((axis, float(value) * scale) for axis, value in COORD_RE.findall(text))
    if "X" not in values or "Y" not in values:
        return None
    return values["X"], values["Y"]


def match_entries(
    reference: list,
    candidate: list,
    describe,
    missing: list[str],
    extra: list[str],
) -> None:
    remaining = list(candidate)
    for entry in reference:
        best = None
        for index, other in enumerate(remaining):
            if entries_match(entry, other):
                best = index
                break
        if best is None:
            missing.append(describe(entry))
        else:
            remaining.pop(best)
    extra.extend(describe(entry) for entry in remaining)


def entries_match(a: Sequence[float], b: Sequence[float]) -> bool:
    if len(a) != len(b):
        return False
    *a_coords, a_diameter = a
    *b_coords, b_diameter = b
    if abs(a_diameter - b_diameter) > DRILL_DIAMETER_TOLERANCE_MM:
        return False
    if len(a_coords) == 2:
        return coords_close(a_coords, b_coords)
    # Slots may list their endpoints in either order (KiCad routes are
    # mirrored around Y relative to nothing in particular).
    return coords_close(a_coords[:2], b_coords[:2]) and coords_close(
        a_coords[2:], b_coords[2:]
    ) or coords_close(a_coords[:2], b_coords[2:]) and coords_close(
        a_coords[2:], b_coords[:2]
    )


def coords_close(a: Sequence[float], b: Sequence[float]) -> bool:
    return all(
        abs(left - right) <= DRILL_POSITION_TOLERANCE_MM for left, right in zip(a, b)
    )


def format_hole(hole: tuple[float, float, float]) -> str:
    return f"hole d={hole[2]:.3f} at ({hole[0]:.3f}, {hole[1]:.3f})"


def format_slot(slot: tuple[float, float, float, float, float]) -> str:
    return (
        f"slot d={slot[4]:.3f} from ({slot[0]:.3f}, {slot[1]:.3f}) "
        f"to ({slot[2]:.3f}, {slot[3]:.3f})"
    )


# --- Reporting ---------------------------------------------------------------


def print_summary(
    results: list[LayerResult],
    drill_result: DrillResult | None,
    args: argparse.Namespace,
) -> bool:
    failed = False
    print(
        f"{'layer':<12} {'kicad mm^2':>12} {'ipc mm^2':>12} {'xor mm^2':>10} "
        f"{'largest':>10} {'status':>8}"
    )
    for result in results:
        layer_failed = result.failed(args)
        failed |= layer_failed
        status = "FAIL" if layer_failed else "ok"
        print(
            f"{result.layer:<12} {result.kicad_area_mm2:>12.4f} "
            f"{result.ipc_area_mm2:>12.4f} {result.diff_mm2:>10.4f} "
            f"{result.largest_component_mm2:>10.4f} {status:>8}"
        )
        if layer_failed:
            print(f"  see {result.panel}")

    if drill_result is not None:
        drill_failed = drill_result.failed()
        failed |= drill_failed
        status = "FAIL" if drill_failed else "ok"
        print(
            f"{'drills':<12} {drill_result.kicad_holes:>6} holes "
            f"{drill_result.kicad_slots:>3} slots vs "
            f"{drill_result.ipc_holes:>6} holes {drill_result.ipc_slots:>3} slots "
            f"{status:>8}"
        )
        for line in drill_result.missing[:10]:
            print(f"  missing in candidate: {line}")
        for line in drill_result.extra[:10]:
            print(f"  extra in candidate: {line}")
    return failed


# --- KiCad / process helpers -------------------------------------------------


def run_kicad_gerbers(
    kicad_cli: str, layout: Path, layers: list[str], output_dir: Path
) -> None:
    if output_dir.exists():
        shutil.rmtree(output_dir)
    output_dir.mkdir(parents=True)
    run(
        [
            kicad_cli,
            "pcb",
            "export",
            "gerbers",
            "--layers",
            ",".join(layers),
            "--check-zones",
            "--output",
            str(output_dir),
            str(layout),
        ]
    )


KICAD_FILE_ALIASES = {
    "F.SilkS": ["F_SilkS", "F_Silkscreen"],
    "B.SilkS": ["B_SilkS", "B_Silkscreen"],
    "F.Mask": ["F_Mask", "F_Soldermask"],
    "B.Mask": ["B_Mask", "B_Soldermask"],
}


def kicad_gerber_file(output_dir: Path, kicad_layer: str) -> Path:
    stem_fragments = KICAD_FILE_ALIASES.get(kicad_layer, [kicad_layer.replace(".", "_")])
    matches = [
        path
        for path in output_dir.iterdir()
        if path.is_file()
        and path.suffix.lower() != ".gbrjob"
        and any(path.stem.endswith(f"-{fragment}") for fragment in stem_fragments)
    ]
    if len(matches) != 1:
        names = ", ".join(path.name for path in output_dir.iterdir()) or "none"
        fail(f"expected one KiCad Gerber for {kicad_layer} in {output_dir}, got: {names}")
    return matches[0]


def prepare_layout_for_exports(
    layout: Path, prepared_layout: Path, kicad_python: str | None
) -> Path:
    shutil.copy2(layout, prepared_layout)
    copy_companion_file(layout, prepared_layout, ".kicad_pro")
    if kicad_python is not None:
        refill_zones(kicad_python, prepared_layout)
    return prepared_layout


def copy_companion_file(layout: Path, prepared_layout: Path, suffix: str) -> None:
    source = layout.with_suffix(suffix)
    if source.exists():
        shutil.copy2(source, prepared_layout.with_suffix(suffix))


def refill_zones(kicad_python: str, layout: Path) -> None:
    script = """
try:
    import wx
    wx.Log.SetLogLevel(wx.LOG_Error)
    _wx_app = wx.GetApp() or wx.App(False)
except Exception:
    _wx_app = None

import pcbnew
import sys

layout = sys.argv[1]
board = pcbnew.LoadBoard(layout)
filler = pcbnew.ZONE_FILLER(board)
filler.Fill(board.Zones())
pcbnew.SaveBoard(layout, board)
"""
    run([kicad_python, "-c", script, str(layout)])


def run_pcbc(args: argparse.Namespace, pcbc_args: Sequence[str]) -> None:
    if args.pcbc_bin:
        cmd = [str(args.pcbc_bin), *pcbc_args]
    else:
        cmd = ["cargo", "run"]
        if args.release:
            cmd.append("--release")
        cmd.extend(["-p", "pcbc", "--", *pcbc_args])
    run(cmd, cwd=REPO_ROOT)


def unzip_to(zip_path: Path, output_dir: Path) -> None:
    if output_dir.exists():
        shutil.rmtree(output_dir)
    output_dir.mkdir(parents=True)
    with zipfile.ZipFile(zip_path) as archive:
        archive.extractall(output_dir)


def resolve_command(value: str | None, fallback: str) -> str:
    if value:
        path = shutil.which(value) if os.sep not in value else value
        if path and Path(path).exists():
            return str(path)
        fail(f"command not found: {value}")
    path = shutil.which(fallback)
    if path:
        return path
    if fallback == "kicad-cli":
        mac_path = "/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"
        if Path(mac_path).exists():
            return mac_path
    fail(f"command not found: {fallback}")


def resolve_kicad_python(value: str | None, kicad_cli: str) -> str:
    candidates: list[str] = []
    if value:
        candidates.append(resolve_command(value, value))
    else:
        app_python = kicad_app_python(kicad_cli)
        if app_python is not None:
            candidates.append(str(app_python))
        python3 = shutil.which("python3")
        if python3:
            candidates.append(python3)

    for candidate in candidates:
        if python_imports_pcbnew(candidate):
            return candidate

    fail(
        "could not find a Python interpreter with pcbnew; pass --kicad-python "
        "or use --no-refill-zones"
    )


def kicad_app_python(kicad_cli: str) -> Path | None:
    kicad_cli_path = Path(kicad_cli).resolve()
    for parent in kicad_cli_path.parents:
        if parent.name != "KiCad.app":
            continue
        candidate = (
            parent
            / "Contents"
            / "Frameworks"
            / "Python.framework"
            / "Versions"
            / "Current"
            / "bin"
            / "python3"
        )
        return candidate if candidate.exists() else None
    return None


def python_imports_pcbnew(python: str) -> bool:
    return (
        subprocess.run(
            [python, "-c", "import pcbnew"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        ).returncode
        == 0
    )


def run(command: Sequence[str], cwd: Path | None = None) -> None:
    print("+ " + " ".join(command), flush=True)
    try:
        subprocess.run(command, cwd=cwd, check=True)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.returncode) from error


def fail(message: str) -> NoReturn:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(2)


if __name__ == "__main__":
    raise SystemExit(main())
