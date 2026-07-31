# Board-array copper-balancing region

Status: reusable safe-region implementation plus a development harness. This
does not add copper or expose a user-facing CLI contract.

## Geometry contract

All geometry is a filled planar point set in panel coordinates, measured in
millimeters:

- `P`: the filled outer profile of the root array/panel.
- `B`: the union of the filled outer profiles of every final board instance.
  Board-profile cutouts are deliberately ignored here, so the whole board
  footprint remains protected.
- `M`: physical material removal already derived for fabrication: root-profile
  cutouts, board cutouts, and generated V-score route reliefs.
- `G`: the geometric footprint of every painted physical `ArraySupport`
  feature on every source layer. Filled paths use their native fill rule.
  Stroked paths use their native width, cap, and join. Feature polarity does
  not change the fact that its footprint is protected. V-cut layers use
  process-spec references to retain score operations while excluding
  same-layer documentation such as arrows and labels.
- `O = B ∪ M ∪ G`: every obstacle.
- `r = 0.5 mm`: the nominal required clearance.
- `q = 1.0 mm`: the minimum-feature radius used to regularize the result.
- `v = 1.0 mm`: the minimum-gap radius that must fit between safe regions.
- `e`: a conservative numerical error budget for polygonization and offsets.

The result is:

```text
construction_clearance = r + e
construction_gap       = max(r, v) + e
panel_clearance_keep_in = P ⊖ disk(construction_clearance)
panel_keep_in           = P ⊖ disk(construction_gap)
obstacle_clearance      = O ⊕ disk(construction_clearance)
obstacle_gap_envelope   = O ⊕ disk(construction_gap)
maximal_safe            = panel_keep_in \ obstacle_gap_envelope
regularization_core     = maximal_safe ⊖ disk(q)
opened_safe             = (regularization_core ⊕ disk(q)) ∩ maximal_safe

H = graph whose vertices are the connected components of opened_safe,
    with an edge (i, j) exactly when distance(component_i, component_j) < 2v

x*                      = arg max Σ area(component_i) x_i
                          subject to x_i + x_j ≤ 1 for every edge (i, j) in H
                          and x_i ∈ {0, 1}
safe                    = union of component_i for which x*_i = 1
removed_by_opening      = maximal_safe \ opened_safe
removed_for_gap         = opened_safe \ safe
```

The last two offsets are a morphological opening by a disk. They round convex
tips, break necks narrower than the disk diameter, and remove components unable
to contain the disk without geometric recognition rules or topology heuristics.
The intersection makes the opening explicitly anti-extensive despite
polygon-offset approximation: regularization can only remove clearance-safe
material, never add material outside `maximal_safe`.

The gap envelope is the dual construction. The outside of the panel and the
obstacle union are treated as one hard forbidden phase: the panel is eroded
and the obstacles are dilated by the same construction gap. This prevents
diagonal shortcuts where an obstacle envelope meets a panel edge. For a
line-like obstacle, the default creates a 2.05 mm corridor instead of the
1.05 mm corridor produced by clearance alone.

Opening removes copper slivers, but it cannot choose which side of a narrow
gap should survive. That is a topology choice rather than another local
offset. The component graph makes the choice explicit and order-independent:
retain the conflict-free subset with maximum total area. Each connected
conflict cluster is solved exactly by branch-and-bound; disconnected clusters
are independent, and equal-area optima use the stable area/bounding-box order.
Exact segment distance with bounding-box pruning finds graph edges without
retaining every component dilation in memory. On a detected conflict only, the
harness constructs the overlapping dilations as debug geometry.

The final optimized union is not a monotone set-valued function of clearance,
feature radius, gap radius, or obstacle additions. Those changes shrink the
pre-selection geometry, but can split components or change the maximum-area
independent set. Equal-area coordinate tie breaks likewise mean rotations and
reflections preserve the objective value and certificate, not necessarily the
chosen union. Correctness is defined by the certificate, not by nesting outputs
from different parameter values.

The independent safety certificate is:

```text
C = safe ⊕ disk(r)

safe \ maximal_safe = ∅
C \ P = ∅
C ∩ O = ∅
safe \ open(safe, q) = ∅
for every pair of distinct components i, j:
    (component_i ⊕ disk(v)) ∩ (component_j ⊕ disk(v)) = ∅
```

The certificate uses the nominal 0.5 mm, not the construction clearance. Its
feature and pairwise-gap checks use the nominal radii. Numerical offset-only
fragments are removed with an opening by `e`; geometric violations survive
that filter and are retained as first-class debug output. The broader
`close(safe, v) \ safe` is also emitted as a non-gating diagnostic: it finds
concave notches within one component as well as gaps between components, so it
is deliberately not the minimum-intercomponent-gap contract.

## Development harness

The harness is
`examples/board_array_balancing_region.rs`. Run it with:

```bash
cargo run -p pcb-ipc2581-tools \
  --example board_array_balancing_region -- \
  path/to/array.xml \
  --output /tmp/balancing-region/case-name
```

Useful options:

```text
--clearance-mm <mm>                 nominal requirement; default 0.5
--minimum-feature-radius-mm <mm>    disk-opening radius; default 1.0
--minimum-gap-radius-mm <mm>        void disk radius; default 1.0
--numerical-guard-mm <mm>           construction guard; default 0.025
--check-area-tolerance-mm2 <mm²>    validation threshold; default 0.0001
--require-a-series-auto             reject manual and minimum-fallback arrays
```

The current 0.025 mm guard is explicit and provisional. It is
`2 * STROKE_OUTLINE_MM + FLATTEN_MM` from the present `pcb-ir` polygonization
pipeline. A zero-guard Icarus run made the Boolean result look correct but the
independent certificate exposed many tiny offset-approximation slivers. Keeping
the guard separate makes that numerical decision visible in JSON and in the
viewer.

The auto A5 ControlHub case exposed a topology defect in the original
boundary-stroke offset implementation. Signed winding cancellation created
five microscopic holes inside the dilated obstacle region near connected
V-score/relief geometry. The nominal certificate then expanded those false
safe islands into 2.159 mm² of obstacle overlap. Increasing the guard through
0.150 mm could not fix a topological error.

`ContourSet` now uses `i_overlay`'s topology-aware outline offset directly.
Outer contours and holes are offset according to their role, the result is
regularized, and `ContourSet::union` performs an explicit Boolean union rather
than relying on concatenated winding. The A5 certificate now has zero obstacle
overlap.

Each run overwrites a deterministic artifact set:

| Artifact | Contents |
|---|---|
| `index.html` | Toggleable overlay, metrics, check status, and links |
| `overview.svg` | Portable combined overlay |
| `00-panel-outer.svg` | Raw root-panel region `P` |
| `10-board-footprints.svg` | Final board-instance region `B` |
| `20-material-removal.svg` | Fabrication material removal `M` |
| `30-support-features.svg` | Cross-layer support footprint `G` |
| `40-raw-obstacles.svg` | Union `O` |
| `50-panel-clearance-keep-in.svg` | Panel eroded by the construction clearance |
| `55-panel-gap-keep-in.svg` | Panel eroded by the construction gap radius |
| `60-obstacle-clearance.svg` | Dilated obstacles |
| `65-obstacle-gap-envelope.svg` | Obstacles dilated to the minimum-gap radius |
| `70-maximal-safe-region.svg` | Maximal clearance-safe set before regularization |
| `75-regularization-core.svg` | Centers where the minimum-feature disk fits |
| `78-opened-safe-region.svg` | Result after minimum-feature opening |
| `80-removed-by-opening.svg` | Material removed by the disk opening |
| `85-removed-by-gap-separation.svg` | Whole components removed to enforce pairwise gaps |
| `88-removed-by-regularization.svg` | All material removed by both stages |
| `90-safe-region.svg` | Final regularized safe region |
| `100-clearance-certificate.svg` | `safe` dilated by nominal clearance |
| `105-minimum-gap-violations.svg` | Overlap between nominal dilations of distinct components |
| `106-void-closing-additions.svg` | Non-gating broad closing diagnostic, including internal notches |
| `110-validation-violations.svg` | Subset, clearance, feature, and gap violations |
| `support-layers/*.svg` | Non-empty support footprints by source layer |
| `regions.json` | Every ring, bounding box, included/excluded count, and check |

`regions.json` is intentionally geometry-bearing rather than only a summary.
When a Boolean result looks wrong, the exact input and output rings can be
loaded into a small reproducer without parsing the original IPC again.
Artifacts are written before validation is reported; incomplete path coverage
or a failed certificate then makes the process exit nonzero.

## Baseline suite

The primary suite should contain only arrays generated by the A-series auto
panelizer:

1. Auto Icarus A7, 1 by 2: ordinary rigid board with real board-cell margins.
2. MicFlex A7, 2 by 2: highly concave 196-segment flex outline and asymmetric
   free space.
3. Blackstar A6, 1 by 1: dense near-circular 481-segment boundary.
4. ControlHub A5, 1 by 1: larger coordinate span and the offset-topology
   regression case.

For the local survey artifacts:

```bash
SURVEY_ROOT=/tmp/pcb-board-survey.oKwNKA
BALANCE_OUT=/tmp/board-array-balancing
mkdir -p "$BALANCE_OUT"

cargo run -q -p pcbc -- ipc board-array create --auto \
  "$SURVEY_ROOT/ipc/icarus_ir0001.xml" \
  --output "$BALANCE_OUT/icarus-auto.xml"

cargo run -q -p pcb-ipc2581-tools \
  --example board_array_balancing_region -- \
  "$BALANCE_OUT/icarus-auto.xml" \
  --output "$BALANCE_OUT/icarus-auto" \
  --require-a-series-auto

cargo run -q -p pcb-ipc2581-tools \
  --example board_array_balancing_region -- \
  "$SURVEY_ROOT/arrays/micflex__auto.xml" \
  --output "$BALANCE_OUT/micflex" \
  --require-a-series-auto

cargo run -q -p pcb-ipc2581-tools \
  --example board_array_balancing_region -- \
  "$SURVEY_ROOT/arrays/blackstar_mic__auto.xml" \
  --output "$BALANCE_OUT/blackstar-a6" \
  --require-a-series-auto

cargo run -q -p pcb-ipc2581-tools \
  --example board_array_balancing_region -- \
  "$SURVEY_ROOT/arrays/impulse_controlhub__auto.xml" \
  --output "$BALANCE_OUT/controlhub-a5" \
  --require-a-series-auto
```

Then broaden the topology set:

- Amoeba A7: complex curved boundary and generated reliefs.
- Forced-sheet A7/A6/A5 cases for one identical source board, separating
  topology changes from sheet-size and coordinate-span changes.
- A nested high-mix fabrication panel: nested panel transforms, rotation, and
  namespaced layers.
- A synthetic no-free-space fixture: verifies that an empty safe region is a
  valid result.

Current A-series results all have complete support-path coverage:

| Case | Sheet | Boards | Safe area | Safe fraction | Certificate |
|---|---|---:|---:|---:|---|
| Auto Icarus | A7 | 2 | 4877.093 mm² | 62.8% | pass |
| MicFlex | A7 | 4 | 4897.722 mm² | 63.1% | pass |
| Blackstar | A6 | 1 | 9538.244 mm² | 61.4% | pass |
| ControlHub | A5 | 1 | 15111.933 mm² | 48.6% | pass |

Across the 20-array auto-panelizer corpus, both regularization stages retained
90.18% to 99.88% of the maximal safe area and discarded 276 gap-conflicting
components. Every final component passed a second independent 1.0 mm erosion;
the smallest component was 60.441 mm² and the smallest component bounding-box
span was 6.284 mm. Every subset, nominal-clearance, minimum-feature, and
pairwise-gap certificate passed. These values are useful as investigation
baselines, not long-lived golden numbers.

## Fast iteration loop

1. Change one geometry or semantic stage.
2. Run the focused geometry tests:

   ```bash
   cargo test -p pcb-ir geom::region
   ```

3. Build the harness once in release mode, then regenerate auto Icarus A7,
   MicFlex A7, Blackstar A6, and ControlHub A5 into the same output
   directories. Always pass `--require-a-series-auto` and put a bounded timeout
   around each corpus case:

   ```bash
   cargo build --release -p pcb-ipc2581-tools \
     --example board_array_balancing_region
   gtimeout 20s target/release/examples/board_array_balancing_region \
     path/to/array.xml --output path/to/report --require-a-series-auto
   ```
4. Check the command summary for:

   - complete painted-path coverage;
   - zero unpainted support paths;
   - a passing nominal-clearance certificate.

5. Open `index.html` and inspect in this order:

   - board footprints cover every board but no intermediate panel-cell
     boundary;
   - material removal includes cutouts and relief pockets;
   - support features include score, drill, copper, and mask geometry present
     on the rails;
   - V-cut support contains physical score lines but no callout arrows or text;
   - obstacle clearance is round and continuous at corners;
   - the minimum-gap envelope creates visibly usable void corridors;
   - the green safe region is confined to true non-board support material;
   - pink discarded components explain every minimum-gap topology choice;
   - minimum-feature and minimum-gap violation layers are empty.

6. Compare `regions.json` before and after. Area, ring count, vertex count, and
   per-layer contributions usually locate an unintended change faster than an
   image diff.
7. Keep A5 as a required passing geometry regression gate: any nonzero
   certificate overlap is a regression. Then run the complex, nested, and
   empty cases.

This loop keeps visual review diagnostic rather than authoritative: the viewer
and automated checks are both projections of the same serialized
`ContourSet`s.

## Production architecture

### 1. Keep the geometry kernel in `pcb-ir::geom`

The generic operations belong in `pcb-ir`, independent of IPC:

- construct a footprint from painted paths;
- union, intersection, and difference of `ContourSet`s;
- disk dilation, erosion, opening, and closing;
- compute and retain certificate violation regions.

`ContourSet` provides `from_painted_paths` and
`ContourSet::disk_open` and `ContourSet::disk_close`. Dilation and erosion use
`i_overlay`'s topology-aware outline offset over canonical rings. Its
sign-aware builders offset outer contours and holes in opposite directions,
after which the result is regularized through the existing Boolean kernel.
Round-join segmentation is derived from `STROKE_OUTLINE_MM`, giving the arc
approximation an explicit sagitta bound. Union is an actual
`OverlayRule::Union`, so overlap cannot cancel filled material through winding
arithmetic. Opening clips its dilation back to the source and closing unions
its erosion with the source, preserving their subset and superset guarantees
at polygon tolerance.

The construction guard and nominal-clearance certificate remain separate.
That makes approximation error conservative and testable: construct with
`r + e`, but certify against `r`. Further manufacturing hardening can choose a
panel-local fixed scale and conservative integer rounding, while retaining the
same `ContourSet` API and Boolean kernel.

### 2. IPC semantic collector

`pcb_ir::dialects::ipc::balancing_region` consumes existing IR views and
produces four semantic input regions without classifying individual IPC shape
kinds:

```rust
pub struct BoardArrayBalancingInput {
    pub panel_outer: ContourSet,
    pub board_footprints: ContourSet,
    pub material_removal: ContourSet,
    pub support_features: ContourSet,
}

pub struct BalancingRegionOptions {
    pub clearance_mm: f64,
    pub minimum_feature_radius_mm: f64,
    pub minimum_gap_radius_mm: f64,
    pub numerical_guard_mm: f64,
}

pub struct BoardArrayBalancingResult {
    pub safe_region: ContourSet,
    pub certificate: ClearanceCertificate,
    pub intermediates: BoardArrayBalancingIntermediates,
}
```

The collector:

- obtains `panel_outer` from the root panel profile;
- obtains `board_footprints` from occurrences whose existing role is
  `BoardInstance`;
- accepts the existing `BoardArrayFabricationProfile.material_removal`;
- unions painted paths from every `ArraySupport` layer;
- on V-cut layers, includes only features whose referenced process
  specification contains a `V_Cut` item.

That is the entire IPC policy. The V-cut distinction is semantic metadata, not
shape or coordinate recognition: generated score operations carry the process
specification while callouts do not. A new tooling feature or fiducial shape
is automatically covered because extraction yields another painted path. A
path with no paint has no defined footprint; the production collector fails
closed instead of guessing from `FeatureKind`. A separate inspection collector
retains incomplete coverage for the debug harness, which still exits nonzero
after writing its artifacts.

### 3. Keep orchestration in `pcb-ipc2581-tools`

The development harness now only:

1. parse the document;
2. call `extract_layout`;
3. derive the existing fabrication profile and V-score reliefs;
4. enumerate every IPC source layer with `View::ArraySupport`;
5. feed those IR documents to the collector;
6. serialize the returned safe region and debug bundle.

SVG/HTML/JSON rendering stays here. `pcb-ir` should not know about files,
browser viewers, Clap, or the source IPC parser.

### 4. Preserve debug data as a stable internal schema

The versioned internal debug schema retains:

- nominal clearance, feature radius, gap radius, error budget, and both
  construction radii;
- source-layer feature/path counts and unsupported-path diagnostics;
- all four semantic inputs;
- the clearance and minimum-gap obstacle envelopes;
- maximal safe region, opening core, and material removed by regularization;
- final safe region;
- certificate footprint plus clearance, minimum-feature, and minimum-gap
  violation regions;
- geometry-kernel tolerance/scale metadata.

Production calls can consume only `safe_region`; tests and diagnostics retain
the full bundle. The renderer does not recompute intermediate regions.

The reusable library boundary, fail-closed collector, certificate, tests, and
debug-harness adapter are implemented. Automatic board-array creation consumes
the certified region before balance copper is emitted.

## Validation plan

### Geometry unit tests

Use analytic shapes with known behavior:

- rectangle erosion and dilation;
- a region with a hole, proving erosion expands the hole;
- concave polygons and narrow necks that split or disappear;
- disconnected islands;
- nested rings under even-odd and non-zero fill rules;
- native round, square, and butt-cap strokes;
- arcs and full circles;
- zero/negative radius identity behavior;
- empty input and an entirely eroded result.

### Metamorphic/property tests

For generated valid regions:

- increasing construction clearance or adding obstacles cannot enlarge
  `maximal_safe`;
- increasing the feature-disk radius cannot enlarge `opened_safe` for a fixed
  `maximal_safe`;
- rigid translation commutes with the result;
- `safe ⊆ panel_keep_in`;
- `safe ⊆ maximal_safe`;
- `safe ∩ obstacle_clearance = ∅`;
- `safe ∩ obstacle_gap_envelope = ∅`;
- `safe \ open(safe, q) = ∅`;
- radius-`v` dilations of every pair of distinct `safe` components are
  disjoint;
- `(safe ⊕ disk(r)) \ P = ∅`;
- `(safe ⊕ disk(r)) ∩ O = ∅`.

These required checks form the acceptance certificate. Broader disk-closing
additions remain diagnostic-only because they also include notches within one
component. Retain violation geometry on failure; a Boolean alone is not enough
to debug a numerical case.

### IPC integration fixtures

Check in small, purpose-built IPC fixtures for deterministic tests:

- direct board repeat with zero gap;
- board-cell nesting whose intermediate profile must not block balancing;
- tooling holes and fiducials on different layers;
- clear-polarity support geometry;
- routed cutout plus V-score relief;
- rotated/mirrored nested panel;
- missing or unpainted geometry, which must fail closed.

Use the survey corpus as an opt-in developer suite because those files are
large and external. Record summaries from the corpus, but keep correctness
assertions on compact checked-in fixtures.

### Performance and determinism

Record elapsed time, input/output ring counts, and peak ring count per stage.
Require deterministic ring serialization for identical inputs. Batch path
footprints and perform one regularized union per layer, then one cross-layer
union; do not repeatedly offset individual features.

## Completion criteria

The safe-region implementation is ready for a copper-balancing consumer when:

- every source layer is successfully projected through `ArraySupport`;
- every support feature has a defined painted footprint or produces a
  fail-closed diagnostic;
- the fixed-grid/offset error budget is explicit and tested;
- the nominal 0.5 mm certificate passes on the A7, A6, and A5 auto-panel
  baselines, all compact fixtures, and the wider survey topology suite;
- the 2 mm minimum-feature and minimum-gap certificates pass on the same
  corpus;
- auto Icarus, MicFlex, Blackstar, ControlHub, complex, nested, and
  empty-region visual reviews are clean;
- the result is deterministic and fast enough to regenerate during ordinary
  board-array iteration.
