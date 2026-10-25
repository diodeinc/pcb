# Board-array copper-balancing region

Status: reusable `pcb-ir` computation plus a development harness. Board-array
creation consumes the result before it emits balancing copper. The harness does
not define a user-facing CLI contract.

The balancing objective and generated copper geometry are documented in
[`../docs/copper-balancing.md`](../docs/copper-balancing.md).

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
- `e`: numerical construction guard, half the geometry accuracy budget
  (0.025 at the balancing profile's 50 µm).

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
symmetrically; a void component too thin to carry an axis stroke is swept
whole instead. The guard `e` separates every checked quantity from every
constructed one: a cut leaves a `2 (v + e)` void, so construction noise cannot
push it back under the nominal test. Each pass only removes material, and the
filled-region opening after every trim prevents new copper slivers. The safe
region is the fixed point where `Nₖ` is empty; a pass that cannot make
meaningful geometric progress reports an error.

The independent nominal certificate is:

```text
safe \ F_l = ∅
safe \ P = ∅
safe ∩ O_l = ∅
dist(∂safe, ∂P ∪ ∂O_l) >= r
```

Construction uses the guard and offsets; certification uses the nominal
clearance and measures boundary-to-boundary distance exactly, so the proof
shares none of the construction's machinery. Regions that do not overlap can
only come within `r` of each other where their boundaries do, so the four
statements together are the clearance requirement.

Filled-feature and void-gap width need no second proof. The last step of the
construction is an opening by `disk(q)`, and it stops only once `G_v` of its
own result is empty — that loop condition is the certificate. The harness
still draws `safe ⊕ disk(r)`, `safe \ open(safe, disk(q))` and `G_v(safe)` as
overlays for debugging.

## IPC boundary

The [IPC collector and input/options types](../../pcb-ir/src/dialects/ipc/balancing_region.rs)
use IR feature spans, sides, and painted paths, not shape recognition. An
included path without paint has no physical envelope, so production collection
fails closed instead of guessing.

## Development harness

Run the harness in release mode for corpus iteration:

```bash
cargo build --release -p pcb-ipc2581-tools \
  --example board_array_balancing_region

timeout 60 target/release/examples/board_array_balancing_region \
  path/to/array.xml \
  --output /tmp/balancing-region/case-name \
  --require-a-series-auto
```

Use GNU `timeout` (`gtimeout` on macOS with GNU coreutils), or omit the timeout
wrapper. Run the example with `--help` for options and defaults, including the
selected copper layer and certificate area tolerance.

Each run overwrites deterministic `index.html`/`overview.svg` views, numbered
intermediate-region SVGs, per-source-layer support views, and `regions.json`
metrics and exact rings. See the [harness source](board_array_balancing_region.rs)
for the complete artifact set. Artifacts are written before a failed certificate
exits nonzero, allowing reproduction without reparsing IPC.

For regression work, run `cargo test -p pcb-ir geom::region`, then inspect varied
auto arrays across sizes and copper layers. Require complete painted-path
coverage, no undersized components, and a passing nominal certificate. The viewer
explains failures; the certificate determines the geometric verdict. Independent
reconstruction of `regions.json` with a second geometry engine is an additional
validation step, not something the harness performs automatically.
