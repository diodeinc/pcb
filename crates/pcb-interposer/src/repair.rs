//! Rip-up of routed vias that break the board's clearance rules, run on
//! the board via KiCad's python.
//!
//! FreeRouting is handed the high-voltage clearance matrix (see
//! [`crate::dsn`]) but does not always hold it for vias, and reports the
//! board clean regardless; it also keeps only its default clearance to
//! the board edge. [`violations`] checks every via the way the board's
//! rules do: the exposed high-voltage clearance between a via and any
//! pad of another net when either carries high voltage, and the copper-
//! to-edge clearance. [`rip_up`] removes each offending via's net (a GND
//! via alone: stitching replaces it) and rings each pad it came too close
//! to with a no-via guard, so the next routing pass has to route around
//! it; [`crate::guard::remove`] takes those guards out with the rest.

use std::path::Path;

use anyhow::{Context, Result};

use crate::bed::Bed;
use crate::emit::HV_CLASSES;
use crate::guard::GUARD_NAME;

const SCRIPT: &str = r#"
import math
import sys

import pcbnew

board_path, name, mode, exposed, hot_names, report_path = sys.argv[1:7]
board = pcbnew.LoadBoard(board_path)
gnd = board.FindNet("GND").GetNetCode()
EXPOSED = pcbnew.FromMM(float(exposed)) if exposed else 0
HOT = set(hot_names.split(",")) if exposed else set()
EDGE = pcbnew.FromMM(0.4)
# A guard ring this much wider than the rule, so the rerouted via is not
# set right at its edge again.
MARGIN = pcbnew.FromMM(0.1)
EPS = pcbnew.FromMM(0.001)
SIDES = 24


def hot(item):
    return bool(HOT & set(item.GetNetClassName().split(",")))


def radius_of(pad):
    size = pad.GetSize()
    return max(size.x, size.y) // 2


pads = [
    pad
    for footprint in board.GetFootprints()
    for pad in footprint.Pads()
    if pad.GetAttribute() != pcbnew.PAD_ATTRIB_NPTH
]
vias = [item for item in board.GetTracks() if item.GetClass() == "PCB_VIA"]
via_radius = max((via.GetWidth(pcbnew.F_Cu) // 2 for via in vias), default=0)
# Where a via of each radius may sit, inset from the outline by its own
# radius: one inset for all would flag a small via by a big one's size.
insides = {}


def inside_for(radius):
    if radius not in insides:
        inside = pcbnew.SHAPE_POLY_SET()
        board.GetBoardPolygonOutlines(inside, False)
        inside.Deflate(EDGE + radius - EPS, pcbnew.CORNER_STRATEGY_ROUND_ALL_CORNERS, pcbnew.FromMM(0.005))
        insides[radius] = inside
    return insides[radius]


bad_vias, bad_pads = [], []
for via in vias:
    position = via.GetPosition()
    radius = via.GetWidth(pcbnew.F_Cu) // 2
    bad = not inside_for(radius).Collide(position)
    for pad in pads:
        if not HOT or pad.GetNetCode() == via.GetNetCode() or not (hot(via) or hot(pad)):
            continue
        if (pad.GetPosition() - position).EuclideanNorm() > radius_of(pad) + radius + EXPOSED:
            continue
        for layer in (pcbnew.F_Cu, pcbnew.B_Cu):
            if pad.IsOnLayer(layer) and pad.GetEffectiveShape(layer).Collide(
                via.GetEffectiveShape(layer), EXPOSED
            ):
                bad = True
                # Kept as plain values: pcbnew's wrappers go stale once
                # the board is edited.
                spot = (pad.GetPosition().x, pad.GetPosition().y, radius_of(pad))
                if spot not in bad_pads:
                    bad_pads.append(spot)
                break
    if bad:
        bad_vias.append(via)

if mode == "rip" and bad_vias:
    for x, y, pad_radius in bad_pads:
        center, radius = pcbnew.VECTOR2I(x, y), pad_radius + EXPOSED + via_radius + MARGIN
        for layer in (pcbnew.F_Cu, pcbnew.B_Cu):
            zone = pcbnew.ZONE(board)
            zone.SetIsRuleArea(True)
            zone.SetZoneName(name)
            zone.SetLayer(layer)
            zone.SetDoNotAllowTracks(False)
            zone.SetDoNotAllowVias(True)
            zone.SetDoNotAllowPads(False)
            zone.SetDoNotAllowFootprints(False)
            zone.SetDoNotAllowZoneFills(False)
            outline = zone.Outline()
            outline.NewOutline()
            for k in range(SIDES):
                angle = 2 * math.pi * k / SIDES
                outline.Append(center.x + round(radius * math.cos(angle)), center.y + round(radius * math.sin(angle)))
            board.Add(zone)
    # Removal last: pcbnew's wrappers go stale once items leave the board.
    nets = {via.GetNetCode() for via in bad_vias if via.GetNetCode() != gnd}
    doomed = [item for item in board.GetTracks() if item.GetNetCode() in nets or item in bad_vias]
    for item in doomed:
        board.Remove(item)
    if not pcbnew.SaveBoard(board_path, board):
        sys.exit("Failed to save ripped-up board")

with open(report_path, "w") as out:
    out.write(f"{len(bad_vias)}\n")
"#;

/// How many vias of the board break the bed's high-voltage clearance at
/// a pad or the copper-to-edge clearance.
pub fn violations(board_path: &Path, bed: Bed) -> Result<usize> {
    run(board_path, bed, "check").context("check via clearances")
}

/// Rip up the nets of every via [`violations`] counts and guard the pads
/// they came too close to; returns the count.
pub fn rip_up(board_path: &Path, bed: Bed) -> Result<usize> {
    run(board_path, bed, "rip").context("rip up vias breaking clearance")
}

fn run(board_path: &Path, bed: Bed, mode: &str) -> Result<usize> {
    let report = tempfile::NamedTempFile::new().context("create via report file")?;
    let exposed = bed
        .hv_clearance()
        .map(|hv| hv.exposed.to_string())
        .unwrap_or_default();
    pcb_kicad::PythonScriptBuilder::new(SCRIPT)
        .arg(board_path.to_string_lossy())
        .arg(GUARD_NAME)
        .arg(mode)
        .arg(exposed)
        .arg(HV_CLASSES.join(","))
        .arg(report.path().to_string_lossy())
        .run()?;
    std::fs::read_to_string(report.path())
        .context("read via report")?
        .trim()
        .parse()
        .context("malformed via report")
}
