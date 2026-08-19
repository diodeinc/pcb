# Fill the emitted board's zones with KiCad's own filler and repair GND
# connectivity: where the two pours split into disconnected groups, add a
# stitching via inside the intersection of a split fragment with the main
# group's fill on the other layer (deflated by 0.6 mm so the via keeps its
# clearances by construction). Runs under KiCad's bundled python.
import sys

import pcbnew

path = sys.argv[1]
board = pcbnew.LoadBoard(path)
filler = pcbnew.ZONE_FILLER(board)
filler.Fill(board.Zones())
gnd = board.GetNetsByName()["GND"]


def fragments():
    out = []
    for z in board.Zones():
        lay = z.GetFirstLayer()
        fill = z.GetFilledPolysList(lay)
        for i in range(fill.OutlineCount()):
            sub = pcbnew.SHAPE_POLY_SET()
            sub.AddOutline(fill.Outline(i))
            out.append((lay, sub))
    return out


def group_map(frags):
    parent = list(range(len(frags)))

    def find(a):
        while parent[a] != a:
            parent[a] = parent[parent[a]]
            a = parent[a]
        return a

    for t in board.Tracks():
        if "VIA" not in t.GetClass().upper() or t.GetNetCode() != gnd.GetNetCode():
            continue
        f = g = None
        for j, (lay, ps) in enumerate(frags):
            if ps.Contains(t.GetPosition()):
                if lay == pcbnew.F_Cu:
                    f = j
                elif lay == pcbnew.B_Cu:
                    g = j
        if f is not None and g is not None:
            parent[find(f)] = find(g)
    groups = {}
    for j in range(len(frags)):
        groups.setdefault(find(j), []).append(j)
    return groups


def area(ps):
    bb = ps.BBox()
    return bb.GetWidth() * float(bb.GetHeight())


for _ in range(8):
    frags = fragments()
    groups = group_map(frags)
    if len(groups) <= 1:
        break
    main = max(groups, key=lambda k: sum(area(frags[j][1]) for j in groups[k]))
    placed = False
    for k, members in groups.items():
        if k == main or placed:
            continue
        for j in members:
            layj, pj = frags[j]
            other = pcbnew.B_Cu if layj == pcbnew.F_Cu else pcbnew.F_Cu
            for m in groups[main]:
                laym, pm = frags[m]
                if laym != other:
                    continue
                inter = pcbnew.SHAPE_POLY_SET(pj)
                inter.BooleanIntersection(pm)
                if inter.IsEmpty():
                    continue
                inter.Deflate(600000, pcbnew.CORNER_STRATEGY_ROUND_ALL_CORNERS, 5000)
                if inter.IsEmpty():
                    continue
                pt = inter.Outline(0).CPoint(0)
                via = pcbnew.PCB_VIA(board)
                via.SetPosition(pcbnew.VECTOR2I(pt.x, pt.y))
                via.SetWidth(600000)
                via.SetDrill(300000)
                via.SetLayerPair(pcbnew.F_Cu, pcbnew.B_Cu)
                via.SetNet(gnd)
                board.Add(via)
                placed = True
                break
            if placed:
                break
    if not placed:
        break
    filler.Fill(board.Zones())

pcbnew.SaveBoard(path, board)
