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

A contact is: bottom-side, `packageRef` is `TestPoint_Pad_D1.0mm`
(ignore a KiCad `_N` suffix), and `Ict` is set.

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

Treat top pogos as **PTH**. They already exist on the bottom copper,
so every net is two terminals on **one layer**. No via planning.
This is single-layer detailed routing of two-pin nets. General
single-layer multi-net routing is NP-complete; we do not need the
general solver. Best-effort greedy is enough, and **dropping nets**
is allowed (e.g. 6 of 8 boards escape).

The assignment **is** the netlist. Autoroute does not choose partners.

The A7 constellation *builds* `Slot`/`MatePin`. Mux topology does
not belong in this state.

## Single-layer autoroute (best-effort)

Theory we actually use:

- **Lee maze (1961) / A\*** — flood or heuristic search on a grid.
  Optimal for *one* net; sequential over nets is the standard
  greedy. Order matters (Kastner, every textbook).
- **Pattern routing** (Kastner 2002) — try L then Z (one or two
  bends) before maze. Fast, predictable, used as the first pass in
  real tools.
- **River / rubber-band** (Hsu; Maley; Cole & Siegel) — if the
  sketch is **planar** (no two assigned segments cross), a single
  layer always exists and can be thickened to width. Polynomial,
  not a maze.
- **Maximum planar subset** — drop a minimum (or cheapest) set of
  nets so the remainder is planar, then river-route. Matches
  “6 of 8 boards is fine.”
- Line-probe (Hightower, Mikami–Tabuchi) is the gridless cousin of
  maze. Skip unless a grid becomes painful.

Do **not** start with negotiated congestion, SAT, or ML.

Power traces are wide (2 A); USB is a pair with a mismatch budget;
LS is whatever is left. Obstacles: board outline, A7 tooling, other
pads, already-committed traces.

### Three implementations to try

**R1 — sequential A\* maze.**
Grid the bottom (~0.1–0.25 mm, or half the minimum space). Paint
pads, holes, keepouts blocked. Route nets in order USB pairs (as
two nets, second sees the first), then Vtarget/VUSB (fatter
clearance), then GND if not poured, then LS. A\* with
Manhattan + bend penalty. If a net fails, leave it open and
continue. One optional rip-up of the last *k* LS if a USB/power
net fails.

This is the baseline. A day of work. Good enough to score S1–S7
on G1.

**R2 — pattern then maze.**
Same order as R1. For each net try an L, then a Z, accept if the
corridor is empty; else A\*. Same fail-open policy. Usually much
faster and less maze-spaghetti. Implement after R1 if the grid
search is slow or ugly.

**R3 — planarize, then river.**
Draw the assignment as straight (or Manhattan) segments. Compute
a **maximum-weight planar subset** (drop crossing nets, keep USB
and power preferentially). River-route / rubber-band the rest on
one layer. Guarantees the kept set is single-layer. Natural
home for “only 6 of 8 boards.” Use when R1 leaves a mess of
almost-crossing long nets, or as a *pre*-pass that tells the
matcher which nets to drop before maze.

R3 can also feed **assignment**: add a crossing penalty to
\(c(d,s)\), or solve a planar matching for USB/power first. That
is a later coupling, not v1.

### Policy

- Success = each *kept* net has a legal bottom trace; report
  coverage (boards fully escaped, nets dropped, by kind).
- Never fail the whole panel because one LS pad could not escape.
- USB and power outrank LS when something must be dropped.
- G1 metrics (\(V, L, U_\Delta, \ldots\)) still apply; \(V\) should
  be ~0 (PTH is the only “via”).

Ship R1 first. Keep R2/R3 as the next two knobs if coverage or
aesthetics are bad.

## Subproblems (still sequential)

1. **Contact extract** — **done** (`Ict` on Ø 1 mm `TestPoint` only).
2. **Demands + Hall + match** — the model above.
3. **A7 mate pattern** — several constellation strategies; score
   escape and array-connector fit.
4. **Route** — PTH → single-layer best-effort (R1 maze first).
5. **Hook** `--interposer` on board-array create.

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

Do not invent more until these have numbers. Orientation of the A7
(74×105 vs 105×74) is a boolean on every strategy, not its own
family.

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

## Open

- A7 at the origin: 74×105 or 105×74.
- Exact panel tooling we copy on the top face (diameter, positions).
- Whether the 16 GND lands are one net or should stay as 8×2 for
  array connectors even if poured together.
- USB polarity is treated as fixed (\(G_s = \{e\}\)) unless the mux
  path later proves otherwise.
- 4-layer only if 2-layer USB on A5 fails.

None of these need to block implementing `Problem` / `hall` /
`assign` against a toy slot set.
