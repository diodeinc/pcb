"""Extract actual KiCad courtyards, never IPC's bounding-hull fallback.

Run with KiCad's Python (usually /usr/bin/python3). Input is the corpus source
archive's layouts/, exports/, provenance/ directories; originals are read only.
KiCad owns courtyard construction and board transforms. This adapter accepts
one closed polygon or circle per side, and reports everything else as missing
evidence. Circles bypass KiCad's flattened cache. No datasheet validation.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import pcbnew


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def extract(layout, provenance, xml):
    board = pcbnew.LoadBoard(str(layout))
    if board is None:
        raise ValueError(f"Cannot load {layout}")
    expected = f"Checked-in layout SHA-256: {digest(layout)}"
    if expected not in provenance["limitations"]:
        raise ValueError(f"Layout hash disagrees with corpus provenance: {layout}")
    components = []
    for fp in sorted(board.GetFootprints(), key=lambda fp: fp.GetReference()):
        fp.BuildCourtyardCaches()
        envelopes, issues = [], []
        primary = pcbnew.B_CrtYd if fp.IsFlipped() else pcbnew.F_CrtYd
        for layer in (pcbnew.F_CrtYd, pcbnew.B_CrtYd):
            # Do not mistake GetCourtyard's empty cache for an empty obstacle.
            graphics = [g for g in fp.GraphicalItems() if g.GetLayer() == layer]
            polygon = fp.GetCourtyard(layer)
            if not graphics and layer != primary:
                continue
            side = "Bottom" if layer == pcbnew.B_CrtYd else "Top"
            # The cache has already tessellated circles with an unrecorded
            # tolerance. Transfer the source primitive, and let pcb-ir prepare
            # it under the same explicit accuracy budget as the board profile.
            if (
                len(graphics) == 1
                and isinstance(graphics[0], pcbnew.PCB_SHAPE)
                and graphics[0].GetShape() == pcbnew.SHAPE_T_CIRCLE
            ):
                circle = graphics[0]
                center = circle.GetCenter()
                envelopes.append(
                    {
                        "side": side,
                        "rings": [],
                        "circle": {
                            "center": [pcbnew.ToMM(center.x), -pcbnew.ToMM(center.y)],
                            "radius_mm": pcbnew.ToMM(circle.GetRadius()),
                        },
                    }
                )
                continue
            reason = None
            if not graphics or polygon.OutlineCount() == 0:
                reason = "missing or malformed courtyard"
            elif polygon.OutlineCount() != 1 or polygon.HoleCount(0) != 0:
                reason = "multiple contours or holes outside this adapter's contract"
            elif any(
                not isinstance(g, pcbnew.PCB_SHAPE)
                or g.GetShape()
                not in (
                    pcbnew.SHAPE_T_SEGMENT,
                    pcbnew.SHAPE_T_RECT,
                    pcbnew.SHAPE_T_POLY,
                )
                or (
                    g.GetShape() == pcbnew.SHAPE_T_POLY
                    and any(
                        g.GetPolyShape().Outline(i).ArcCount()
                        for i in range(g.GetPolyShape().OutlineCount())
                    )
                )
                for g in graphics
            ):
                reason = "unsupported source curves or non-shape courtyard graphics"
            else:
                ring = polygon.Outline(0)
                if not ring.IsClosed() or ring.PointCount() < 3:
                    reason = "courtyard is not closed"
                elif ring.ArcCount():
                    reason = "curved courtyard needs an explicit flattening budget"
                elif polygon.IsSelfIntersecting():
                    reason = "self-intersecting courtyard"
            if reason:
                issues.append(f"{side}: {reason}")
            else:
                envelopes.append(
                    {
                        "side": side,
                        "rings": [
                            [
                                [
                                    pcbnew.ToMM(ring.CPoint(i).x),
                                    -pcbnew.ToMM(ring.CPoint(i).y),
                                ]
                                for i in range(ring.PointCount())
                            ]
                        ],
                    }
                )
        at = fp.GetPosition()
        components.append(
            {
                "id": fp.GetReference(),
                "uuid": str(fp.m_Uuid.AsString()),
                "side": "Bottom" if fp.IsFlipped() else "Top",
                "dnp": fp.IsDNP(),
                "at": [pcbnew.ToMM(at.x), -pcbnew.ToMM(at.y)],
                "envelopes": envelopes,
                "issues": issues,
            }
        )
    zones = list(board.Zones())
    for fp in board.GetFootprints():
        zones.extend(fp.Zones())
    return {
        "version": 1,
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
            c["id"] for c in result["components"] if c["issues"] and not c["dnp"]
        ]
        print(
            f"{layout.stem}: {len(result['components'])} footprints; incomplete populated courtyards: {', '.join(missing) or 'none'}"
        )


if __name__ == "__main__":
    main()
