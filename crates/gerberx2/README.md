# gerberx2

Fast Gerber X2 parser, typed data model, and writer scaffolding for PCB fabrication layers.

This crate is intentionally shaped like `ipc2581`: a pure parser/data-model crate with no CLI concerns. Higher-level tools should live in a separate crate or in `pcb` commands.

## Spec references read

Primary local reference:

- PDF: `/Users/akhilles/.pcb/cache/datasheets/materialized/c8655187-b74b-558f-985f-dc59209475c9/gerber-layer-format-specification-revision-2022-02_en.pdf`
- Markdown: `/Users/akhilles/.pcb/cache/datasheets/materialized/c8655187-b74b-558f-985f-dc59209475c9/gerber-layer-format-specification-revision-2022-02_en.md`

Important concepts from the spec:

- A Gerber file is one complete 2D binary vector image, represented as an ordered command stream.
- X2 means the file uses attributes: `TF`, `TA`, `TO`, `TD`.
- Attributes do not affect geometry, but preserve design intent such as layer function, aperture function, net, pin, component, generation software, project id, and checksum.
- The graphics state controls operation interpretation: units, coordinate format, current point, current aperture, plot mode, polarity, mirroring, rotation, and scaling.
- Graphical objects are ordered. Dark objects add material/image; clear objects erase previously generated material/image.
- Core objects are draws, arcs, flashes, and regions.
- Standard apertures are circle, rectangle, obround, and regular polygon; aperture macros and block apertures extend this.
- `G36/G37` creates regions from one or more closed contours. Regions can carry aperture attributes.
- `SR` step-and-repeat and `AB` block apertures create reusable/repeated object streams.
- `M02*` is mandatory and must be the final command.
- `.FileFunction` is the primary X2 layer identifier (`Copper,L1,Top`, `Paste,Top`, `Soldermask,Bot`, `Plated,1,4,PTH`, `Profile,NP`, etc.).
- `.AperFunction` identifies object intent (`SMDPad`, `HeatsinkPad`, `ViaPad`, `Conductor`, `Material`, `ViaDrill`, `ComponentDrill`, etc.).

## Initial crate shape

- `GerberX2` owns an `Interner`, parsed commands, attributes, aperture definitions, macro definitions, and final graphics state.
- `types` contains fat structs/enums for commands, attributes, apertures, graphics state, and future graphical objects.
- `parse` is a fast direct scanner over the input string. It avoids regex and parses Gerber word/extended commands in one pass.

## Proposed fat data model direction

Keep data broad and explicit rather than overly normalized:

```rust
pub struct GerberX2 {
    interner: Interner,
    commands: Vec<Command>,
    file_attributes: Vec<Attribute>,
    aperture_definitions: Vec<ApertureDefinition>,
    aperture_macros: Vec<ApertureMacro>,
    objects: Vec<GraphicalObject>,
    final_state: GraphicsState,
    diagnostics: Vec<Diagnostic>,
}
```

For fast rendering/export, lower command streams into object streams:

```rust
pub struct GraphicalObject {
    kind: ObjectKind,
    polarity: Polarity,
    mirroring: Mirroring,
    rotation_degrees: f64,
    scaling: f64,
    aperture_attributes: Vec<Attribute>,
    object_attributes: Vec<Attribute>,
}
```

This keeps the original command stream for round-trip/generation work while providing a direct, renderer-friendly object list for SVG and IPC-2581 conversion.

Next implementation steps:

1. Parse and evaluate fixed-format coordinates into real units using `FS` + `MO`.
2. Maintain graphics state while parsing commands.
3. Build ordered `GraphicalObject`s for `D01/D02/D03` outside regions.
4. Build region contours for `G36/G37`.
5. Lower standard apertures to geometry paths.
6. Add aperture macro expression parsing/evaluation.
7. Add block aperture and step-repeat object expansion.
8. Add Gerber writer that emits attributes comprehensively.
9. Add SVG renderer using the same geometry/rendering concepts as IPC-2581 tools.
