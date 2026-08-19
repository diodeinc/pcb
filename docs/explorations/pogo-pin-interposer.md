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
2. **Demands** — map `Ict` to kinds and *bundle* USB into ordered
   pairs. One demand per LS/GND/VUSB pad; one demand per USB pair;
   one Vtarget demand per board (up to 2 pads into one bank).
3. **Instantiate** — apply each array placement transform so demands
   exist in panel coordinates (same step math the array already has).
4. **Hall check** — per kind, \(|D_t| \le |S_t|\) and each demand
   fits its slot shape. Fail closed if a panel wants 9 USB pairs or
   3 Vtarget pads on one board.
5. **Match** — inject demands into slots. Electrically this is just
   typed injection. Geometrically it is min-cost matching, **USB
   first, then power, then LS** (see below).
6. **Emit + route** — interposer outline = panel; top pogos at
   contact XY; bottom A7 constellation; nets = the assignment; then
   autoroute. This should be an easy autoroute: two layers, pours,
   fat power, a few USB pairs, and a large interchangeable LS set
   that can absorb leftovers.

The A7 constellation itself is still a family of strategies we
score; it is not a prerequisite for writing (1)–(4).

## Assignment: notation and representation

This is **typed pin assignment** (EDA: pin swapping + differential
pair assignment). Feasibility is **injection into interchangeability
classes**. Quality is **min-cost bipartite matching** on a contracted
graph. Joint crossings across kinds are a mild QAP; v1 does not
solve that — it places kinds in priority order instead.

### Sorts

\[
T = \{\mathrm{ls},\; \mathrm{gnd},\; \mathrm{vusb},\; \mathrm{vtarget},\; \mathrm{usb\_hs}\}
\]

For each kind \(t\), \(S_t\) is the set of **slots** on the mate and
\(D_t\) is the set of **demands** from the panel. A feasible
electrical assignment is an injection \(\alpha_t : D_t \hookrightarrow S_t\)
per kind (pools are independent). Existence is Hall, and here it
collapses to counting:

\[
|D_t| \le |S_t|
\quad\text{and each demand fits in one slot.}
\]

### Slot shape

A slot has pins and a symmetry group \(G_s\) — the legal ways to
label those pins. That is the whole type system.

| Kind | \(|S_t|\) | Shape | \(G_s\) | Meaning |
|---|---:|---|---|---|
| LS | 48 | unit | trivial | any demand ↔ any unused pin |
| GND | 16 | unit | trivial | same; pour may make them one net |
| VUSB | 8 | unit | trivial | one 5 V land per demand |
| Vtarget | 8 | **unordered** 2-set | \(S_2\) | both pins swappable |
| USB HS | 8 | **ordered** pair \((+,−)\) | \(\{e\}\) | no polarity swap; unsplittable |

- **Unordered** (power bank): any bijection from the demand’s pads
  onto the two lands is legal.
- **Ordered** (USB): the demand is an oriented pair. \(D+\) lands on
  the slot’s \(+\) pin.
- **Unsplittable**: both members occupy the *same* slot. Never match
  \(D+\) and \(D−\) as two LS pads.

USB is contracted to a **supervertex** before matching. Power pads
on one board are one Vtarget demand of size \(\le 2\).

### Records

```text
Kind     = ls | gnd | vusb | vtarget | usb_hs
Shape    = unit | unordered{n} | ordered[n]

MatePin  = { id, xy }
Slot     = { id, kind, shape, pins: [MatePin] }
Contact  = { id, board, xy, net?, footprint }
Demand   = { id, kind, board, members: [Contact] }  # USB: ordered
Assign   = demand → slot
         + member → pin                    # must lie in G_shape
```

Rules by construction, not by a constraint language: USB is born as
one demand `(dp, dm)`; `Assign` is injective on slots; `member → pin`
is a bijection onto the slot pins that \(G_{\mathrm{shape}}\)
allows. The crosspoint does not appear in the interposer model — it
is why `ls` is 48 unit slots in one class.

### Matching order (preview)

Autoroute is in the loop, but the assignment should make it easy.
Place the picky, scarce, high-speed stuff first; let LS soak up
whatever geometry remains.

1. **USB** — fewest slots, polarity-preserving, impedance. Match
   pair-demands to pair-slots (min-cost on the contracted graph).
2. **Power** — Vtarget banks then VUSB (fat traces, 2 A, 8×2 / 8×1).
   Unordered pin maps: pick the cheaper of the two.
3. **Low-speed** — leftover unit slots, fully interchangeable.
   Ordinary min-cost matching (or even nearest-unused).

GND can sit with power or last; if it is one poured net, geometry
is almost free.

Do not start with a joint quadratic assignment across kinds. If a
later scorer wants to swap two LS pins or two Vtarget banks after a
failed route, that is a local improvement on this greedy order.

## Subproblems (still sequential)

1. **Contact extract** — **done** (`Ict` on Ø 1 mm `TestPoint` only).
2. **Demands** — map `Ict` → kind / bundle.
3. **A7 mate pattern** — several constellation strategies; score
   escape and array-connector fit.
4. **Match** — USB → power → LS, then emit nets.
5. **Route** the 2-layer interposer (easy autoroute if 4 is decent).
6. **Hook** `--interposer` (or similar) on board-array create.

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

None of these need to block writing an extractor or generating the
first pattern family.
