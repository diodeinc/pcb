"""Run with /usr/bin/python3 -m unittest discover -s this-directory."""

from __future__ import annotations

import unittest

try:
    import pcbnew
except ImportError as error:
    raise unittest.SkipTest("requires KiCad's Python bindings") from error

from extract import courtyard


class CourtyardTest(unittest.TestCase):
    def test_rectangle_rotation_and_exact_circle(self):
        shape = pcbnew.PCB_SHAPE()
        shape.SetShape(pcbnew.SHAPE_T_RECT)
        shape.SetStart(pcbnew.VECTOR2I(2000000, 3000000))
        shape.SetEnd(pcbnew.VECTOR2I(6000000, 5000000))
        shape.Rotate(pcbnew.VECTOR2I(0, 0), pcbnew.EDA_ANGLE(90, pcbnew.DEGREES_T))
        ring = courtyard([shape])["rings"][0]
        self.assertEqual(
            set(map(tuple, ring)), {(3.0, 2.0), (3.0, 6.0), (5.0, 2.0), (5.0, 6.0)}
        )
        shape.SetShape(pcbnew.SHAPE_T_CIRCLE)
        shape.SetCenter(pcbnew.VECTOR2I(4000000, 7000000))
        shape.SetEnd(pcbnew.VECTOR2I(5250000, 7000000))
        self.assertEqual(
            courtyard([shape]),
            {"rings": [], "circle": {"center": [4.0, -7.0], "radius_mm": 1.25}},
        )

    def segments(self, points):
        result = []
        for a, b in zip(points, points[1:] + points[:1]):
            shape = pcbnew.PCB_SHAPE()
            shape.SetShape(pcbnew.SHAPE_T_SEGMENT)
            shape.SetStart(pcbnew.VECTOR2I(*a))
            shape.SetEnd(pcbnew.VECTOR2I(*b))
            result.append(shape)
        return result

    def test_original_vertices_not_inset_cache(self):
        points = [
            (8000000, 1999000),
            (11000000, 1999000),
            (11000000, 3000000),
            (8000000, 3000000),
        ]
        shapes = self.segments(points)
        # Reordering and reversing source segments must not change the polygon.
        shapes[1].SetStart(pcbnew.VECTOR2I(*points[2]))
        shapes[1].SetEnd(pcbnew.VECTOR2I(*points[1]))
        ring = courtyard(list(reversed(shapes)))["rings"][0]
        self.assertEqual(
            set(map(tuple, ring)),
            {(8.0, -1.999), (11.0, -1.999), (11.0, -3.0), (8.0, -3.0)},
        )

    def test_no_endpoint_repair_and_no_hull(self):
        shapes = self.segments([(0, 0), (3000000, 0), (3000000, 2000000), (0, 2000000)])
        shapes[0].SetStart(pcbnew.VECTOR2I(1, 0))
        with self.assertRaisesRegex(ValueError, "closed loop"):
            courtyard(shapes)
        with self.assertRaisesRegex(ValueError, "self-intersecting"):
            courtyard(
                self.segments([(0, 0), (3000000, 2000000), (0, 2000000), (3000000, 0)])
            )
        with self.assertRaisesRegex(ValueError, "multiple"):
            courtyard(
                self.segments([(0, 0), (1000000, 0), (0, 1000000)])
                + self.segments([(4000000, 0), (5000000, 0), (4000000, 1000000)])
            )


if __name__ == "__main__":
    unittest.main()
