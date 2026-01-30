"""
Pure geometry functions for HierPlace layout algorithm.

This module implements the HierPlace algorithm without any KiCad dependencies.
All functions are pure (no side effects) and operate on simple geometry types.

The core algorithm:
1. Sort items by area (largest first) for deterministic placement
2. Place first item at origin
3. For each subsequent item, try placement points (corners of placed items)
4. Choose the placement that minimizes: width + height + |width - height|
   (prefers more square layouts)

Placement strategies:
- pack_at_origin: Pack items into a cluster at (0,0) - for local/relative positioning
- hierplace: Pack items AND position relative to existing content (or sheet center)

The key insight: "existing content" is just a single anchor box. All placement
scenarios (root items, orphans, fragments) use the same algorithm with different
anchors.
"""

from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple

from .types import EntityId

# Bounding box: (left, top, width, height)
Rect = Tuple[int, int, int, int]

# A4 sheet dimensions in nanometers
DEFAULT_SHEET_WIDTH = 297_000_000
DEFAULT_SHEET_HEIGHT = 210_000_000
DEFAULT_MARGIN = 10_000_000


@dataclass
class PlacementRect:
    """A rectangle to be placed by HierPlace."""

    entity_id: EntityId
    width: int
    height: int
    x: int = 0  # top-left x
    y: int = 0  # top-left y

    @property
    def left(self) -> int:
        return self.x

    @property
    def top(self) -> int:
        return self.y

    @property
    def right(self) -> int:
        return self.x + self.width

    @property
    def bottom(self) -> int:
        return self.y + self.height

    @property
    def area(self) -> int:
        return self.width * self.height

    def as_rect(self) -> Rect:
        return (self.x, self.y, self.width, self.height)

    def move_to(self, x: int, y: int) -> "PlacementRect":
        """Return a new PlacementRect moved to (x, y)."""
        return PlacementRect(
            entity_id=self.entity_id,
            width=self.width,
            height=self.height,
            x=x,
            y=y,
        )


def rects_intersect(a: Rect, b: Rect) -> bool:
    """Check if two bounding boxes intersect.

    Boxes that are exactly touching (sharing an edge) are NOT considered intersecting.
    """
    a_left, a_top, a_width, a_height = a
    b_left, b_top, b_width, b_height = b

    a_right = a_left + a_width
    a_bottom = a_top + a_height
    b_right = b_left + b_width
    b_bottom = b_top + b_height

    return not (
        a_right <= b_left or b_right <= a_left or a_bottom <= b_top or b_bottom <= a_top
    )


def merge_rects(a: Rect, b: Rect) -> Rect:
    """Return the smallest bounding box containing both rectangles."""
    min_x = min(a[0], b[0])
    min_y = min(a[1], b[1])
    max_x = max(a[0] + a[2], b[0] + b[2])
    max_y = max(a[1] + a[3], b[1] + b[3])

    return (min_x, min_y, max_x - min_x, max_y - min_y)


def compute_cluster_bbox(rects: List[PlacementRect]) -> Optional[Rect]:
    """Compute the bounding box of all placed rectangles."""
    if not rects:
        return None

    result = rects[0].as_rect()
    for r in rects[1:]:
        result = merge_rects(result, r.as_rect())
    return result


def _size_metric(bbox: Rect) -> int:
    """Compute the size metric for a bounding box.

    Prefers more square layouts by penalizing aspect ratio difference.
    """
    width, height = bbox[2], bbox[3]
    return width + height + abs(width - height)


def _add_corners(placement_pts: List[Tuple[int, int]], r: PlacementRect) -> None:
    """Add a placed rectangle's corners as candidate placement points."""
    placement_pts.extend(
        [
            (r.left, r.top),  # Top-left: enables placing above
            (r.right, r.bottom),  # Bottom-right: enables placing to the right
        ]
    )


def translate_layout(
    layout: Dict[EntityId, Tuple[int, int]],
    dx: int,
    dy: int,
) -> Dict[EntityId, Tuple[int, int]]:
    """Translate all positions in a layout by (dx, dy)."""
    if not layout or (dx == 0 and dy == 0):
        return dict(layout) if layout else {}
    return {eid: (x + dx, y + dy) for eid, (x, y) in layout.items()}


def normalize_layout(
    layout: Dict[EntityId, Tuple[int, int]]
) -> Dict[EntityId, Tuple[int, int]]:
    """Normalize layout so cluster bbox top-left is at (0, 0)."""
    if not layout:
        return {}
    min_x = min(x for x, _ in layout.values())
    min_y = min(y for _, y in layout.values())
    return translate_layout(layout, -min_x, -min_y)


def pack_at_origin(rects: List[PlacementRect]) -> Dict[EntityId, Tuple[int, int]]:
    """Pack rectangles at origin using corner-based placement.

    This is Phase 1 of HierPlace: pack items compactly using a greedy
    algorithm that minimizes the size metric.

    The returned positions are normalized so the cluster's top-left corner
    is at (0, 0). This means all x and y values are non-negative, and at
    least one position has x == 0 and at least one has y == 0.

    Args:
        rects: List of PlacementRect with width/height set (x/y ignored)

    Returns:
        Dict mapping entity_id -> (x, y) for top-left corner positions,
        normalized so cluster bbox starts at origin
    """
    # Filter out zero-size rectangles
    valid_rects = [r for r in rects if r.width > 0 and r.height > 0]
    if not valid_rects:
        return {}

    # Sort by area (largest first), then by entity_id for determinism
    valid_rects = sorted(valid_rects, key=lambda r: (-r.area, str(r.entity_id.path)))

    # Track placement points (corners of placed items)
    # These are "bottom-left" targets for new items
    placement_pts: List[Tuple[int, int]] = []
    placed: List[PlacementRect] = []
    result: Dict[EntityId, Tuple[int, int]] = {}

    for i, rect in enumerate(valid_rects):
        if i == 0:
            # First item: place at origin
            placed_rect = rect.move_to(0, 0)
            placed.append(placed_rect)
            result[rect.entity_id] = (0, 0)
            _add_corners(placement_pts, placed_rect)
        else:
            # Find best placement point
            best_pos: Optional[Tuple[int, int]] = None
            best_size = float("inf")

            for pt_x, pt_y in placement_pts:
                # Place item's bottom-left at this point
                # So top-left is at (pt_x, pt_y - height)
                candidate = rect.move_to(pt_x, pt_y - rect.height)

                # Check for collisions
                collision = False
                for p in placed:
                    if rects_intersect(candidate.as_rect(), p.as_rect()):
                        collision = True
                        break

                if not collision:
                    # Compute merged bbox
                    merged = candidate.as_rect()
                    for p in placed:
                        merged = merge_rects(merged, p.as_rect())

                    size = _size_metric(merged)
                    if size < best_size:
                        best_size = size
                        best_pos = (candidate.x, candidate.y)

            if best_pos:
                placed_rect = rect.move_to(best_pos[0], best_pos[1])
                placed.append(placed_rect)
                result[rect.entity_id] = best_pos
                _add_corners(placement_pts, placed_rect)
            else:
                # Fallback: place to the right of all placed items
                if placed:
                    max_right = max(p.right for p in placed)
                    margin = 5_000_000  # 5mm
                    fallback_x = max_right + margin
                    fallback_y = 0
                    placed_rect = rect.move_to(fallback_x, fallback_y)
                    placed.append(placed_rect)
                    result[rect.entity_id] = (fallback_x, fallback_y)
                    _add_corners(placement_pts, placed_rect)

    # Normalize so cluster bbox top-left is at (0, 0)
    return normalize_layout(result)


def hierplace(
    rects: List[PlacementRect],
    anchor: Optional[Rect] = None,
    margin: int = DEFAULT_MARGIN,
    sheet_width: int = DEFAULT_SHEET_WIDTH,
    sheet_height: int = DEFAULT_SHEET_HEIGHT,
) -> Dict[EntityId, Tuple[int, int]]:
    """Pack rectangles and position relative to an anchor (or sheet center).

    This is THE unified placement algorithm. All placement scenarios use this:
    - Root items: anchor = existing board content bbox
    - Orphans: anchor = fragment bbox
    - No existing content: anchor = None (centers on sheet)

    The anchor is treated as a single immovable box. The packed cluster is
    positioned to the right of it, vertically center-aligned.

    Args:
        rects: Rectangles to pack and position
        anchor: Existing content bbox to position relative to (or None for sheet center)
        margin: Gap between anchor and new cluster
        sheet_width: Sheet width for centering (default A4)
        sheet_height: Sheet height for centering (default A4)

    Returns:
        Dict mapping entity_id -> (x, y) for top-left corner positions
    """
    layout = pack_at_origin(rects)
    if not layout:
        return {}

    # Compute cluster bbox from packed layout
    placed_rects = [
        r.move_to(*layout[r.entity_id]) for r in rects if r.entity_id in layout
    ]
    cluster = compute_cluster_bbox(placed_rects)
    if not cluster:
        return layout

    cluster_center_x = cluster[0] + cluster[2] // 2
    cluster_center_y = cluster[1] + cluster[3] // 2

    if anchor:
        # Position right of anchor, vertically center-aligned
        anchor_right = anchor[0] + anchor[2]
        anchor_center_y = anchor[1] + anchor[3] // 2
        target_x = anchor_right + margin + cluster[2] // 2
        target_y = anchor_center_y
    else:
        # Center on sheet
        target_x = sheet_width // 2
        target_y = sheet_height // 2

    return translate_layout(
        layout, target_x - cluster_center_x, target_y - cluster_center_y
    )



