# Panel geometry corpus v1

First-wave ENG-1428 inputs and inspection, not a panelizer or approved output
snapshots. Replay consumes only JSON (optionally zstd compressed); no IPC parsing,
KiCad, GUI, network, material defaults, or source workspace is required at runtime.
Build the executable once before using it offline. pcb-ir itself still depends on
the IPC type crate; replay does not call its parser.

```sh
cargo run -p pcb-corpus -- /tmp/corpus crates/pcb-corpus/fixtures/v1/*
# Expected nonzero exit: complete-removal deliberately rejects all material.
# Open /tmp/corpus/index.html in a browser; results.json is machine-readable.
cargo nextest run -p pcb-corpus
```

The runner sorts input paths, does not timestamp results, and writes both outputs
even for missing files or malformed JSON. Any non-completed result exits nonzero.
`Component::evaluate` is the independent prep-component boundary; `replay` validates
the versioned fixture first. Geometry replay performs even-odd regularization and
subtracts an explicit removal region. `Completed` means this operation completed,
**not** that panel manufacturing or a support arrangement is feasible. Components
own termination and return distinct physical, numerical, search, timeout,
tolerance-ambiguous, rejected, or unavailable outcomes. This first wave has no
solver or process watchdog; it does not invent timeout or physical results.

## Representation and inspection

Coordinates are board-local mm, Y up. Rings use even-odd fill, including holes;
the first/last point need not repeat. Region significance and extraction flatten
tolerances are explicit. `pcb-ir::ContourSet` owns all region interpretation and
boolean operations; `render::svg_path_data` owns path rendering. Overlays retain
their names and semantic uncertainty independently. Package and assembly outlines
are not unioned, not treated as body/courtyard, and not promoted to collision
constraints. Drill spans are retained; unknown-span holes are not subtracted.

The HTML compares the same viewport before/after, with supplied removal and
toggleable overlays. Input/output ring counts expose polygon topology changes,
not a connectivity proof. Source identities/evidence are expandable. Only net
planar area is calculated; mechanics are explicitly unavailable. Polygon results
do not establish source-curve accuracy, tolerance-band admissibility, cutter
access, full-panel support, or fracture behavior. These belong to other prep
components and physical coupon validation.

## Inputs and provenance

* `curves-hole`: canonical pcb-ir arc construction of a radius-10 mm circle,
  flattened at 0.005 mm, with an explicit 4 × 4 mm central removal.
* `concave-narrow-overhang`: a concave polygon with a hole, a 0.1 mm residual
  clearance, and an explicit synthetic 2 mm component overhang.
* `complete-removal`: deliberate geometric rejection, not a physical failure.
* `demo-dm0003`: genuine dioderobot/demo CM5 carrier export v2.0.1, source revision
  recorded in `sources/demo-dm0003.json`. The archive is hosted in diode's release
  tests; that does **not** establish a separate diode-designed-board example.
* `workspace-dm0002`: genuine existing pcb workspace IPC test board. Its export
  path/revision is known, its upstream design revision is not claimed.

Real-source fixtures retain all extracted regions without sampling or decimation;
zstd keeps their combined checked-in size below 1 MB. Their SHA-256 fields identify
the uncompressed source XML, not an expected geometry output. Source metadata,
diagnostics, per-layer copper, holes, and separately labelled component envelopes
are retained. Debug identity strings are snapshot-local; resolved material names
and thickness evidence are also present, but no mechanical model is supplied.

**Coverage gap:** no distinct diode-designed IPC export or third unrelated
workspace design was available. No exploratory output was accepted as a golden
snapshot. Add genuine exports with provenance when supplied, not fabricated board
geometry or renamed copies. The current two real boards are not evidence of
representativeness across manufacturing processes.

## Extraction (separate from replay)

The feature-gated extraction example requires ENG-1429's
`ImportedDesign::physical_board` API. It delegates all source interpretation to
`import_design -> physical_board`; it is not a second IPC geometry importer.
The initial real inputs were re-extracted against ENG-1429 commit
`6db3704ef789be6fbbefc2d05bbc80647b00353d`, including final composed drill/rout
images and preserved unresolved specification references. Both compressed inputs
were byte-identical after the final positive-aperture importer fix.

```sh
cargo run -p pcb-corpus --example synthetic -- /tmp/synthetic-inputs
unzip -p /path/to/diode/projects/api/test/fixtures/release/happy/artifact.zip \
  manufacturing/ipc2581.xml > /tmp/demo.xml
cargo run -p pcb-corpus --features extract --example extract -- \
  /tmp/demo.xml crates/pcb-corpus/sources/demo-dm0003.json demo-dm0003 /tmp/demo.json.zst
zstd -dc crates/ipc2581/tests/data/DM0002-IPC-2518.xml.zst > /tmp/dm0002.xml
cargo run -p pcb-corpus --features extract --example extract -- \
  /tmp/dm0002.xml crates/pcb-corpus/sources/workspace-dm0002.json workspace-dm0002 /tmp/dm0002.json.zst
```

Inspect regenerated inputs and their provenance before replacing versioned files.
These commands generate inputs, not approved expected-output snapshots. Tests
check analytical area/error bounds, hole membership, residual material, classified
failures, source-independent real replay, deterministic bytes, and HTML escaping.
