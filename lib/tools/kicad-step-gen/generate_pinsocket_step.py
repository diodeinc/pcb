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
from dataclasses import dataclass, replace
from math import cos, radians, sin
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
from generators.connector.pin_socket.cq_base_parameters import CaseType, PinStyle
from generators.connector.pin_socket.cq_socket_strips import (
    angled_socket_strip,
    smd_socket_strip,
    socket_strip,
)
from generators.tools.model.stepreduce import stepreduce


DEFAULT_FOOTPRINT_ROOT = Path("lib/std/kicad-footprints")
DEFAULT_OUT_DIR = Path("target/kicad-step-gen/pinsockets")
DEFAULT_PCBC = Path("target/debug/pcbc")

FOOTPRINT_RE = re.compile(
    r"PinSocket_(?P<rows>[12])x(?P<pins>\d{2})_P(?P<pitch>\d\.\d{2})mm_"
    r"(?P<mount>Horizontal|Vertical(?:_SMD(?:_Pin1Left|_Pin1Right)?)?)"
    r"\.kicad_mod$"
)

BLACK_RGB = (0.148000001907, 0.144999995828, 0.144999995828)
METAL_RGB = (0.85900002718, 0.737999975681, 0.495999991894)


@dataclass(frozen=True)
class SocketSpec:
    rows: int
    pins: int
    pitch: float
    kind: str
    model_class: str
    params: dict[str, object]

    @property
    def name(self) -> str:
        return f"PinSocket_{self.rows}x{self.pins:02}_P{self.pitch:.2f}mm_{self.kind}"


def tht_params(
    *,
    model_class: str,
    rows: int,
    pitch: float,
    pin_style: str,
    body_width: float,
    body_height: float,
    body_overlength: float,
    body_offset: float,
    pin_width: float,
    pin_thickness: float,
    pin_length: float,
    pins_offset: float,
) -> dict[str, object]:
    return {
        "model_class": model_class,
        "type": CaseType.THT,
        "pin_style": pin_style,
        "num_pin_rows": rows,
        "pin_pitch": pitch,
        "body_width": body_width,
        "body_height": body_height,
        "body_overlength": body_overlength,
        "body_offset": body_offset,
        "pin_width": pin_width,
        "pin_thickness": pin_thickness,
        "pin_length": pin_length,
        "pins_offset": pins_offset,
        "rotation": 90,
    }


def smd_params(
    *,
    model_class: str,
    rows: int,
    pitch: float,
    body_width: float,
    body_height: float,
    body_board_distance: float,
    body_overlength: float,
    pin1start_right: bool,
    pin_width: float,
    pin_thickness: float,
    pin_length: float,
    rotation: int,
) -> dict[str, object]:
    return {
        "model_class": model_class,
        "type": CaseType.SMD,
        "pin_style": PinStyle.STRAIGHT,
        "num_pin_rows": rows,
        "pin_pitch": pitch,
        "body_width": body_width,
        "body_height": body_height,
        "body_board_distance": body_board_distance,
        "body_overlength": body_overlength,
        "body_offset": 0.0,
        "pin1start_right": pin1start_right,
        "pin_width": pin_width,
        "pin_thickness": pin_thickness,
        "pin_length": pin_length,
        "pins_offset": 0.0,
        "rotation": rotation,
    }


SOCKET_VARIANTS: dict[tuple[str, int, float, str | None], dict[str, object]] = {
    ("Vertical", 1, 1.27, None): tht_params(
        model_class="THT-1x1.27mm_Vertical",
        rows=1,
        pitch=1.27,
        pin_style=PinStyle.STRAIGHT,
        body_width=2.54,
        body_height=4.4,
        body_overlength=0.0,
        body_offset=0.0,
        pin_width=0.5,
        pin_thickness=0.2,
        pin_length=2.4,
        pins_offset=0.0,
    ),
    ("Vertical", 2, 1.27, None): tht_params(
        model_class="THT-2x1.27mm_Vertical",
        rows=2,
        pitch=1.27,
        pin_style=PinStyle.STRAIGHT,
        body_width=3.05,
        body_height=4.4,
        body_overlength=0.0,
        body_offset=0.0,
        pin_width=0.5,
        pin_thickness=0.2,
        pin_length=2.4,
        pins_offset=0.0,
    ),
    ("Horizontal", 2, 1.27, None): tht_params(
        model_class="THT-2x1.27mm_Horizontal",
        rows=2,
        pitch=1.27,
        pin_style=PinStyle.ANGLED,
        body_width=4.4,
        body_height=3.10,
        body_overlength=0.35,
        body_offset=0.15,
        pin_width=0.5,
        pin_thickness=0.15,
        pin_length=2.0,
        pins_offset=0.0,
    ),
    ("Vertical_SMD", 1, 1.27, "Pin1Left"): smd_params(
        model_class="SMD-1x1.27mm_Vertical_Left",
        rows=1,
        pitch=1.27,
        body_width=1.8,
        body_height=4.3,
        body_board_distance=0.3,
        body_overlength=0.35,
        pin1start_right=False,
        pin_width=0.42,
        pin_thickness=0.15,
        pin_length=3.5,
        rotation=90,
    ),
    ("Vertical_SMD", 1, 1.27, "Pin1Right"): smd_params(
        model_class="SMD-1x1.27mm_Vertical_Right",
        rows=1,
        pitch=1.27,
        body_width=1.8,
        body_height=4.3,
        body_board_distance=0.3,
        body_overlength=0.35,
        pin1start_right=True,
        pin_width=0.42,
        pin_thickness=0.15,
        pin_length=3.5,
        rotation=-90,
    ),
    ("Vertical_SMD", 2, 1.27, None): smd_params(
        model_class="SMD-2x1.27mm_Vertical",
        rows=2,
        pitch=1.27,
        body_width=2.54,
        body_height=4.4,
        body_board_distance=0.2,
        body_overlength=0.0,
        pin1start_right=True,
        pin_width=0.4,
        pin_thickness=0.15,
        pin_length=5.11,
        rotation=90,
    ),
    ("Vertical", 1, 2.54, None): tht_params(
        model_class="THT-1x2.54mm_Vertical",
        rows=1,
        pitch=2.54,
        pin_style=PinStyle.STRAIGHT,
        body_width=2.54,
        body_height=8.5,
        body_overlength=0.0,
        body_offset=0.0,
        pin_width=0.6,
        pin_thickness=0.2,
        pin_length=2.9,
        pins_offset=0.0,
    ),
    ("Vertical", 2, 2.54, None): tht_params(
        model_class="THT-2x2.54mm_Vertical",
        rows=2,
        pitch=2.54,
        pin_style=PinStyle.STRAIGHT,
        body_width=5.08,
        body_height=8.5,
        body_overlength=0.0,
        body_offset=0.0,
        pin_width=0.6,
        pin_thickness=0.2,
        pin_length=2.9,
        pins_offset=0.0,
    ),
    ("Horizontal", 1, 2.54, None): tht_params(
        model_class="THT-1x2.54mm_Horizontal",
        rows=1,
        pitch=2.54,
        pin_style=PinStyle.ANGLED,
        body_width=8.51,
        body_height=2.54,
        body_overlength=0.0,
        body_offset=1.52,
        pin_width=0.6,
        pin_thickness=0.2,
        pin_length=2.9,
        pins_offset=0.0,
    ),
    ("Horizontal", 2, 2.54, None): tht_params(
        model_class="THT-2x2.54mm_Horizontal",
        rows=2,
        pitch=2.54,
        pin_style=PinStyle.ANGLED,
        body_width=8.51,
        body_height=5.08,
        body_overlength=0.0,
        body_offset=1.52,
        pin_width=0.6,
        pin_thickness=0.2,
        pin_length=2.5,
        pins_offset=0.0,
    ),
    ("Vertical_SMD", 1, 2.54, "Pin1Left"): smd_params(
        model_class="SMD-1x2.54mm_Vertical_Left",
        rows=1,
        pitch=2.54,
        body_width=2.54,
        body_height=7.1,
        body_board_distance=0.4,
        body_overlength=0.2,
        pin1start_right=False,
        pin_width=0.6,
        pin_thickness=0.2,
        pin_length=4.54,
        rotation=90,
    ),
    ("Vertical_SMD", 1, 2.54, "Pin1Right"): smd_params(
        model_class="SMD-1x2.54mm_Vertical_Right",
        rows=1,
        pitch=2.54,
        body_width=2.54,
        body_height=7.1,
        body_board_distance=0.4,
        body_overlength=0.2,
        pin1start_right=True,
        pin_width=0.6,
        pin_thickness=0.2,
        pin_length=4.54,
        rotation=-90,
    ),
    ("Vertical_SMD", 2, 2.54, None): smd_params(
        model_class="SMD-2x2.54mm_Vertical",
        rows=2,
        pitch=2.54,
        body_width=5.08,
        body_height=7.15,
        body_board_distance=0.3,
        body_overlength=0.0,
        pin1start_right=True,
        pin_width=0.64,
        pin_thickness=0.2,
        pin_length=6.70,
        rotation=90,
    ),
}


def qcolor(rgb: tuple[float, float, float]) -> Quantity_Color:
    return Quantity_Color(*rgb, Quantity_TypeOfColor.Quantity_TOC_sRGB)


def loc(delta: tuple[float, float, float]) -> TopLoc_Location:
    trsf = gp_Trsf()
    trsf.SetTranslation(gp_Vec(*delta))
    return TopLoc_Location(trsf)


def shape(workplane: cq.Workplane):
    return workplane.val().wrapped


def param_float(params: dict[str, object], key: str) -> float:
    value = params[key]
    if not isinstance(value, int | float):
        raise TypeError(f"{key} must be numeric")
    return float(value)


def param_int(params: dict[str, object], key: str) -> int:
    value = params[key]
    if not isinstance(value, int):
        raise TypeError(f"{key} must be an integer")
    return value


def make_doc(name: str) -> TDocStd_Document:
    app = XCAFApp_Application.GetApplication_s()
    doc = TDocStd_Document(TCollection_ExtendedString(name))
    app.NewDocument(TCollection_ExtendedString("MDTV-XCAF"), doc)
    return doc


def rotate_z(
    delta: tuple[float, float, float], degrees: float
) -> tuple[float, float, float]:
    angle = radians(degrees)
    x, y, z = delta
    return (
        x * cos(angle) - y * sin(angle),
        x * sin(angle) + y * cos(angle),
        z,
    )


def parse_footprint(path: Path) -> SocketSpec:
    match = FOOTPRINT_RE.fullmatch(path.name)
    if match is None:
        raise ValueError(f"not a supported pin socket footprint: {path}")

    rows = int(match["rows"])
    pins = int(match["pins"])
    pitch = float(match["pitch"])
    mount = match["mount"]
    if mount.startswith("Vertical_SMD"):
        kind = "Vertical_SMD"
        pin_1_start = (
            "Pin1Right"
            if mount.endswith("_Pin1Right")
            else ("Pin1Left" if rows == 1 else None)
        )
    else:
        kind = mount
        pin_1_start = None

    params = SOCKET_VARIANTS[(kind, rows, pitch, pin_1_start)]
    return SocketSpec(
        rows=rows,
        pins=pins,
        pitch=pitch,
        kind=mount,
        model_class=str(params["model_class"]),
        params=params,
    )


def discover_footprints(root: Path) -> list[tuple[Path, SocketSpec]]:
    footprints = sorted(root.glob("Connector_PinSocket_*.pretty/PinSocket_*.kicad_mod"))
    return [(path, parse_footprint(path)) for path in footprints]


def export_translation(spec: SocketSpec) -> tuple[float, float, float]:
    p = spec.params
    pin_num = spec.pins
    pitch = param_float(p, "pin_pitch")
    rows = param_int(p, "num_pin_rows")
    pin_width = param_float(p, "pin_width")
    pin_thickness = param_float(p, "pin_thickness")
    model_class = spec.model_class

    if model_class == "THT-1x1.27mm_Vertical":
        return (
            pitch * (pin_num / 2.0 / rows - 0.5),
            pin_width / 2.0 - pin_thickness - 0.05,
            0.0,
        )
    if model_class == "THT-1x2.54mm_Vertical":
        return (
            pitch * (pin_num / 2.0 / rows - 0.5),
            pin_width / 2.0 - pin_thickness - 0.1,
            0.0,
        )
    if model_class.startswith("THT-1") and model_class.endswith("Horizontal"):
        return (pitch * (pin_num - 1) / 2.0, 0.0, 0.0)
    if model_class.startswith("THT-2") and model_class.endswith("Horizontal"):
        return (
            pitch * (pin_num - 1) / 2.0,
            pitch / 2.0 - pin_width / 2.0 + pin_thickness + 0.1,
            0.0,
        )
    if model_class.startswith("SMD"):
        return (0.0, 0.0, pin_width / 2.0)
    return (pitch * (pin_num - 1) / 2.0, pitch / 2.0, 0.0)


def export_transform(spec: SocketSpec, part: cq.Workplane) -> cq.Workplane:
    tx, ty, tz = export_translation(spec)
    part = part.translate((-tx, ty, tz))
    rotation = param_float(spec.params, "rotation")
    if rotation:
        part = part.rotate((0, 0, 0), (0, 0, 1), rotation)
    return part


def cqm_for_columns(
    spec: SocketSpec, columns: int, *, body_overlength: float | None = None
):
    params = dict(spec.params)
    params["num_pins"] = spec.rows * columns
    if body_overlength is not None:
        params["body_overlength"] = body_overlength
    if spec.model_class.startswith("SMD"):
        return smd_socket_strip(params)
    if spec.model_class.endswith("Horizontal"):
        return angled_socket_strip(params)
    return socket_strip(params)


def cqm_for(spec: SocketSpec):
    return cqm_for_columns(spec, spec.pins)


def vertical_pin_components(
    cqm,
) -> list[tuple[cq.Workplane, list[tuple[float, float, float]]]]:
    pitch = cqm.pin_pitch
    count = int(cqm.num_pins / cqm.num_pin_rows)
    first = (
        cqm._make_straight_pin()
        .union(cqm._make_pinsocket())
        .translate(cqm.first_pin_pos + (cqm.body_board_distance,))
    )
    components = [(first, [(-pitch * i, 0.0, 0.0) for i in range(count)])]
    if cqm.num_pin_rows == 2:
        second = first.rotate((0, 0, 0), (0, 0, 1), 180)
        components.append((second, [(pitch * i, 0.0, 0.0) for i in range(count)]))
    return components


def horizontal_pin_components(
    cqm,
) -> list[tuple[cq.Workplane, list[tuple[float, float, float]]]]:
    pitch = cqm.pin_pitch
    count = int(cqm.num_pins / cqm.num_pin_rows)
    tl = cqm.translate[1] - (0.0 if cqm.num_pin_rows == 1 else cqm.pin_pitch / 2.0)

    first = (
        cqm._make_angled_pin(
            style=CaseType.THT,
            pin_height=cqm.pin_length + cqm.pin_pitch / 2.0,
            top_length=tl,
        )
        .union(cqm._make_pinsocket())
        .translate(cqm.first_pin_pos + (cqm.body_board_distance,))
        .rotate((0, 0, 0), (1, 0, 0), -90)
        .translate(cqm.translate)
    )
    components = [(first, [(-pitch * i, 0.0, 0.0) for i in range(count)])]

    if cqm.num_pin_rows == 2:
        second = (
            cqm._make_angled_pin(
                style=CaseType.THT,
                pin_height=cqm.pin_length + cqm.pin_pitch * 1.5,
                top_length=tl + cqm.pin_pitch,
            )
            .union(cqm._make_pinsocket())
            .translate(
                (cqm.first_pin_pos[0], -cqm.first_pin_pos[1], cqm.body_board_distance)
            )
            .rotate((0, 0, 0), (1, 0, 0), -90)
            .translate(cqm.translate)
        )
        components.append((second, [(-pitch * i, 0.0, 0.0) for i in range(count)]))

    return components


def smd_pin_components(
    cqm,
) -> list[tuple[cq.Workplane, list[tuple[float, float, float]]]]:
    pitch = cqm.pin_pitch
    base = (
        cqm._make_angled_pin(top_length=cqm.body_board_distance)
        .union(cqm._make_pinsocket())
        .translate((0.0, 0.0, cqm.body_board_distance))
    )

    if cqm.num_pin_rows == 1:
        first = base.translate(
            (
                cqm.first_pin_pos[0] + (0.0 if not cqm.pin1start_right else -pitch),
                cqm.first_pin_pos[1],
                0.0,
            )
        )
        count = int(cqm.num_pins / 2)
        components = [(first, [(-pitch * 2.0 * i, 0.0, 0.0) for i in range(count)])]

        second = first.rotate((0, 0, 0), (0, 0, 1), 180)
        second_offsets = [(pitch * 2.0 * i, 0.0, 0.0) for i in range(count)]
        if cqm.odd_pins:
            if cqm.pin1start_right:
                odd_pin = first.rotate((0, 0, 0), (0, 0, 1), 180).translate(
                    (-pitch, 0.0, 0.0)
                )
            else:
                odd_pin = first.translate((-cqm.first_pin_pos[0] * 2.0, 0.0, 0.0))
            components.append((odd_pin, [(0.0, 0.0, 0.0)]))
            second_offsets = [(x + pitch, y, z) for x, y, z in second_offsets]
        components.append((second, second_offsets))
        return components

    first = base.translate(cqm.first_pin_pos + (0.0,))
    count = int(cqm.num_pins / cqm.num_pin_rows)
    components = [(first, [(-pitch * i, 0.0, 0.0) for i in range(count)])]
    second = first.rotate((0, 0, 0), (0, 0, 1), 180)
    components.append((second, [(pitch * i, 0.0, 0.0) for i in range(count)]))
    return components


def pin_components(
    spec: SocketSpec, cqm
) -> list[tuple[cq.Workplane, list[tuple[float, float, float]]]]:
    if spec.model_class.startswith("SMD"):
        return smd_pin_components(cqm)
    if spec.model_class.endswith("Horizontal"):
        return horizontal_pin_components(cqm)
    return vertical_pin_components(cqm)


def body_cell_offsets(spec: SocketSpec) -> list[tuple[float, float, float]]:
    if spec.model_class.startswith("SMD"):
        return [
            (((spec.pins - 1) / 2.0 - i) * spec.pitch, 0.0, 0.0)
            for i in range(spec.pins)
        ]
    return [(-spec.pitch * i, 0.0, 0.0) for i in range(spec.pins)]


def make_body_end_cap(spec: SocketSpec, cqm, length: float) -> cq.Workplane:
    width = cqm.body_width
    body = cq.Workplane(cq.Plane.XY()).rect(length, width).extrude(cqm.body_height)
    if cqm.body_board_distance > 0.0:
        body = (
            body.faces("<Z")
            .rect(length, width - width / 3.0)
            .cutBlind(cqm.body_board_distance)
        )
    if spec.model_class.endswith("Horizontal"):
        body = body.rotate((0, 0, 0), (1, 0, 0), -90).translate(cqm.translate)
    return body


def body_end_cap_offsets(
    spec: SocketSpec, cap_length: float
) -> list[tuple[float, float, float]]:
    if cap_length <= 0.0:
        return []

    if spec.model_class.startswith("SMD"):
        center = spec.pins * spec.pitch / 2.0 + cap_length / 2.0
        return [(-center, 0.0, 0.0), (center, 0.0, 0.0)]

    return [
        (-(spec.pins - 1) * spec.pitch - cap_length / 2.0, 0.0, 0.0),
        (cap_length / 2.0, 0.0, 0.0),
    ]


def add_body_components(
    shape_tool,
    color_tool,
    assembly,
    spec: SocketSpec,
    cqm,
) -> None:
    rotation = param_float(spec.params, "rotation")
    cell_spec = replace(spec, pins=1)
    cell_cqm = cqm_for_columns(spec, 1, body_overlength=0.0)

    body_label = shape_tool.AddShape(
        shape(export_transform(cell_spec, cell_cqm._make_body())),
        False,
    )
    color_tool.SetColor(
        body_label,
        qcolor(BLACK_RGB),
        XCAFDoc_ColorType.XCAFDoc_ColorSurf,
    )
    for offset in body_cell_offsets(spec):
        shape_tool.AddComponent(assembly, body_label, loc(rotate_z(offset, rotation)))

    body_overlength = param_float(spec.params, "body_overlength")
    cap_length = body_overlength / 2.0
    if cap_length <= 0.0:
        return

    cap_spec = spec if spec.model_class.startswith("SMD") else cell_spec
    cap_label = shape_tool.AddShape(
        shape(export_transform(cap_spec, make_body_end_cap(spec, cqm, cap_length))),
        False,
    )
    color_tool.SetColor(
        cap_label,
        qcolor(BLACK_RGB),
        XCAFDoc_ColorType.XCAFDoc_ColorSurf,
    )
    for offset in body_end_cap_offsets(spec, cap_length):
        shape_tool.AddComponent(assembly, cap_label, loc(rotate_z(offset, rotation)))


def write_step(path: Path, spec: SocketSpec, reduce: bool) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    cqm = cqm_for(spec)

    doc = make_doc(path.stem)
    shape_tool = XCAFDoc_DocumentTool.ShapeTool_s(doc.Main())
    color_tool = XCAFDoc_DocumentTool.ColorTool_s(doc.Main())
    assembly = shape_tool.NewShape()

    add_body_components(shape_tool, color_tool, assembly, spec, cqm)

    rotation = param_float(spec.params, "rotation")
    for pin, offsets in pin_components(spec, cqm):
        pin_label = shape_tool.AddShape(shape(export_transform(spec, pin)), False)
        color_tool.SetColor(
            pin_label,
            qcolor(METAL_RGB),
            XCAFDoc_ColorType.XCAFDoc_ColorSurf,
        )
        for offset in offsets:
            shape_tool.AddComponent(
                assembly, pin_label, loc(rotate_z(offset, rotation))
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
        description="Generate compact solid STEP assemblies for stdlib KiCad pin sockets.",
    )
    parser.add_argument("--footprint-root", type=Path, default=DEFAULT_FOOTPRINT_ROOT)
    parser.add_argument("--out-dir", type=Path, default=DEFAULT_OUT_DIR)
    parser.add_argument("--pcbc", type=Path, default=DEFAULT_PCBC)
    parser.add_argument(
        "--embed",
        action="store_true",
        help="Embed each generated STEP into its matching .kicad_mod using pcbc embed-step.",
    )
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
        raise SystemExit("no pin socket footprints matched")

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
