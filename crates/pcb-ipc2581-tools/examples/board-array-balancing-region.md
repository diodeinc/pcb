# Board-array copper-balancing region

Status: reusable `pcb-ir` computation plus a development harness. Board-array
creation consumes the result before it emits balancing copper. The harness does
not define a user-facing CLI contract.

The balancing objective and generated copper geometry are documented in
[`../docs/board-array-copper-balancing.md`](../docs/board-array-copper-balancing.md).

## Geometry contract

All geometry is a filled planar point set in panel coordinates, in millimeters:

- `P`: filled outer profile of the root panel.
- `B`: union of every final board-instance profile. Board-profile holes remain
  protected because copper balancing must not enter a board footprint.
- `M`: fabrication material removal, including profile cutouts, board cutouts,
  and generated V-score reliefs.
- `G_l`: painted `ArraySupport` geometry whose IR feature span reaches copper
  layer `l`. Exact copper spans stay on that layer; surface-side spans map to
  the adjacent outer copper; through-stack or unresolved physical spans affect
  every copper layer. V-cut layers include only features tied to a `V_Cut`
  process, excluding callout arrows and labels.
- `O_l = B ∪ M ∪ G_l`: obstacle union for copper layer `l`.
- `r = 0.5`: required feature and panel-edge clearance.
- `q = 0.5`: filled-region rolling-disk radius.
- `v = 0.5`: void-gap rolling-disk radius.
- `e = 0.025`: numerical construction guard.

Clearance and filled-region regularization are direct morphology:

```text
c = r + e
F_l = (P ⊖ disk(c)) \ (O_l ⊕ disk(c))
A_l = open(F_l, disk(q))
```

`F_l` is the layer's clearance-safe set. Treating the panel exterior and all
obstacles as one forbidden phase covers corners and diagonal approaches without
rules for individual feature types. `A_l` is the union of all radius-`q` disks contained in
`F_l`. It rounds outward corners and removes tips, necks, slivers, or components
that cannot contain that disk. There is no component-area threshold. With the
defaults, both surviving filled regions and empty gaps use a 1 mm nominal
scale, while clearance remains an independent 0.5 mm distance.

For any set `X`, define `G_t(X)` as the connected components of
`close(X, disk(t)) \ X` that touch nonincident, separated source-boundary
branches on distinct rings, or on one ring with opposing tangents. This keeps
genuine two-sided gaps—including gaps between components, hairpins, notches,
and holes—and rejects the ordinary closing bite at an isolated concave corner.

Gap regularization repeats one monotone local trim:

```text
S₀   = A_l
Nₖ   = G_v(Sₖ)
Γₖ   = source-boundary generalized Voronoi axis inside Nₖ
Sₖ₊₁ = open(Sₖ \ (Γₖ ⊕ disk(v + e)), disk(q)) ∩ Sₖ
```

The Voronoi axis gives the least local cut and treats competing sides
symmetrically. The guard `e` separates every checked quantity from every
constructed one: a cut leaves a `2 (v + e)` void, so construction noise cannot
push it back under the nominal test. Each pass only removes material, and the
filled-region opening after every trim prevents new copper slivers. Iteration
ends when `Nₖ` is empty or reports an error if a pass cannot make meaningful
geometric progress.

The independent nominal certificate is:

```text
C = safe ⊕ disk(r)

safe \ F_l = ∅
safe \ open(safe, disk(q)) = ∅, after numerical denoising
G_v(safe) = ∅
C \ P = ∅
C ∩ O_l = ∅
```

Construction uses the guard; certification uses the nominal requirements.
Every violation remains available as geometry for debugging.

## IPC boundary

`pcb_ir::dialects::ipc::balancing_region` supplies only semantic collection and
orchestration:

```rust
pub struct BoardArrayBalancingInput {
    pub panel_outer: ContourSet,
    pub board_footprints: ContourSet,
    pub material_removal: ContourSet,
    pub support_features: ContourSet,
}

pub struct BalancingRegionOptions {
    pub clearance_mm: f64,
    pub regularization_radius_mm: f64,
    pub gap_radius_mm: f64,
    pub numerical_guard_mm: f64,
}
```

The collector uses existing IR feature spans, sides, and painted paths. It does
not recognize tooling holes, fiducials, score lines, or other features from
their shape. Support geometry is stored once in copper-reach buckets; a target
layer's `support_features` union is derived on demand. An included path without
paint has no physical envelope, so production collection fails closed instead
of guessing.

Generic Boolean operations, disk morphology, gap classification, Voronoi-axis
construction, and certificates remain in `pcb-ir::geom`. File parsing and
debug rendering remain in `pcb-ipc2581-tools`.

## Development harness

Run the harness in release mode for corpus iteration:

```bash
cargo build --release -p pcb-ipc2581-tools \
  --example board_array_balancing_region

gtimeout 60 target/release/examples/board_array_balancing_region \
  path/to/array.xml \
  --output /tmp/balancing-region/case-name \
  --require-a-series-auto
```

Options:

```text
--clearance-mm <mm>                  nominal clearance; default 0.5
--regularization-radius-mm <mm>      filled-region disk radius; default 0.5
--gap-radius-mm <mm>                 void-gap disk radius; default 0.5
--numerical-guard-mm <mm>            construction guard; default 0.025
--check-area-tolerance-mm2 <mm²>     certificate threshold; default 0.0001
--copper-layer <name>                layer to inspect; default first copper layer
--require-a-series-auto              reject non-A-series auto arrays
```

Each run overwrites a deterministic artifact set:

| Artifact | Contents |
|---|---|
| `index.html`, `overview.svg` | Interactive and portable combined views |
| `00-panel-outer.svg` | Raw panel region `P` |
| `10-board-footprints.svg` | Board-instance region `B` |
| `20-material-removal.svg` | Fabrication removal `M` |
| `30-support-features.svg` | Selected layer's support footprint `G_l` |
| `40-raw-obstacles.svg` | Selected layer's obstacle union `O_l` |
| `50-panel-keep-in.svg` | Panel eroded by construction clearance |
| `60-obstacle-keep-out.svg` | Obstacles dilated by construction clearance |
| `70-clearance-safe-region.svg` | Clearance-safe set `F` |
| `75-opened-candidates.svg` | Filled-region opening `A` |
| `80-removed-by-opening.svg` | Material rejected by filled regularization |
| `82-narrow-voids.svg` | Detected two-sided closing residuals |
| `83-gap-separator-keep-out.svg` | Medial-axis and residual sweep tubes |
| `85-removed-by-gap-regularization.svg` | Material locally trimmed around gaps |
| `90-safe-region.svg` | Final balancing-safe region |
| `100-clearance-certificate.svg` | Safe region swept by nominal clearance |
| `105-gap-violations.svg` | Remaining nominal two-sided gap violations |
| `110-validation-violations.svg` | Union of all certificate failures |
| `support-layers/*.svg` | Physical support footprint by source layer |
| `regions.json` | Metrics and exact rings for every intermediate |

Artifacts are written before a failed certificate exits nonzero. Exact rings
make a failure reproducible without reparsing IPC.

## Fast validation loop

1. Run `cargo test -p pcb-ir geom::region`.
2. Build the release harness once.
3. Regenerate varied A7, A6, A5, and A4 auto-panelized arrays with a bounded
   timeout.
4. Require complete painted-path coverage, zero undersized components, and an
   empty nominal certificate.
5. Inspect board footprints, material removal, per-layer support, V-cut
   inclusion, every regularization intermediate, and the final safe region.
6. Compare `regions.json` area, component, ring, and vertex metrics.
7. Parse every SVG and independently reconstruct the serialized regions with a
   second geometry engine for area, subset, clearance, filled-disk, and
   inter-component-gap checks.

The viewer explains failures; the set-theoretic certificate determines
correctness.
