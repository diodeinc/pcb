# gerberx2

Fast Gerber X2 parser, typed data model, and writer scaffolding for PCB fabrication layers.

This crate is intentionally shaped like `ipc2581`: a pure parser/data-model crate with no CLI concerns. Higher-level tools should live in a separate crate or in `pcb` commands.

## Gerber concepts

Important concepts from the Gerber layer format specification:

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

Remaining implementation steps:

1. Validate region contours for self-intersection constraints.
2. Broaden writer coverage as IPC-2581 lowering exposes additional Gerber constructs.
3. Add end-to-end IPC-2581-to-Gerber smoke tests once IPC lowering is wired to the writer.

Initial support already decodes fixed-format coordinates through `FS` + `MO`, maintains graphics state for operations, builds ordered graphical objects for flashes/draws/arcs/regions, and lowers standard apertures (`C`, `R`, `O`, `P`) to geometry paths.

## Writer direction for IPC-2581 export

The writer intentionally accepts a string-backed artwork/object IR (`GerberLayer`) rather than flattened render geometry. IPC-2581 export should lower manufacturing layers into this level so semantic Gerber X2 can be emitted:

- standard pads as aperture flashes,
- tracks as aperture draws and native circular arcs,
- filled copper/mask/paste/legend as regions,
- file/aperture/object attributes as `TF`/`TA`/`TO`,
- flattened regions only when a source feature cannot be represented faithfully as a Gerber primitive.

The current writer emits standard apertures, aperture macros, block apertures, flashes, draws, arcs, regions, polarity changes, and X2 file/aperture/object attributes. Macro and block apertures are emitted as native Gerber constructs rather than silently flattened or approximated.

The intended smoke test for IPC-2581 export is geometry-level comparison between two generated fabrication sets:

1. `layout.kicad_pcb` → KiCad Gerber X2 export.
2. `layout.kicad_pcb` → KiCad IPC-2581 export → `gerberx2` writer export.
3. Parse both Gerber sets into `pcb_ir::dialects::artwork::ArtworkDocument`s, then compare each matching layer with `pcb_ir::dialects::gerber::compare::compare_documents`.

This comparison deliberately allows different command streams/aperture tables, but fails on layer function, final image bounds, or filled area drift beyond tolerance.
