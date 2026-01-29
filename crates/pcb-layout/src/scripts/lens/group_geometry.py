"""
Group geometry helpers for layout fragments.
"""

from pathlib import Path
from typing import Any, Optional, Tuple
import logging

logger = logging.getLogger("pcb.lens.geometry")


def read_fragment_bbox(
    layout_path: str,
    board_dir: Path,
    pcbnew: Any,
) -> Tuple[Optional[Tuple[int, int]], Optional[Tuple[int, int]]]:
    """Read a layout fragment's bbox from its layout.kicad_pcb file.

    Returns a tuple of (bbox_size, bbox_anchor_offset), where:
    - bbox_size = (width, height)
    - bbox_anchor_offset = (left, top)

    Values are in KiCad internal units (nanometers). Returns (None, None)
    if the fragment can't be loaded or bbox can't be computed.
    """
    path = Path(layout_path)
    if not path.is_absolute():
        path = board_dir / path

    layout_file = path / "layout.kicad_pcb"
    if not layout_file.exists():
        return None, None

    fragment_board = pcbnew.LoadBoard(str(layout_file))
    items = list(fragment_board.GetFootprints())
    items.extend(fragment_board.GetTracks())
    items.extend(fragment_board.Zones())
    items.extend(fragment_board.GetDrawings())
    if not items:
        return None, None

    min_x = min_y = float("inf")
    max_x = max_y = float("-inf")

    lset = pcbnew.LSET.AllLayersMask()
    lset.RemoveLayer(pcbnew.F_Fab)
    lset.RemoveLayer(pcbnew.B_Fab)

    for item in items:
        if hasattr(pcbnew, "FOOTPRINT") and isinstance(item, pcbnew.FOOTPRINT):
            bbox = item.GetLayerBoundingBox(lset)
        elif hasattr(item, "GetBoundingBox"):
            bbox = item.GetBoundingBox()
        else:
            bbox = None

        if not bbox:
            continue

        min_x = min(min_x, bbox.GetLeft())
        min_y = min(min_y, bbox.GetTop())
        max_x = max(max_x, bbox.GetRight())
        max_y = max(max_y, bbox.GetBottom())

    if min_x == float("inf"):
        return None, None

    size = (int(max_x - min_x), int(max_y - min_y))
    anchor_offset = (int(min_x), int(min_y))
    return size, anchor_offset
