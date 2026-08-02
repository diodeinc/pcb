# Board-array copper-balancing region

Status: reusable `pcb-ir` computation plus a development harness. Board-array
creation consumes the result before it emits balancing copper. The harness does
not define a user-facing CLI contract.

## Geometry contract

All geometry is a filled planar point set in panel coordinates, in millimeters:

- `P`: filled outer profile of the root panel.
- `B`: union of every final board-instance profile. Board-profile holes remain
  protected because copper balancing must not enter a board footprint.
- `M`: fabrication material removal, including profile cutouts, board cutouts,
  and generated V-score reliefs.
- `G`: union of painted physical geometry on every `ArraySupport` layer.
  V-cut layers include features tied to a `V_Cut` process and exclude callout
  arrows and labels.
- `O = B ∪ M ∪ G`: obstacle union.
- `r = 0.5`: required feature and panel-edge clearance.
- `q = 0.5`: filled-region rolling-disk radius.
- `v = 0.5`: void-gap rolling-disk radius.
- `e = 0.025`: numerical construction guard.

Clearance and filled-region regularization are direct morphology:

```text
c = r + e
F = (P ⊖ disk(c)) \ (O ⊕ disk(c))
A = open(F, disk(q))
```

`F` is the clearance-safe set. Treating the panel exterior and all obstacles as
one forbidden phase covers corners and diagonal approaches without rules for
individual feature types. `A` is the union of all radius-`q` disks contained in
`F`. It rounds outward corners and removes tips, necks, slivers, or components
that cannot contain that disk. There is no component-area threshold. With the
defaults, both surviving filled regions and empty gaps use a 1 mm nominal
scale, while clearance remains an independent 0.5 mm distance.

For any set `X`, define `G_t(X)` as the connected components of
`close(X, disk(t)) \ X` that touch nonincident source-boundary branches with
opposing tangents. This keeps genuine two-sided gaps—including gaps between
components, hairpins, notches, and holes—and rejects the ordinary closing bite
at an isolated concave corner.

Gap regularization is a monotone local trim:

```text
N₀   = G_(v + e)(A)
Γ₀   = source-boundary generalized Voronoi axis inside N₀
S₀   = open(A \ (Γ₀ ⊕ disk(v + e)), disk(q))

Vₖ   = open(G_v(Sₖ), disk(e))
Sₖ₊₁ = open(Sₖ \ (Vₖ ⊕ disk(v + e)), disk(q))
```

The Voronoi axis gives the least local first cut and treats competing sides
symmetrically. Any meaningful certificate residual is swept directly, avoiding
repeated Voronoi construction over Boolean-generated boundaries. Each pass only
removes material, and the filled-region opening after every trim prevents new
copper slivers. Iteration ends when `Vₖ` is empty or reports an error if a pass
cannot make meaningful geometric progress.

The independent nominal certificate is:

```text
C = safe ⊕ disk(r)

safe \ F = ∅
safe \ open(safe, disk(q)) = ∅, after numerical denoising
G_v(safe) = ∅, after numerical denoising
C \ P = ∅
C ∩ O = ∅
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

The collector uses existing IR roles and painted paths. It does not recognize
tooling holes, fiducials, score lines, or other features from their shape. A new
physical support feature is covered automatically when it appears as painted
`ArraySupport` geometry. An included path without paint has no physical
envelope, so production collection fails closed instead of guessing.

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
--require-a-series-auto              reject non-A-series auto arrays
```

Each run overwrites a deterministic artifact set:

| Artifact | Contents |
|---|---|
| `index.html`, `overview.svg` | Interactive and portable combined views |
| `00-panel-outer.svg` | Raw panel region `P` |
| `10-board-footprints.svg` | Board-instance region `B` |
| `20-material-removal.svg` | Fabrication removal `M` |
| `30-support-features.svg` | Cross-layer support footprint `G` |
| `40-raw-obstacles.svg` | Obstacle union `O` |
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
