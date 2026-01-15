"""
Pure geometry functions for HierPlace layout algorithm.

This module implements the HierPlace algorithm without any KiCad dependencies.
All functions are pure (no side effects) and operate on simple geometry types.

The algorithm:
1. Sort items by area (largest first) for deterministic placement
2. Place first item at origin
3. For each subsequent item, try placement points (corners of placed items)
4. Choose the placement that minimizes: width + height + |width - height|
   (prefers more square layouts)
5. After packing at origin, translate the cluster to its final position
   (centered on sheet or right of existing content)
"""

from dataclasses import dataclass
from typing import Dict, List, Optional, Tuple

from .types import EntityId

# Bounding box: (left, top, width, height)
Rect = Tuple[int, int, int, int]


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


def pack_at_origin(rects: List[PlacementRect]) -> Dict[EntityId, Tuple[int, int]]:
    """Pack rectangles at origin using corner-based placement.

    This is Phase 1 of HierPlace: pack items starting at (0,0) using
    a greedy algorithm that minimizes the size metric.

    Args:
        rects: List of PlacementRect with width/height set (x/y ignored)

    Returns:
        Dict mapping entity_id -> (x, y) for top-left corner positions
    """
    # Filter out zero-size rectangles
    valid_rects = [r for r in rects if r.width > 0 and r.height > 0]
    if not valid_rects:
        return {}

    # Sort by area (largest first), then by entity_id for determinism
    valid_rects = sorted(
        valid_rects, key=lambda r: (-r.area, str(r.entity_id.path))
    )

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

            # Add corners as placement points
            placement_pts.extend([
                (placed_rect.left, placed_rect.top),  # Top-left
                (placed_rect.right, placed_rect.bottom),  # Bottom-right
            ])
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

                # Add new placement points
                placement_pts.extend([
                    (placed_rect.left, placed_rect.top),
                    (placed_rect.right, placed_rect.bottom),
                ])
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

    return result


def hierplace_layout(
    rects: List[PlacementRect],
    existing_bbox: Optional[Rect],
    sheet_width: int = 297_000_000,  # A4 width in nm
    sheet_height: int = 210_000_000,  # A4 height in nm
    margin: int = 10_000_000,  # 10mm margin from existing content
) -> Dict[EntityId, Tuple[int, int]]:
    """Compute final positions for rectangles using HierPlace algorithm.

    This is the main entry point for the pure HierPlace algorithm.

    Phase 1: Pack at origin
    Phase 2: Translate cluster to final position
            - If existing_bbox: right of existing, center-aligned vertically
            - Otherwise: centered on sheet

    Args:
        rects: List of PlacementRect with width/height
        existing_bbox: Bounding box of existing content (or None)
        sheet_width: Sheet width in nanometers (default A4)
        sheet_height: Sheet height in nanometers (default A4)
        margin: Margin between existing content and new cluster

    Returns:
        Dict mapping entity_id -> (x, y) for top-left corner positions
    """
    # Phase 1: Pack at origin
    origin_layout = pack_at_origin(rects)

    if not origin_layout:
        return {}

    # Reconstruct placed rectangles for cluster bbox calculation
    placed_rects = []
    for r in rects:
        if r.entity_id in origin_layout:
            pos = origin_layout[r.entity_id]
            placed_rects.append(r.move_to(pos[0], pos[1]))

    cluster_bbox = compute_cluster_bbox(placed_rects)
    if not cluster_bbox:
        return origin_layout

    # Compute cluster center
    cluster_center_x = cluster_bbox[0] + cluster_bbox[2] // 2
    cluster_center_y = cluster_bbox[1] + cluster_bbox[3] // 2

    # Phase 2: Compute target position
    if existing_bbox:
        # Position to the right of existing content, vertically center-aligned
        existing_right = existing_bbox[0] + existing_bbox[2]
        existing_center_y = existing_bbox[1] + existing_bbox[3] // 2

        target_x = existing_right + margin + cluster_bbox[2] // 2
        target_y = existing_center_y
    else:
        # Center on sheet
        target_x = sheet_width // 2
        target_y = sheet_height // 2

    # Compute offset
    offset_x = target_x - cluster_center_x
    offset_y = target_y - cluster_center_y

    # Apply offset to all positions
    final_layout = {}
    for entity_id, (x, y) in origin_layout.items():
        final_layout[entity_id] = (x + offset_x, y + offset_y)

    return final_layout
