# PCBIR geometry accuracy

`GeometryAccuracy::new(mm)` requests a total accumulated boundary approximation
allowance in millimetres. It is independent of `ContourSet::tolerance` (feature significance
and containment slack) and `tol::EPSILON_MM` (numerical coincidence).
Geometry operations require this budget. CLI entry points choose a 0.01 mm
default once and pass it through. There is no compatibility mode.

Use `ContourSet::from_contours` to prepare source curves, then
`disk_dilate` or `disk_erode` for disk offsets.
The `GeometryAccuracy` API documentation includes an example.

Contours, arenas, and regions carry `uncertainty_mm`. Transforms, composition,
decimation, offsets, and region distance measurements propagate it. Retained
arcs and analytic ellipses can be prepared more finely, including after arena
copies and affine placement. Polygon round trips cannot recover lost precision.

Raw command/ring extraction discards provenance. Supply the known error with
`ContourBuf::with_uncertainty` or `ContourSet::from_regularized`
when reconstructing geometry; zero asserts that the commands/polygons are the
actual source. `with_uncertainty` also discards retained ellipse parameters.

Unmet budgets return `AccuracyError`, including these backend limits:

- Ellipse cubics without retained source have a `0.0003 * max(width, height) / 2`
  error floor.
- Boolean and paint composition retain the largest input error and add rounding allowance.
- Disk offsets include input uncertainty and round-join
  error. The current minimum join angle is `0.01π`, requiring an additional
  allowance of at least `radius * (1 - cos(0.01π))`.
- Kurbo's fixed round-cap/join conversion adds `0.0004 * stroke_width / 2`.
  Pattern placement requires lines or circular arcs.
- Coordinate resolution and excessive subdivision can prevent meeting a budget.

Small rings remain in prepared geometry so significance thresholds do not
discard geometry or its accuracy. Operations allocate one quarter of the remaining
budget to new approximation, divided among their approximation stages.
This practical accounting does not certify final Hausdorff distance, topology, area error, or grazing
line-span endpoints. Containment, coverage, and line spans describe the prepared
polygons; manufacturing policy remains with the consumer.

For headless IPC use, pass `accuracy` to `physical_view`, `physical_lands`,
`physical_holes`, or `composed_layer_image`. These prepare retained sources
before resolving voids and cutouts. `feature_region` prepares one occurrence.
`artwork::compose_owner_regions` accepts a separate significance tolerance and
a required accuracy budget.
