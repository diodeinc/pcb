"""
Pure geometry functions for HierPlace layout algorithm.

Algorithm:
1. Sort items by area (largest first) for deterministic placement
2. For each item, try placement at corners of already-placed items
3. Choose placement minimizing: width + height + |width - height| (prefers square)
4. Optionally normalize so cluster top-left is at (0, 0)
"""

from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple

from .types import EntityId

# Bounding box: (left, top, width, height)
Rect = Tuple[int, int, int, int]

# A4 sheet dimensions in nanometers
DEFAULT_SHEET_WIDTH = 297_000_000
DEFAULT_SHEET_HEIGHT = 210_000_000

# Depth-based padding constants
BASE_PAD = 2_000_000  # 2mm per depth level
MAX_PAD = 10_000_000  # cap at 10mm


def pad_for_depth(depth: int) -> int:
    """Compute padding from hierarchy depth. Depth 0 = tight, scales linearly."""
    return 0 if depth <= 0 else min(MAX_PAD, BASE_PAD * depth)


@dataclass
class PlacementRect:
    """A rectangle to be placed by HierPlace."""

    entity_id: EntityId
    width: int
    height: int
    x: int = 0
    y: int = 0

    def move_to(self, x: int, y: int) -> "PlacementRect":
        """Return a new PlacementRect at (x, y)."""
        return PlacementRect(self.entity_id, self.width, self.height, x, y)


def rects_intersect(a: Rect, b: Rect) -> bool:
    """Check if two bounding boxes intersect (touching edges don't count)."""
    return not (
        a[0] + a[2] <= b[0]
        or b[0] + b[2] <= a[0]
        or a[1] + a[3] <= b[1]
        or b[1] + b[3] <= a[1]
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
    result = (rects[0].x, rects[0].y, rects[0].width, rects[0].height)
    for r in rects[1:]:
        result = merge_rects(result, (r.x, r.y, r.width, r.height))
    return result


def pack(
    rects: List[PlacementRect],
    gap: int = 0,
    anchor: Optional[Rect] = None,
    sheet_width: int = DEFAULT_SHEET_WIDTH,
    sheet_height: int = DEFAULT_SHEET_HEIGHT,
    normalize: bool = False,
) -> Dict[EntityId, Tuple[int, int]]:
    """Pack rectangles using corner-based greedy placement.

    Args:
        rects: Rectangles to place
        gap: Spacing between items
        anchor: Existing content bbox (pack around it)
        sheet_width/height: Sheet size for centering when no anchor
        normalize: If True, translate result so top-left is at (0, 0)

    Returns:
        Dict mapping entity_id -> (x, y) top-left positions
    """
    valid = [r for r in rects if r.width > 0 and r.height > 0]
    if not valid:
        return {}
    valid = sorted(valid, key=lambda r: (-r.width * r.height, str(r.entity_id.path)))

    half_gap = gap // 2
    placed: List[Rect] = []  # (x, y, w, h) tuples
    pts: List[Tuple[int, int]] = []  # candidate bottom-left points
    result: Dict[EntityId, Tuple[int, int]] = {}

    # Initialize placement points and pre-placed rects
    if anchor:
        placed.append(anchor)
        pts.extend(
            [
                (anchor[0], anchor[1] - gap),
                (anchor[0] + anchor[2] + gap, anchor[1] + anchor[3]),
            ]
        )
    else:
        # First item centers on sheet
        first = valid[0]
        start_x = (sheet_width - first.width) // 2
        start_y = (sheet_height - first.height) // 2
        pts.append((start_x, start_y + first.height))

    for rect in valid:
        best_pos: Optional[Tuple[int, int]] = None
        best_size = float("inf")

        for pt_x, pt_y in pts:
            # Candidate top-left from bottom-left point
            cx, cy = pt_x, pt_y - rect.height
            cand = (
                cx - half_gap,
                cy - half_gap,
                rect.width + 2 * half_gap,
                rect.height + 2 * half_gap,
            )

            # Check collisions
            if any(
                rects_intersect(
                    cand,
                    (
                        p[0] - half_gap,
                        p[1] - half_gap,
                        p[2] + 2 * half_gap,
                        p[3] + 2 * half_gap,
                    ),
                )
                for p in placed
            ):
                continue

            # Compute merged bbox size metric
            merged = cand
            for p in placed:
                merged = merge_rects(
                    merged,
                    (
                        p[0] - half_gap,
                        p[1] - half_gap,
                        p[2] + 2 * half_gap,
                        p[3] + 2 * half_gap,
                    ),
                )
            size = merged[2] + merged[3] + abs(merged[2] - merged[3])

            if size < best_size:
                best_size = size
                best_pos = (cx, cy)

        assert best_pos is not None, "No valid placement found"
        placed.append((best_pos[0], best_pos[1], rect.width, rect.height))
        result[rect.entity_id] = best_pos
        # Add corners for next iteration
        pts.extend(
            [
                (best_pos[0], best_pos[1] - gap),  # top-left corner
                (
                    best_pos[0] + rect.width + gap,
                    best_pos[1] + rect.height,
                ),  # bottom-right corner
            ]
        )

    if normalize and result:
        min_x = min(x for x, _ in result.values())
        min_y = min(y for _, y in result.values())
        result = {eid: (x - min_x, y - min_y) for eid, (x, y) in result.items()}

    return result
