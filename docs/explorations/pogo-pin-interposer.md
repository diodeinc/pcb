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

The assignment **is** the netlist. Autoroute does not choose partners.

The A7 constellation *builds* `Slot`/`MatePin`. Mux topology does
not belong in this state.

## Subproblems (still sequential)

1. **Contact extract** — **done** (`Ict` on Ø 1 mm `TestPoint` only).
2. **Demands + Hall + match** — the model above.
3. **A7 mate pattern** — several constellation strategies; score
   escape and array-connector fit.
4. **Route** the 2-layer interposer (easy if 2 is decent).
5. **Hook** `--interposer` on board-array create.

The base tile (ADG2128, MAX4999, switches, host) is a parallel
hardware project against the same mate contract. It is not a
prerequisite for (1)–(3).

## Pattern evaluation (next interesting artifact)

Bottom strategies to generate and run against real A5/A6/A7 panels
with ≤ 8 boards, for example:

- Functional blocks: one LS field (even-wide arrays) + 8 USB pair
  slots + 8 Vtarget 2-pin arrays + 8 VUSB + GND arrays.
- Board-shaped slots: 8 identical constellations (even if the LS
  fabric is still a global pool). May waste space but makes the base
  silk obvious.
- Pitch families: 2.54 mm arrays vs 2.00 mm vs mixed (coarse power,
  finer LS).
- USB on the rim vs USB in a dedicated column (length, reference
  pour, fewer crossings with the A5 funnel).

Score each strategy on the same panels: routed / unrouted, via count,
USB pair length and mismatch, min power-trace width, whether every
land sits on a vendor 2×N array pitch, and how ugly A5→A7
concentration is vs A7→A7 (identity).

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
