# MCAD-driven placement (IDF) demo

A small USB-C board whose enclosure dictates where the connector and the
mounting holes go. The mechanical CAD exports that contract as an IDF board
file, and `pcb layout` honors it: MCAD-owned components land at the exact
mechanical pose, locked; everything else stays ECAD-owned and is auto-placed
around them.

## Files

| File | Role |
| --- | --- |
| `board.zen` | The board: USB-C receptacle, CC pull-downs, power LED, two M3 mounting holes. |
| `usbc.zen` | USB-C receptacle (USB 2.0, 16-pin) wrapped as a module. |
| `mechanical/usbc_mcad.emn` | IDF 3.0 export "from the enclosure CAD". Its `.PLACEMENT` section claims `J1`, `H1`, `H2` as `MCAD`-owned at fixed poses. |
| `mechanical/footprint-datums.toml` | Maps each IDF package's mechanical datum into footprint-local coordinates. |

## Run it

```sh
pcb layout examples/usbc_mcad/board.zen
```

The log reports `applied 3 MCAD position(s) from IDF …/usbc_mcad.emn`. In
`layout/layout.kicad_pcb` (a 30 × 20 mm board, centered on the A4 sheet —
IDF coordinates map 1:1 onto the KiCad sheet frame):

- `J1` sits at (148.5, 111.35) mm, locked. The IDF claims the *mating face*
  at (148.5, 115) — centered on the bottom board edge; the datum catalog says
  that face is 3.65 mm along +Y from the footprint origin, so the resolver
  back-computes the footprint origin and the face lands exactly on the edge.
- `H1`/`H2` sit at (137, 98.5) and (160, 98.5) mm, locked (datum offset is
  zero — hole axis is the footprint origin).
- `R1`–`R3` and `D1` are ECAD-owned: HierPlace drops them on first
  generation, after which their positions live in the `.kicad_pcb` (this
  example has them hand-placed mid-board). `D1` has an `ECAD` claim in the
  IDF file, which is ignored — only `MCAD` status transfers ownership.

The board outline on `Edge.Cuts` was drawn once in the layout file (the IDF
`.BOARD_OUTLINE` section is not imported — the system is placement-only) and
persists through re-syncs, like any other destination-owned content.
`pcb layout --check` passes DRC with no errors.

## The MCAD revision loop

When the enclosure changes, the mechanical engineer re-exports the `.emn`.
Edit the `J1` coordinate record in `mechanical/usbc_mcad.emn` (e.g.
`148.5 → 151.5`) and re-run `pcb layout`: `J1` moves to (151.5, 111.35);
every other footprint keeps its position.

## How discovery works

This example uses the zero-config convention: `mechanical/<board>.emn` next
to the board's `.zen` file (`<board>` is the `Board(name = …)`). You can also
declare it explicitly in `pcb.toml`:

```toml
[board.mechanical.idf]
emn = "mechanical/usbc_mcad.emn"
```

## Datum integrity (optional)

A `[[datum]]` entry can pin `footprint_hash = "blake3:…"`. If the footprint
file changes after the datum was calibrated, resolution fails instead of
silently placing against a stale origin.
