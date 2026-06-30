#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pillow>=10"]
# ///
"""Compare KiCad Gerber render against IPC-2581 -> Gerber render output."""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import zipfile
from collections.abc import Sequence
from pathlib import Path
from typing import NoReturn, cast
from xml.etree import ElementTree as ET

from PIL import Image, ImageChops, ImageDraw


REPO_ROOT = Path(__file__).resolve().parents[1]
SVG_NAMESPACE = "http://www.w3.org/2000/svg"
DEFAULT_PX_PER_MM = 100
DEFAULT_TOTAL_TOLERANCE_MM2 = 5.0
DEFAULT_COMPONENT_TOLERANCE_MM2 = 0.5

GERBER_BY_LAYER = {
    "F.Cu": "F_Cu.gtl",
    "B.Cu": "B_Cu.gbl",
    "F.Mask": "F_Mask.gts",
    "B.Mask": "B_Mask.gbs",
    "F.Paste": "F_Paste.gtp",
    "B.Paste": "B_Paste.gbp",
    "F.SilkS": "F_SilkS.gto",
    "B.SilkS": "B_SilkS.gbo",
}


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
    layer = cast(str, args.layer)
    gerber_name = cast(str | None, args.gerber_file) or GERBER_BY_LAYER.get(layer)
    if gerber_name is None:
        fail(f"no default Gerber filename for layer {layer!r}; pass --gerber-file")

    out_dir = (
        args.output_dir
        or Path.cwd() / "build" / "kicad-ipc-gerber-svg-diff" / layout.stem
    ).resolve()
    if out_dir.exists() and args.clean:
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    paths = OutputPaths(out_dir, layer, gerber_name)
    prepared_layout = prepare_layout_for_exports(
        layout, paths.prepared_layout, kicad_python
    )
    run_kicad_gerber(kicad_cli, prepared_layout, layer, paths.kicad_gerber_dir)
    kicad_gerber_layer = find_exported_kicad_gerber(paths.kicad_gerber_dir)
    run_kicad_ipc(kicad_cli, prepared_layout, paths.ipc_xml)
    run_pcbc(
        args,
        [
            "ipc2581",
            "gerber",
            "--layout-target",
            args.layout_target,
            "--output",
            str(paths.gerber_zip),
            str(paths.ipc_xml),
        ],
    )
    unzip_to(paths.gerber_zip, paths.gerber_dir)
    gerber_layer = paths.gerber_dir / gerber_name
    if not gerber_layer.is_file():
        fail(f"Gerber export did not create {gerber_name}; see {paths.gerber_dir}")

    run_pcbc(
        args,
        ["gerber", "render", "--output", str(paths.ipc_gerber_svg), str(gerber_layer)],
    )
    run_pcbc(
        args,
        [
            "gerber",
            "render",
            "--output",
            str(paths.kicad_gerber_svg),
            str(kicad_gerber_layer),
        ],
    )
    ensure_svg_size_from_viewbox(paths.ipc_gerber_svg, paths.ipc_gerber_sized_svg)
    ensure_svg_size_from_viewbox(paths.kicad_gerber_svg, paths.kicad_gerber_sized_svg)

    rasterize_svg(
        rsvg_convert,
        paths.kicad_gerber_sized_svg,
        paths.kicad_png,
        args.px_per_mm,
    )
    rasterize_svg(
        rsvg_convert,
        paths.ipc_gerber_sized_svg,
        paths.ipc_gerber_png,
        args.px_per_mm,
    )

    report = compare_rasters(
        paths.kicad_png,
        paths.ipc_gerber_png,
        paths,
        args.alpha_threshold,
        args.px_per_mm,
    )
    write_panel(paths.diff_panel_png, report)
    print_report(paths, report, args)

    failed = (
        report.diff_mm2 > args.max_total_diff_mm2
        or report.largest_component_mm2 > args.max_component_diff_mm2
    )
    if failed:
        print("FAIL: Gerber raster diff exceeds tolerance", file=sys.stderr)
        return 1

    print("PASS: Gerber raster diff is within tolerance")
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Generate KiCad Gerber and IPC-2581 -> Gerber output for a "
            ".kicad_pcb file, render both to SVG, rasterize both, and fail on "
            "significant copper diff."
        )
    )
    parser.add_argument("layout", type=Path, help="Path to a .kicad_pcb file")
    parser.add_argument("--layer", default="F.Cu", help="KiCad layer to compare")
    parser.add_argument(
        "--gerber-file",
        help="Expected Gerber filename inside the IPC Gerber package; inferred for common layers",
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
        help="Rasterization resolution; 100 means 10,000 pixels per mm^2",
    )
    parser.add_argument(
        "--max-total-diff-mm2",
        type=float,
        default=DEFAULT_TOTAL_TOLERANCE_MM2,
        help="Fail when total XOR area exceeds this value",
    )
    parser.add_argument(
        "--max-component-diff-mm2",
        type=float,
        default=DEFAULT_COMPONENT_TOLERANCE_MM2,
        help="Fail when the largest connected XOR component exceeds this value",
    )
    parser.add_argument(
        "--alpha-threshold",
        type=int,
        default=8,
        help="Alpha value above which a raster pixel counts as painted copper",
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
    parser.set_defaults(clean=True, refill_zones=True)
    return parser.parse_args()


class OutputPaths:
    def __init__(self, out_dir: Path, layer: str, gerber_file: str) -> None:
        safe_layer = layer.replace(".", "_")
        self.out_dir = out_dir
        self.prepared_layout = out_dir / "prepared-layout.kicad_pcb"
        self.ipc_xml = out_dir / "layout.ipc2581.xml"
        self.gerber_zip = out_dir / "ipc-gerbers.zip"
        self.gerber_dir = out_dir / "ipc-gerbers"
        self.kicad_gerber_dir = out_dir / "kicad-gerbers"
        self.kicad_gerber_svg = out_dir / f"kicad-gerber-{safe_layer}.svg"
        self.kicad_gerber_sized_svg = out_dir / f"kicad-gerber-{safe_layer}.sized.svg"
        self.kicad_png = out_dir / f"kicad-{safe_layer}.png"
        self.kicad_mask_png = out_dir / f"kicad-{safe_layer}.mask.png"
        self.ipc_gerber_svg = out_dir / f"ipc-gerber-{safe_layer}.svg"
        self.ipc_gerber_sized_svg = out_dir / f"ipc-gerber-{safe_layer}.sized.svg"
        self.ipc_gerber_png = out_dir / f"ipc-gerber-{safe_layer}.png"
        self.ipc_gerber_mask_png = out_dir / f"ipc-gerber-{safe_layer}.mask.png"
        self.diff_png = out_dir / f"kicad-vs-ipc-gerber-{safe_layer}.diff.png"
        self.xor_png = out_dir / f"kicad-vs-ipc-gerber-{safe_layer}.xor.png"
        self.diff_panel_png = out_dir / f"kicad-vs-ipc-gerber-{safe_layer}.panel.png"
        self.gerber_file = gerber_file


class DiffReport:
    def __init__(
        self,
        *,
        size: tuple[int, int],
        reference_area_px: int,
        candidate_area_px: int,
        diff_px: int,
        px_per_mm: int,
        components: list[tuple[int, tuple[int, int, int, int]]],
    ) -> None:
        self.size = size
        self.reference_area_px = reference_area_px
        self.candidate_area_px = candidate_area_px
        self.diff_px = diff_px
        self.px_per_mm = px_per_mm
        self.components = components

    @property
    def px_per_mm2(self) -> int:
        return self.px_per_mm * self.px_per_mm

    @property
    def reference_area_mm2(self) -> float:
        return self.reference_area_px / self.px_per_mm2

    @property
    def candidate_area_mm2(self) -> float:
        return self.candidate_area_px / self.px_per_mm2

    @property
    def diff_mm2(self) -> float:
        return self.diff_px / self.px_per_mm2

    @property
    def largest_component_px(self) -> int:
        return self.components[0][0] if self.components else 0

    @property
    def largest_component_mm2(self) -> float:
        return self.largest_component_px / self.px_per_mm2


def run_kicad_gerber(
    kicad_cli: str, layout: Path, layer: str, output_dir: Path
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
            layer,
            "--check-zones",
            "--output",
            str(output_dir),
            str(layout),
        ]
    )


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


def find_exported_kicad_gerber(output_dir: Path) -> Path:
    files = [
        path
        for path in output_dir.iterdir()
        if path.is_file() and path.suffix.lower() != ".gbrjob"
    ]
    if len(files) != 1:
        names = ", ".join(path.name for path in files) or "none"
        fail(f"expected exactly one KiCad Gerber in {output_dir}, got {names}")
    return files[0]


def run_kicad_ipc(kicad_cli: str, layout: Path, output: Path) -> None:
    run([kicad_cli, "pcb", "export", "ipc2581", "--output", str(output), str(layout)])


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


def ensure_svg_size_from_viewbox(input_svg: Path, output_svg: Path) -> None:
    text = input_svg.read_text()
    try:
        root = ET.fromstring(text)
    except ET.ParseError as error:
        fail(f"invalid SVG XML in {input_svg}: {error}")

    if root.tag.rsplit("}", 1)[-1] != "svg":
        fail(f"expected SVG root in {input_svg}, got {root.tag!r}")

    viewbox = root.attrib.get("viewBox")
    if viewbox is None:
        fail(f"SVG has no viewBox: {input_svg}")

    if "width" in root.attrib and "height" in root.attrib:
        output_svg.write_text(text)
        return

    values = [float(value) for value in viewbox.replace(",", " ").split()]
    if len(values) != 4 or values[2] <= 0 or values[3] <= 0:
        fail(f"invalid SVG viewBox in {input_svg}: {viewbox!r}")
    width, height = values[2], values[3]
    root.attrib["width"] = f"{width}mm"
    root.attrib["height"] = f"{height}mm"
    ET.register_namespace("", SVG_NAMESPACE)
    output_svg.write_text(ET.tostring(root, encoding="unicode") + "\n")


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


def compare_rasters(
    reference_png: Path,
    candidate_png: Path,
    paths: OutputPaths,
    alpha_threshold: int,
    px_per_mm: int,
) -> DiffReport:
    reference = alpha_mask(reference_png, alpha_threshold)
    candidate = alpha_mask(candidate_png, alpha_threshold)
    ensure_nonempty_mask(reference, "KiCad Gerber", reference_png)
    ensure_nonempty_mask(candidate, "IPC Gerber", candidate_png)
    if reference.size != candidate.size:
        fail(
            f"raster sizes differ: KiCad {reference.size}, IPC Gerber {candidate.size}"
        )
    reference.save(paths.kicad_mask_png)
    candidate.save(paths.ipc_gerber_mask_png)

    only_reference = ImageChops.subtract(reference, candidate)
    only_candidate = ImageChops.subtract(candidate, reference)
    common = ImageChops.multiply(reference, candidate)
    xor = ImageChops.difference(reference, candidate).point(
        lambda value: 255 if value else 0, "L"
    )
    xor.save(paths.xor_png)

    diff = Image.new("RGB", reference.size, "white")
    diff_pixels = diff.load()
    ref_pixels = only_reference.load()
    candidate_pixels = only_candidate.load()
    common_pixels = common.load()
    width, height = reference.size
    for y in range(height):
        for x in range(width):
            if common_pixels[x, y]:
                diff_pixels[x, y] = (18, 18, 18)
            if ref_pixels[x, y]:
                diff_pixels[x, y] = (220, 38, 38)
            if candidate_pixels[x, y]:
                diff_pixels[x, y] = (37, 99, 235)
    diff.save(paths.diff_png)

    components = connected_components(xor)
    return DiffReport(
        size=reference.size,
        reference_area_px=count_painted(reference),
        candidate_area_px=count_painted(candidate),
        diff_px=count_painted(xor),
        px_per_mm=px_per_mm,
        components=components,
    )


def alpha_mask(png: Path, threshold: int) -> Image.Image:
    image = Image.open(png).convert("RGBA")
    return image.getchannel("A").point(
        lambda value: 255 if value > threshold else 0, "L"
    )


def ensure_nonempty_mask(mask: Image.Image, label: str, source_png: Path) -> None:
    if mask.getbbox() is None:
        fail(f"{label} raster contains no painted pixels: {source_png}")


def count_painted(mask: Image.Image) -> int:
    return sum(mask.histogram()[1:])


def connected_components(
    mask: Image.Image,
) -> list[tuple[int, tuple[int, int, int, int]]]:
    width, height = mask.size
    pixels = mask.load()
    seen = bytearray(width * height)
    components: list[tuple[int, tuple[int, int, int, int]]] = []
    for y in range(height):
        for x in range(width):
            start = y * width + x
            if seen[start] or not pixels[x, y]:
                continue
            seen[start] = 1
            stack = [(x, y)]
            min_x = max_x = x
            min_y = max_y = y
            count = 0
            while stack:
                current_x, current_y = stack.pop()
                count += 1
                min_x = min(min_x, current_x)
                max_x = max(max_x, current_x)
                min_y = min(min_y, current_y)
                max_y = max(max_y, current_y)
                for next_y in range(max(0, current_y - 1), min(height, current_y + 2)):
                    for next_x in range(
                        max(0, current_x - 1), min(width, current_x + 2)
                    ):
                        if next_x == current_x and next_y == current_y:
                            continue
                        next_index = next_y * width + next_x
                        if not seen[next_index] and pixels[next_x, next_y]:
                            seen[next_index] = 1
                            stack.append((next_x, next_y))
            components.append((count, (min_x, min_y, max_x + 1, max_y + 1)))
    components.sort(reverse=True)
    return components


def write_panel(output: Path, report: DiffReport) -> None:
    diff = Image.open(
        output.with_name(output.name.replace(".panel.", ".diff."))
    ).convert("RGB")
    max_width = 1600
    scale = min(1.0, max_width / diff.size[0])
    image_width = int(diff.size[0] * scale)
    image_height = int(diff.size[1] * scale)
    header_height = 96
    legend_height = 48
    panel = Image.new(
        "RGB", (image_width, header_height + image_height + legend_height), "white"
    )
    draw = ImageDraw.Draw(panel)
    draw.rectangle([0, 0, image_width, header_height], fill=(245, 245, 245))
    draw.text((18, 14), "KiCad Gerber vs IPC -> Gerber", fill=(20, 20, 20))
    draw.text(
        (18, 42),
        (
            f"XOR {report.diff_mm2:.4f} mm^2; largest component "
            f"{report.largest_component_mm2:.4f} mm^2; "
            f"KiCad {report.reference_area_mm2:.4f} mm^2; "
            f"candidate {report.candidate_area_mm2:.4f} mm^2"
        ),
        fill=(40, 40, 40),
    )
    panel.paste(
        diff.resize((image_width, image_height), Image.Resampling.LANCZOS),
        (0, header_height),
    )
    legend_y = header_height + image_height
    legend = [
        ((18, 18, 18), "common copper"),
        ((220, 38, 38), "KiCad only / missing in candidate"),
        ((37, 99, 235), "candidate only / extra copper"),
    ]
    x = 18
    for color, label in legend:
        draw.rectangle([x, legend_y + 14, x + 24, legend_y + 38], fill=color)
        draw.text((x + 34, legend_y + 14), label, fill=(30, 30, 30))
        x += 330
    panel.save(output)


def print_report(
    paths: OutputPaths, report: DiffReport, args: argparse.Namespace
) -> None:
    print(f"Artifacts: {paths.out_dir}")
    print(f"Prepared layout: {paths.prepared_layout}")
    print(f"KiCad Gerber dir: {paths.kicad_gerber_dir}")
    print(f"KiCad Gerber SVG: {paths.kicad_gerber_svg}")
    print(f"IPC XML: {paths.ipc_xml}")
    print(f"Gerber ZIP: {paths.gerber_zip}")
    print(f"IPC Gerber SVG: {paths.ipc_gerber_svg}")
    print(f"Diff panel: {paths.diff_panel_png}")
    print(
        "Areas: "
        f"KiCad {report.reference_area_mm2:.6f} mm^2, "
        f"candidate {report.candidate_area_mm2:.6f} mm^2, "
        f"delta {report.candidate_area_mm2 - report.reference_area_mm2:.6f} mm^2"
    )
    print(
        "Diff: "
        f"total {report.diff_mm2:.6f} mm^2 "
        f"({report.diff_px:,} px), "
        f"largest component {report.largest_component_mm2:.6f} mm^2 "
        f"({report.largest_component_px:,} px), "
        f"components {len(report.components)}"
    )
    print(
        "Tolerances: "
        f"total <= {args.max_total_diff_mm2:.6f} mm^2, "
        f"largest component <= {args.max_component_diff_mm2:.6f} mm^2"
    )
    for index, (pixels, bbox) in enumerate(report.components[:8], start=1):
        print(
            f"component {index}: {pixels / report.px_per_mm2:.6f} mm^2, "
            f"bbox {bbox}, size {bbox[2] - bbox[0]}x{bbox[3] - bbox[1]} px"
        )


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
