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

board_path, report_path, sites_arg = sys.argv[1], sys.argv[2], sys.argv[3]
# Escape sites reserved before routing (`guard`), in mm.
escape_sites = [
    (pcbnew.FromMM(float(x)), pcbnew.FromMM(float(y)))
    for x, y in (site.split(",") for site in sites_arg.split(";") if site)
]

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
EDGE_KEEPIN = pcbnew.FromMM(0.75)
RING_INSET = pcbnew.FromMM(1.0)

board = pcbnew.LoadBoard(board_path)
gnd = board.FindNet("GND").GetNetCode()

outline = pcbnew.SHAPE_POLY_SET()
board.GetBoardPolygonOutlines(outline, False)

# The router keeps only its default clearance to the board edge; drop
# any GND via it set inside the edge keep-in (an inner GND plane draws
# them). Whatever it joined gets stitched again below.
inside = pcbnew.SHAPE_POLY_SET(outline)
inside.Deflate(EDGE_KEEPIN, pcbnew.CORNER_STRATEGY_ROUND_ALL_CORNERS, ERR)
for item in list(board.GetTracks()):
    if item.GetClass() == "PCB_VIA" and item.GetNetCode() == gnd and not inside.Collide(item.GetPosition()):
        board.Remove(item)

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


# Per-layer obstacle masks back the in-pad rescue: a through-via at a
# pad center clears its own face by construction but must still clear
# the opposite face's copper.
layer_obstacles = {pcbnew.F_Cu: pcbnew.SHAPE_POLY_SET(), pcbnew.B_Cu: pcbnew.SHAPE_POLY_SET()}

for track in board.GetTracks():
    layer = pcbnew.F_Cu if track.Type() == pcbnew.PCB_VIA_T else track.GetLayer()
    add(track, layer, envelope, ENVELOPE_EXTRA)
    if track.GetNetCode() != gnd:
        add(track, layer, obstacles)
        if layer in layer_obstacles:
            add(track, layer, layer_obstacles[layer])

gnd_land_centers = []
for footprint in board.GetFootprints():
    for pad in footprint.Pads():
        layer = pcbnew.F_Cu if pad.IsOnLayer(pcbnew.F_Cu) else pcbnew.B_Cu
        add(pad, layer, envelope, ENVELOPE_EXTRA)
        if pad.GetNetCode() != gnd:
            add(pad, layer, obstacles)
            add(pad, layer, layer_obstacles[layer])
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
for x, y in escape_sites:
    place(x, y)
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
# GND pads no via can join to the main pour: foreign copper over them on
# the opposite face and routed traces around them. Reported, not fatal —
# the rest of the board still needs its stitching, and DRC flags the pad.
orphans = []


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
def in_pad_via(x, y, top, radius):
    # A GND via already inside the pad (the router's, a stitch or an
    # earlier rescue) bridges it.
    vias = [
        (track.GetPosition().x, track.GetPosition().y)
        for track in board.GetTracks()
        if track.Type() == pcbnew.PCB_VIA_T and track.GetNetCode() == gnd
    ]
    if any((ox - x) ** 2 + (oy - y) ** 2 < radius * radius for ox, oy in vias + accepted):
        return
    # In-pad candidates: the via clears this face inside its own pad, but
    # each spot must clear the opposite face's copper and keep its own
    # distance from every already-placed via.
    opposite = layer_obstacles[pcbnew.B_Cu if top else pcbnew.F_Cu]
    step = pcbnew.FromMM(0.35)
    spots = [(x, y)] + [
        (x + round(step * math.cos(k * math.pi / 3)), y + round(step * math.sin(k * math.pi / 3)))
        for k in range(6)
    ]
    spot = next(
        (
            s
            for s in spots
            if not opposite.Collide(pcbnew.VECTOR2I(*s))
            and all(
                (ox - s[0]) ** 2 + (oy - s[1]) ** 2 >= RESCUE_SEP * RESCUE_SEP
                for ox, oy in accepted
            )
        ),
        None,
    )
    if spot is None:
        orphans.append((x, y))
        return
    add_via(pcbnew.VECTOR2I(*spot))


for x, y, top, radius in gnd_pads:
    layer = pcbnew.F_Cu if top else pcbnew.B_Cu
    fill = gnd_fills.get(layer)
    if fill is None:
        continue
    # A connected pad has fill copper (spokes or solid) overlapping it,
    # so the fill comes within the pad's own radius of its center; an
    # orphan's nearest fill sits a full thermal gap further out.
    touched = fill.Collide(pcbnew.VECTOR2I(x, y), radius + pcbnew.FromMM(0.05))
    if not touched:
        in_pad_via(x, y, top, radius)

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
            # A thermal-relief pad's center sits in its own fill cutout;
            # the island reaches it only at the spokes, on the pad's rim.
            pads_in = [
                (x, y, top, radius)
                for x, y, top, radius in gnd_pads
                if top == on_top
                and island.Collide(pcbnew.VECTOR2I(x, y), radius + pcbnew.FromMM(0.05))
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
            # A sliver island can hold no legal via of its own (foreign
            # copper on the opposite face shadows all of it); bridge it
            # through its pad instead.
            if not done:
                in_pad_via(*pads_in[0])

if rescued and not filler.Fill(board.Zones()):
    sys.exit("Failed to refill zones after rescue vias")

# An inner GND plane ties every GND via to every other.
inner_planes = any(
    zone.GetNetCode() == gnd
    and (zone.IsOnLayer(pcbnew.In1_Cu) or zone.IsOnLayer(pcbnew.In2_Cu))
    for zone in board.Zones()
)


def gnd_clusters():
    """Group the GND fill islands of both faces into connected clusters:
    vias join the islands they sit in, pads the islands they touch, and a
    via inside a pad joins that pad's islands; over an inner GND plane all
    vias join one another. Returns the island map, the union-find root
    lookup, each GND via with its islands, and each GND pad with its
    islands."""
    nodes = {}
    for zone in board.Zones():
        if zone.GetNetCode() != gnd:
            continue
        for layer in (pcbnew.F_Cu, pcbnew.B_Cu):
            if zone.IsOnLayer(layer):
                fill = zone.GetFilledPolysList(layer)
                for i in range(fill.OutlineCount()):
                    island = pcbnew.SHAPE_POLY_SET()
                    island.AddOutline(fill.COutline(i))
                    nodes[(layer, len(nodes))] = island
    parent = {node: node for node in nodes}

    def find(node):
        while parent[node] != node:
            parent[node] = parent[parent[node]]
            node = parent[node]
        return node

    def join(touching):
        for a, b in zip(touching, touching[1:]):
            parent[find(a)] = find(b)

    def islands_at(point, layer, reach=0):
        return [
            node
            for node, island in nodes.items()
            if node[0] == layer and island.Collide(point, reach)
        ]

    pad_nodes = []
    for x, y, top, radius in gnd_pads:
        layer = pcbnew.F_Cu if top else pcbnew.B_Cu
        touching = islands_at(pcbnew.VECTOR2I(x, y), layer, radius + pcbnew.FromMM(0.05))
        join(touching)
        pad_nodes.append(((x, y, top, radius), touching))
    via_nodes = []
    plane = []
    for track in board.GetTracks():
        if track.Type() != pcbnew.PCB_VIA_T or track.GetNetCode() != gnd:
            continue
        at = track.GetPosition()
        touching = islands_at(at, pcbnew.F_Cu) + islands_at(at, pcbnew.B_Cu)
        for (x, y, _, radius), pad_touching in pad_nodes:
            if (x - at.x) ** 2 + (y - at.y) ** 2 < radius * radius:
                touching += pad_touching
        if inner_planes and touching:
            if plane:
                touching = touching + plane
            else:
                plane.append(touching[0])
        join(touching)
        via_nodes.append((track, touching))
    return nodes, find, via_nodes, pad_nodes


# Prune pass: fill islands on both faces joined only by stitch vias, with
# no GND pad anywhere in the cluster, are dead copper the zones' island
# removal keeps (each island touches a via). Drop their vias and refill;
# the islands then go too.
nodes, find, via_nodes, pad_nodes = gnd_clusters()
anchored = {find(node) for _, touching in pad_nodes for node in touching}
pruned = 0
for track, touching in via_nodes:
    if not any(find(node) in anchored for node in touching):
        board.Remove(track)
        pruned += 1
if pruned and not filler.Fill(board.Zones()):
    sys.exit("Failed to refill zones after pruning dead vias")

# Bridge pass: a cluster holding GND pads but cut off from the main GND
# (most pads) is a broken return path, not dead copper. Bridge it with a
# via where its fill on one face overlies the main cluster's fill on the
# other; a cluster with no such legal spot is reported.
bridged = 0
for _ in range(3):
    nodes, find, via_nodes, pad_nodes = gnd_clusters()
    pads_by_root = {}
    for pad, touching in pad_nodes:
        for root in {find(node) for node in touching}:
            pads_by_root.setdefault(root, []).append(pad)
    if len(pads_by_root) <= 1:
        break
    main = max(pads_by_root, key=lambda root: len(pads_by_root[root]))
    placed = False
    for root, pads_cut in pads_by_root.items():
        if root == main:
            continue
        mine = [node for node in nodes if find(node) == root]
        theirs = [node for node in nodes if find(node) == main]
        spot = None
        for node in mine:
            other_layer = pcbnew.B_Cu if node[0] == pcbnew.F_Cu else pcbnew.F_Cu
            box = nodes[node].BBox()
            y = box.GetY()
            while spot is None and y <= box.GetBottom():
                x = box.GetX()
                while spot is None and x <= box.GetRight():
                    pt = pcbnew.VECTOR2I(x, y)
                    if (
                        nodes[node].Collide(pt)
                        and allowed.Collide(pt)
                        and any(
                            other[0] == other_layer and nodes[other].Collide(pt)
                            for other in theirs
                        )
                        and all(
                            (ox - x) ** 2 + (oy - y) ** 2 >= RESCUE_SEP * RESCUE_SEP
                            for ox, oy in accepted
                        )
                    ):
                        spot = pt
                    x += RESCUE_STEP
                y += RESCUE_STEP
            if spot is not None:
                break
        if spot is not None:
            add_via(spot)
            bridged += 1
            placed = True
    if not placed or not filler.Fill(board.Zones()):
        break
nodes, find, via_nodes, pad_nodes = gnd_clusters()
roots = {}
for pad, touching in pad_nodes:
    for root in {find(node) for node in touching}:
        roots.setdefault(root, []).append(pad)
if len(roots) > 1:
    main = max(roots, key=lambda root: len(roots[root]))
    for root, pads_cut in roots.items():
        if root != main:
            orphans.extend((x, y) for x, y, _, _ in pads_cut)

if not pcbnew.SaveBoard(board_path, board):
    sys.exit("Failed to save stitched board")

with open(report_path, "w") as report:
    report.write(f"{len(accepted) - pruned}\n")
    for x, y in orphans:
        report.write(f"{pcbnew.ToMM(x):.2f} {pcbnew.ToMM(y):.2f}\n")
"#;

/// Stitch the routed board's GND with vias and refill its zones, drilling
/// the escape `sites` (mm) reserved before routing first. Returns the
/// number of vias placed; GND pads no via could reach are reported as
/// warnings.
pub fn stitch(board_path: &Path, sites: &[[f64; 2]]) -> Result<usize> {
    let sites = sites
        .iter()
        .map(|[x, y]| format!("{x},{y}"))
        .collect::<Vec<_>>()
        .join(";");
    let report = tempfile::NamedTempFile::new().context("create stitch report file")?;
    pcb_kicad::PythonScriptBuilder::new(STITCH_SCRIPT)
        .arg(board_path.to_string_lossy())
        .arg(report.path().to_string_lossy())
        .arg(sites)
        .run()
        .context("stitch GND vias")?;
    let report = std::fs::read_to_string(report.path()).context("read stitch report")?;
    let mut lines = report.lines();
    let count = lines
        .next()
        .unwrap_or_default()
        .parse()
        .context("parse stitch report")?;
    for orphan in lines {
        let (x, y) = orphan.split_once(' ').unwrap_or((orphan, ""));
        eprintln!(
            "warning: GND pad at ({x}, {y}) is cut off from the main GND pour: routed traces \
             surround it and no legal via can bridge it"
        );
    }
    Ok(count)
}

const UNCONNECTED_SCRIPT: &str = r#"
import sys

import pcbnew

board = pcbnew.LoadBoard(sys.argv[1])
board.BuildConnectivity()
with open(sys.argv[2], "w") as report:
    report.write(str(board.GetConnectivity().GetUnconnectedCount(True)))
"#;

/// Connections the finished board still lacks, by KiCad's connectivity:
/// unrouted signal legs and GND copper the stitching could not join.
pub fn unconnected(board_path: &Path) -> Result<usize> {
    let report = tempfile::NamedTempFile::new().context("create connectivity report file")?;
    pcb_kicad::PythonScriptBuilder::new(UNCONNECTED_SCRIPT)
        .arg(board_path.to_string_lossy())
        .arg(report.path().to_string_lossy())
        .run()
        .context("count unconnected items")?;
    let count = std::fs::read_to_string(report.path()).context("read connectivity report")?;
    count.trim().parse().context("parse connectivity report")
}
