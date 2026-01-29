"""
Footprint geometry helpers for source-authoritative layout data.
"""

from typing import Any, Dict, Optional, Tuple
import logging

logger = logging.getLogger("pcb.lens.geometry")


def read_footprint_bbox(
    fpid: str,
    footprint_lib_map: Dict[str, str],
    pcbnew: Any,
) -> Tuple[Optional[Tuple[int, int]], Optional[Tuple[int, int]]]:
    """Read the footprint bbox from its library definition.

    Returns a tuple of (bbox_size, bbox_anchor_offset), where:
    - bbox_size = (width, height)
    - bbox_anchor_offset = (left, top)

    Values are in KiCad internal units (nanometers). Returns (None, None)
    if the footprint can't be loaded or bbox can't be computed.
    """
    if ":" not in fpid:
        logger.warning("Invalid FPID format for bbox read: %s", fpid)
        return None, None

    fp_lib, fp_name = fpid.split(":", 1)
    lib_uri = footprint_lib_map.get(fp_lib)
    if not lib_uri:
        logger.warning("Unknown footprint library '%s' for %s", fp_lib, fpid)
        return None, None

    lib_uri = lib_uri.replace("\\\\?\\", "")
    fp = pcbnew.FootprintLoad(lib_uri, fp_name)
    if fp is None:
        logger.warning(
            "Footprint '%s' not found in library '%s'", fp_name, fp_lib
        )
        return None, None

    lset = pcbnew.LSET.AllLayersMask()
    lset.RemoveLayer(pcbnew.F_Fab)
    lset.RemoveLayer(pcbnew.B_Fab)
    bbox = fp.GetLayerBoundingBox(lset)
    if not bbox:
        return None, None

    size = (bbox.GetWidth(), bbox.GetHeight())
    anchor_offset = (bbox.GetLeft(), bbox.GetTop())
    return size, anchor_offset
