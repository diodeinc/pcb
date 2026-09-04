# PCBA assembly report v4

`assembly::build_report` lowers one cached `pcb-ir` imported design into the
deterministic contract used by the CLI and native library. The report carries
design and manufacturing facts only. It does not select a part offer, infer
commercial process policy, multiply by order quantity, or contain prices.

The CLI reports the complete board array by default. Pass `--scope board` to
select one canonical board instead.

```bash
pcb ipc assembly design.xml --scope board-array > assembly-report.json
```

The report has no generated timestamp or host file path. Repeating a build
with the same IPC input and scope produces identical JSON. Stable identifiers
derive from semantic source facts rather than `pcb-ir` vector indices.

## Units and coordinates

All lengths and coordinates are millimeters. Areas are square millimeters and
angles are degrees. Affine matrices use `[a, b, c, d, tx, ty]`, where
`x' = a*x + b*y + tx` and `y' = c*x + d*y + ty`. Coordinates use the IPC-2581
design frame: +X right, +Y up.

Source `Xform` records remain explicit. IPC-2581C applies their operations in
this order: origin offset, rotation, mirror, then scale.

`board` selects one canonical board and normalizes it to the design origin.
`board_array` selects the root Step and materializes nested StepRepeat records.
Each board occurrence retains the complete source StepRepeat path and its
board-local-to-scope transform.

## Physical profiles

`profiles` contains each IPC-2581C Step profile as an outer contour and
explicit cutout contours. Contours retain line, arc, cubic, and close commands
in source order. Each profile also has local bounds and net material area.

The selected scope and each board occurrence reference their profile
definitions by ID and expose the combined local bounds and area. Board-array
scope uses only the root Step profile as the panel envelope. If the source has
no root profile, these scope metrics are `null`; child board outlines are not
invented as a panel outline. Bounds come from the canonical line and circular
arc commands, and area is integrated directly from canonical path segments;
neither metric uses tolerance-based curve flattening.

## Package geometry

Each package preserves its IPC-2581C primary, topside, and other-side views
when present. Package-level fields retain pin-one metadata, height, negative
body extension, comment, and pickup point when supplied. A view can contain:

- a body outline and its source transform;
- a land pattern with pads, targets, pin references, and transforms;
- silkscreen outlines and markings;
- an assembly drawing with its outline and markings;
- structural text with content, font evidence, transform, and bounds.

Package pins retain their local location, transform, source attributes, and
shape. Standard primitives are lowered to ordered canonical paths with fill or
stroke style and dark or clear polarity. Geometry references retain the IPC
dictionary identity used by the source.

Every shape has one explicit status:

| Status | Meaning |
| --- | --- |
| `complete` | All represented source geometry was lowered to canonical paths. |
| `partial` | Usable paths exist, but some source styling or geometry is not represented. |
| `unresolved` | A referenced dictionary entry is absent. |
| `unsupported` | The source geometry exists but has no exact canonical lowering. |

The importer does not guess package families or dimensions, fabricate default
shapes, or use package-name classification. User primitives remain typed source
references with `unsupported` status until an exact lowerer exists. These
statuses are report evidence, not IPC-2581 output. Hatch and mesh fills retain
their source references and canonical boundary paths as `partial`, but those
paths remain unpainted rather than pretending to represent the source fill. An
exporter must resolve all references before it can emit conforming IPC-2581C
XML.

## Source facts and unknown values

`null` means that the IPC source did not supply or resolve a fact. Empty arrays
mean a genuinely empty source-backed collection. A missing BOM `pinCount`
remains `null`; it never becomes a default pin or joint count.
`quantity_source` and `pin_count_source` retain the IPC lexical values alongside
any parsed integer.

IPC-2581C defines `RefDes/@populate` as optional without a schema default. The
report records an omitted attribute as `unspecified` and emits a readiness
diagnostic. The source XML remains valid IPC-2581C.

Termination `total` and paste `islands` describe all source-backed physical
facts in scope. Fields ending in `on_included_populated_components` are the
safe assembly-work subset; excluded DOCUMENT records, DNP components, and
unresolved population do not contribute to those fields.

Packages contain only definitions referenced by components in the selected
scope. Physical terminations include only pins explicitly marked
`electricalType="ELECTRICAL"`. Copper replicas collapse only when component,
pin, padstack, and location identities match exactly. Paste islands link only
through that same exact identity.

`holes` contains stable source-backed drilled-hole facts, including location,
source name, finished diameter, plating, padstack, net, and layer span. Each
termination lists its associated `hole_ids`, paste islands, and source-backed
assembly-side soldermask openings. Source component/pin evidence takes
precedence when present. Otherwise, a hole associates with a termination only
when its declared span reaches exactly one land on a known assembly side.
Multiple exact overlaps are `ambiguous`, and no exact overlap is `unresolved`;
nearest-object matching is never used. `basis` preserves whether source
identity or exact geometry produced the candidates.

Via protection is an independent intent assembled from explicit source
evidence. `VIA_CAPPED` contributes `capped`; `HOLEFILL` contributes `filled` or
`plugged`. Coating layers contribute no method unless their specification
values name a protection action. Explicit specification values can identify
`open`, `tented`, `plugged`, `filled`, and `capped`. Fill material is normalized
only from specification material and property values, never layer or
specification names. Missing protection evidence remains `unknown`: geometry
does not imply fill, protection, factory capability, or quote readiness.

Components remain present when excluded. A preferred BOM item with
`category="DOCUMENT"` sets `assembly_status` to `excluded` and
`exclusion_reason` to `document_bom_category`. DNP is not an exclusion: it is
the independent `do_not_populate` population state.

Report construction rejects non-finite numeric evidence instead of allowing
JSON serialization to replace it with `null`.

## Readiness

`ready` means the implemented source-completeness checks found no diagnostics.
`review_required` means only warnings remain. `incomplete` means at least one
error remains. This is assembly-data readiness, not a price or factory-routing
decision.

The report emits errors for an included component with absent or conflicting
population, a missing reference designator, a populated component without a
resolved package, or a populated SMT or through-hole component without an
exact physical termination.

It emits warnings when a hole has ambiguous or conflicting termination
evidence, or when an associated via has unknown or conflicting protection
intent.

The complete report contract is declared in `src/assembly/report.rs`. The
IPC-2581C-valid fixture in `src/assembly/testdata/report.xml` exercises package
geometry, structural text, board cutouts, via protection, and mirrored panel
repeats.
