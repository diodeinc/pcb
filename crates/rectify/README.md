# rectify

`rectify` checks and corrects the rotation and offset of STEP models referenced
by KiCad footprints. It infers the transform from model geometry and the
footprint's copper pads and drilled holes.

## Check and fix footprints

Run `check` before changing any files:

```bash
pcb rectify check path/to/components/
pcb rectify check path/to/components/ --jsonl
pcb rectify check path/to/components/ --strict
```

The command searches directories recursively for `.kicad_mod` files. Use
`--kind smd`, `--kind tht`, or `--kind mixed` to restrict the footprint type.
`--jsonl` writes machine-readable findings and correction candidates.

Apply the proposed transforms only after reviewing the check output:

```bash
pcb rectify fix path/to/components/
```

`fix` rewrites the `(rotate ...)` and `(offset ...)` values in every flagged
footprint. Commit or back up the source files before running it.

The default comparison permits a Z-axis-equivalent rotation and an offset error
of at most 0.20 mm on each axis. `--strict` requires the exact stored rotation
and reduces the offset limit to 0.10 mm.

## Solver model

For each footprint, the solver:

1. Reads its pads, holes, and referenced or embedded STEP model.
2. Tessellates the STEP model into triangles.
3. Evaluates the 24 axis-aligned rotations supported by KiCad.
4. Rasterizes each rotation into a 0.10 mm bottom-height image.
5. Extracts candidate contact or pin features.
6. Aligns those features with the footprint and selects the highest-scoring
   rotation and translation.

The feature extraction depends on the footprint type:

- **SMD** footprints use low model surfaces as contact features and align them
  with copper pads.
- **Through-hole** footprints extract connected pin islands from low model
  cross-sections and align them with connected drill holes. Mechanical holes
  affect tie-breaking but do not control the initial alignment.
- **Mixed** footprints use hole alignment as the primary signal and pad contact
  as an additional score.

The solver rejects or penalizes poses with missing pad or hole coverage,
contacts outside their expected targets, or an implausible support plane.

## Developer commands

The `pcb-rectify` binary also provides `solve`, `patch`, `audit`, and `bench` for
solver development. These commands are hidden from the normal
`pcb rectify --help` output. Set `RUST_LOG=warn` or a narrower filter to enable
diagnostic logging.

### Benchmark the solver

`bench` compares inferred transforms with the transforms stored in a set of
footprints. It replaces each stored transform with a deterministic randomized
starting transform before solving, so the benchmark measures geometry-based
inference rather than preservation of the existing value.

```bash
pcb-rectify bench path/to/components
pcb-rectify bench path/to/components --mode strict
pcb-rectify bench path/to/components --kind smd
pcb-rectify bench path/to/components --initial-transform-seed 2
pcb-rectify bench path/to/components --jsonl
```

Use `--use-stored-initial-transform` only when comparing against the older
benchmark behavior. The benchmark reports pass rate, reward score, exact
rotation rate, high-percentile offset error, and per-footprint-type results.

## Source layout

| Module | Responsibility |
|---|---|
| `footprint` | Parse footprints and load embedded or referenced STEP data. |
| `mesh` | Tessellate STEP geometry. |
| `pose` | Generate KiCad-compatible axis-aligned rotations. |
| `raster` | Build pad, hole, contact, and pin masks. |
| `solver` | Evaluate poses, translations, support planes, and scores. |
| `bench` | Compare inferred and stored transforms. |
| `patch` | Rewrite footprint model transforms. |

Run the crate tests with:

```bash
cargo test -p rectify
```
