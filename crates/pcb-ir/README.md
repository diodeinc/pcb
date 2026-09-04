# PCBIR geometry accuracy

`GeometryAccuracy::new(mm)` requests a total accumulated boundary approximation
allowance in millimetres. It is independent of `ContourSet::tolerance` (minimum
ring area and containment slack) and `tol::EPSILON_MM` (numerical coincidence).
Existing APIs keep their default 5 µm curve and 10 µm outline sampling.

Use `ContourSet::from_contours_with_accuracy` to prepare source curves, then
`disk_dilate_with_accuracy` or `disk_erode_with_accuracy` for disk offsets.
The `GeometryAccuracy` API documentation includes an example.

Contours, arenas, and regions carry `uncertainty_mm`. Transforms, composition,
decimation, offsets, and region distance measurements propagate it. Retained
arcs and analytic ellipses can be prepared more finely, including after arena
copies and affine placement. Polygon round trips cannot recover lost precision.

Raw command/ring extraction discards provenance. Supply the known error with
`ContourBuf::with_uncertainty` or `ContourSet::from_regularized_with_uncertainty`
when reconstructing geometry; zero asserts that the commands/polygons are the
actual source. `with_uncertainty` also discards retained ellipse parameters.
Legacy untracked ring constructors use the default flattening allowance.

Unmet budgets return `AccuracyError`, including these backend limits:

- Ellipse cubics without retained source have a `0.0003 * max(width, height) / 2`
  error floor.
- Disk offsets include input uncertainty, miter amplification, and round-join
  error. The current minimum join angle is `0.01π`, requiring an additional
  allowance of at least `radius * (1 - cos(0.01π))`.
- Kurbo's fixed round-cap/join conversion adds `0.0004 * stroke_width / 2`.
  Patterned strokes have no bounded preparation path.
- Coordinate resolution and excessive subdivision can prevent meeting a budget.

Significance filtering that removes a ring makes uncertainty infinite. Offsets
also report unbounded uncertainty when an uncertain input changes ring count.
The allowance does not certify topology near tangencies, area error, or grazing
line-span endpoints. Containment, coverage, and line spans describe the prepared
polygons; manufacturing policy remains with the consumer.

For headless IPC use, set `ImportedDesign::region_accuracy = Some(accuracy)`
before calling `physical_view`, `physical_lands`, `physical_holes`, or
`composed_layer_image`. These prepare retained sources before resolving voids
and cutouts. `feature_region_with_accuracy` prepares one occurrence; the legacy
`feature_region` keeps defaults. `artwork::compose_owner_regions` accepts a
separate significance tolerance and optional accuracy budget.
