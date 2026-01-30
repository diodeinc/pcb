"""
Property-based tests for the HierPlace layout algorithm.

Tests the pure geometry functions in hierplace.py without any KiCad dependencies.

Invariants tested:
1. All valid inputs get a placement
2. No overlaps between placed rectangles
3. Dimensions are preserved (width/height unchanged)
4. Algorithm is deterministic
5. Final layout is translation-only from origin layout
6. Cluster is positioned correctly relative to existing content

Run with: pytest -v test_hierplace.py
"""

import pytest

try:
    from hypothesis import given, settings, assume
    from hypothesis import strategies as st

    HYPOTHESIS_AVAILABLE = True
except ImportError:
    HYPOTHESIS_AVAILABLE = False

from ..types import EntityPath, EntityId
from ..hierplace import (
    PlacementRect,
    Rect,
    rects_intersect,
    merge_rects,
    translate_layout,
    normalize_layout,
    pack_at_origin,
    hierplace,
)

pytestmark = pytest.mark.skipif(
    not HYPOTHESIS_AVAILABLE, reason="hypothesis not installed"
)

# A4 sheet dimensions
SHEET_CENTER_X = 297_000_000 // 2
SHEET_CENTER_Y = 210_000_000 // 2


# ═══════════════════════════════════════════════════════════════════════════════
# Helpers
# ═══════════════════════════════════════════════════════════════════════════════


def make_id(name: str) -> EntityId:
    """Create an EntityId from a simple name."""
    return EntityId(path=EntityPath((name,)))


def make_rect(name: str, width: int, height: int) -> PlacementRect:
    """Create a PlacementRect with given dimensions."""
    return PlacementRect(entity_id=make_id(name), width=width, height=height)


def valid_ids(rects: list[PlacementRect]) -> set[EntityId]:
    """Get set of entity IDs for rects with positive dimensions."""
    return {r.entity_id for r in rects if r.width > 0 and r.height > 0}


def placed_rects(rects: list[PlacementRect], layout: dict) -> list[Rect]:
    """Reconstruct final rectangles from layout positions."""
    return [
        (layout[r.entity_id][0], layout[r.entity_id][1], r.width, r.height)
        for r in rects
        if r.entity_id in layout
    ]


def cluster_bbox(rects: list[Rect]) -> Rect:
    """Compute bounding box of all rectangles."""
    result = rects[0]
    for r in rects[1:]:
        result = merge_rects(result, r)
    return result


def cluster_center(bbox: Rect) -> tuple[int, int]:
    """Get center point of a bounding box."""
    return (bbox[0] + bbox[2] // 2, bbox[1] + bbox[3] // 2)


def assert_no_overlaps(rects: list[Rect]) -> None:
    """Assert no pair of rectangles overlap."""
    for i, a in enumerate(rects):
        for b in rects[i + 1 :]:
            assert not rects_intersect(a, b), f"Overlap: {a} and {b}"


# ═══════════════════════════════════════════════════════════════════════════════
# Strategies
# ═══════════════════════════════════════════════════════════════════════════════

dims = st.integers(min_value=500_000, max_value=30_000_000)  # 0.5mm to 30mm


@st.composite
def rect_list(draw, min_size: int = 1, max_size: int = 8):
    """Generate a list of PlacementRects with unique entity IDs."""
    n = draw(st.integers(min_value=min_size, max_value=max_size))
    return [
        PlacementRect(
            entity_id=make_id(f"item{i}"),
            width=draw(dims),
            height=draw(dims),
        )
        for i in range(n)
    ]


@st.composite
def existing_bbox(draw):
    """Generate an optional existing content bounding box."""
    if draw(st.booleans()):
        return None
    return (
        draw(st.integers(min_value=0, max_value=200_000_000)),
        draw(st.integers(min_value=0, max_value=150_000_000)),
        draw(dims),
        draw(dims),
    )


# ═══════════════════════════════════════════════════════════════════════════════
# Unit tests for primitive functions
# ═══════════════════════════════════════════════════════════════════════════════


class TestRectPrimitives:
    def test_overlapping_rects_intersect(self):
        assert rects_intersect((0, 0, 10, 10), (5, 5, 10, 10))

    def test_touching_edges_do_not_intersect(self):
        assert not rects_intersect((0, 0, 10, 10), (10, 0, 10, 10))

    def test_separate_rects_do_not_intersect(self):
        assert not rects_intersect((0, 0, 10, 10), (20, 20, 10, 10))

    def test_merge_overlapping(self):
        assert merge_rects((0, 0, 10, 10), (5, 5, 10, 10)) == (0, 0, 15, 15)

    def test_merge_disjoint(self):
        assert merge_rects((0, 0, 10, 10), (20, 0, 10, 10)) == (0, 0, 30, 10)


class TestTranslateLayout:
    def test_empty_layout(self):
        assert translate_layout({}, 10, 20) == {}

    def test_zero_offset(self):
        layout = {make_id("A"): (5, 10)}
        assert translate_layout(layout, 0, 0) == layout

    def test_positive_offset(self):
        layout = {make_id("A"): (0, 0), make_id("B"): (10, 5)}
        result = translate_layout(layout, 100, 200)
        assert result[make_id("A")] == (100, 200)
        assert result[make_id("B")] == (110, 205)

    def test_negative_offset(self):
        layout = {make_id("A"): (50, 60)}
        result = translate_layout(layout, -20, -30)
        assert result[make_id("A")] == (30, 30)


class TestNormalizeLayout:
    def test_empty_layout(self):
        assert normalize_layout({}) == {}

    def test_already_normalized(self):
        layout = {make_id("A"): (0, 0), make_id("B"): (10, 5)}
        assert normalize_layout(layout) == layout

    def test_negative_x(self):
        layout = {make_id("A"): (-5, 0), make_id("B"): (10, 5)}
        result = normalize_layout(layout)
        assert result[make_id("A")] == (0, 0)
        assert result[make_id("B")] == (15, 5)

    def test_negative_y(self):
        layout = {make_id("A"): (0, -10), make_id("B"): (5, 5)}
        result = normalize_layout(layout)
        assert result[make_id("A")] == (0, 0)
        assert result[make_id("B")] == (5, 15)

    def test_both_negative(self):
        layout = {make_id("A"): (-3, -7), make_id("B"): (2, -2)}
        result = normalize_layout(layout)
        # min_x = -3, min_y = -7
        assert result[make_id("A")] == (0, 0)
        assert result[make_id("B")] == (5, 5)

    def test_preserves_relative_positions(self):
        layout = {make_id("A"): (-10, -20), make_id("B"): (5, 10), make_id("C"): (0, 0)}
        result = normalize_layout(layout)
        # Check relative distances preserved
        orig_ab = (15, 30)  # B - A
        new_ab = (result[make_id("B")][0] - result[make_id("A")][0],
                  result[make_id("B")][1] - result[make_id("A")][1])
        assert orig_ab == new_ab


class TestHierplace:
    """Tests for the unified hierplace function."""

    def test_with_anchor_positions_right(self):
        """When anchor is provided, cluster is positioned to the right of it."""
        rects = [make_rect("A", 10_000_000, 10_000_000)]
        anchor = (0, 0, 50_000_000, 20_000_000)  # 50mm x 20mm

        result = hierplace(rects, anchor=anchor, margin=10_000_000)

        # Cluster should be right of anchor (50mm + 10mm margin)
        # Cluster center at 50 + 10 + 5 = 65mm, so top-left at 60mm
        assert result[make_id("A")][0] == 60_000_000
        # Vertically center-aligned: anchor center at 10mm, cluster center at 5mm
        # So cluster top at 10 - 5 = 5mm
        assert result[make_id("A")][1] == 5_000_000

    def test_without_anchor_centers_on_sheet(self):
        """When no anchor, cluster is centered on sheet."""
        rects = [make_rect("A", 10_000_000, 10_000_000)]

        result = hierplace(rects, anchor=None)

        # Sheet center is (148.5mm, 105mm) for A4
        # Cluster center should be at sheet center
        # For a 10mm x 10mm rect at (x, y), center is at (x+5, y+5)
        # So x = 148.5 - 5 = 143.5mm, y = 105 - 5 = 100mm
        assert result[make_id("A")] == (143_500_000, 100_000_000)

    def test_multiple_rects_with_anchor(self):
        """Multiple rects are packed then positioned right of anchor."""
        rects = [make_rect("A", 10_000_000, 10_000_000), make_rect("B", 5_000_000, 8_000_000)]
        anchor = (0, 0, 30_000_000, 30_000_000)

        result = hierplace(rects, anchor=anchor)

        # Both items should be positioned right of anchor
        for eid in result:
            assert result[eid][0] >= 30_000_000  # right of anchor


# ═══════════════════════════════════════════════════════════════════════════════
# Property tests
# ═══════════════════════════════════════════════════════════════════════════════


@pytest.mark.skipif(not HYPOTHESIS_AVAILABLE, reason="hypothesis not installed")
class TestHierPlaceInvariants:
    """Property-based tests for HierPlace invariants."""

    @given(rects=rect_list(min_size=1, max_size=10))
    @settings(max_examples=100)
    def test_all_valid_rects_placed(self, rects):
        """Every rectangle with positive dimensions gets placed."""
        assert set(pack_at_origin(rects).keys()) == valid_ids(rects)
        assert set(hierplace(rects).keys()) == valid_ids(rects)

    @given(rects=rect_list(min_size=1, max_size=10))
    @settings(max_examples=200)
    def test_pack_at_origin_is_normalized(self, rects):
        """pack_at_origin returns positions with cluster top-left at (0, 0).

        This is a critical invariant - callers depend on positions being
        non-negative and starting at origin. Violation causes overlapping
        components when positions are used as local offsets within a group.
        """
        layout = pack_at_origin(rects)
        if not layout:
            return

        min_x = min(x for x, _ in layout.values())
        min_y = min(y for _, y in layout.values())

        assert min_x == 0, f"Expected min_x=0, got {min_x}"
        assert min_y == 0, f"Expected min_y=0, got {min_y}"

        # Also verify all positions are non-negative
        for eid, (x, y) in layout.items():
            assert x >= 0, f"Negative x for {eid}: {x}"
            assert y >= 0, f"Negative y for {eid}: {y}"

    @given(rects=rect_list(min_size=2, max_size=10), existing=existing_bbox())
    @settings(max_examples=200)
    def test_no_overlaps(self, rects, existing):
        """Placed rectangles do not overlap."""
        assert_no_overlaps(placed_rects(rects, pack_at_origin(rects)))
        assert_no_overlaps(placed_rects(rects, hierplace(rects, anchor=existing)))

    @given(rects=rect_list())
    @settings(max_examples=100)
    def test_dimensions_preserved(self, rects):
        """Layout only changes position, not dimensions."""
        layout = hierplace(rects)
        rect_by_id = {r.entity_id: r for r in rects}

        for entity_id, (x, y) in layout.items():
            orig = rect_by_id[entity_id]
            for px, py, pw, ph in placed_rects(rects, layout):
                if px == x and py == y:
                    assert (pw, ph) == (orig.width, orig.height)
                    break

    @given(rects=rect_list(), existing=existing_bbox())
    @settings(max_examples=100)
    def test_deterministic(self, rects, existing):
        """Same inputs always produce same outputs."""
        assert pack_at_origin(rects) == pack_at_origin(rects)
        assert hierplace(rects, anchor=existing) == hierplace(rects, anchor=existing)

    @given(rects=rect_list(min_size=2, max_size=8), existing=existing_bbox())
    @settings(max_examples=100)
    def test_translation_only(self, rects, existing):
        """Phase 2 only translates; relative positions preserved."""
        origin = pack_at_origin(rects)
        final = hierplace(rects, anchor=existing)

        if len(origin) < 2:
            return

        ids = list(origin.keys())
        first = ids[0]
        dx = final[first][0] - origin[first][0]
        dy = final[first][1] - origin[first][1]

        for eid in ids[1:]:
            assert final[eid][0] == origin[eid][0] + dx
            assert final[eid][1] == origin[eid][1] + dy

    @given(rects=rect_list(min_size=1, max_size=5), existing=existing_bbox())
    @settings(max_examples=100)
    def test_cluster_right_of_existing(self, rects, existing):
        """When existing content exists, cluster is placed to its right."""
        assume(existing is not None)

        layout = hierplace(rects, anchor=existing)
        if not layout:
            return

        bbox = cluster_bbox(placed_rects(rects, layout))
        assert bbox[0] >= existing[0] + existing[2]  # left >= existing right

    @given(rects=rect_list(min_size=1, max_size=5))
    @settings(max_examples=50)
    def test_centered_when_no_existing(self, rects):
        """When no existing content, cluster is centered on A4 sheet."""
        layout = hierplace(rects)
        if not layout:
            return

        cx, cy = cluster_center(cluster_bbox(placed_rects(rects, layout)))
        assert (cx, cy) == (SHEET_CENTER_X, SHEET_CENTER_Y)


# ═══════════════════════════════════════════════════════════════════════════════
# Edge cases
# ═══════════════════════════════════════════════════════════════════════════════


class TestEdgeCases:
    def test_empty_input(self):
        assert pack_at_origin([]) == {}
        assert hierplace([]) == {}

    def test_single_rect_centered(self):
        rects = [make_rect("R1", 10_000_000, 5_000_000)]
        layout = hierplace(rects)

        x, y = layout[make_id("R1")]
        cx, cy = x + 5_000_000, y + 2_500_000
        assert (cx, cy) == (SHEET_CENTER_X, SHEET_CENTER_Y)

    def test_zero_size_filtered(self):
        rects = [
            make_rect("good", 10_000_000, 5_000_000),
            make_rect("bad", 0, 5_000_000),
        ]
        layout = pack_at_origin(rects)

        assert make_id("good") in layout
        assert make_id("bad") not in layout

    def test_two_rects_no_overlap(self):
        rects = [
            make_rect("R1", 10_000_000, 10_000_000),
            make_rect("R2", 10_000_000, 10_000_000),
        ]
        assert_no_overlaps(placed_rects(rects, pack_at_origin(rects)))
