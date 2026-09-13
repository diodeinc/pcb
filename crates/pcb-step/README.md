# pcb-step

`pcb-step` writes the STEP assembly for a `.kicad_pcb` file without OCCT:
the board body is emitted directly as a BRep, and every footprint's STEP
model is copied through with its ids renumbered. Its output matches what
`kicad-cli pcb export step --include-silkscreen --include-soldermask`
produces, and it is around two orders of magnitude faster. It began life
as `kicad-step` in the KiCad tree.

```bash
pcb step export board.kicad_pcb -o board.step
```

Models come from the board's embedded files; nothing is looked up on disk.

## What is exported

The defaults follow KiCad's exporter (`pcbnew/exporters/step/`):

- **Board body**: one solid per `Edge.Cuts` outline, spanning `z ∈ [0, T]`
  where `T` is the sum of the dielectric and inner copper layers. Outer
  copper sits outside the body, as in KiCad. Without a `(stackup ...)`
  section the stack is built the way `BOARD_STACKUP::BuildDefaultStackupList`
  does it (35 µm copper, 10 µm masks). Colour is the front mask colour from
  the stackup, darkened by 0.2 and encoded to sRGB.
- **Drills**: every pad drill, plated or not, cut clean through. Round
  drills are cylinders; slots are stadiums of two lines and two arcs. Via
  drills are cut only with `--cut-vias-in-body`.
- **Via finish and machining**, where this is deliberately more physical
  than KiCad: the body is dielectric plus via fill, so a filled or capped
  via is left solid (KiCad cuts it), and mask-only features (tenting,
  covering, plugging) do not touch it (KiCad ignores them too). Blind vias
  are pockets with a flat floor. Backdrills, counterbores and countersinks
  on pads and vias are cut as KiCad cuts them, with depths measured from
  the outer copper surface, but as exact cylinders, annular shoulders and
  cones rather than boolean results. A round hole is one radius-versus-depth
  profile, so any combination of these is a single analytic wall set.
- **Outline**: `gr_line`, `gr_arc`, `gr_circle`, `gr_rect`, `gr_poly`
  (including arc segments) and the `fp_*` equivalents on `Edge.Cuts`. Arcs
  and circles stay analytic. Beziers are flattened. Several outlines give
  several solids; closed loops inside an outline are cutouts. Edges are
  chained with KiCad's 0.01 mm tolerance.
- **Drills that cross an edge or another hole** are merged in with a loop
  boolean, so castellations and overlapping slots come out as one clean
  shell instead of failing.
- **Components**: every `model` with a `.step`/`.stp` name, placed with
  KiCad's transform (position, rotation, bottom flip, model offset, then
  the model's own rotation, 0.05 mm above the copper). Embedded files and
  `${VAR}`/project-relative paths are resolved; a file used by several
  footprints, or embedded twice under different names, is copied once and
  instanced. Uniform scale is baked into the copy. DNP and unspecified
  footprints are included unless `--no-dnp`/`--no-unspecified`;
  `--component-filter` takes reference designator globs.
- **Donor STEP files** are copied without interpretation: the displayed
  representations are found through the product structure, the assembly
  tree is walked through `NEXT_ASSEMBLY_USAGE_OCCURRENCE` the way OCCT
  does and rebuilt with `MAPPED_ITEM`, surface styles are kept, curve
  styles and parametric curves are dropped (as OCCT does on write), and
  lengths and angles are converted from the donor's units. Reals are
  rewritten with 12 significant digits, OCCT's precision, and each model's
  representations declare the 1e-4 mm accuracy KiCad reads models at.

- **Copper** (`--include-pads`, `--include-tracks`, `--include-zones`,
  `--include-inner-copper`), following KiCad's exporter: pads are one
  exact prism per pad per layer; tracks, arc tracks, via annular rings and
  zone fills on one layer and net are unioned in 2D and each island is
  extruded; plated pads and vias get a 25 µm copper tube through the
  stack. Three products, `<board>_copper`, `<board>_pad` and
  `<board>_via`, coloured as KiCad colours them. Three deliberate
  differences: pads are exactly the copper thickness (KiCad adds 5 µm
  unless `--no-extra-pad-thickness`); layer copper is cut at the drill
  wall so it meets the plating tube instead of overlapping it, which makes
  through-hole pads 1 to 10% lighter than KiCad's; and tracks and zones
  are cut away under every pad that overlaps them, whatever its net, so
  nothing overlaps a pad solid (KiCad leaves both, and the 5 µm hides the
  overlap). Fills go stale when a pad is placed after the last refill, so
  clearance is never assumed; each pad and drill is tested against the
  island before it joins the boolean. Pads are also bitten by holes and
  vias through them, as KiCad cuts them. The 2D work is pcb-ir: arc-aware
  contours, `i_overlay` booleans, connected components, point queries. Rings are then cleaned (0.2 µm vertex merge, pinch
  splitting, 2 µm collinear simplification) and chord runs refitted to
  arcs, so islands are compact and every shell is manifold. Unused-layer
  removal on pads and vias uses touching tracks and containing fills as
  the connectivity test.

- **Silkscreen and solder mask**, on by default (`--no-silkscreen` and
  `--no-soldermask` leave them out; kicad-cli's `--include-*` flags are
  accepted), as KiCad exports them: flat faces 0.04 mm and
  0.015 mm above the outer copper (and as far below the back copper),
  one product per side, coloured from the stackup with KiCad's
  transparency. Silkscreen is the union of the layer's graphics and text
  with the drills, backdrills and counterbores taken out, clipped to the
  board. Text is KiCad's stroke font (`newstroke`, embedded up to U+26FF)
  laid out with KiCad's spacing, justification, italic tilt and markup;
  `${NAME}` variables come from the project file beside the board, and
  `${REFERENCE}`/`${VALUE}` from the footprint. The mask is the board
  minus every opening: pads on the mask layer grown by their margin (pad,
  then footprint, then the board's clearance), untented via rings grown
  by the board's clearance with their drills, graphics and text drawn on
  the mask layer, and machined mouths. One deliberate difference: pads
  open the mask whether or not pads are exported, where kicad-cli only
  opens them with `--include-pads`. Not drawn: text boxes, tables,
  barcodes, knockout text (drawn as plain text), dashed strokes (drawn
  solid) and outline fonts (drawn with the stroke font).

Missing model files are warned about and skipped; a model that exists but
cannot be read makes the exit code 1 after the file is written, both as
`kicad-cli` does. A board with no closed outline is an error, where KiCad
would silently export its bounding box.

## Layout

| File | Role |
| --- | --- |
| `src/sexpr.rs` | Pull parser over the board bytes. No tree: callers open the lists they need and skip the rest. |
| `src/board.rs` | The board as flat arrays: footprints, models, drills, vias, outline edges, embedded files, stackup. One pass. |
| `src/outline.rs` | Line/arc loops, chaining with exact gap closing, outer/hole classification; drills that touch anything go through a polygon boolean and are snapped back onto the exact source curves. |
| `src/holes.rs` | Round holes as radius profiles: drills, blind vias, backdrills, counterbores, countersinks, fill. |
| `src/copper.rs` | Pad outlines, track strokes, per-layer unions through pcb-ir with pads and drills cut out, plating tubes. |
| `src/rings.rs` | Rings from a boolean into loops: vertex merge, pinch split, nesting, arc refit with a crossing guard. |
| `src/faces.rs` | Silkscreen and mask faces: strokes, fills and openings unioned cell by cell in parallel, drills cut and the board edge applied where they meet an island. |
| `src/font.rs` | KiCad's stroke font: glyph decoding, line layout, justification, markup, pen width. |
| `src/newstroke.rs` | The `newstroke` glyph table, generated from `common/newstroke_font.cpp`. |
| `src/donor.rs` | Donor STEP files: statement split, kind byte per statement, product tree, geometry closure, renumbered copy. |
| `src/step.rs` | The output buffer, the STEP vocabulary, the extruded board solid, product structure. |
| `src/lib.rs` | `Board::parse` and `export`: options, model batching across threads, placement math. |
| `pcbc/src/step.rs` | The `pcb step` command and the release entry point. |

Data lives in flat vectors indexed by integers. The board file is parsed
once and never copied; names are borrowed slices. Donor files are owned
buffers with `u32` offsets and are freed as soon as they are copied. Each
donor statement is classified into a byte once; the closure walk, the style
index and the id renumbering are table lookups. Output is streamed to the
sink in model-sized chunks, so peak memory is the board plus one batch of
models rather than the output.

Copper is built per layer and net on worker threads, largest groups
first; the board's contours are handed to pcb-ir once per group and the
islands come back as polylines. Copper solids are written in parallel
batches with ids from 1, and each batch is moved up to its place in the
file by rewriting its ids, which is far cheaper than formatting them.

Models are copied in batches on worker threads: a batch is decoded and
analyzed in parallel, given id ranges serially in model order, emitted in
parallel into private buffers, and written in order. A donor over 4 MB is
itself scanned and copied on several threads, since one large connector
model is often the whole batch. Ids depend only on model order, so the
output is identical whatever the thread count.

## Validation

`scripts/oracle.py` loads two STEP files through OCCT (via `cadquery`) and
compares them: `BRepCheck` validity of every solid, occurrence count, board
volume and bounding box, and per-reference component volumes and bounding
boxes.

```bash
uv run --with cadquery crates/pcb-step/scripts/oracle.py ours.step kicad.step
```

`scripts/corpus.py` runs both exporters over every board under a directory,
times them, runs the oracle on each pair and prints the speedup, size ratio
and match count.

## Not done

- `--fuse-shapes` and `--net-filter` are not implemented.
- Non-uniform model scale is skipped with a warning; KiCad converts such
  models to B-splines.
- A donor that itself uses `MAPPED_ITEM` is rejected.
- A donor OCCT cannot build solids from is copied as is; KiCad's OCCT
  pipeline heals some such files on write, this exporter has no kernel to
  do that with.
- Presentation entities are copied per donor rather than deduplicated
  across the file, so files with many small styled faces are a few percent
  larger than OCCT's.
