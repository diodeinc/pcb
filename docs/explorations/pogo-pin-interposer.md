# Automatic pogo-pin interposer generation

Exploration notes. Nothing here is a commitment. Work is meant to go
one subproblem at a time.

The first thing we are actually generating is the **interposer**. The
base / tile electronics are a contract we design against, not this
project’s first deliverable.

## Stack

```
  assembly panel  (A5 / A6 / A7, ≤ 8 boards)
  test pads on the bottom face
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
- USB D+/D− → one of the 8 HS pair slots (pair kept together,
  polarity may or may not be swappable — assume not, until proven).
- Each DUT’s Vtarget → one 2-pin bank.
- Each DUT’s VUSB → one VUSB land.
- GND → any GND land (likely all common through the pours).

That is a much looser problem than “SWDIO must hit pin B7.” The
expensive part is **geometry**: custom top XY on an A5/A6/A7 sheet,
concentrated into the origin A7, on two layers, with 2 A power traces
and 90 Ω-ish USB pairs.

## What we already know about DUT contacts

From the demo boards, mockingbird-feather, and a diodehub sample:

- Modal debug land is Tag-Connect TC2030-IDC-NL (Ø 0.79 mm, 1.27 mm
  pitch) or discrete `TestPoint` pads (Ø 1.0 / 1.5 mm). Some DNP
  FTSH-105 headers are already documented as pogo-able.
- v1 assumes every contact we care about is on **one face**, and that
  face is the bottom of the panel in the fixture.
- A complete SWD + rail + GND + USB set on that face is uncommon.
  Allocate from the pools; do not require every role.

Extraction can start as a `.kicad_pcb` walk (footprint family + net +
side + XY), optionally joined to a Zener netlist for `Power` /
`Ground` / voltage. IPC / pcb-ir is not the first contact API.

## Subproblems (still sequential)

1. **Contact inventory / extract** — pads we are willing to pogo on
   the panel bottom.
2. **Allocate pools** — which DUT nets consume which LS / USB /
   Vtarget / VUSB / GND budget (≤ 8 boards, 1 tile).
3. **A7 mate pattern** — several constellation strategies; score
   escape and array-connector fit. Do not pick one on paper.
4. **Assignment** — match top contacts onto a candidate pattern
   (LS is a matching into an interchangeable set).
5. **Route the interposer** — 2-layer, pours, 2 A power, USB pairs.
6. **Emit** a real board (unrouted `.kicad_pcb` first).

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
- Whether USB polarity is reversible in the MAX4999 path.
- Whether the 16 GND lands are one net or should stay as 8×2 for
  array connectors even if poured together.
- 4-layer only if 2-layer USB on A5 fails.

None of these need to block writing an extractor or generating the
first pattern family.
