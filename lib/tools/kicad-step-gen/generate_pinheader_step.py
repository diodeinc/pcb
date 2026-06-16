#!/usr/bin/env -S uv run --script
# Copyright (c) 2026 Diode
# SPDX-License-Identifier: GPL-3.0-or-later
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "kicad-library-generators[3d] @ git+https://gitlab.com/kicad/libraries/kicad-footprint-generator.git@1a927734a1f860a223ec0fd41d28ec3f4d323013",
# ]
# ///
from __future__ import annotations

from argparse import ArgumentParser
from dataclasses import dataclass
from pathlib import Path
import re
import subprocess

import cadquery as cq
from OCP.IFSelect import IFSelect_ReturnStatus
from OCP.Quantity import Quantity_Color, Quantity_TypeOfColor
from OCP.STEPCAFControl import STEPCAFControl_Writer
from OCP.TCollection import TCollection_ExtendedString
from OCP.TDocStd import TDocStd_Document
from OCP.TopLoc import TopLoc_Location
from OCP.XCAFApp import XCAFApp_Application
from OCP.XCAFDoc import XCAFDoc_ColorType, XCAFDoc_DocumentTool
from OCP.gp import gp_Trsf, gp_Vec
from generators.connector.pin_header.other.cq_pinheader import (
    make_Horizontal_THT_base,
    make_Vertical_SMD_base,
    make_Vertical_THT_base,
)
from generators.tools.model.stepreduce import stepreduce


DEFAULT_FOOTPRINT_ROOT = Path("lib/std/kicad-footprints")
DEFAULT_OUT_DIR = Path("target/kicad-step-gen/pinheaders")
DEFAULT_PCBC = Path("target/debug/pcbc")

FOOTPRINT_RE = re.compile(
    r"PinHeader_(?P<rows>[12])x(?P<pins>\d{2})_P(?P<pitch>\d\.\d{2})mm_"
    r"(?P<mount>Horizontal|Vertical(?:_SMD(?:_Pin1Left|_Pin1Right)?)?)"
    r"\.kicad_mod$"
)

BLACK_RGB = (0.148000001907, 0.144999995828, 0.144999995828)
METAL_RGB = (0.85900002718, 0.737999975681, 0.495999991894)
BODY_CELL_PINS = 1


@dataclass(frozen=True)
class HeaderSpec:
    rows: int
    pins: int
    pitch: float
    kind: str
    body_width: float
    body_height: float
    pin_width: float
    pin_length_above_body: float
    pin_length_below_board: float | None = None
    body_x_offset: float | None = None
    body_z_offset: float = 0.0
    pin_length_horizontal: float | None = None
    pin_1_start: str | None = None

    @property
    def name(self) -> str:
        return f"PinHeader_{self.rows}x{self.pins:02}_P{self.pitch:.2f}mm_{self.kind}"

    @property
    def body_chamfer(self) -> float:
        return self.pitch / 10.0

    @property
    def pin_end_chamfer(self) -> float:
        return self.pin_width / 4.0


HEADER_VARIANTS: dict[tuple[str, int, float], dict[str, float]] = {
    ("Vertical", 1, 2.54): {
        "body_width": 2.54,
        "body_height": 2.54,
        "pin_width": 0.64,
        "pin_length_above_body": 6.0,
        "pin_length_below_board": 3.0,
    },
    ("Vertical", 2, 2.54): {
        "body_width": 5.08,
        "body_height": 2.54,
        "pin_width": 0.64,
        "pin_length_above_body": 6.0,
        "pin_length_below_board": 3.0,
    },
    ("Horizontal", 1, 2.54): {
        "body_width": 2.54,
        "body_height": 2.54,
        "body_x_offset": 1.5,
        "pin_width": 0.64,
        "pin_length_above_body": 6.0,
        "pin_length_below_board": 3.0,
    },
    ("Horizontal", 2, 2.54): {
        "body_width": 5.08,
        "body_height": 2.54,
        "body_x_offset": 1.5,
        "pin_width": 0.64,
        "pin_length_above_body": 6.0,
        "pin_length_below_board": 3.0,
    },
    ("Vertical_SMD", 1, 2.54): {
        "body_width": 2.54,
        "body_height": 2.54,
        "body_z_offset": 0.76,
        "pin_width": 0.64,
        "pin_length_above_body": 6.0,
        "pin_length_horizontal": 2.82,
    },
    ("Vertical_SMD", 2, 2.54): {
        "body_width": 5.08,
        "body_height": 2.54,
        "body_z_offset": 0.76,
        "pin_width": 0.64,
        "pin_length_above_body": 6.0,
        "pin_length_horizontal": 2.82,
    },
    ("Vertical", 1, 1.27): {
        "body_width": 2.1,
        "body_height": 1.0,
        "pin_width": 0.4,
        "pin_length_above_body": 3.0,
        "pin_length_below_board": 2.3,
    },
    ("Vertical", 2, 1.27): {
        "body_width": 3.4,
        "body_height": 1.0,
        "pin_width": 0.4,
        "pin_length_above_body": 3.0,
        "pin_length_below_board": 2.3,
    },
    ("Horizontal", 1, 1.27): {
        "body_width": 2.1,
        "body_height": 1.0,
        "body_x_offset": 0.5,
        "pin_width": 0.4,
        "pin_length_above_body": 4.0,
        "pin_length_below_board": 2.4,
    },
    ("Horizontal", 2, 1.27): {
        "body_width": 3.4,
        "body_height": 1.0,
        "body_x_offset": 0.5,
        "pin_width": 0.4,
        "pin_length_above_body": 4.0,
        "pin_length_below_board": 2.4,
    },
    ("Vertical_SMD", 1, 1.27): {
        "body_width": 2.1,
        "body_height": 0.5,
        "body_z_offset": 0.4,
        "pin_width": 0.4,
        "pin_length_above_body": 3.0,
        "pin_length_horizontal": 2.65,
    },
    ("Vertical_SMD", 2, 1.27): {
        "body_width": 3.4,
        "body_height": 0.5,
        "body_z_offset": 0.4,
        "pin_width": 0.4,
        "pin_length_above_body": 3.5,
        "pin_length_horizontal": 2.35,
    },
}


def qcolor(rgb: tuple[float, float, float]) -> Quantity_Color:
    return Quantity_Color(*rgb, Quantity_TypeOfColor.Quantity_TOC_sRGB)


def loc(delta: tuple[float, float, float]) -> TopLoc_Location:
    trsf = gp_Trsf()
    trsf.SetTranslation(gp_Vec(*delta))
    return TopLoc_Location(trsf)


def shape(workplane: cq.Workplane):
    return workplane.val().wrapped


def required_float(value: float | None, field: str) -> float:
    if value is None:
        raise ValueError(f"{field} is required for this pin header variant")
    return value


def make_doc(name: str) -> TDocStd_Document:
    app = XCAFApp_Application.GetApplication_s()
    doc = TDocStd_Document(TCollection_ExtendedString(name))
    app.NewDocument(TCollection_ExtendedString("MDTV-XCAF"), doc)
    return doc


def parse_footprint(path: Path) -> HeaderSpec:
    match = FOOTPRINT_RE.fullmatch(path.name)
    if match is None:
        raise ValueError(f"not a supported pin header footprint: {path}")

    rows = int(match["rows"])
    pins = int(match["pins"])
    pitch = float(match["pitch"])
    mount = match["mount"]
    kind = "Vertical_SMD" if mount.startswith("Vertical_SMD") else mount

    params = HEADER_VARIANTS[(kind, rows, pitch)]
    pin_1_start = None
    if rows == 1 and kind == "Vertical_SMD":
        pin_1_start = "right" if mount.endswith("_Pin1Right") else "left"

    return HeaderSpec(
        rows=rows,
        pins=pins,
        pitch=pitch,
        kind=mount,
        pin_1_start=pin_1_start,
        **params,
    )


def discover_footprints(root: Path) -> list[tuple[Path, HeaderSpec]]:
    footprints = sorted(root.glob("Connector_PinHeader_*.pretty/PinHeader_*.kicad_mod"))
    return [(path, parse_footprint(path)) for path in footprints]


def make_vertical_tht_body(spec: HeaderSpec, pins: int) -> cq.Workplane:
    return make_Vertical_THT_base(
        pins,
        spec.pitch,
        spec.rows,
        spec.body_width,
        spec.body_height,
        spec.body_chamfer,
    )


def make_vertical_tht_pin(spec: HeaderSpec) -> cq.Workplane:
    pin_length_below_board = required_float(
        spec.pin_length_below_board,
        "pin_length_below_board",
    )
    total_length = (
        pin_length_below_board + spec.body_height + spec.pin_length_above_body
    )
    pin = (
        cq.Workplane("XY")
        .workplane(centerOption="CenterOfMass", offset=-pin_length_below_board)
        .box(spec.pin_width, spec.pin_width, total_length, centered=(True, True, False))
    )
    if spec.pin_end_chamfer > 0:
        pin = pin.edges("#Z").chamfer(spec.pin_end_chamfer)
    return pin


def make_horizontal_tht_body(spec: HeaderSpec, pins: int) -> cq.Workplane:
    body_x_offset = required_float(spec.body_x_offset, "body_x_offset")
    return make_Horizontal_THT_base(
        pins,
        spec.pitch,
        spec.rows,
        spec.body_width,
        spec.body_height,
        body_x_offset,
        spec.body_chamfer,
    )


def make_horizontal_tht_pin_prototypes(spec: HeaderSpec) -> list[cq.Workplane]:
    body_x_offset = required_float(spec.body_x_offset, "body_x_offset")
    pin_length_below_board = required_float(
        spec.pin_length_below_board,
        "pin_length_below_board",
    )
    pins = []
    for row in range(spec.rows):
        row_offset = row * spec.pitch
        pin = (
            cq.Workplane("XZ")
            .workplane(centerOption="CenterOfMass", offset=-spec.pin_width / 2)
            .moveTo(
                body_x_offset
                + (spec.rows - 1) * spec.pitch
                + spec.body_height
                + spec.pin_length_above_body,
                (spec.body_width - ((spec.rows - 1) * spec.pitch)) / 2
                + spec.pin_width / 2
                + row * spec.pitch,
            )
            .vLine(-spec.pin_width)
            .hLine(
                -spec.pin_length_above_body
                - spec.body_height
                - body_x_offset
                - row_offset
                + spec.pin_width / 2
            )
            .vLine(
                -(spec.body_width / spec.rows - spec.pin_width) / 2
                - pin_length_below_board
                - row_offset
            )
            .hLine(-spec.pin_width)
            .vLine(
                ((spec.body_width / spec.rows) - spec.pin_width) / 2
                + pin_length_below_board
                + row_offset
                + spec.pin_width
            )
            .close()
            .extrude(spec.pin_width)
            .edges("<X and >Z")
            .fillet(spec.pin_width)
        )

        if spec.pin_end_chamfer > 0:
            pin = pin.faces("<Z").chamfer(spec.pin_end_chamfer)
            pin = pin.faces(">X").chamfer(spec.pin_end_chamfer)

        pins.append(pin)

    return pins


def make_vertical_smd_body(spec: HeaderSpec, pins: int) -> cq.Workplane:
    return make_Vertical_SMD_base(
        pins,
        spec.pitch,
        spec.body_width,
        spec.body_height,
        spec.body_chamfer,
        spec.body_z_offset,
    )


def make_vertical_smd_pin_prototypes(spec: HeaderSpec) -> list[cq.Workplane]:
    pin_length_horizontal = required_float(
        spec.pin_length_horizontal,
        "pin_length_horizontal",
    )
    y_offset = -((spec.pins - 1) * spec.pitch + spec.pin_width) / 2
    left = (
        cq.Workplane("XZ")
        .workplane(centerOption="CenterOfMass", offset=y_offset)
        .moveTo(
            -((spec.rows - 1) * spec.pitch - spec.pin_width) / 2,
            spec.body_z_offset + spec.body_height + spec.pin_length_above_body,
        )
        .hLine(-spec.pin_width)
        .vLine(
            -spec.body_z_offset
            - spec.body_height
            - spec.pin_length_above_body
            + spec.pin_width
        )
        .hLine(-pin_length_horizontal + spec.pin_width)
        .vLine(-spec.pin_width)
        .hLine(pin_length_horizontal)
        .close()
        .extrude(spec.pin_width)
        .edges(">X and <Z")
        .fillet(spec.pin_width)
    )
    if spec.pin_end_chamfer > 0:
        left = left.faces(">Z").chamfer(spec.pin_end_chamfer)
        left = left.faces("<X").chamfer(spec.pin_end_chamfer)

    right = (
        cq.Workplane("XZ")
        .workplane(centerOption="CenterOfMass", offset=y_offset)
        .moveTo(
            ((spec.rows - 1) * spec.pitch - spec.pin_width) / 2,
            spec.body_z_offset + spec.body_height + spec.pin_length_above_body,
        )
        .hLine(spec.pin_width)
        .vLine(
            -spec.body_z_offset
            - spec.body_height
            - spec.pin_length_above_body
            + spec.pin_width
        )
        .hLine(pin_length_horizontal - spec.pin_width)
        .vLine(-spec.pin_width)
        .hLine(-pin_length_horizontal)
        .close()
        .extrude(spec.pin_width)
        .edges("<X and <Z")
        .fillet(spec.pin_width)
    )
    if spec.pin_end_chamfer > 0:
        right = right.faces(">Z").chamfer(spec.pin_end_chamfer)
        right = right.faces(">X").chamfer(spec.pin_end_chamfer)

    return [left, right]


def make_body_cell(spec: HeaderSpec) -> cq.Workplane:
    if spec.kind == "Horizontal":
        return make_horizontal_tht_body(spec, BODY_CELL_PINS)
    if spec.kind == "Vertical":
        return make_vertical_tht_body(spec, BODY_CELL_PINS)
    if spec.kind.startswith("Vertical_SMD"):
        return make_vertical_smd_body(spec, BODY_CELL_PINS)
    raise ValueError(f"unsupported pin header kind: {spec.kind}")


def body_cell_location(spec: HeaderSpec, index: int) -> TopLoc_Location:
    if spec.kind.startswith("Vertical_SMD"):
        # SMD bodies are centered around the full pin range. THT bodies start at
        # pin 1 and extend in negative Y, matching a direct -pitch translation.
        y = ((spec.pins - BODY_CELL_PINS) / 2 - index) * spec.pitch
    else:
        y = -index * spec.pitch
    return loc((0, y, 0))


def write_step(path: Path, spec: HeaderSpec, reduce: bool) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)

    doc = make_doc(path.stem)
    shape_tool = XCAFDoc_DocumentTool.ShapeTool_s(doc.Main())
    color_tool = XCAFDoc_DocumentTool.ColorTool_s(doc.Main())
    assembly = shape_tool.NewShape()

    if spec.kind == "Horizontal":
        pin_prototypes = make_horizontal_tht_pin_prototypes(spec)
    elif spec.kind == "Vertical":
        pin_prototypes = [make_vertical_tht_pin(spec)]
    elif spec.kind.startswith("Vertical_SMD"):
        pin_prototypes = make_vertical_smd_pin_prototypes(spec)
    else:
        raise ValueError(f"unsupported pin header kind: {spec.kind}")

    body_label = shape_tool.AddShape(shape(make_body_cell(spec)), False)
    color_tool.SetColor(
        body_label,
        qcolor(BLACK_RGB),
        XCAFDoc_ColorType.XCAFDoc_ColorSurf,
    )
    for index in range(spec.pins):
        shape_tool.AddComponent(
            assembly,
            body_label,
            body_cell_location(spec, index),
        )

    pin_labels = []
    for pin in pin_prototypes:
        pin_label = shape_tool.AddShape(shape(pin), False)
        color_tool.SetColor(
            pin_label,
            qcolor(METAL_RGB),
            XCAFDoc_ColorType.XCAFDoc_ColorSurf,
        )
        pin_labels.append(pin_label)

    if spec.kind == "Vertical":
        for row in range(spec.rows):
            for index in range(spec.pins):
                shape_tool.AddComponent(
                    assembly,
                    pin_labels[0],
                    loc((row * spec.pitch, index * -spec.pitch, 0)),
                )
    elif spec.kind == "Horizontal":
        for row, pin_label in enumerate(pin_labels):
            for index in range(spec.pins):
                shape_tool.AddComponent(
                    assembly,
                    pin_label,
                    loc((0, index * -spec.pitch, 0)),
                )
    elif spec.kind.startswith("Vertical_SMD"):
        if spec.rows == 1:
            if spec.pin_1_start == "right":
                left_indices = range(0, spec.pins, 2)
                right_indices = range(1, spec.pins, 2)
            else:
                left_indices = range(1, spec.pins, 2)
                right_indices = range(0, spec.pins, 2)
        else:
            right_indices = range(spec.pins)
            left_indices = range(spec.pins)

        for index in right_indices:
            shape_tool.AddComponent(
                assembly,
                pin_labels[0],
                loc((0, index * -spec.pitch, 0)),
            )
        for index in left_indices:
            shape_tool.AddComponent(
                assembly,
                pin_labels[1],
                loc((0, index * -spec.pitch, 0)),
            )

    shape_tool.UpdateAssemblies()

    writer = STEPCAFControl_Writer()
    writer.SetColorMode(True)
    writer.SetNameMode(True)
    if not writer.Transfer(assembly):
        raise RuntimeError("STEP writer transfer failed")

    status = writer.Write(str(path))
    if status != IFSelect_ReturnStatus.IFSelect_RetDone:
        raise RuntimeError(f"STEP writer failed: {status}")

    if reduce:
        stepreduce(str(path), str(path))


def build_parser() -> ArgumentParser:
    parser = ArgumentParser(
        description="Generate compact solid STEP assemblies for stdlib KiCad pin headers.",
    )
    parser.add_argument("--footprint-root", type=Path, default=DEFAULT_FOOTPRINT_ROOT)
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR)
    parser.add_argument(
        "--embed",
        action="store_true",
        help="Embed each generated STEP into its matching .kicad_mod using pcbc embed-step.",
    )
    parser.add_argument("--pcbc", type=Path, default=DEFAULT_PCBC)
    parser.add_argument(
        "--no-reduce",
        action="store_true",
        help="Skip KiCad stepreduce post-processing.",
    )
    parser.add_argument(
        "--filter",
        help="Only process footprints whose filename contains this string.",
    )
    return parser


def embed_step(pcbc: Path, footprint: Path, step: Path) -> None:
    subprocess.run(
        [str(pcbc), "embed-step", str(footprint), str(step)],
        check=True,
    )


def main() -> None:
    args = build_parser().parse_args()
    footprints = discover_footprints(args.footprint_root)
    if args.filter:
        footprints = [item for item in footprints if args.filter in item[0].name]

    if args.embed and not args.pcbc.exists():
        raise SystemExit(f"{args.pcbc} does not exist; run `cargo build -p pcbc` first")

    total = len(footprints)
    if total == 0:
        raise SystemExit("no pin header footprints matched")

    generated_bytes = 0
    for index, (footprint, spec) in enumerate(footprints, start=1):
        step = args.out_dir / f"{spec.name}.step"
        write_step(step, spec, reduce=not args.no_reduce)
        generated_bytes += step.stat().st_size
        if args.embed:
            embed_step(args.pcbc, footprint, step)
        if index % 25 == 0 or index == total:
            action = "generated and embedded" if args.embed else "generated"
            print(f"{action} {index}/{total}")

    print(f"wrote {total} STEP files ({generated_bytes:,} bytes)")


if __name__ == "__main__":
    main()
