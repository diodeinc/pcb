"""Extract actual KiCad courtyards, never IPC's bounding-hull fallback.

Run with KiCad's Python (usually /usr/bin/python3). Input is the corpus source
archive's layouts/, exports/, provenance/ directories; originals are read only.
KiCad owns board transforms. This adapter accepts
one closed polygon or circle per side, and reports everything else as missing
evidence. Source vertices bypass KiCad's inset/joined courtyard cache entirely.
No datasheet validation or endpoint snapping.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import pcbnew


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def courtyard(graphics):
    if not graphics or any(not isinstance(g, pcbnew.PCB_SHAPE) for g in graphics):
        raise ValueError("missing or non-shape courtyard graphics")
    shape = graphics[0]
    if len(graphics) == 1 and shape.GetShape() == pcbnew.SHAPE_T_CIRCLE:
        center = shape.GetCenter()
        if shape.GetRadius() <= 0:
            raise ValueError("empty courtyard circle")
        return {
            "rings": [],
            "circle": {
                "center": [pcbnew.ToMM(center.x), -pcbnew.ToMM(center.y)],
                "radius_mm": pcbnew.ToMM(shape.GetRadius()),
            },
        }
    if len(graphics) == 1 and shape.GetShape() == pcbnew.SHAPE_T_RECT:
        points = [(p.x, p.y) for p in shape.GetRectCorners()]
    elif len(graphics) == 1 and shape.GetShape() == pcbnew.SHAPE_T_POLY:
        polygon = shape.GetPolyShape()
        if polygon.OutlineCount() != 1 or polygon.HoleCount(0):
            raise ValueError("expected one hole-free courtyard polygon")
        ring = polygon.Outline(0)
        if not ring.IsClosed() or ring.ArcCount():
            raise ValueError("open or curved courtyard polygon")
        points = [
            (ring.CPoint(i).x, ring.CPoint(i).y) for i in range(ring.PointCount())
        ]
    elif all(g.GetShape() == pcbnew.SHAPE_T_SEGMENT for g in graphics):
        neighbors = {}
        for g in graphics:
            a, b = g.GetStart(), g.GetEnd()
            a, b = (a.x, a.y), (b.x, b.y)
            neighbors.setdefault(a, []).append(b)
            neighbors.setdefault(b, []).append(a)
        if any(len(ns) != 2 for ns in neighbors.values()):
            raise ValueError("courtyard endpoints do not form a closed loop")
        points, current, previous = [], min(neighbors), None
        while current not in points:
            points.append(current)
            ns = neighbors[current]
            previous, current = current, ns[1] if ns[0] == previous else ns[0]
        if current != points[0] or len(points) != len(neighbors):
            raise ValueError("multiple courtyard loops")
    else:
        raise ValueError("unsupported courtyard curves or mixed primitives")
    if len(points) < 3:
        raise ValueError("degenerate courtyard")
    polygon = pcbnew.SHAPE_POLY_SET()
    polygon.NewOutline()
    for x, y in points:
        polygon.Append(x, y)
    if polygon.IsSelfIntersecting():
        raise ValueError("self-intersecting courtyard")
    return {"rings": [[[pcbnew.ToMM(x), -pcbnew.ToMM(y)] for x, y in points]]}


def extract(layout, provenance, xml):
    board = pcbnew.LoadBoard(str(layout))
    if board is None:
        raise ValueError(f"Cannot load {layout}")
    expected = f"Checked-in layout SHA-256: {digest(layout)}"
    if expected not in provenance["limitations"]:
        raise ValueError(f"Layout hash disagrees with corpus provenance: {layout}")
    components = []
    for fp in sorted(board.GetFootprints(), key=lambda fp: fp.GetReference()):
        envelopes, issues = [], []
        primary = pcbnew.B_CrtYd if fp.IsFlipped() else pcbnew.F_CrtYd
        for layer in (pcbnew.F_CrtYd, pcbnew.B_CrtYd):
            graphics = [g for g in fp.GraphicalItems() if g.GetLayer() == layer]
            if not graphics and layer != primary:
                continue
            side = "Bottom" if layer == pcbnew.B_CrtYd else "Top"
            try:
                envelopes.append({"side": side, **courtyard(graphics)})
            except ValueError as error:
                issues.append(f"{side}: {error}")
        at = fp.GetPosition()
        # Deliberately independent of BOM/position flags, reference and library
        # names: none of those asserts that a footprint has no physical body.
        role = next(
            (f.GetText() for f in fp.GetFields() if f.GetName() == "PhysicalRole"),
            "",
        )
        if role not in ("component", "board-feature"):
            issues.append(f"PhysicalRole {role!r}: expected component or board-feature")
        components.append(
            {
                "id": fp.GetReference(),
                "uuid": str(fp.m_Uuid.AsString()),
                "side": "Bottom" if fp.IsFlipped() else "Top",
                "dnp": fp.IsDNP(),
                "physical_role": role,
                "at": [pcbnew.ToMM(at.x), -pcbnew.ToMM(at.y)],
                "envelopes": envelopes,
                "issues": issues,
            }
        )
    zones = list(board.Zones())
    for fp in board.GetFootprints():
        zones.extend(fp.Zones())
    return {
        "version": 2,
        "name": layout.stem,
        "provenance": provenance,
        "layout_sha256": digest(layout),
        "xml_sha256": digest(xml),
        "kicad_version": pcbnew.GetBuildVersion(),
        "components": components,
        "rule_area_count": sum(zone.GetIsRuleArea() for zone in zones),
        "contract": "Actual source courtyards; one closed polygon or circle per side. Circles retain source center/radius until pcb-ir preparation. No body, Fab, silk, pad or bounding-hull fallback. Dimensions/KLC compliance are not datasheet-validated.",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sources", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    for layout in sorted((args.sources / "layouts").glob("*.kicad_pcb")):
        provenance = json.loads(
            (
                args.sources / "provenance" / f"demo-{layout.stem.lower()}.json"
            ).read_text()
        )
        xml = args.sources / "exports" / f"{layout.stem}.xml"
        result = extract(layout, provenance, xml)
        destination = args.output / f"{layout.stem}.json"
        destination.write_text(json.dumps(result, indent=2) + "\n")
        missing = [
            c["id"]
            for c in result["components"]
            if c["issues"] and not c["dnp"] and c["physical_role"] != "board-feature"
        ]
        print(
            f"{layout.stem}: {len(result['components'])} footprints; incomplete role/courtyard evidence: {', '.join(missing) or 'none'}"
        )


if __name__ == "__main__":
    main()
