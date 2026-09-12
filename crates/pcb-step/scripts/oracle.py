#!/usr/bin/env python3
"""Compare two STEP files with OCCT as the oracle.

    uv run --with cadquery scripts/oracle.py ours.step kicad.step [--json out.json]

Loads both files through OCCT's XCAF reader, checks every solid with
BRepCheck, and compares the assembly structure (occurrence count), the
board body (volume, bounding box) and the component solids (count, total
volume, bounding box). Exit code 1 when the files disagree beyond tolerance.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from typing import Any

from OCP.Bnd import Bnd_Box
from OCP.BRepBndLib import BRepBndLib
from OCP.BRepCheck import BRepCheck_Analyzer
from OCP.BRepGProp import BRepGProp
from OCP.GProp import GProp_GProps
from OCP.IFSelect import IFSelect_RetDone
from OCP.STEPCAFControl import STEPCAFControl_Reader
from OCP.TCollection import TCollection_ExtendedString
from OCP.TDF import TDF_LabelSequence
from OCP.TDocStd import TDocStd_Document
from OCP.TopAbs import TopAbs_SOLID
from OCP.TopExp import TopExp_Explorer
from OCP.TopoDS import TopoDS
from OCP.XCAFDoc import XCAFDoc_DocumentTool
from OCP.XCAFPrs import XCAFPrs_DocumentExplorer, XCAFPrs_DocumentExplorerFlags_None


def load(path: str):
    doc = TDocStd_Document(TCollection_ExtendedString("doc"))
    reader = STEPCAFControl_Reader()
    reader.SetColorMode(True)
    reader.SetNameMode(True)
    if reader.ReadFile(path) != IFSelect_RetDone:
        raise SystemExit(f"{path}: OCCT could not read the file")
    if not reader.Transfer(doc):
        raise SystemExit(f"{path}: OCCT transfer failed")
    return doc


def solids_of(shape):
    explorer = TopExp_Explorer(shape, TopAbs_SOLID)
    out = []
    while explorer.More():
        out.append(TopoDS.Solid_s(explorer.Current()))
        explorer.Next()
    return out


def volume(shape) -> float:
    props = GProp_GProps()
    BRepGProp.VolumeProperties_s(shape, props)
    return props.Mass()


def bbox(shape):
    box = Bnd_Box()
    BRepBndLib.Add_s(shape, box, False)
    if box.IsVoid():
        return None
    xmin, ymin, zmin, xmax, ymax, zmax = box.Get()
    return [xmin, ymin, zmin, xmax, ymax, zmax]


def summarize(path: str) -> dict:
    start = time.time()
    doc = load(path)
    shape_tool = XCAFDoc_DocumentTool.ShapeTool_s(doc.Main())
    roots = TDF_LabelSequence()
    shape_tool.GetFreeShapes(roots)

    occurrences = []
    explorer = XCAFPrs_DocumentExplorer(doc, XCAFPrs_DocumentExplorerFlags_None)
    while explorer.More():
        node = explorer.Current()
        if explorer.CurrentDepth() == 1:
            label = node.RefLabel
            name = ""
            from OCP.TDataStd import TDataStd_Name

            attr = TDataStd_Name()
            if node.Label.FindAttribute(TDataStd_Name.GetID_s(), attr):
                name = attr.Get().ToExtString()
            shape = shape_tool.GetShape_s(label)
            located = shape.Located(node.Location)
            occurrences.append((name, located))
        explorer.Next()

    invalid = 0
    board: dict[str, Any] | None = None
    components: list[dict[str, Any]] = []
    for name, shape in occurrences:
        for solid in solids_of(shape):
            if not BRepCheck_Analyzer(solid).IsValid():
                invalid += 1
        entry: dict[str, Any] = {
            "name": name,
            "solids": len(solids_of(shape)),
            "volume": volume(shape),
            "bbox": bbox(shape),
        }
        if ("PCB" in name or name.startswith("=>")) and (
            board is None or entry["volume"] > board["volume"]
        ):
            if board is not None:
                components.append(board)
            board = entry
            continue
        components.append(entry)

    # KiCad names the board occurrence with an XCAF path; fall back to the
    # largest solid when no name matched.
    if board is None and components:
        components.sort(key=lambda e: -e["volume"])
        board = components.pop(0)

    total_bbox = None
    for _, shape in occurrences:
        b = bbox(shape)
        if b is None:
            continue
        if total_bbox is None:
            total_bbox = list(b)
        else:
            total_bbox = [min(total_bbox[i], b[i]) for i in range(3)] + [
                max(total_bbox[i], b[i]) for i in range(3, 6)
            ]

    return {
        "path": path,
        "load_seconds": time.time() - start,
        "occurrences": len(occurrences),
        "invalid_solids": invalid,
        "board": board,
        "component_count": len(components),
        "component_solids": sum(c["solids"] for c in components),
        "component_volume": sum(c["volume"] for c in components),
        "components": sorted(components, key=lambda c: (c["name"], c["volume"])),
        "bbox": total_bbox,
    }


def close(a, b, tol):
    return abs(a - b) <= tol


def compare(ours: dict, ref: dict) -> list[str]:
    problems = []
    if ours["invalid_solids"] > ref["invalid_solids"]:
        problems.append(
            f"{ours['invalid_solids']} invalid solids in ours vs {ref['invalid_solids']} in reference"
        )
    if ours["occurrences"] != ref["occurrences"]:
        problems.append(f"occurrences {ours['occurrences']} vs {ref['occurrences']}")
    ob, rb = ours["board"], ref["board"]
    if (ob is None) != (rb is None):
        problems.append("board presence differs")
    elif ob is not None:
        if not close(ob["volume"], rb["volume"], max(0.01 * rb["volume"], 1e-3)):
            problems.append(f"board volume {ob['volume']:.4f} vs {rb['volume']:.4f}")
        if ob["bbox"] and rb["bbox"]:
            for i, (x, y) in enumerate(zip(ob["bbox"], rb["bbox"])):
                if not close(x, y, 0.01):
                    problems.append(f"board bbox[{i}] {x:.4f} vs {y:.4f}")
    if ours["component_count"] != ref["component_count"]:
        problems.append(
            f"component count {ours['component_count']} vs {ref['component_count']}"
        )
    cv, rv = ours["component_volume"], ref["component_volume"]
    if not close(cv, rv, max(0.005 * rv, 1e-3)):
        problems.append(f"component volume {cv:.4f} vs {rv:.4f}")
    if ours["bbox"] and ref["bbox"]:
        for i, (x, y) in enumerate(zip(ours["bbox"], ref["bbox"])):
            if not close(x, y, 0.02):
                problems.append(f"assembly bbox[{i}] {x:.4f} vs {y:.4f}")

    # Per-occurrence comparison by name; a footprint with several models
    # repeats its reference, so match multiset-wise by nearest volume and
    # position.
    def distance(a, b):
        volume = abs(a["volume"] - b["volume"])
        if a["bbox"] and b["bbox"]:
            return volume + max(abs(x - y) for x, y in zip(a["bbox"], b["bbox"]))
        return volume

    by_name: dict[str, list] = {}
    for c in ref["components"]:
        by_name.setdefault(c["name"], []).append(c)
    for c in ours["components"]:
        candidates = by_name.get(c["name"], [])
        if not candidates:
            problems.append(f"{c['name']}: only in ours (volume {c['volume']:.3f})")
            continue
        r = min(candidates, key=lambda x: distance(x, c))
        candidates.remove(r)
        if not close(c["volume"], r["volume"], max(0.01 * abs(r["volume"]), 1e-4)):
            problems.append(
                f"{c['name']}: volume {c['volume']:.4f} vs {r['volume']:.4f}"
            )
        elif c["bbox"] and r["bbox"]:
            worst = max(abs(x - y) for x, y in zip(c["bbox"], r["bbox"]))
            if worst > 0.02:
                problems.append(f"{c['name']}: bbox differs by {worst:.4f} mm")
    for name, rest in by_name.items():
        for r in rest:
            problems.append(f"{name}: only in reference (volume {r['volume']:.3f})")
    return problems


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ours")
    parser.add_argument("reference")
    parser.add_argument("--json")
    args = parser.parse_args()
    ours = summarize(args.ours)
    ref = summarize(args.reference)
    problems = compare(ours, ref)
    result = {"ours": ours, "reference": ref, "problems": problems}
    if args.json:
        with open(args.json, "w") as f:
            json.dump(result, f, indent=1)
    for side in (ours, ref):
        b = side["board"] or {}
        print(
            f"{side['path']}: occurrences={side['occurrences']} invalid={side['invalid_solids']} "
            f"board_volume={b.get('volume', 0):.3f} components={side['component_count']} "
            f"component_volume={side['component_volume']:.3f} load={side['load_seconds']:.1f}s"
        )
    for p in problems[:40]:
        print("MISMATCH:", p)
    if len(problems) > 40:
        print(f"... {len(problems) - 40} more")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
