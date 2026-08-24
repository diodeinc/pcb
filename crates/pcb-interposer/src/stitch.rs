//! Post-route GND stitching, run on the routed board via KiCad's python.
//!
//! One placement rule, borrowed from KiCad master's via-stitch generator:
//! inflate every copper item — routed tracks, pads, signal and GND alike —
//! by (via radius + clearance) and walk the merged envelope's contours,
//! dropping a via every pitch of arc length. That single rule yields a
//! guard fence along every trace, a ring around every pogo and land (so
//! every GND pad gets its nearby via), and it is what keeps GND whole:
//! any pour fragment a routed trace slices off carries fence vias that
//! bridge it through the opposite face. A second contour — the board
//! outline deflated — adds the perimeter ring. Candidates are accepted
//! against one validity mask (board interior minus inflated foreign
//! copper and holes) with a minimum via-to-via spacing.

use std::path::Path;

use anyhow::{Context, Result};

const STITCH_SCRIPT: &str = r#"
import math
import sys

import pcbnew

board_path, report_path = sys.argv[1], sys.argv[2]

ERR = pcbnew.FromMM(0.01)
VIA_WIDTH = pcbnew.FromMM(0.6)
VIA_DRILL = pcbnew.FromMM(0.3)
VIA_RADIUS = VIA_WIDTH // 2
# The board's min hole-to-copper constraint; NPTH pads carry no netclass
# clearance, so this is what actually bounds a via near a tooling hole.
HOLE_CLEARANCE = pcbnew.FromMM(0.25)
# The stitch via's own Default-netclass clearance: pair clearance is the
# max of both items', so it floors every margin (USB tracks carry less).
VIA_CLEARANCE = pcbnew.FromMM(0.2)
# Envelope candidates must sit outside the obstacle mask, whose items
# are polygonized with outward error; walk a little further out.
ENVELOPE_EXTRA = pcbnew.FromMM(0.02)
PITCH = pcbnew.FromMM(2.54)
MIN_SEP = pcbnew.FromMM(1.8)
# Copper-to-edge clearance + via radius.
EDGE_KEEPIN = pcbnew.FromMM(0.65)
RING_INSET = pcbnew.FromMM(1.0)

board = pcbnew.LoadBoard(board_path)
gnd = board.FindNet("GND").GetNetCode()

outline = pcbnew.SHAPE_POLY_SET()
board.GetBoardPolygonOutlines(outline, False)

obstacles = pcbnew.SHAPE_POLY_SET()
envelope = pcbnew.SHAPE_POLY_SET()


def margin(item, layer):
    clearance = max(item.GetOwnClearance(layer), VIA_CLEARANCE)
    if hasattr(item, "GetDrillSize") and item.GetDrillSize().x > 0:
        clearance = max(clearance, HOLE_CLEARANCE)
    return VIA_RADIUS + clearance + ERR


def add(item, layer, poly, extra=0):
    item.TransformShapeToPolygon(
        poly, layer, margin(item, layer) + extra, ERR, pcbnew.ERROR_OUTSIDE
    )


for track in board.GetTracks():
    layer = pcbnew.F_Cu if track.Type() == pcbnew.PCB_VIA_T else track.GetLayer()
    add(track, layer, envelope, ENVELOPE_EXTRA)
    if track.GetNetCode() != gnd:
        add(track, layer, obstacles)

gnd_land_centers = []
for footprint in board.GetFootprints():
    for pad in footprint.Pads():
        layer = pcbnew.F_Cu if pad.IsOnLayer(pcbnew.F_Cu) else pcbnew.B_Cu
        add(pad, layer, envelope, ENVELOPE_EXTRA)
        if pad.GetNetCode() != gnd:
            add(pad, layer, obstacles)
        elif layer == pcbnew.B_Cu:
            gnd_land_centers.append((pad.GetPosition().x, pad.GetPosition().y))

obstacles.Simplify()
envelope.Simplify()

allowed = pcbnew.SHAPE_POLY_SET(outline)
allowed.Deflate(EDGE_KEEPIN, pcbnew.CORNER_STRATEGY_ROUND_ALL_CORNERS, ERR)
allowed.BooleanSubtract(obstacles)

ring = pcbnew.SHAPE_POLY_SET(outline)
ring.Deflate(RING_INSET, pcbnew.CORNER_STRATEGY_ROUND_ALL_CORNERS, ERR)


def chains(poly):
    for i in range(poly.OutlineCount()):
        yield poly.COutline(i)
        for j in range(poly.HoleCount(i)):
            yield poly.CHole(i, j)


def samples(chain):
    cursor, next_sample = 0.0, PITCH / 2.0
    for k in range(chain.SegmentCount()):
        seg = chain.CSegment(k)
        ax, ay, bx, by = seg.A.x, seg.A.y, seg.B.x, seg.B.y
        length = math.hypot(bx - ax, by - ay)
        if length < 1.0:
            continue
        while next_sample <= cursor + length:
            t = (next_sample - cursor) / length
            yield round(ax + t * (bx - ax)), round(ay + t * (by - ay))
            next_sample += PITCH
        cursor += length


accepted = []
buckets = {}


def far_enough(x, y):
    cx, cy = x // MIN_SEP, y // MIN_SEP
    return all(
        (ox - x) ** 2 + (oy - y) ** 2 >= MIN_SEP * MIN_SEP
        for dx in (-1, 0, 1)
        for dy in (-1, 0, 1)
        for ox, oy in buckets.get((cx + dx, cy + dy), ())
    )


def place(x, y):
    if far_enough(x, y) and allowed.Collide(pcbnew.VECTOR2I(x, y)):
        accepted.append((x, y))
        buckets.setdefault((x // MIN_SEP, y // MIN_SEP), []).append((x, y))
        return True
    return False


# A via in each GND mate land (same net, pressure contact — no solder)
# guarantees its pour pocket bridges the faces even where routed traces
# and neighboring lands squeeze out every contour candidate.
for x, y in gnd_land_centers:
    place(x, y)
for chain in chains(envelope):
    for x, y in samples(chain):
        place(x, y)
for chain in chains(ring):
    for x, y in samples(chain):
        place(x, y)

for x, y in accepted:
    via = pcbnew.PCB_VIA(board)
    via.SetViaType(pcbnew.VIATYPE_THROUGH)
    via.SetPosition(pcbnew.VECTOR2I(x, y))
    via.SetWidth(VIA_WIDTH)
    via.SetDrill(VIA_DRILL)
    via.SetLayerPair(pcbnew.F_Cu, pcbnew.B_Cu)
    via.SetNetCode(gnd)
    board.Add(via)

filler = pcbnew.ZONE_FILLER(board)
if not filler.Fill(board.Zones()):
    sys.exit("Failed to fill zones after stitching")

# Rescue pass: a routed-trace pocket can sever a fill island holding a
# GND pad from every via (a top pogo pad can't take one in-pad). Any
# island with a GND pad and no via gets one dropped inside it.
RESCUE_STEP = pcbnew.FromMM(0.4)
RESCUE_SEP = pcbnew.FromMM(0.7)
gnd_pads = [
    (
        pad.GetPosition().x,
        pad.GetPosition().y,
        pad.IsOnLayer(pcbnew.F_Cu),
        max(pad.GetSize().x, pad.GetSize().y) // 2,
    )
    for footprint in board.GetFootprints()
    for pad in footprint.Pads()
    if pad.GetNetCode() == gnd
]
rescued = 0


def add_via(pt):
    global rescued
    via = pcbnew.PCB_VIA(board)
    via.SetViaType(pcbnew.VIATYPE_THROUGH)
    via.SetPosition(pt)
    via.SetWidth(VIA_WIDTH)
    via.SetDrill(VIA_DRILL)
    via.SetLayerPair(pcbnew.F_Cu, pcbnew.B_Cu)
    via.SetNetCode(gnd)
    board.Add(via)
    accepted.append((pt.x, pt.y))
    rescued += 1


# A pad ringed by foreign traces at minimum clearance gets no spokes,
# no fill, and leaves no legal spot beside it — the only bridge left is
# a via in the pad itself. Fixture pads tolerate that.
gnd_fills = {}
for zone in board.Zones():
    if zone.GetNetCode() == gnd:
        for layer in (pcbnew.F_Cu, pcbnew.B_Cu):
            if zone.IsOnLayer(layer):
                gnd_fills[layer] = zone.GetFilledPolysList(layer)
for x, y, top, radius in gnd_pads:
    layer = pcbnew.F_Cu if top else pcbnew.B_Cu
    fill = gnd_fills.get(layer)
    if fill is None:
        continue
    # A connected pad has fill copper (spokes or solid) overlapping it,
    # so the fill comes within the pad's own radius of its center; an
    # orphan's nearest fill sits a full thermal gap further out.
    touched = fill.Collide(pcbnew.VECTOR2I(x, y), radius + pcbnew.FromMM(0.05))
    if not touched and all(
        (ox - x) ** 2 + (oy - y) ** 2 >= RESCUE_SEP * RESCUE_SEP for ox, oy in accepted
    ):
        add_via(pcbnew.VECTOR2I(x, y))

for zone in board.Zones():
    if zone.GetNetCode() != gnd:
        continue
    for layer in (pcbnew.F_Cu, pcbnew.B_Cu):
        if not zone.IsOnLayer(layer):
            continue
        fill = zone.GetFilledPolysList(layer)
        for i in range(fill.OutlineCount()):
            island = pcbnew.SHAPE_POLY_SET()
            island.AddOutline(fill.COutline(i))
            on_top = layer == pcbnew.F_Cu
            pads_in = [
                (x, y)
                for x, y, top, _ in gnd_pads
                if top == on_top and island.Collide(pcbnew.VECTOR2I(x, y))
            ]
            if not pads_in:
                continue
            if any(island.Collide(pcbnew.VECTOR2I(x, y)) for x, y in accepted):
                continue
            box = island.BBox()
            done = False
            y = box.GetY()
            while not done and y <= box.GetBottom():
                x = box.GetX()
                while not done and x <= box.GetRight():
                    pt = pcbnew.VECTOR2I(x, y)
                    clear = all(
                        (ox - x) ** 2 + (oy - y) ** 2 >= RESCUE_SEP * RESCUE_SEP
                        for ox, oy in accepted
                    )
                    if clear and island.Collide(pt) and allowed.Collide(pt):
                        add_via(pt)
                        done = True
                    x += RESCUE_STEP
                y += RESCUE_STEP

if rescued and not filler.Fill(board.Zones()):
    sys.exit("Failed to refill zones after rescue vias")
if not pcbnew.SaveBoard(board_path, board):
    sys.exit("Failed to save stitched board")

with open(report_path, "w") as report:
    report.write(str(len(accepted)))
"#;

/// Stitch the routed board's GND with vias and refill its zones.
/// Returns the number of vias placed.
pub fn stitch(board_path: &Path) -> Result<usize> {
    let report = tempfile::NamedTempFile::new().context("create stitch report file")?;
    pcb_kicad::PythonScriptBuilder::new(STITCH_SCRIPT)
        .arg(board_path.to_string_lossy())
        .arg(report.path().to_string_lossy())
        .run()
        .context("stitch GND vias")?;
    let count = std::fs::read_to_string(report.path()).context("read stitch report")?;
    count.trim().parse().context("parse stitch report")
}
