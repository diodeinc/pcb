# Automatic pogo-pin interposer generation

Exploration notes. Nothing here is a commitment. Work is meant to go
one subproblem at a time.

The first thing we are actually generating is the **interposer**. The
base / tile electronics are a contract we design against, not this
project’s first deliverable.

## Stack

```
  assembly panel  (A5 / A6 / A7, ≤ 8 boards)
  Ø 1 mm TestPoints (`ict` set) on the bottom face
          ↕  individual pogos, custom XY
  INTERPOSER      outline = panel
                  top: pogos + same tooling holes as the panel
                  bot: A7 mate at the origin + A7 tooling
          ↕  pogo arrays on the base, fixed pattern
  BASE TILE       one A7 block
                  1 board live at a time
```

The assembly panel may be larger than A7. The interposer is always the
**same size as the panel**. All DUT contacts are brought into one A7
region on the interposer bottom. That region is the reusable mate to
the base tile.

v1 is **1 tile**. Two tiles (16 boards) can come later. Same contract,
repeated.

## Tile contract (1 × A7)

ISO A7 is 74 × 105 mm. The mate sits in the **origin corner** of the
interposer. Leave a **5 mm margin** inside that rectangle for A7
tooling holes. Pads / pogos may use everything inside the margin
(64 × 95 mm if the A7 is 74 × 105). Orientation (which edge is X) is
still open; we should try both and keep the one that escapes better.

Electrical budget on the mate — think **pools**, not per-board pins:

| Pool | Lands | On the base | Notes |
|---|---:|---|---|
| Low-speed | 48 | Crosspoint (ADG2128-class) down to **8** host lines | SWCLK, SWDIO, UART, I2C, GPIO, reset, … |
| USB 2.0 HS | 8 pairs (16 pads, D+/D− only) | MAX4999-class mux | 480 Mbps |
| GND | 16 | Common return | |
| Vtarget | 16, in **8×2** banks | 8 switches on the base | DUT rail, 2 A per land |
| VUSB | 8, in 8 banks | 8 switches | Always 5 V, 2 A per land |

**104** electrical lands, plus tooling.

Because the 48 LS pins are a fully switched fabric down to 8 host
lines, the interposer does **not** have to know which land is SWDIO.
Any DUT low-speed pad may attach to any of the 48. The base names
them later. USB pairs and the power banks are the remaining colored
constraints.

v1 tests **one board at a time** per tile. 8 host LS lines is enough
for one DUT (e.g. SWDIO+SWCLK+NRST+UART+…); it is not enough to flash
eight boards in parallel. The mux walks the panel.

Vtarget grouping on the mate: **8 banks of 2 lands**. A DUT’s power
pads land on one bank. Unused banks stay unused (4 boards → 4 banks).
Do not regroup 16 pins into 4×4 on the copper for a smaller panel.

## Interposer rules

- Outline = assembly panel (A5 / A6 / A7 now; A4 later).
- Top tooling holes copy the panel. Bottom tooling holes are the A7
  mate’s.
- Top contacts are **individual** pogos. We do not control DUT pad
  pitch, so arrays on top are a non-starter.
- Bottom mate should prefer **even-count constellations** that match
  multi-pin pogo arrays (2 / 4 / 6 / 8-wide blocks). The base is much
  nicer to build if it is a handful of array connectors rather than
  104 press-fit singles. Pattern is **not frozen** — generate several
  strategies and score them on real panels.
- Stackup target: **2-layer**, GND pours on both sides, stitched.
  Power and signals are routed (no power planes: too many distinct
  Vtarget/VUSB nets). Size power traces for **2 A per land**. Move to
  4-layer only if USB or the A5 funnel cannot be made honest on 2.

## Why the LS pool matters for routing

A normal board has a sacred netlist. This one does not, for the 48
LS lands.

Assignment is:

- LS DUT pad → any unused LS mate land (interchangeable).
- USB D+/D− → one of the 8 HS pair slots (unsplittable ordered pair;
  no polarity swap).
- Each DUT’s Vtarget → one 2-pin bank.
- Each DUT’s VUSB → one VUSB land.
- GND → any GND land (likely all common through the pours).

That is a much looser problem than “SWDIO must hit pin B7.” The
expensive part is **geometry**: custom top XY on an A5/A6/A7 sheet,
concentrated into the origin A7, on two layers, with 2 A power traces
and 90 Ω-ish USB pairs.

## Identifying contacts — **solved: `ict` on Ø 1 mm TestPoints**

v1 contacts are only **`TestPoint` `Pad_D1.0mm`** (the usual 1 mm
circular pad) with `ict` set. No Tag-Connect, no FTSH, no other
TestPoint variants.

`TestPoint` takes optional `ict=…`. When set, it is copied onto the
footprint. Our IPC export (`pcb release` / `--bom-col-int-id Path`)
emits it as BOM **`Ict`**. Match case-insensitively.

A contact is: **bottom-side** (`B.Cu`), `packageRef` is
`TestPoint_Pad_D1.0mm` (ignore a KiCad `_N` suffix), and `Ict`
is set. Front-side (`F.Cu`) pads are ignored even if `Ict` is
set — the press-down fixture hits the panel bottom.

| `ict` | Demand |
|---|---|
| `swdio` `swclk` `nrst` `swo` | discrete LS |
| `gnd` | GND |
| `vusb` | 5 V USB rail |
| `vtarget` | DUT rail |
| `usb_dp` `usb_dm` | USB HS pair (ordered, unsplittable) |
| `ls` | other low-speed |

`swd` (whole TC2030) is out of v1. Leave it in the `TestPoint`
`allowed` list if already there; the extractor ignores it.

Join `Component` (`part` = Path, `packageRef`, `layerRef`) to
`BomItem` (`Ict`) to the single pad (`PinRef` pin 1). That is the
list. Nothing else.

Verified on Seward: `ict="gnd"` / `"vtarget"` survive layout as
`(property "Ict" …)` and our Path-keyed IPC as separate `BomItem`s.

## Where it hooks in

Do not parse a finished board-array XML and invent the interposer
after the fact. `pcb ipc2581 board-array create` already has the DUT
IPC and is about to compute placements, panel tooling, and fids.
Generate the interposer in the same command (e.g. `--interposer`).
Copy panel tooling / fids onto the interposer top; stamp A7 tooling
on the bottom mate. The boards we must fixture are boards we just
arrayed.

## Problem breakdown

1. **Contacts** — **done.** DUT IPC, bottom, `Ict` set. See above.
2. **Demands** — map `Ict` to kinds and bundle USB / Vtarget.
3. **Instantiate** — apply each array placement transform so contacts
   exist in panel coordinates (same step math the array already has).
4. **Hall check** — per kind, count only.
5. **Match** — min-cost assignment per kind; emit two-pin nets.
6. **Route** — 2-layer autoroute of those nets.

The A7 constellation *builds* slots; it is not a second matcher.

## Assignment — in-memory model

This is the assignment problem (Kuhn–Munkres) on a **complete
bipartite graph per kind**, after contacts have been bundled into
demands. No SAT, no constraint language. Hall is a counting check.
The router then sees only **two-pin nets**.

`Ict` is sequencer semantics. `Kind` is what matching sees. Several
ict values collapse to `ls`.

### Objects

Four sets and two maps:

```
Contact   pogo site on the panel
Demand    bundled contacts of one kind   (left vertex)
Slot      bundled mate pins of one kind  (right vertex)
MatePin   pad on the A7 mate

α : Demand  ↪  Slot      injection, per kind
β : Contact  →  MatePin  pin map inside that slot
```

They form a commuting square:

```
  Contact  ── β ──►  MatePin
     │                  │
  bundle              pins
     ▼                  ▼
  Demand   ── α ──►   Slot
```

`β` is `α` plus a choice in the slot’s symmetry group \(G_s\)
(identity for unit and USB; \(S_2\) for a Vtarget bank).

### Sorts and shapes

\[
T = \{\mathrm{ls},\; \mathrm{gnd},\; \mathrm{vusb},\; \mathrm{vtarget},\; \mathrm{usb\_hs}\}
\]

| Kind | From `ict` | \|slots\| | Shape | \(G_s\) |
|---|---|---:|---|---|
| LS | `swdio` `swclk` `nrst` `swo` `ls` | 48 | unit | \{e\} |
| GND | `gnd` | 16 | unit | \{e\} |
| VUSB | `vusb` | 8 | unit | \{e\} |
| Vtarget | `vtarget` | 8 | unordered 2-set | \(S_2\) |
| USB HS | `usb_dp`+`usb_dm` | 8 | ordered pair \((+,−)\) | \{e\} |

Bundle (by construction):

- unit kinds → one demand per contact
- USB → one demand per board, members `[dp, dm]`, or fail if unpaired
- Vtarget → one demand per board, 1 or 2 members (fail if 3+)

### Hall (feasibility)

Kinds are independent. For each \(t\):

\[
|D_t| \le |S_t|
\quad\text{and}\quad
\forall d \in D_t.\ |d| \le c(S_t)
\]

If that fails, the panel does not fit the tile. No matching algorithm.

### Geometric matching (quality)

Slots of a kind are electrically identical and geometrically not.
After Hall, pick \(\alpha_t\) as min-cost assignment on
\(K_{D_t,S_t}\):

\[
\min_\alpha \sum_d c(d,\alpha(d))
\]

- unit: distance contact → pin
- USB: \(\|p_{dp}-q_+\| + \|p_{dm}-q_-\|\) plus length mismatch;
  polarity fixed
- Vtarget: \(\min_{\sigma \in S_2} \sum_i \|p_i - q_{\sigma(i)}\|\)

Kuhn–Munkres / min-cost flow; \(n \le 48\). Slot sets are disjoint, so
kind order does not change feasibility. Run **USB → Vtarget → VUSB →
GND → LS** for a stable dump. Cross-kind crossings are QAP; out of
v1. A failed route may later swap two LS pins.

### State

```text
Ict      = swdio | swclk | nrst | swo | gnd | vusb | vtarget
         | usb_dp | usb_dm | ls
Kind     = ls | gnd | vusb | vtarget | usb_hs
Shape    = unit | unordered{n} | ordered[n]

Contact  = { id, board, xy, ict }
Demand   = { id, kind, board, members: [ContactId] }  # USB ordered
MatePin  = { id, xy }
Slot     = { id, kind, shape, pins: [MatePinId] }

Problem  = { contacts, demands, pins, slots,
             by_kind: Kind → {demands, slots} }

Assign   = { α: DemandId → SlotId,           # injective per kind
             β: ContactId → MatePinId }      # in G_s of α(bundle(c))
```

Assert, do not search: USB is born as `(dp, dm)`; `α` injective on
slots; `β` is a legal labeling of the assigned slot.

### From Assign to the router

One **two-pin net** per `β` edge:
`{ top_pogo(contact.xy), bottom_land(pin.xy) }`, plus a USB pair
grouping for length match, plus GND pours.

Treat top pogos as **PTH**. They already exist on both coppers, so
a net can start on top or bottom and must finish on the mate
(bottom SMT). v1 *assumed* single-layer (the pogo is the via).
The POC later added a second layer plus extra vias because
single-layer never escaped a full A5. General multi-net routing
is still NP-complete; we used greedy A\*, not a general solver.
Dropping boards was allowed early; the later bar was “near 100 %
boards complete.” We hit that bar only on a coarse maze, not on
manufacturable copper.

The assignment **is** the netlist. Autoroute does not choose partners.

The A7 constellation *builds* `Slot`/`MatePin`. Mux topology does
not belong in this state.

## Single-layer autoroute (best-effort)

Yes: PTH turns this into a **single-layer two-pin** problem. The
interesting question is which *easy* algorithm matches the geometry,
not how to build a general autorouter.

### Why this instance is easier than “an autorouter”

- **Two-pin only.** \(\beta\) already chose partners.
- **One layer, no vias.** The pogo *is* the via.
- **Sinks are clustered.** Every mate pad sits in the origin A7
  (64 × 95 mm usable). Sources sit in ≤ 8 board clusters. The
  panel is a *funnel*, not a general ratsnest.
- **The mate is a lattice.** 2.54 mm (or 2.00 mm) 2×N arrays have
  regular *streets*. Inside the A7 this is channel routing, not
  open-field maze.
- **Fail-open.** 6 of 8 boards is a success. Drop LS before USB
  or power; drop a **whole board** rather than keep 7/8 of every
  board.
- **GND can pour.** Do not spend maze budget on the return.

PCB literature already splits this (Yan & Wong, ICCAD 2010
tutorial): **escape** (pins ↔ a component boundary, often network
flow) and **area** (between escaped buses). For us the A7
*interior* is reverse-escape into a pin array; the rest of the
panel is area / funnel. That split is the useful SOTA, not
Freerouting.

### Theory worth stealing (still implementable)

**Maze / A\*.** Lee (1961) is BFS on a grid — optimal for *one*
net, slow, memory-heavy. Hadlock (1977) costs *detours* instead
of length and expands toward the target. Soukup (1978) shoots a
line at the target and falls back to maze. A\* (Hart–Nilsson–
Raphael 1968) with Manhattan + a small bend penalty is the
practical form. Sequential over nets is the standard greedy.
Order matters (Kastner; every textbook). First nets hog the A7
mouth.

**Pattern routing** (Kastner 2002, and every FPGA/PCB global
router since). Try an L (one bend), then a Z (two bends); accept
if the corridor is empty; else maze. Fast, less spaghetti.
Textbook L/Z **dies** once other pads are reserved — the L walks
through a pin. The cheap fix is a *street-jog*: the same L/Z
but snapped onto the 2.54 mm channels of the mate lattice.

**River / rubber-band.** If the sketch is **planar** (no two
assigned segments cross), a single-layer realization exists and
can be thickened to width given space (Maley, *Single-Layer Wire
Routing and Compaction*, 1990; Hsu’s general river router; Dai’s
SURF rubber-band, 1993). Polynomial, not a maze. Existence is
the theorem; thickening is the algorithm.

**Maximum planar subset.** Drop a cheap set of nets so the
remainder is planar, then river-route. Finding a *maximum*
non-crossing subset of segments is hard; greedy “drop the net
in the most crossings” is easy. **Net-level** drop is too
hungry (it deletes USB). **Board-level** drop is the 6-of-8
knob: delete the board whose nets cause the most crossings,
repeat.

**Ordered escape / network flow** (Yan & Wong 2010; Luo & Wong
2008). SOTA for routing out of — or into — a pin array with
capacity, including a correct diagonal-capacity model. More
mechanism than v1. The regular 2.54 mm streets are a cheap
stand-in: treat each array gap as a channel of known track
capacity.

Line-probe (Hightower, Mikami–Tabuchi) is the gridless cousin
of maze. Skip unless a grid becomes painful.

Do **not** start with negotiated congestion (PathFinder),
Freerouting’s shape-based expansion rooms, SAT, or ML/MCTS
(He 2024). Those solve a harder problem than we have.

### Playground (synthetic S3, PTH, greedy assign)

A coarse 0.5 mm grid, every terminal reserved so early nets
cannot plow through later pads, USB → power → LS, S3-style
6-pin kits + LS field. Not the corpus — just enough to feel
the geometry.

| Instance | Nets | Straight crossings | R1 maze | R2 L/Z+maze | R3 drop ≤2 boards, then maze |
|---|---:|---:|---:|---:|---:|
| A7 1-up | 8 | 4 | 5/8 | 5/8 (0 L/Z hits) | 5/8 |
| A6 4-up | 32 | 99 | 14/32 | 14/32 | 12/32 (dropped 2 boards) |
| A5 8-up | 64 | 378 | 23/64 | 24/64 | 26/64 (dropped 2 boards) |

Lessons:

1. **It really is single-layer.** No via search ever fired.
2. **The A7 pad field is the bottleneck**, not panel distance.
   USB of board 0 takes a short hug that seals a street; later
   power/LS die next to an open-looking ratsnest.
3. **Reserve every terminal before search.** Otherwise the first
   USB pair walks through the Vtarget pad and the net is
   “unroutable” for a stupid reason.
4. **Plain L/Z almost never hits** once pads are reserved
   (0/8 on A7). Pattern routing needs street jogs, not
   textbook L’s.
5. **Net-level planarize is the wrong drop** — 42 of 64 nets
   vanished on the synthetic A5, including USB. Board-level
   drop is the 6-of-8 policy. After dropping 2 of 8, 125
   crossings remained: **assignment** has to help uncross,
   not just the drop.
6. **Score boards, not nets.** 26/64 nets with 0/8 boards
   complete is not a 40 % success.
7. Sequential maze is enough to *score* S1–S7. It will not
   ship 8/8 on A5. That is in-spec.

Working combo: **S8 or S9 + R4** (edge-facing kits + 2-layer
HV A* with vias). Angular matching uses LS interchangeability
so the funnel does not cross. USB pairs get a parallel-ribbon
bias; power uses a fatter clearance. PTH sources start on
either layer; mate pads finish on bottom.

Latest corpus (bottom-side Ø 1 mm `ict`, GND poured). Coverage
is **boards fully escaped**:

| Board | TPs | Sheet | Packed | Best coverage |
|---|---:|---|---:|---|
| Renfield | 7 | A7 / A6 / A5 | 4 / 8 / 8 | **4/4, 8/8, 8/8** S8+R4 (48/48 maze on A5) |
| Feign | 6 | A7 / A6 / A5 | 4 / 8 / 8 | **4/4, 8/8, 8/8** S8+R4 |
| Demeter | 5 | A7 / A6 / A5 | 5 / 7 / 8 | **5/5, 7/7, 8/8** S8+R4 |
| Seward | 8 (USB+SWD) | A7 / A6 / A5 | 4 / 8 / 8 | **4/4, 8/8, 8/8** S8+R4 (16/16 USB, 16/16 power, 24/24 LS) |

S9+R4 matches S8+R4 on that maze. S2+R4 is worse on Seward A5
(3/8) because the old core packing still clogs the A7 mouth.
Single-layer R2 stays far behind (often 0–3/8).

Those 8/8 numbers are **not** a shippable solution. They mean
“a 0.4 mm 2-layer A\* found some path.” They do not mean the
traces are clean, DRC-clean, 2 A, or 90 Ω. See **Handoff**.

### Three implementations to try

**R1 — sequential A\* maze.**
Grid the bottom (~0.15–0.25 mm, half the minimum space). Paint
**all** pogo pads, mate pads, tooling holes, and the outline
blocked; open only the current net’s two terminals. Route in
order: USB pairs (DP then DM immediately, same clearance),
Vtarget/VUSB (fatter), skip GND if poured, then LS. A\* with
Manhattan + bend penalty. Fail-open: if a net has no path,
leave it and continue. Optional: rip up the last *k* LS (or
the LS of that board) if a USB/power net fails.

Baseline. A day of work. Good enough to put a number on G1
for S1–S7.

**R2 — street pattern, then maze.**
Same order and fail-open as R1. For each net, try:

1. axis-aligned L / Z **if the corridor is empty**;
2. the same L / Z snapped onto the mate’s 2.54 mm streets
   (enter the nearest channel, ride it, exit at the pad);
3. A\*.

USB is a **pair pattern**: two parallel streets, length-mismatch
checked before commit. Playground textbook-L died; this is the
version that should actually fire. Implement after R1 if the
maze is slow or the A7 traces look like spaghetti.

**R3 — board-planarize, river the funnel, street-unpack the A7.**
Three cheap steps, each polynomial or greedy:

1. **Drop boards, not nets.** While the straight-line sketch
   has crossings and we still have more than \(k\) boards
   (default \(k = n-2\)), delete the board whose nets
   participate in the most crossings. USB/power crossings
   count extra so a messy LS board goes first.
2. **River the funnel.** Cut at the A7 boundary. Sort kept
   nets by the angle of the pogo (or by the intersection
   with the cut). Route that ordered bundle through the
   three A7 edges as a river — capacity is not the issue
   (~80 nets vs ~200 mm of edge at 0.5 mm pitch). This is
   Hsu/Maley in spirit, not a full rubber-band engine.
3. **Street-unpack** inside the A7: each net is already
   attached to a boundary slot; walk the lattice streets to
   its mate pad (channel routing). Fall back to A\* per net
   if a street is full.

Guarantees the *kept boards* are a planar sketch before
detailed routing. Natural home for “only 6 of 8.” Also a
*pre*-pass that can tell the matcher which boards to ignore.

R3 can later feed **assignment**: add a crossing penalty to
\(c(d,s)\), or solve a non-crossing matching for USB/power
first (min-cost non-crossing matching is DP when the two
point sets are linearly separable — pogos-outside vs
A7-inside often are). That coupling is not v1.

### Policy

- Success = each *kept* net has a legal bottom trace.
  Report **boards fully escaped**, nets dropped by kind, and
  which boards were sacrificed. Do not quote net-% alone.
- Never fail the whole panel because one LS pad could not
  escape.
- USB and power outrank LS when something must be dropped.
  Prefer dropping a board to dropping the USB of every board.
- G1 metrics (\(V, L, U_\Delta, \ldots\)) still apply. PTH is
  free; **extra** vias on R4 are real and currently unchecked.
- Pair DP/DM in the router, not just in the matcher.

R1 scored the problem. R4 is what completed A5. Neither is
a layout. Next work is streets + a real board file, not
another maze.

## Subproblems (still sequential)

1. **Contact extract** — **done** (`Ict` on Ø 1 mm `TestPoint` only).
2. **Demands + Hall + match** — the model above.
3. **A7 mate pattern** — several constellation strategies; score
   escape and array-connector fit.
4. **Route** — started as PTH → single-layer (R1); POC also
   ran R2/R3 and a 2-layer via maze (R4). Not a fab router.
5. **Hook** `--interposer` on board-array create — **not done.**

The base tile (ADG2128, MAX4999, switches, host) is a parallel
hardware project against the same mate contract. It is not a
prerequisite for (1)–(3).

## Bottom pad patterns

104 lands, usable A7 interior 64 × 95 mm (5 mm tooling margin).
Every land sits in a **4 / 6 / 8-pin** pogo array (2×2, 2×3, 2×4) at
one pitch (try 2.54 mm first; 2.00 mm as a variant). No singleton
pogos on the bottom.

104 = 13×8 = 26×4 = 8×6 + 7×8. Those identities are the palettes.

The A7 constellation **builds** `Slot`/`MatePin`. Evaluation waits
for the autorouter, but the generators and the score can be written
now. G0 metrics (below) do not need a router.

### Strategies

**S1 — thirteen 8-pin blocks, by function.**
6× LS (48) + 2× USB (16) + 2× Vtarget (16) + 1× VUSB (8) + 2× GND
(16). Cleanest connector count. USB is four pairs per 8-pin (no
dedicated GND in the pair). Place LS as a field, power in a row,
USB in a row.

**S2 — USB as 4-pin with GND reference.**
8× (DP, DM, GND, GND) uses all 16 USB lands and all 16 GND lands.
Then 6×8 LS, 2×8 Vtarget, 1×8 VUSB. Better 2-layer HS (local
return). GND slots for matching are those USB-adjacent grounds
(still one pour).

**S3 — eight 6-pin board columns + LS field.**
Per board: `(DP, DM, VT, VT, VUSB, GND)`. Uses 8 USB pairs, 8
Vtarget banks, 8 VUSB, 8 GND. Remaining 48 LS + 8 GND → 6×8 LS and
2×4 GND. Matches the electrical story (one power/USB/gnd kit per
DUT, LS is a global pool).

**S4 — eight 8-pin board columns + leftover LS.**
Per board: S3’s six plus two LS. 16 LS sit “near” a board; the
other 32 LS + 8 GND stay in shared 8-pin blocks. Assignment is
still a global LS pool — the extra two pins are just closer to
that board’s other nets.

**S5 — funnel vs origin USB.**
Same grouping as S1 or S3; only placement changes. **S5a:** USB
arrays on the A7 edges that face the rest of the panel (where A5/A6
traces enter). **S5b:** USB on the origin/outer edges (short on the
base, longer on the interposer). Power next to USB; LS fills the
core.

**S6 — regular lattice, color later.**
Pack as many 2×4 footprints as fit, pick 13 (or 26× 2×2), paint
them USB / power / LS after the fact. Regular escape, dumb
function. Control for “clever placement actually helped.”

**S7 — pitch variant, not a new packing.**
Replay S1 and S3 at 2.54 mm and 2.00 mm (and mixed: 2.54 power,
2.00 USB). Same topology, different density.

**S8 — edge-facing kits (implemented).**
Eight 6-pin kits `(DP, DM, VT, VT, VUSB, GND)` on the A7 **+X
and +Y edges** (the sides that face the rest of an A5/A6
panel). LS in the core with ~5 mm streets. Same electrical
grouping as S3; only placement changes. This is S5a with S3
kits.

**S9 — board-aligned kits (implemented).**
Same kit as S8, but the eight perimeter sites are assigned in
the **angular order of board centroids** so a board’s USB/power
land near the A7 edge that faces that board. LS still a global
pool in the core.

Orientation of the A7 (74×105 vs 105×74) is a boolean on every
strategy, not its own family. S4–S7 were not coded.

### Eval corpus

Same contact sets, every strategy:

| Fixture | Why |
|---|---|
| A7, 1 board | identity, almost no funnel |
| A6, 2–4 boards | medium |
| A5, 8 boards | worst concentration into the origin A7 |

Use real diodehub panels once they have Ø 1 mm `ict` TestPoints.
Until then, synthetic contacts at real board XY are enough for G0.

### Metrics

Hard (any fail → disqualified, record the reason):

- Hall holds for that panel
- every land is in a 4/6/8 array at the strategy’s pitch
- 5 mm A7 tooling margin kept
- USB pair not split across arrays; polarity preserved
- after route: bottom-layer DRC clean, power width ≥ 2 A
  (PTH pogos; no signal vias)

Soft (lower is better unless noted):

| Symbol | What |
|---|---|
| \(V\) | via count |
| \(L\) | total routed length (mm) |
| \(L_\max\) | longest net (A5 funnel) |
| \(U_\Delta\) | max USB pair length mismatch (mm) |
| \(U\) | mean USB pair length |
| \(P\) | mean Vtarget/VUSB length |
| \(A\) | assignment cost \(\sum c(d,s)\) (mm, no router) |
| \(N_\text{arr}\) | number of pogo arrays (base complexity) |
| \(W\) | unused pins in those arrays |
| \(X\) | estimated crossings (Manhattan, pre-route) |

### Score

Two gates. **G0** (now): hard packing + Hall + \(A, U_\Delta, L_\max, N_\text{arr}, X\)
from the matcher and a Manhattan sketch. **G1** (when the router
exists): hard DRC + the routed \(V, L, U, P\).

Disqualified strategies rank last. Among survivors, min-max each
soft metric across (strategy × panel), then

\[
\begin{aligned}
S_0 &= 3\,\widehat{A} + 4\,\widehat{U_\Delta} + 2\,\widehat{L_\max}
      + \widehat{N_\text{arr}} + \widehat{X} + 0.5\,\widehat{W} \\
S_1 &= S_0 + 3\,\widehat{V} + 2\,\widehat{L} + 2\,\widehat{U} + \widehat{P}
\end{aligned}
\]

Hats are \((x - x_{\min}) / (x_{\max}-x_{\min})\) on that corpus
slice (0 if degenerate). Lower \(S\) wins. Weights put USB mismatch
and vias above connector count; change them only if a winner is
obviously wrong on inspection.

Report the table, not just \(S\): a strategy that routes A7 and dies
on A5 is not the winner. Pick the one with the best worst-panel
\(S_1\), ties broken by fewer arrays.

## POC campaign (what we actually ran)

Code lives in `crates/pcb-interposer`. Driver:
`interposer-poc --corpus <demo> --out <dir>`. Not wired to
`pcb ipc2581 board-array`.

### Focus

Prove the **pipeline**, not a pretty board:

extract bottom Ø 1 mm `ict` TestPoints → bundle / Hall /
Hungarian assign → A7 constellation → autoroute two-pin nets
on A5 / A6 / A7 packs (≤ 8 boards, 1 tile).

The question we were answering: *can we get every tagged
board on a panel electrically to the A7 mate, using the
flexibilities we already have* (LS/GND interchangeable, USB
as an ordered pair, Vtarget \(S_2\), PTH on both layers,
GND poured, dropping boards allowed, 2-layer if 1-layer
dies)?

We were **not** answering: emit a KiCad interposer, pass
KiCad DRC, hold 90 Ω, size 2 A copper, or look like a human
laid it out. Those got treated as “later.” That is why the
results can look like 8/8 and still be unsatisfactory.

Corpus: diodehub demo boards we tagged. **Renfield** (7 TPs,
no USB pair), **Feign** (6, power+LS), **Demeter** (5, no
USB), **Seward** (8, including `usb_dp`/`usb_dm`). amoeba /
whisper had no Ø 1 mm `ict` TPs. Packing is a naive n-up,
**not** `board-array create`.

### What we tried

| Knob | What | Outcome |
|---|---|---|
| S1 / S2 / S3 | Functional blocks in the A7 core | Easy to generate. Core packing **clogs the A7 mouth**. S2 slightly better for USB+local GND. |
| S8 / S9 | Kits on the +X/+Y A7 edges; S9 angular | **This is the placement win.** Same electrical story as S3, much easier funnel. S8 ≈ S9 on this corpus. |
| S4–S7 | 8-pin columns, lattice, pitch | **Not implemented.** |
| Angular + crossing cost on \(\alpha\) | \(c = \) Euclidean \(+ 30\,\Delta\theta + \) 8 mm per already-bound crossing | Uses LS interchangeability. Helps the ratsnest. Does **not** uncross after copper is committed. |
| R1 sequential A\* | 0.4 mm grid, one layer, USB→power→LS | Baseline. A7 pad field kills later nets. Ugly. |
| R2 L/Z + street-jog | Pattern first, then A\* | Textbook L almost never hits once pads are reserved. Coverage still poor. Slightly cleaner when it hits. |
| R3 board-planarize + drop | Drop crossing / incomplete boards, retry | Does what it says: 5/8 instead of 0/8. **Does not raise quality.** River/street-unpack from the note was never written; R3 is drop + R2. |
| R4 2-layer HV A\* | Layer 0 prefers X, layer 1 prefers Y, vias, USB ribbon bias, fatter power halo | **This is the connectivity win.** S8+R4 and S9+R4 went **80/80 boards, 440/440 maze nets** on the tagged corpus. Traces are still maze spaghetti. |
| PTH-only vias vs extra vias | First campaign: no extra vias. Later: vias allowed | Extra vias are why A5 completes. They are **not** DRC’d (no drill, no annular, no via-to-via). |
| GND as maze nets | Counted as routed | Inflated success. Fixed: GND is a pour, reported separately. |
| Front-side TPs | First extract accepted `F.Cu` | Wrong for a press-down fixture. Extract is **B.Cu only**. Demo TPs were flipped to bottom for the POC. |
| Edge.Cuts bbox | Packed Seward from a 32×14 mm outline while extra TPs sat outside | Overlapped copies, 0/8. Union bbox with TP XY fixed the pack. That “8/8 Seward” is on a **small** board with stuffed TPs, not a large DUT. |
| Score \(S_0/S_1\) | Min-max of length / vias / … | **Actively misleading.** Failed short routes win. Rank **boards complete**, then kind coverage. |

### Numbers (last run, S8/S9/S2 × R4/R2)

Board-complete / maze-complete, GND poured separately.
Recommended combo **S8+R4** (S9+R4 identical here):

| Strategy | Boards | Maze | USB | Power | LS |
|---|---|---|---|---|---|
| **S8+R4** | **80/80 (100 %)** | 440/440 | 40/40 | 120/120 | 280/280 |
| **S9+R4** | **80/80 (100 %)** | 440/440 | 40/40 | 120/120 | 280/280 |
| S2+R4 | 71/80 (89 %) | 431/440 | 40/40 | 111/120 | 280/280 |
| S2+R2 | 22/80 (28 %) | 281/440 | 39/40 | 76/120 | 166/280 |
| S8+R2 | 19/80 (24 %) | 286/440 | 40/40 | 97/120 | 149/280 |

Per panel, S8+R4:

| Board | A7 | A6 | A5 |
|---|---|---|---|
| Renfield (7 TPs) | 4/4 | 8/8 | 8/8, 48/48 maze |
| Feign (6) | 4/4 | 8/8 | 8/8 |
| Demeter (5) | 5/5 | 7/7 | 8/8 |
| Seward (8, has USB) | 4/4 | 8/8 | 8/8, 16/16 USB, 16/16 pwr, 24/24 LS |

Single-layer never generalized. 2-layer + edge kits is what
moved the **count**. It did not move **quality**.

## Handoff (results are not good enough) — **superseded**

Everything below in this section described the R4 state. The
follow-up campaign (see **R5 + S10** at the end of this note)
did items 2, 3, 4 and 7 of the list, replaced the router, and
deleted R1–R4. Kept for the reasoning record.

The next person should treat S8+R4 as a **connectivity
existence proof**, not a layout. The interposer is still
unshipped.

### What we were trying to do

Stand up an e2e generator that, given tagged DUT TPs and a
panel size, produces an A7-mate assignment and copper from
every pogo to a mate land. Iterate placement and routing
until almost every board on A5/A6/A7 actually escapes.
Prefer 2-layer. Use the loose netlist (any LS land, any GND
land). Handle USB as a pair and power as a fat net.

### What worked

- **`ict` on Ø 1 mm `TestPoint`**, Path-keyed IPC, bottom-only
  extract. That identification problem is solved.
- **Hall + per-kind Hungarian** is the right matcher. Keep
  the commuting square. Do not introduce SAT for v1.
- **Edge-facing kits (S8/S9)** beat core blocks (S1–S3). The
  A7 mouth was the real bottleneck; moving USB/power to the
  funnel edges is the placement lesson.
- **2 layers + vias (R4)** is what made A5 8/8 possible.
  Single-layer greedy cannot be rescued by more maze.
- **Score boards and kinds, not net-%.** GND must not count
  as a maze win.
- **Pack bbox must include the TPs**, not only Edge.Cuts.

### What did not work

- **Single-layer R1/R2** — 24–28 % boards. First USB hugs a
  street shut; later power/LS die. L/Z almost never fires.
- **R3 drop-boards** — honest 6-of-8, zero aesthetic value.
  The river / rubber-band / street-unpack from the note
  above was **not built**.
- **“100 %” on R4** — a 0.4 mm grid found a path. Routes are
  HV maze, not buses. USB “pair bias” is a distance-field
  nudge, not length-matched 90 Ω. Power “fat” is a larger
  halo, not a 2 A width. Vias have no drill/annular/clearance.
  No KiCad DRC. No poured-GND islands check. No length
  report you should trust for HS.
- **\(S_0/S_1\)** — ranks the *worst* connectivity winner
  (short failed nets). Do not use it to pick a strategy.
- **Naive n-up** — not `board-array create`. Seward’s 8/8 is
  on a ~32×16 mm cell with stuffed TPs. A real large DUT
  packed by the production arrayer will look different.
- **No interposer artifact** — no `.kicad_pcb`, no zones, no
  tooling copy, no `--interposer` flag.
- **Tiny tagged set** — four boards. No Tag-Connect, no 1.5 mm
  TPs, no A4, no 2-tile.
- **Assignment still per-kind.** Cross-kind QAP (USB pair
  crossing a Vtarget bank) is untouched.
- **Visuals** — 72 ratsnest-and-maze SVGs. Useful to see
  vias (yellow squares) and layer (dashed = top). Not useful
  as a design review of a board you would fab.

### What to do next (in order)

1. **Emit a real interposer** from one happy-path (Seward or
   Feign, A7, S8+R4): outline, PTH pogos, mate footprints,
   traces, vias, GND zones. Open it in KiCad. That will
   immediately show how bad the maze is.
2. **Replace maze-in-the-A7 with street/channel unpack**
   (the R3 step we skipped). Funnel is a river onto the A7
   edge; the lattice is channels. A\* only for leftovers.
3. **USB as a true pair:** same layer, parallel, gap from a
   90 Ω target, \(U_\Delta\) budget, vias only as a matched
   pair or not at all.
4. **Power as a width, not a halo.** 2 A at the stackup’s
   oz. Prefer no via. Thermal/clearance to the pour.
5. **Couple assignment to routing.** After a failed net,
   swap two LS lands and retry (the note already allows
   this). Angular matching is not enough.
6. **Drive packing from `board-array create`**, same
   transforms the panel will actually use. Re-score S8/S9
   on that geometry before trusting 8/8.
7. **Throw out \(S_1\)** or invert it so failed nets cannot
   win. Primary gate: boards complete, USB pair legal,
   power width legal, KiCad DRC clean.
8. Only then consider 4-layer or Freerouting. 2-layer is
   not disproven; **this router** is.

Do not spend another cycle adding S6/S7 or another A\*
variant. The connectivity experiment is done. The layout
experiment has not started.

## R5 + S10 campaign (2026-08-19)

Second campaign. Goal: replace the maze with a router whose
output looks like a board a person would sign off, handle USB
as true diff pairs and power as real 2 A copper, and pick the
bottom constellation we would actually commit to. R1–R4 and
the S0/S1 score are **deleted**; `route.rs` keeps only the
shared net/trace types.

### The router (R5, `router.rs`)

One engine, four stages. Design cross-checked against what is
still SOTA-but-simple: PathFinder-family negotiation, KiCad
PNS diff pairs, Freerouting pull-tight.

1. **Octilinear A\*** on a 0.4 mm 2-layer grid with direction
   in the state: step 10/14, 45° turn +6, 90° +30, >90°
   forbidden, via +90 (and a via needs a free 3×3 on both
   layers — a barrel is wider than a thin trace). Per-class
   maps with real widths and clearances: LS 0.25/0.2 mm,
   power 0.9 mm (2 A at 1 oz per IPC-2221) /0.25, USB ribbon
   envelope 0.55/0.2. Static blockage (pads, holes, edge) is
   exemptable near a net's own terminals — with deny disks so
   a foreign pad inside the exemption keeps its blockage;
   committed routes are never exemptable. That split is what
   finally made "routed" mean "legal".
2. **Rip-up by reordering.** Greedy passes; failed nets are
   promoted to the front of their class next pass; keep the
   best attempt (fewest failures, then loose pairs, then
   vias). Converges in ≤ 3 passes on this corpus.
3. **USB pairs as one centerline ribbon.** DP/DM collapse to
   a single fat net; gateway waypoints hold the ribbon
   perpendicular to both pad axes and pick the waypoint side
   so polarity never twists; every kit's entry corridor is
   pre-reserved so nobody parks across it; the fixed tails
   are validated before the A\* runs. After smoothing, offset
   ±(w+gap)/2 into two rails — parallel by construction —
   then triangular bumps on the shorter rail (clearance-
   checked, split into smaller bumps when tight) length-match
   the pair. If no legal ribbon exists (pogo sitting on the
   band), the pair falls back to two individual traces and is
   counted in `loose_pairs`, never silently.
4. **Gridless smoothing + self-DRC.** Line-of-sight
   shortcutting per layer run against an exact capsule world
   (two global rounds), so traces become a few long straight
   segments; then every final segment is distance-checked
   against everything else. `drc` and `min_gap` in the score
   are measured, not assumed.

### The constellation (S10 "ring")

All 104 lands in a double-row band along the two funnel-facing
A7 edges: 15 structures, alternating 8 six-pin kits
(outer column DP DM VUSB, inner VT VT GND) and 7 2×4 LS
arrays (8 of their inner pads are the remaining GND). No core
to clog; every land is at most one pad deep from open copper.
This is the S5a idea taken to its limit, and it beats S8/S9
on every axis at once (vias, bends, detour, completion).

### Physical conflicts, suppression, fail-open

A pogo is a PTH drill through the interposer; a board sitting
over the mate band puts drills onto lands. Slots whose lands
sit within a per-kind radius of any pogo (LS 1.45 mm, USB
1.4, power 1.65 — pad + clearance + trace half + grid slack)
are **suppressed** before matching; the pins stay as routing
obstacles. If suppression breaks Hall, the driver drops the
board that kills the most lands and retries — the 6-of-8
policy, now driven by real geometry instead of crossings.

### Score (replaces S0/S1)

`quality_score`, lower is better: 1000·(boards incomplete
fraction) + 200·DRC + 30·loose pair + 8·UΔ + 25·(detour−1)
+ per-net via/bend terms. Completion gates; a failed net can
never help a strategy win. Metrics now measured on final
copper: vias, bends (>30°), sharp bends (>60°), detour
(routed / Euclidean), routed pair mismatch UΔ, DRC count,
min gap.

### Results (tagged corpus × A7/A6/A5, S10+R5)

| Board | A7 | A6 | A5 | notes |
|---|---|---|---|---|
| Renfield | 4/4 | 8/8 | 8/8 | 0 DRC everywhere |
| Feign | 4/4 | 8/8 | 8/8 | |
| Demeter | 5/5 | 7/7 | 8/8 | 0 sharp bends |
| Seward (USB) | 4/4 | 7/8 | 7/8 | 1 board each physically on the band; 3+1 loose pairs |

S10 aggregate: **78/80 boards, 100 % of kept nets, 0 DRC
violations, min gap ≥ 0.2 mm, UΔ ≤ 0.10 mm on every ribbon
pair**, detour 1.05–1.10, ~0.3 vias/net. Versus R4 on the
same corpus: 1935 right-angle bends → 269 sharp bends (almost
all USB pad breakouts), 1628 → 709 vias (S10 alone: 159),
detour 1.23 → 1.08, and R4's numbers were on copper that was
never legality-checked at all. S8/S9 under R5 lose one LS or
board on several panels and never beat S10 on quality.

The two lost Seward boards are not router failures: their
pogo drills land on kit lands, so no fixed constellation can
serve them at that panel position. That is a packing-level
fact the board-array integration will have to own (keep the
mate corner clear, or accept n−1 boards on dense panels).

### What R5 changed about the earlier conclusions

- "2-layer + vias is what moved the count" still holds, but
  most S10 routes are near-planar: the ring removed the A7
  mouth, and via counts fell 5–10×.
- Sequential greedy is enough **when legality is honest and
  ordering retries exist**; PathFinder-style soft negotiation
  was designed but not needed at this scale.
- The matcher's job grew: suppression means the constellation
  adapts to each panel by *assignment*, not by moving copper.

## Terminal-via revision (2026-08-19, later the same day)

Three directives changed the physical model and paid off
everywhere: **no free vias** (layer changes only *at* a net's
own pads), **SMT pogo pads** (top copper, via optional — no
drill through the interposer), and **do the diff-pair ends
properly**.

### The model

Pogo pads are SMT on top; mate lands are SMT on bottom. Every
net therefore crosses layers exactly once, through a via-in-pad
at one of its own terminals: a **top run** dives at its mate
land, a **bottom run** rises at its pogo pad. Nothing else
drills the board. Consequences:

- The router lost a dimension. No z in the A* state, no via
  transitions, no via costs — each net is a **single-layer
  2-D path** plus a layer choice. Both layers are searched
  and the cheaper one wins, after a handicap: pairs and power
  lean top (the bottom pour stays whole), LS follows an H/V
  discipline (horizontal-dominant nets lean top, vertical
  bottom) because without mid-route vias a long trace is a
  *wall*, and crossing nets must land on opposite layers.
- The pogo-drill-versus-land collision class **vanished**.
  Suppression, per-kind conflict radii, and the fail-open
  board drop are all deleted; Seward A6/A5 route 8/8.
- Via legality is a real constraint with real teeth: a barrel
  exists on both layers, so it must clear every foreign pad on
  either side, committed copper (checked against the dynamic
  maps in exactly the extra ring a barrel adds over a trace),
  and earlier traces must not park on an assigned land's via
  spot (those spots are statically reserved). The matcher
  refuses via-locked pairings up front (`via_feasible` in the
  assignment cost), which is LS interchangeability doing load
  bearing work again.
- The bottom is a near-solid GND pour with most signal on top
  — microstrip over a return plane, which is what the USB
  ribbons wanted anyway.

### Diff pairs, done properly

Read the actual KiCad PNS source (`pns_diff_pair.cpp`,
`pns_meander.cpp`, `direction_45.cpp`) and took its geometry:

- **Two-stage entry (DP_GATEWAY):** both rails leave their
  pads straight along the pair-axis perpendicular by a fan
  distance (0.9 mm), then one slanted segment each converges
  the separation from the pad pitch down to the 0.35 mm rail
  gap at anchors g/2 either side of the trunk waypoint. Every
  corner is obtuse by construction; polarity still can't
  twist. The stubs are validated before search, stamped after,
  and live in the capsule world so no later net can cross a
  taper (that was a real DRC leak in the first cut).
- **Constrained trunk:** the A* seeds the entry direction and
  restricts the arrival direction (KiCad's allowed-entry-angle
  mask), so the trunk never kinks against the stubs; smoothing
  pins the trunk's first and last segments for the same
  reason. All waypoint variants are scored and the cheapest
  wins — taking the first workable one produced hooked
  approaches.
- **Length matching as 45° chamfered trapezoids** (KiCad's
  meander shape, amplitude solved from the needed extra
  length), placed on the shorter rail away from the
  centerline, clearance-checked, fewest-first to a 0.25 mm
  target.

### Results (S10, full corpus)

**80/80 boards. 426/426 kept nets. 0 DRC violations. 0 free
vias — one terminal via per net by construction.** Ribbon
UΔ ≤ 0.25 mm except one crowded Seward A6 pair at 1.24 mm
(the conservative spec budget); two Seward A5 pairs go loose
because one rail can only via at its land while the other can
only via at its pogo — a coupled ribbon cannot mix modes, and
the loose fallback is the honest answer. Detour 1.04–1.19
(Seward A6 1.29). Runtime ~40 s for the 36-case corpus.

Trade recorded honestly: versus the PTH model the bend count
rose (the H/V split and single-layer walls cost some
directness — Seward A6 96 → 196 bends) in exchange for **zero
free vias, zero drops, and a fabrication-realistic pad
model**. Everything else improved or held.

### Fold-correct mate orientation, no length tuning

Two corrections after review:

- **The mate follows the ISO fold.** Each halving cuts the
  sheet's long side, so the A7 descendant at the origin corner
  alternates orientation: A7 → 74×105, **A6 → 105×74**,
  A5 → 74×105 (`mate_dims`). Patterns are generated
  canonically and rotated 90° into the folded footprint
  (`orient_pattern`) — a proper rotation, never a mirror; the
  mate is a rigid contract. Of the two fold-valid rotations we
  commit to the one with the long band along the sheet-edge
  strip: it scored strictly better than the "funnel-facing"
  alternative (80/80 vs dropped nets), because boards cover
  the whole sheet anyway and the quiet margin wins. Everything
  that assumed a fixed 74×105 mate (gateway normals, the
  matcher's angular term, the viz outline) now derives its
  center from the folded region or the pins themselves.
- **Length matching is deleted.** The rails are parallel by
  construction, so residual intra-pair skew is a couple of
  corner miters — measured at ≤ 0.65 mm untuned on the whole
  corpus, far inside any USB 2.0 budget. The meander
  machinery wasn't worth its copper or its code; UΔ is now a
  reported number and a ranking tie-breaker only.

**Connector formats are now a hard rule**: 2.54 mm pitch, and
only **2×3** and **2×4** blocks — every eval constellation is
exactly 8× 2×3 kits + 7× 2×4 LS arrays, and a test asserts
both the formats and the pitch. `Pattern` now carries the
per-connector pin groups (`arrays`, derived from adjacency),
and the viz draws each connector body (amber = kit, teal = LS
array) plus the board outlines, so an assembly panel reads as
boards and connectors rather than floating test points.

### Connector spacing — S11 wins (2026-08-19, cont.)

S10's 3.2 mm pin-gap leaves ~zero housing-to-housing room, and
a single L-band cannot give more (15 structures on ~147 mm of
band). The generators were refactored into one engine (a
structure list of the 15 kits/arrays + a band walker /
site grid) and two roomy layouts added:

- **S11 — perimeter ring**: double-row bands along **all four**
  mate edges, **9 mm between connectors**.
- **S12 — cluster grid**: 3×5 structure sites across the mate
  interior, ≥ 11 mm streets in every direction.

Getting these to route exposed the last via-legality bug: the
old check probed the *dilated* grid maps and false-rejected
via spots whenever a perfectly legal trace passed nearby (a
0.4 mm-wide phantom band). It now checks exact distances
against a committed-copper registry. That fix alone recovered
several nets on every layout.

| Strategy | Boards | Loose pairs | Worst-panel Q |
|---|---|---|---|
| **S11 perimeter, 9 mm** | **80/80** | **0** | **16.4** |
| S10 L-band, 3.2 mm | 80/80 | 2 | 71.4 |
| S12 grid, ≥11 mm | 79/80 | 1 | — |

**S11 is the new committed constellation**: full corpus, zero
loose pairs, zero DRC, one via-in-pad per net, real housing
clearance, and the best worst-panel quality. Kits spread
around the whole perimeter also suit the angular matcher.

### Symmetric S11 + the multi-pass router (2026-08-19, cont.)

Two more directives closed out the day: make S11 symmetric
(extra lands are fine), and kill the remaining routing jank
with a multi-pass architecture.

**Symmetric S11.** Four structures on each of the four mate
edges, every band mirror-ordered `[A K K A]` and centered:
8 kits + 8 arrays with mirror and 180° placement symmetry.
The sixteenth structure is a **spare all-GND 2×4** — eight
extra pour-connected lands (112 total, 104 electrical) that
buy the symmetry. Still only 2×3/2×4 blocks at 2.54 mm,
asserted by test.

**Multi-pass router.** The grid phase became commit/uncommit
(dynamic maps are counters; committed copper lives in an
exact registry) and runs:

1. **Greedy** in class order, longest first, cheapest of both
   layers per net.
2. **Quality sweeps** (Freerouting-style): rip each net up,
   worst cost first, reroute in the finished context, keep
   strictly better only; ≤ 4 sweeps, stop when a sweep
   accepts nothing. This is where unlucky-order detours
   straighten.
3. **Escalating rescue** for nets still failing, all atomic
   with full revert: retry in freed space → **reassign** to
   any free land of the same kind (LS/VUSB
   interchangeability) → **peer swap** (exchange lands with a
   routed same-kind net and reroute both — full-pool kinds) →
   **shove** (cheapest soft path with occupied cells costed,
   rip only the crossed nets, route, put them back).

Plus: 0.2 mm grid (a 2 mm TP cluster leaves a 0.19 mm legal
sliver a 0.4 mm grid never samples), KiCad turn-cost ratio
(45° = one step, 90° = 3×), a 45° two-segment **bypass
operator** in smoothing (collapses staircases a straight
shortcut can't), and land via-reservations became dynamic
with a lifecycle (reserved while unrouted, lapse at commit)
so they never over-protect.

**Corpus grew**: `mockingbird-feather` (Feather, CC3501E +
on-board CMSIS-DAP probe, 50.8×22.9 mm) joined with 8
injected ict pads — gnd, vtarget(V3V3), vusb(VBUS),
usb_dp/dm (to the DAP probe), and the G0's swdio/swclk/nrst.

**Result: 288/288 boards, 1656/1656 nets, 0 DRC, 0 loose
pairs, 0 free vias** over 5 boards × A7/A6/A5 × 3 strategies;
S11 ranks first (worst-panel Q 15.4). Sharp bends: 70 across
all 15 S11 cases (six panels have zero). GND confirmed
end-to-end: extracted, 24 lands in the constellation, never
routed — poured on the bottom, stitching vias deferred.
Runtime ~1:45 for the 45-case corpus.

## KiCad emission — 15/15 boards DRC-clean

`emit.rs` writes each S11 case as a real `.kicad_pcb`
(KiCad 10 format) plus a sibling `.kicad_pro`: sheet outline,
NPTH tooling holes, SMT pogo pads (F.Cu) and mate lands
(B.Cu) as one-pad footprints, per-net traces with their
in-pad terminal vias (0.6/0.3 signal, 0.8/0.4 power), GND
pours on **both faces**, and design rules + a USB netclass
that legalizes the 0.15 mm intra-pair gap. The external
check is `kicad-cli pcb drc --refill-zones --severity-error`
(10.0.5), violations *and* unconnected items.

GND became fully concrete here: every GND pogo takes a via
into the bottom pour (in-pad when legal, else a ring-searched
spot with a short top stub), and every GND land takes one
into the **top** pour the same way — the bottom fill
fragments around dense trace fields, and the top pour is the
bridge that makes GND one net. Fill connectivity is what
forced the second zone.

The DRC loop then burned down a systematic list — each fix a
structural invariant, not a tweak:

- **Land binding**: a routed contact binds the land its trace
  actually terminates on (grid endpoints are ≤0.15 mm off pad
  centers, so match ≤0.3 mm at the far terminal only — pogos
  can hover ~0.5 mm over a foreign land).
- **Polarity untwist by land swap**: DP/DM lands are
  interchangeable, so a side mismatch between the two gateway
  ends swaps the members' dst lands instead of forcing the
  trunk into a hairpin. Both source-normal signs became
  candidates.
- **Attempt snapshots**: the kept attempt's routables are
  snapshotted with its paths — later attempts keep
  reassigning lands, and emitted geometry must agree with the
  members it was routed against.
- **Offset joins**: `offset_polyline` grew inner-trim /
  outer-bevel joins (trim clamped near the corner). The old
  scaled miter spiked rails ~0.42 mm outside the modeled
  0.275 mm envelope at sharp corners — the single biggest
  source of KiCad-only shorts, invisible to the self-DRC
  because the *model* was fine and the *geometry writer*
  wasn't.
- **≤90° trunk corners**: A* never turns more than 90°, and
  that invariant is exactly what keeps offset rails inside
  the envelope and un-crossed — so a smoothed pair trunk that
  loses it (shortcuts can even manufacture a 180° reversal
  into the fixed leads) reverts to its raw path.
- **Trunk vs own entry copper**: a pair's stubs commit
  together with its trunk, so neither the maps nor the world
  ever made them mutual obstacles; the trunk could legally
  ride along its own dst stub row. Candidate acceptance and
  smoothing now keep the ribbon envelope off its own
  pad→knee segments explicitly.
- **Short pairs route direct**: when the run is shorter than
  the entry geometry itself (≈7 mm), the gateways interlock —
  those pairs route as two plain traces (not counted loose);
  coupling over a couple of millimetres is electrically
  irrelevant.
- **Narrower gateway corridor** (0.6 mm half-width): the old
  1.0 mm exemption let a trunk ride over its own lands'
  terminal vias.
- Pair-lead trunks: both rails leave each gateway anchor on a
  fixed 1.2 mm straight lead along the snapped normal, so
  offsetting near the junction is a clean parallel translate.

**Result: all 15 emitted S11 panels (5 boards × A7/A6/A5)
pass KiCad DRC with 0 violations and 0 unconnected items**,
with S11 still at 288/288 boards and 1656/1656 nets across
the eval. Sharp bends collapsed as a side effect (the ≤90°
invariant): worst S11 case has 3.

## Open

- Hook `--interposer` on `board-array create` and re-score S11
  on production packings.
- Land the mockingbird-feather fixture pads in the real repo
  (they live in a corpus copy today; the zen diff is in the
  Terminal-via section's corpus note).
- Exact panel tooling we copy on the top face (diameter, positions).
- 2.00 mm pitch S11 variant if a denser mate is ever needed.
- USB polarity is treated as fixed (\(G_s = \{e\}\)) unless the mux
  path later proves otherwise.
- 4-layer remains unexplored — and now looks unnecessary.

Resolved along the way: mate orientation is settled by the ISO
fold rule (rotated on A6-class sheets); the loose-pair fallback
still exists but fires zero times on the corpus; GND lands stay
electrically one poured net while remaining physically grouped
in the 2×N connectors.

`Problem` / `hall` / `assign` / `router` / patterns are
implemented, and emission is real and externally checked. The
open list is the board-array hook.
