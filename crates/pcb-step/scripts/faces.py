#!/usr/bin/env python3
"""Compare the silkscreen and solder mask faces of two STEP files.

    uv run --with cadquery scripts/faces.py ours.step kicad.step [--tol 0.05]

Loads both files through OCCT's XCAF reader, takes every face of the
silkscreen and solder mask occurrences, and pairs the faces of each layer
by centroid (within `--tol` mm) and area (within 3%). The unpaired faces
are listed largest first, so a missing, misplaced or misshapen item shows
up by where it is. Exit code 1 when any face is unpaired.
"""

from __future__ import annotations

import argparse
import sys

from OCP.BRepGProp import BRepGProp
from OCP.GProp import GProp_GProps
from OCP.IFSelect import IFSelect_RetDone
from OCP.STEPCAFControl import STEPCAFControl_Reader
from OCP.TCollection import TCollection_ExtendedString
from OCP.TDataStd import TDataStd_Name
from OCP.TDocStd import TDocStd_Document
from OCP.TopAbs import TopAbs_FACE
from OCP.TopExp import TopExp_Explorer
from OCP.TopoDS import TopoDS
from OCP.XCAFDoc import XCAFDoc_DocumentTool
from OCP.XCAFPrs import XCAFPrs_DocumentExplorer, XCAFPrs_DocumentExplorerFlags_None

LAYERS = ("Top Silkscreen", "Bottom Silkscreen", "Top Soldermask", "Bottom Soldermask")

# Area and centroid of one face.
Face = tuple[float, tuple[float, float, float]]


def load(path: str):
    doc = TDocStd_Document(TCollection_ExtendedString("doc"))
    reader = STEPCAFControl_Reader()
    reader.SetNameMode(True)
    if reader.ReadFile(path) != IFSelect_RetDone:
        raise SystemExit(f"{path}: OCCT could not read the file")
    if not reader.Transfer(doc):
        raise SystemExit(f"{path}: OCCT transfer failed")
    return doc


def label_name(label) -> str:
    attr = TDataStd_Name()
    if label.FindAttribute(TDataStd_Name.GetID_s(), attr):
        return attr.Get().ToExtString()
    return ""


def faces_of(shape) -> list[Face]:
    out = []
    explorer = TopExp_Explorer(shape, TopAbs_FACE)
    while explorer.More():
        props = GProp_GProps()
        BRepGProp.SurfaceProperties_s(TopoDS.Face_s(explorer.Current()), props)
        c = props.CentreOfMass()
        out.append((props.Mass(), (c.X(), c.Y(), c.Z())))
        explorer.Next()
    return out


def tech_faces(path: str) -> dict[str, list[Face]]:
    """The faces of each tech layer, found by the occurrence's name and
    told apart by side from the sign of their height."""
    doc = load(path)
    shape_tool = XCAFDoc_DocumentTool.ShapeTool_s(doc.Main())
    out: dict[str, list[Face]] = {name: [] for name in LAYERS}
    explorer = XCAFPrs_DocumentExplorer(doc, XCAFPrs_DocumentExplorerFlags_None)
    while explorer.More():
        node = explorer.Current()
        if explorer.CurrentDepth() == 1:
            name = f"{label_name(node.Label)} {label_name(node.RefLabel)}".lower()
            kind = (
                "Silkscreen"
                if "silk" in name
                else "Soldermask"
                if "mask" in name
                else None
            )
            if kind:
                shape = shape_tool.GetShape_s(node.RefLabel).Located(node.Location)
                faces = faces_of(shape)
                if faces:
                    side = "Top" if faces[0][1][2] > 0 else "Bottom"
                    out[f"{side} {kind}"].extend(faces)
        explorer.Next()
    return out


def pair(
    ours: list[Face], ref: list[Face], tol: float
) -> tuple[list[Face], list[Face]]:
    """The faces of each side left without a partner."""
    free = list(range(len(ref)))
    unpaired = []
    for area, (x, y, _) in ours:
        found = None
        for k in free:
            ref_area, (rx, ry, _) = ref[k]
            close = abs(rx - x) <= tol and abs(ry - y) <= tol
            if close and abs(ref_area - area) <= 0.03 * max(area, ref_area) + 1e-4:
                found = k
                break
        if found is None:
            unpaired.append((area, (x, y, _)))
        else:
            free.remove(found)
    return unpaired, [ref[k] for k in free]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ours")
    parser.add_argument("reference")
    parser.add_argument(
        "--tol", type=float, default=0.05, help="centroid tolerance in mm"
    )
    parser.add_argument(
        "--list", type=int, default=12, help="unpaired faces to list per side"
    )
    args = parser.parse_args()
    ours = tech_faces(args.ours)
    ref = tech_faces(args.reference)
    unpaired = 0
    for name in LAYERS:
        a, b = ours[name], ref[name]
        if not a and not b:
            continue
        only_ours, only_ref = pair(a, b, args.tol)
        unpaired += len(only_ours) + len(only_ref)
        print(
            f"{name}: ours {len(a)} faces {sum(f[0] for f in a):.3f} mm2, "
            f"reference {len(b)} faces {sum(f[0] for f in b):.3f} mm2, "
            f"unpaired ours {len(only_ours)} reference {len(only_ref)}"
        )
        for label, faces in (("only ours", only_ours), ("only reference", only_ref)):
            for area, (x, y, _) in sorted(faces, reverse=True)[: args.list]:
                print(f"  {label}: {area:9.4f} mm2 at ({x:8.3f}, {y:8.3f})")
    return 1 if unpaired else 0


if __name__ == "__main__":
    sys.exit(main())
