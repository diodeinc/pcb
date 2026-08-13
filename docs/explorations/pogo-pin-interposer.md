# Automatic pogo-pin interposer generation

Exploration notes. Nothing here is a commitment. The work is meant to
be taken one subproblem at a time.

This came out of looking at the demo boards, mockingbird-feather, and a
sample of the broader diodehub corpus, then running small extract /
classify / assign / sketch prototypes against real `.kicad_pcb` files.

## Why this exists

Shop-floor bringup of these boards is a repeated ritual: locate the
PCB, press spring probes onto test/debug lands, power up, flash over
SWD (or SWIO), maybe talk USB or UART, run a self-test.

Today that fixture is designed by hand. The idea is to generate the
**per-SKU translator board** automatically:

```
          DUT  (custom test pads / Tag-Connect / DNP header)
                 ↕  pogo pins, custom XY, one copper side
          INTERPOSER   ← this is what we generate
                 ↕  fixed pad pattern
          TEST MASTER  (reused: PSU, mux banks, debugger, sequencer)
```

The interposer’s job is only to route a custom top-side pogo pattern
onto a standard bottom-side pattern. The master is a separate, reused
design. We do not have to invent it in order to start, but we do have
to say what its pin/bank contract is, because that contract is the
interposer’s routing target.

## What makes this different from a normal board

A normal layout has a sacred netlist. Pad A is net N; pad B is net N;
you route them.

The interposer does **not** start with that pairing:

- Top XY is fixed by the DUT’s existing lands.
- Bottom XY is fixed by the master’s grid.
- Which top land owns which bottom pin is **chosen**.

That freedom is real (a Tag-Connect SWDIO pad can legally attach to
any unused pin of the debug bank) and bounded (GND cannot go to 5 V;
SWDIO and SWCLK cannot be split across two mux trees). So this is not
a normal autoroute, and it is also not a free crossbar.

Working name for the problem:

> colored, bank-constrained assignment
> (contact → bank/channel)
> followed by ordinary geometric routing of that matching.

The assignment **is** the netlist we then route.

## Scope for the first cut

In:

- Lands already designed as test/debug contacts: `TestPoint` pads,
  Tag-Connect TC2030/TC2050 pad-only footprints, unpopulated debug
  headers a pogo can sit on (mockingbird `J_SWD` FTSH-105).
- One copper side of the DUT per fixture.
- SWD/SWIO, rails, GND, reset, and USB only when those pads actually
  exist.

Out, for now:

- Designing the test-master PCB itself.
- Falling back to DNP part pads or random component pins.
- Dual-side fixtures, flying probes, ICT program generation.
- USB SuperSpeed / RF / controlled-impedance pogo work.

## What the boards actually look like

Parsed from layout, not from names. Seed set: Renfield, Demeter,
Feign, Seward, Governor, amoeba, marlow, mockingbird-feather. Also
walked a few dozen other diodehub boards.

Recurring geometry:

| Pattern | Typical pad | Pitch | Notes |
|---|---|---|---|
| `TestPoint_Pad_D1.0mm` | Ø 1.0 mm | n/a | Smallest common land |
| `TestPoint_Pad_D1.5mm` | Ø 1.5 mm | n/a | marlow SWD + rails |
| TC2030-IDC-NL 2×3 | Ø 0.79 mm | 1.27 mm | Modal debug land. 6 electrical + 3 NPTH |
| FTSH-105 2×5 | 0.74 × 2.79 mm | 1.27 mm | DNP ARM 10-pin; pogo-able |

Seed scoreboard:

| Board | Side | Contacts worth hitting |
|---|---|---|
| Renfield | F.Cu | TC2030 SWD + VTREF + GND + UART + USB-CC + scope + extra GND |
| Demeter | F.Cu | TC2030 + `TP_GND` |
| Feign, Governor, amoeba | F.Cu | TC2030 only (amoeba reset is RP2040 `RUN`) |
| Seward | F.Cu | TC2030 is the *probe* MCU; `J_TGT` is the *target* header |
| marlow | **B.Cu** | Ø 1.5 mm `TP_SWDIO` / `TP_SWCLK` / `TP_3V3` / `TP_VBUS` / `TP_GND`. Front FTSH is the target |
| mockingbird-feather | **B.Cu** | DNP FTSH-105 `J_SWD` (firmware already says pogo it) |

Facts that later steps should not paper over:

- Same-side is not “always front.”
- Stdlib `Swd` is only `{SWDIO, SWCLK}`. VSENSE, NRST, SWO ride beside
  it on the Tag-Connect module.
- SWO is usually `NotConnected()`.
- NRST is not always named NRST (`RUN`, `RST`).
- Probe boards have two SWD worlds. When the probe is the DUT, ignore
  the target header.
- A complete same-side bringup set (SWD + rail + GND + USB + UART) is
  rare. Renfield is the rich case. Most boards are Tag-Connect-only.
- None of the eight seeds have USB D+/D− test pads. Renfield CC pads
  are analog observation, not an enumerate pair.

Broader corpus sketch (47 boards, 43 with contact footprints):
TestPoint is common, Tag-Connect shows up often, mixed-side boards
exist. Treat the eight seeds as the first test set, not the whole
distribution.

## Suggested vocabulary

Keep this small. Rename later if a better established term appears.

| Term | Meaning |
|---|---|
| DUT | Board under test |
| Contact site | A DUT copper feature we are willing to pogo |
| Role | Semantic class of the net (`GND`, `PWR_3V3`, `SWDIO`, …) |
| Color | Electrical compatibility class (`ground`, `power:3v3`, `digital`, …) |
| Bundle | Roles that must land on one master instrument (e.g. SWD) |
| Bank | Master pins that share a mux / instrument port |
| Channel | One pin inside a bank |
| Affinity | Colors a bank is allowed to carry |
| Assignment | contact → (bank, channel). This becomes the netlist |
| Interposer | Generated translator PCB |
| Test master | Reused sequencer / PSU / mux / programmer |
| Same-side | Every chosen contact for one fixture sits on one copper face |

## Subproblems

These are different kinds of work. They can be tackled in order.
They should not be collapsed into “write a script.”

### 1. Inventory

Which contact sites exist, on which side, carrying which nets, at
what size and pitch?

This is measurement. Failure mode: inventing pads, missing back-side
sets, treating Seward’s target header as the DUT SWD.

### 2. Extraction

Smallest path from Zener + `.kicad_pcb` (+ IPC / pcb-ir later) to a
contact record: ref, footprint, pad, XY, size, side, net, Zener kind
if we have it.

A first prototype just walked the `.kicad_pcb` S-expression. That was
enough to recover every seed pad we cared about. KiCad’s IPC-2581
export (10.0.5) was a poor contact API in the one trial: no logical
nets in the file we got, and Y flipped. pcb-ir is a fabrication
dialect, not a “find the Tag-Connect” API.

Likely v1 extractor: parse `.kicad_pcb` for geometry and nets, then
join a Zener netlist when we want `Power` / `Ground` / voltage.
Filter on footprint family (`TestPoint_*`, `Tag-Connect_*`,
`FTSH-105*`, plus explicit `J_SWD` / `TP_*` paths).

### 3. Taxonomy and ranking

Which nets are bringup roles, which pads are pogo-able, which one
side do we actually hit?

Classification can start coarse:

- Zener net kind and interface when present (`Power`, `Ground`, `Swd`).
- Footprint pin map (TC2030 pad 1 is VSENSE, pad 2 SWDIO, …).
- Name heuristics as a last resort (`SWDIO`, `V3V3`, `GND`, `CC1`).

Ranking is mechanical: pad diameter, mask opening, pitch, not under a
populated body, not a stuffed connector, not an NPTH alignment hole.
Then pick one side. Refuse to mix `F.Cu` and `B.Cu`. On probe boards,
prefer the self-SWD lands when the probe is the DUT.

A first classifier ran on the eight seeds. All of them had a same-side
SWD+GND set. marlow’s SWD bundle is partial (no NRST). No seed had
USB DP/DM lands.

### 4. Assignment language

This is the new intellectual object. Until it exists, a solver and a
master pin map will silently disagree.

What the language needs to say, and not much more:

1. DUT contacts with roles (and the color each role inherits).
2. Master banks with affinities and channel counts.
3. Bundles that must share a bank.
4. Hard rails (`GND` → ground, `VCC` → matching rail).
5. Whether permutation inside a bank is free, or some channels are
   reserved (e.g. a dedicated VTREF sense pin).

Prior art that is useful as analogy, not as a drop-in:

- ICT fixture CAD (CheckSum, Digitaltest, SPEA) is excellent at
  *probe XY + wiring list* once the netlist is known. Their testers
  already type resources (power vs multiplexed measure). They assume
  the netlist is an input. We do not.
- FPGA I/O banking is the right metaphor for “this bundle must sit in
  one bank; pins inside the bank permute.”
- Analog mux trees are why this is not a full crossbar.
- Min-cost flow assigns pins well but cannot keep a bundle atomic.
  SAT can. For the sizes we saw (≤12 contacts) greedy backtrack plus
  a small permutation search is enough to try first.

A tiny JSON shape (`diode.fixture/v0` or whatever we call it) is
enough to start. Do not invent a compiler language until a few boards
have been assigned by hand against that JSON.

### 5. Interposer geometry

Once assignment exists, this is mostly CAD:

- DUT-facing features at the contact XY, after one stack transform
  (Y-mirror iff the fixture side is `F.Cu`, so the interposer faces
  the lands). Back-side contacts are identity.
- 50 mil ICT probes in pressed receptacles for the 1.27 mm Tag-Connect
  / FTSH pitch. SMT pogo strips do not have the stroke to clear a
  USB-C.
- A small **stamp** that covers the contact cluster, not necessarily
  the whole DUT.
- Tooling: Tag-Connect already ships three NPTH holes; reuse them
  when the stamp is a TC2030. Otherwise board-outline + fiducials.
- Bottom side is the frozen master grid, no second mirror.

Some boards will be unfixturable with a given stamp (tall parts in
the way, contacts on both sides, pitch tighter than the probe
family). That should be a hard report, not a silent bad board.

### 6. Test-master contract

The master is not generated, but its banks *are* the other half of
the matching. First-gen can be boring:

- GND rails.
- One 5 V source, one 3V3 sense (and/or VTREF on the debug bank).
- One stuffed SWD instrument (CMSIS-DAP). A second, unstuffed, if we
  ever want probe+target in one press.
- A small digital bank behind jellybean analog muxes (not an FPGA).
- USB-CC sense if we care; USB D+/D− only if we start seeing those
  pads in the corpus.

Whatever grid we pick should be written down as a pin map and then
left alone. The prototypes used a 4×10 @ 2.54 mm sketch. That number
is not sacred; the point is to freeze *a* contract before writing a
router.

## What the prototypes showed

These were throwaways. Useful as existence proofs, not as code to
keep.

- Walking `.kicad_pcb` recovered 78 pads / 21 footprints on the eight
  seeds, with XY that matched the layouts.
- A role classifier produced a same-side fixture set for every seed.
- A two-stage assigner (bundles → banks, then permute channels)
  produced legal matchings for Renfield (11 contacts), marlow (5),
  and mockingbird (7), and correctly rejected a 3-rail / 2-pin
  negative case.
- Sketching the chosen matching as Manhattan lines: marlow is
  trivial; mockingbird is a small header; Renfield is messy because
  USB-CC lives on the west lip and the legal USB-CC pins live on the
  east of the grid. A geometrically prettier left-to-right zip had
  fewer crossings **and was electrically illegal**.

So assignment flexibility is doing electrical work. It is not a
crossing optimizer. Geometry still has to finish the job (real
routing, or a two-island stamp, or dropping optional contacts).

## A simple sequence to try

One subproblem at a time. Each step should leave an artifact we can
look at on real boards before starting the next.

1. **Contact extractor** on `.kicad_pcb` (+ optional Zener netlist).
   Output: JSON of in-scope pads. Test: the eight seeds match what
   we already counted by hand.
2. **Classifier** that emits roles, colors, a chosen side, and a
   recommended fixture set. Test: refuse mixed sides; ignore Seward
   / marlow target headers when the probe is the DUT.
3. **Assignment schema + one frozen master pin map.** Even a
   handwritten JSON contract is enough. Test: Renfield SAT, 3-rail
   negative UNSAT.
4. **Geometry transform + unrouted interposer** as a `.kicad_pcb`
   (top receptacle holes, bottom grid, nets = the assignment).
   Start with Feign (Tag-Connect only). Then a back-side board
   (mockingbird or marlow). Renfield last.
5. **Route** with ordinary KiCad (or later our own router). Only
   after a few unrouted boards exist and the matching looks right.
6. **Master board** as its own project, against the same pin map.
   Not a prerequisite for (1)–(4).

Possible later, not now: DNP-part fallbacks, dual-side, SAT, IPC /
pcb-ir as the contact API, a compiler subcommand, production nest
and CAM.

## Open questions

- Exact master grid (count, pitch, which pins are reserved).
- Whether VTREF is a dedicated sense pin or just another channel in
  the debug bank.
- How much user input we allow when the automatic set is incomplete
  (no NRST, no rail, contacts on both sides).
- Whether a “stamp” smaller than the DUT is acceptable for v1, or we
  always outline the whole board.
- How to represent this in Zener, if at all, versus a side-car JSON
  plus a generated KiCad file.

None of these need to be answered before step 1.
