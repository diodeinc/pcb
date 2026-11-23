//! Routing guards around GND pads and alignment keep-outs, run on the
//! board via KiCad's python.
//!
//! The router sees GND as a pour and assumes every GND pad is connected,
//! so it is free to hug a GND land with signal traces until the fill
//! around the land is a sealed sliver; with a foreign pogo pad over the
//! land on the opposite face, no stitch via can reach it either. Before
//! routing, [`add`] rings every GND pad with a keep-out (no tracks, no
//! vias) on the pad's own layer, wide enough for the thermal gap, spokes,
//! and a minimum-width ring of fill, notched wherever it would close off
//! a neighboring foreign pad. A ring is not enough where foreign pads
//! leave the pad no via site of its own (a land under a signal pogo):
//! routed traces around the ring can still seal it. So every GND pad also
//! gets an escape, reserved before any track exists: its own center when
//! no foreign pad blocks a via there (lands only — a via would wick a
//! soldered pogo), else the nearest legal via site within 4 mm with a
//! no-tracks corridor to it on the pad's face. The site is guarded on
//! both faces and handed to the stitch pass, which drills it first. The
//! bed's alignment features carry
//! their copper keep-out as pad clearance, which the router does not
//! see, so they get a guard on both faces too; fiducials, netless, get
//! one on their face, and a no-via band runs along the board edge.
//! [`remove`] takes the guards out again after
//! routing, before stitching.
//!
//! Over inner GND planes a GND pad reaches GND through a via, not the
//! pour around it, so it gets no ring or corridor (see [`Rings`]):
//! reserved at the board's uniform 0.6 mm, those would wall off most of
//! the routing room. It gets no guard at all ([`Rings::None`]), or, where
//! routing sealed a pad that way, only a via site overlapping it
//! ([`Rings::Sites`]); each costs some boards the other routes cleanly.
//!
//! High-voltage tracks and vias are under solder mask, so the pour keeps
//! its own clearance to them; a high-voltage pad's exposed clearance
//! comes from the board's custom rules, which pcbnew reports as the pad's
//! own.

use std::path::Path;

use anyhow::{Context, Result};

pub(crate) const GUARD_NAME: &str = "routing-guard";

/// The GND pour's clearance to foreign copper.
const ZONE_CLEARANCE_MM: f64 = 0.25;

const ADD_SCRIPT: &str = r#"
import math
import sys

import pcbnew

board_path, name, margin, clearance, sites_path, rings = sys.argv[1:7]
MODE = rings  # "rings" | "sites" | "none"
board = pcbnew.LoadBoard(board_path)
gnd = board.FindNet("GND").GetNetCode()

MARGIN = pcbnew.FromMM(float(margin))
CLEARANCE = pcbnew.FromMM(float(clearance))
VIA_RADIUS = pcbnew.FromMM(0.3)
# The via, the clearance foreign tracks keep, and a ring of fill joining
# the via to the pour.
SITE_GUARD = VIA_RADIUS + CLEARANCE + pcbnew.FromMM(0.3)
# Minimum fill width plus the clearance on both sides, plus a hair.
CORRIDOR = pcbnew.FromMM(0.25) + 2 * CLEARANCE + pcbnew.FromMM(0.2)
SITE_REACH = pcbnew.FromMM(4.0)
SITE_STEP = pcbnew.FromMM(0.25)
EDGE_KEEPIN = pcbnew.FromMM(0.75)
# Half the widest track (Power, 0.8 mm) plus a hair: the notch a guard
# leaves around a foreign pad so any track can still end at its center.
NOTCH_TRACK = pcbnew.FromMM(0.45)
SIDES = 24


def disk(center, radius):
    poly = pcbnew.SHAPE_POLY_SET()
    poly.NewOutline()
    for k in range(SIDES):
        angle = 2 * math.pi * k / SIDES
        poly.Append(
            center.x + round(radius * math.cos(angle)),
            center.y + round(radius * math.sin(angle)),
        )
    return poly


def guard(shape, layer, tracks=True):
    for i in range(shape.OutlineCount()):
        zone = pcbnew.ZONE(board)
        zone.SetIsRuleArea(True)
        zone.SetZoneName(name)
        zone.SetLayer(layer)
        zone.SetDoNotAllowTracks(tracks)
        zone.SetDoNotAllowVias(True)
        zone.SetDoNotAllowPads(False)
        zone.SetDoNotAllowFootprints(False)
        zone.SetDoNotAllowZoneFills(False)
        outline = zone.Outline()
        outline.NewOutline()
        chain = shape.COutline(i)
        for k in range(chain.PointCount()):
            point = chain.CPoint(k)
            outline.Append(point.x, point.y)
        board.Add(zone)


pads = [pad for footprint in board.GetFootprints() for pad in footprint.Pads()]


def radius_of(pad):
    size = pad.GetSize()
    return max(size.x, size.y) // 2


def notched(shape, layer):
    """A guard shape minus a notch around every foreign pad on its layer:
    a pad's center must stay reachable by the widest track at that pad's
    clearance."""
    for other in pads:
        if other.GetNetCode() in (gnd, 0) or not other.IsOnLayer(layer):
            continue
        notch = NOTCH_TRACK + other.GetOwnClearance(layer)
        if shape.Collide(other.GetPosition(), notch):
            shape.BooleanSubtract(disk(other.GetPosition(), notch))
    return shape


inside = pcbnew.SHAPE_POLY_SET()
board.GetBoardPolygonOutlines(inside, False)
inside.Deflate(EDGE_KEEPIN, pcbnew.CORNER_STRATEGY_ROUND_ALL_CORNERS, pcbnew.FromMM(0.01))
blockers = [pad for pad in pads if pad.GetNetCode() != gnd]


def site_ok(point):
    """A GND via fits here against every fixed foreign pad and hole."""
    if not inside.Collide(point):
        return False
    for other in blockers:
        for layer in (pcbnew.F_Cu, pcbnew.B_Cu):
            if not other.IsOnLayer(layer):
                continue
            reach = radius_of(other) + max(other.GetOwnClearance(layer), CLEARANCE) + VIA_RADIUS
            if (other.GetPosition() - point).EuclideanNorm() < reach:
                return False
    return True


def escape_site(pad, gap=pcbnew.FromMM(0.3)):
    center = pad.GetPosition()
    soldered = pad.IsOnLayer(pcbnew.F_Paste) or pad.IsOnLayer(pcbnew.B_Paste)
    if not soldered and site_ok(center):
        return center
    r = radius_of(pad) + VIA_RADIUS + gap
    while r <= radius_of(pad) + SITE_REACH:
        steps = max(12, int(2 * math.pi * r / SITE_STEP))
        for k in range(steps):
            angle = 2 * math.pi * k / steps
            point = pcbnew.VECTOR2I(
                center.x + round(r * math.cos(angle)), center.y + round(r * math.sin(angle))
            )
            if site_ok(point):
                return point
        r += SITE_STEP
    return None


def corridor(a, b, width=CORRIDOR):
    dx, dy = b.x - a.x, b.y - a.y
    length = math.hypot(dx, dy)
    nx, ny = -dy / length * width / 2, dx / length * width / 2
    poly = pcbnew.SHAPE_POLY_SET()
    poly.NewOutline()
    for x, y in ((a.x + nx, a.y + ny), (b.x + nx, b.y + ny), (b.x - nx, b.y - ny), (a.x - nx, a.y - ny)):
        poly.Append(round(x), round(y))
    return poly


sites = []

# The router keeps only its default clearance to the board edge: a
# no-via band along every outline edge keeps vias out of the board's
# copper-to-edge clearance.
edges = pcbnew.SHAPE_POLY_SET()
board.GetBoardPolygonOutlines(edges, False)
for i in range(edges.OutlineCount()):
    chain = edges.COutline(i)
    for k in range(chain.PointCount()):
        a, b = chain.CPoint(k), chain.CPoint((k + 1) % chain.PointCount())
        if a != b:
            for layer in (pcbnew.F_Cu, pcbnew.B_Cu):
                guard(corridor(a, b, 2 * EDGE_KEEPIN), layer, tracks=False)


for footprint in board.GetFootprints():
    item = str(footprint.GetFPID().GetLibItemName())
    alignment = item.startswith("Alignment_")
    fiducial = item.startswith("Fiducial_")
    for pad in footprint.Pads():
        size = pad.GetSize()
        if alignment:
            # The keep-out disk: the drill's narrow side plus its clearance.
            radius = min(size.x, size.y) // 2 + pad.GetOwnClearance(pcbnew.F_Cu)
            for layer in (pcbnew.F_Cu, pcbnew.B_Cu):
                guard(disk(pad.GetPosition(), radius), layer)
        elif fiducial:
            # Netless, so the router ignores its clearance.
            for layer in (pcbnew.F_Cu, pcbnew.B_Cu):
                if pad.IsOnLayer(layer):
                    radius = radius_of(pad) + pad.GetOwnClearance(layer)
                    guard(disk(pad.GetPosition(), radius), layer)
        elif pad.GetNetCode() == gnd and MODE == "sites":
            # Over GND planes: only a via site overlapping the pad (in it,
            # on a land), kept clear of tracks by the via's own clearance.
            site = escape_site(pad, -pcbnew.FromMM(0.05))
            if site is not None:
                sites.append(site)
                for layer in (pcbnew.F_Cu, pcbnew.B_Cu):
                    guard(notched(disk(site, VIA_RADIUS + CLEARANCE), layer), layer)
        elif pad.GetNetCode() == gnd and MODE == "rings":
            layer = pcbnew.F_Cu if pad.IsOnLayer(pcbnew.F_Cu) else pcbnew.B_Cu
            other_layer = pcbnew.B_Cu if layer == pcbnew.F_Cu else pcbnew.F_Cu
            center = pad.GetPosition()
            site = escape_site(pad)
            shape = disk(center, radius_of(pad) + MARGIN)
            if site is not None:
                sites.append(site)
                if site != center:
                    shape.BooleanAdd(corridor(center, site))
                    shape.BooleanAdd(disk(site, SITE_GUARD))
                guard(notched(disk(site, SITE_GUARD), other_layer), other_layer)
            guard(notched(shape, layer), layer)

if not pcbnew.SaveBoard(board_path, board):
    sys.exit("Failed to save guarded board")
with open(sites_path, "w") as out:
    for site in sites:
        out.write(f"{pcbnew.ToMM(site.x)} {pcbnew.ToMM(site.y)}\n")
"#;

const REMOVE_SCRIPT: &str = r#"
import sys

import pcbnew

board_path, name = sys.argv[1], sys.argv[2]
board = pcbnew.LoadBoard(board_path)
for zone in list(board.Zones()):
    if zone.GetIsRuleArea() and zone.GetZoneName() == name:
        board.Remove(zone)
if not pcbnew.SaveBoard(board_path, board):
    sys.exit("Failed to save unguarded board")
"#;

/// Whether GND pads get guarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rings {
    /// The pour is the pads' only path to GND: ring each and reserve its
    /// escape via.
    Pour,
    /// Inner GND planes: reserve only a via site overlapping each pad.
    Sites,
    /// Inner GND planes: no GND guard; the stitch pass finds each pad a
    /// via after routing.
    None,
}

/// Guard the alignment keep-outs, fiducials and board edge, and with
/// [`Rings::Pour`] ring every GND pad of the board with a routing
/// keep-out reaching past the pad edge by the thermal gap (0.3), a ring
/// of fill above its 0.25 minimum thickness (0.35), and the fill's
/// clearance to foreign tracks; reserve each pad's escape via site.
/// `clearance` is the board's uniform clearance, when it has one; the
/// pour's own (0.25) otherwise. Returns the sites (mm), for
/// [`crate::stitch::stitch`].
pub fn add(board_path: &Path, rings: Rings, clearance: Option<f64>) -> Result<Vec<[f64; 2]>> {
    let clearance = clearance.unwrap_or(ZONE_CLEARANCE_MM);
    let margin = 0.3 + 0.35 + clearance;
    let sites = tempfile::NamedTempFile::new().context("create escape site file")?;
    run(
        ADD_SCRIPT,
        board_path,
        &[
            margin.to_string(),
            clearance.to_string(),
            sites.path().to_string_lossy().into_owned(),
            match rings {
                Rings::Pour => "rings",
                Rings::Sites => "sites",
                Rings::None => "none",
            }
            .into(),
        ],
    )
    .context("add GND pad routing guards")?;
    std::fs::read_to_string(sites.path())
        .context("read escape sites")?
        .lines()
        .map(|line| {
            let (x, y) = line.split_once(' ').context("malformed escape site")?;
            Ok([x.parse()?, y.parse()?])
        })
        .collect()
}

/// Remove the keep-outs [`add`] placed.
pub fn remove(board_path: &Path) -> Result<()> {
    run(REMOVE_SCRIPT, board_path, &[]).context("remove GND pad routing guards")
}

fn run(script: &str, board_path: &Path, args: &[String]) -> Result<()> {
    pcb_kicad::PythonScriptBuilder::new(script)
        .arg(board_path.to_string_lossy())
        .arg(GUARD_NAME)
        .args(args.iter().cloned())
        .run()
}
