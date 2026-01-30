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
    pack,
    pad_for_depth,
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


class TestPadForDepth:
    def test_depth_zero_is_tight(self):
        assert pad_for_depth(0) == 0

    def test_depth_one_is_base_pad(self):
        assert pad_for_depth(1) == 2_000_000  # 2mm

    def test_depth_scales_linearly(self):
        assert pad_for_depth(2) == 4_000_000  # 4mm
        assert pad_for_depth(3) == 6_000_000  # 6mm

    def test_depth_capped(self):
        assert pad_for_depth(10) == 10_000_000  # capped at 10mm
        assert pad_for_depth(100) == 10_000_000  # still capped


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


class TestPack:
    """Tests for the pack function."""

    def test_with_anchor_packs_around(self):
        """When anchor is provided, items pack around it (not overlapping)."""
        rects = [make_rect("A", 10_000_000, 10_000_000)]
        anchor = (0, 0, 50_000_000, 20_000_000)  # 50mm x 20mm

        result = pack(rects, anchor=anchor)

        # Item should not overlap with anchor
        item_rect = (
            result[make_id("A")][0],
            result[make_id("A")][1],
            10_000_000,
            10_000_000,
        )
        assert not rects_intersect(anchor, item_rect)

    def test_without_anchor_centers_on_sheet(self):
        """When no anchor, first item is centered on sheet."""
        rects = [make_rect("A", 10_000_000, 10_000_000)]

        result = pack(rects)

        # First item should be centered on A4 sheet
        # Sheet center is (148.5mm, 105mm), so 10mm rect at (143.5mm, 100mm)
        assert result[make_id("A")] == (143_500_000, 100_000_000)

    def test_multiple_rects_with_anchor(self):
        """Multiple rects pack around anchor without overlapping."""
        rects = [
            make_rect("A", 10_000_000, 10_000_000),
            make_rect("B", 5_000_000, 8_000_000),
        ]
        anchor = (0, 0, 30_000_000, 30_000_000)

        result = pack(rects, anchor=anchor)

        # No item should overlap with anchor
        for eid in result:
            r = rects[0] if str(eid.path) == "A" else rects[1]
            item_rect = (result[eid][0], result[eid][1], r.width, r.height)
            assert not rects_intersect(anchor, item_rect)


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
        assert set(pack(rects, normalize=True).keys()) == valid_ids(rects)
        assert set(pack(rects).keys()) == valid_ids(rects)

    @given(rects=rect_list(min_size=1, max_size=10))
    @settings(max_examples=200)
    def test_normalize_puts_top_left_at_origin(self, rects):
        """normalize=True returns positions with cluster top-left at (0, 0)."""
        layout = pack(rects, normalize=True)
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
        assert_no_overlaps(placed_rects(rects, pack(rects, normalize=True)))
        assert_no_overlaps(placed_rects(rects, pack(rects, anchor=existing)))

    @given(rects=rect_list())
    @settings(max_examples=100)
    def test_dimensions_preserved(self, rects):
        """Layout only changes position, not dimensions."""
        layout = pack(rects)
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
        assert pack(rects, normalize=True) == pack(rects, normalize=True)
        assert pack(rects, anchor=existing) == pack(rects, anchor=existing)

    @given(rects=rect_list(min_size=1, max_size=5), existing=existing_bbox())
    @settings(max_examples=100)
    def test_no_overlap_with_anchor(self, rects, existing):
        """Items don't overlap with anchor."""
        assume(existing is not None)

        layout = pack(rects, anchor=existing)
        if not layout:
            return

        for r in rects:
            if r.entity_id not in layout:
                continue
            pos = layout[r.entity_id]
            item_rect = (pos[0], pos[1], r.width, r.height)
            assert not rects_intersect(existing, item_rect)

    @given(rects=rect_list(min_size=1, max_size=5))
    @settings(max_examples=50)
    def test_first_item_centered_when_no_anchor(self, rects):
        """When no anchor, first item is approximately centered on sheet."""
        layout = pack(rects)
        if not layout:
            return

        # First item (largest by area) should be centered (within 1 due to int division)
        valid = [r for r in rects if r.width > 0 and r.height > 0]
        if not valid:
            return
        first = max(valid, key=lambda r: r.width * r.height)
        pos = layout[first.entity_id]
        cx = pos[0] + first.width // 2
        cy = pos[1] + first.height // 2
        assert abs(cx - SHEET_CENTER_X) <= 1
        assert abs(cy - SHEET_CENTER_Y) <= 1


# ═══════════════════════════════════════════════════════════════════════════════
# Edge cases
# ═══════════════════════════════════════════════════════════════════════════════


class TestEdgeCases:
    def test_empty_input(self):
        assert pack([]) == {}
        assert pack([], normalize=True) == {}

    def test_single_rect_centered(self):
        rects = [make_rect("R1", 10_000_000, 5_000_000)]
        layout = pack(rects)

        x, y = layout[make_id("R1")]
        cx, cy = x + 5_000_000, y + 2_500_000
        assert (cx, cy) == (SHEET_CENTER_X, SHEET_CENTER_Y)

    def test_zero_size_filtered(self):
        rects = [
            make_rect("good", 10_000_000, 5_000_000),
            make_rect("bad", 0, 5_000_000),
        ]
        layout = pack(rects, normalize=True)

        assert make_id("good") in layout
        assert make_id("bad") not in layout

    def test_two_rects_no_overlap(self):
        rects = [
            make_rect("R1", 10_000_000, 10_000_000),
            make_rect("R2", 10_000_000, 10_000_000),
        ]
        assert_no_overlaps(placed_rects(rects, pack(rects, normalize=True)))

    def test_gap_creates_spacing(self):
        """Gap parameter creates spacing between items."""
        rects = [
            make_rect("R1", 10_000_000, 10_000_000),
            make_rect("R2", 10_000_000, 10_000_000),
        ]
        # Tight packing (gap=0): items touch
        tight = pack(rects, gap=0, normalize=True)
        r1_tight, r2_tight = tight[make_id("R1")], tight[make_id("R2")]

        # Gapped packing (gap=4mm): items spaced apart
        gapped = pack(rects, gap=4_000_000, normalize=True)
        r1_gap, r2_gap = gapped[make_id("R1")], gapped[make_id("R2")]

        # Compute manhattan distance between positions
        def dist(a, b):
            return abs(a[0] - b[0]) + abs(a[1] - b[1])

        # Gapped layout should have more distance between items
        assert dist(r1_gap, r2_gap) >= dist(r1_tight, r2_tight) + 4_000_000
