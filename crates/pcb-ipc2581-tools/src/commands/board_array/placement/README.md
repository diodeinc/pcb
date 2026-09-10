# Frame-only placement analysis

`board-array create --mouse-bite --mouse-bite-plan policy.json` selects candidate
tab positions and counts using the existing eligibility, mesh, mechanics and
budgeted support-selection APIs. Output is JSON, never panel XML. Omitting
`--mouse-bite-plan` retains the existing single-board eligibility report. Omitting
`--mouse-bite` retains V-score generation.

Use `--auto` or `--sheet` for the existing automatic array layout. Alternatively
use the existing columns/rows/margins/edge-rail options. The analysis models the
retained frame as the existing rounded outer stock profile minus each board bounding rectangle expanded by
`routing_gap_mm`. All margins must exceed that gap to leave continuous internal
rails. Auto layout reserves margins but cannot guarantee usable component-clear
sites, sufficient landing depth, mechanical feasibility or available search
resources. A failed frame connection never falls back to another board.

## Explicit policy

The following is a **synthetic numerical example**, not an FR-4 calibration or a
manufacturing recommendation. Every field is required; unknown fields fail.

```json
{
  "routing_gap_mm": 0.5,
  "frame_landing_mm": 0.3,
  "max_span_mm": 3.0,
  "candidate_pitch_mm": 10.0,
  "max_candidates": 32,
  "bending": [[10, 2, 0], [2, 10, 0], [0, 0, 4]],
  "connection_stiffness": [[10, 0, 0], [0, 10, 0], [0, 0, 10]],
  "scales": [1, 0.1],
  "mesh_max_area_mm2": 20,
  "mesh_min_angle_degrees": 0,
  "mesh_max_additional_vertices": 100,
  "max_dofs": 500,
  "max_subsets": 1000,
  "clamp_sides": [false, true, false, false],
  "load_cases": [{"resultant_per_board": [1, 0.3, -0.7], "compliance_limit": 100}]
}
```

```sh
pcb ipc2581 board-array create board.xml --mouse-bite \
  --mouse-bite-width 1 --mouse-bite-inward 0.25 \
  --mouse-bite-outward 1 --mouse-bite-clearance 0 \
  --mouse-bite-plan policy.json --auto -o placement.json
```

The existing width/inward/outward/clearance values still define the eligibility
band; placement additionally checks the entire straight connection envelope all
the way to the frame against supplied courtyard evidence and other boards.
Only open `Eligible` intervals are sampled, at equal-bin midpoints with bin size
at most `candidate_pitch_mm`. This parameter controls candidate resolution, not
a preferred support spacing. Short intervals still receive a midpoint. Search
only optimizes this finite set; candidate-budget overflow is an error, not silent
truncation. Overlapping envelopes and overlapping cyclic attachment spans conflict.

## Mechanical interpretation

Boards and the connected frame remain distinct flexible plates. The common
supplied bending tensor acts on `[w,xx, w,yy, 2w,xy]` in global axes, in N·mm.
Connections couple finite-area board/frame landings through the existing fitted
rigid-plane model. Curved board landings are clipped to substrate. Each supplied
connection tensor acts on `[w, theta_x, theta_y]`, referenced at the candidate's
`board_point`, in global axes: units are N/mm, N and N·mm for translation,
translation/rotation coupling and rotation respectively. This is an explicit
idealization; footprint extent/reference and stiffness must be calibrated together.

Each load case applies its `[Fz, Mx, My]` to every board at its bounding-box center,
through a whole-board area-L² fitted plane. It is not a uniform pressure or a
component point load. Clamp booleans select top/right/bottom/left exterior frame
sides; both displacement and normal slope are zero there. No board is implicitly
grounded. Numerical scales/tolerances do not establish physical acceptance.

The selector minimizes count subject to the supplied compliance limits, then
worst normalized compliance. JSON contains candidate board/frame points, IDs,
rejections/conflicts, selected IDs/count, compliance, strain energy, residuals,
whole-board fitted displacement/rotations, mesh quality, and unresolved/search-budget status. Unsupported smaller subsets
retain `Unresolved`, even if a verified feasible selection exists. A null
selection never means zero tabs suffice. Dense storage/solves and exhaustive
search limit this phase to small models; budgets do not promise wall-clock bounds.

Footprints without any courtyard contribute no obstruction and are listed in
`eligibility.ignored_footprints`. All usable courtyards from either side remain
obstacles; present but unusable courtyards are errors. Clearance is conditional
on supplied courtyards being complete, not a physical-component clearance guarantee.
Explicit unknown exclusions still leave sites Unknown. A successful
mechanical result remains conditional on supplied stiffness, load and fixture
assumptions and mesh convergence. No tab shapes, holes, route masks, router-access
certification, generated rail-tooling clearance, fabrication export or breaking
strength assessment are included.
