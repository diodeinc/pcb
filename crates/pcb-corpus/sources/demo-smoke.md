# Seven-repository source smoke · 2026-09-07

All seven repositories clone successfully and contain a checked-in main KiCad
layout. No pre-existing IPC export was found in this inventory. KiCad CLI 10.0.6
exports each main layout successfully; canonical `import_design -> physical_board`
and geometry replay complete for all seven. This is **software smoke only**:
not a Zen build, layout synchronization check, DRC, support calculation,
manufacturing approval, or physical fracture validation.

## Exact source revisions and observed outcomes

Repository URLs share `https://code.diode.computer/demo/b/`. The main layout path
is `layout/layout.kicad_pcb` except Bramble's `layout/DM0003/layout.kicad_pcb`.
All rows have export / extraction / replay status `completed`.

| Fixture | Repository | Source revision | Observed substrate area, mm² | Unspecified / incomplete envelopes |
|---|---|---|---:|---:|
| demo-bramble | DM0003.git | 7a193fc02608d0938bdf067a0a3d35bbb06ee15a | 3479.489536 | 82 / 78 |
| demo-demeter | Demeter.git | 6f7f3993c04b25cb283a5e5868a12ccf72f6d6fd | 960.000000 | 53 / 51 |
| demo-feign | Feign.git | edeea2bb93b52e493c6173ed3771d198a246027a | 606.900000 | 41 / 40 |
| demo-governor | DM0001.git | a02803dbc47eb6a31ef6a511c77ec921ecf85a16 | 1281.062500 | 149 / 149 |
| demo-marlow | DM0002.git | 440a5dafb04c8badf372e508634102315ba8f77a | 763.057173 | 73 / 71 |
| demo-renfield | Renfield.git | 0bad28f2b5f48ea4b37cd31fe6d189edbb3db6f7 | 697.000000 | 52 / 51 |
| demo-seward | Seward.git | 823d276cb18a2e9df6e9169389f018bdb5332eb0 | 448.000000 | 49 / 48 |

Areas are measurements from this replay, not golden values asserted by tests.
Every substrate has one exterior ring. Substrate precedes drill/rout removal;
the lack of interior substrate rings is not evidence of no holes. Per-source hole
apertures, polarity-composed drill/rout layers, copper and component-envelope
overlays are retained independently. There is no supplied attachment site,
support fixture, load case, or mechanical model.

No source-import diagnostics were emitted. All seven physical views retain
unspecified/incomplete package-envelope diagnostics and ambiguous/missing
material or invalid zero-thickness evidence on source stack layers. These are
not dropped or interpreted as a qualified laminate/body model. Overall thickness
source evidence is 1.566 mm for Bramble/Governor and 1.6062 mm for the others;
this smoke does not turn it into assumed mechanical properties.

## Source/build guidance and blockers

Root `pcb.toml` identifies the board and registry dependencies in every repository.
All main layouts already exist, so no `pcb build`, dependency resolution, library
download, or KiCad GUI is required for this export smoke. Vendored module/reference
layouts are not selected as substitute boards. No `AGENTS.md` files were present
in the cloned sources at these revisions.

* Bramble README identifies the CM5 carrier and main layout directory; it notes
  imported-source component dependencies may require resolution for a fresh build.
* Demeter, Feign and Renfield READMEs describe their CAN, UART and PD designs.
* Governor README describes the BLDC reference design; its older directory/source
  names are not used to override the actual `pcb.toml` board identity.
* Marlow has no root README; `pcb.toml` identifies the RP2040 CMSIS-DAP probe.
* Seward README explicitly states that its latest hardware changes still need
  layout synchronization and bench validation. Its fixture represents only the
  checked-in layout, not a verified realization of current `Seward.zen`.

There are no export/extraction/replay blockers for these checked-in layouts.
Physical qualification, complete measured component bodies, material-model
resolution and source/layout synchronization remain unverified.

## Reproduction

Clone from any existing parent directory, then pin each checkout to the revision
in the table before extraction (for example `git -C Bramble checkout --detach
7a193fc02608d0938bdf067a0a3d35bbb06ee15a`).

```sh
git clone https://code.diode.computer/demo/b/DM0003.git Bramble
git clone https://code.diode.computer/demo/b/Demeter.git
git clone https://code.diode.computer/demo/b/Feign.git
git clone https://code.diode.computer/demo/b/DM0001.git Governor
git clone https://code.diode.computer/demo/b/DM0002.git Marlow
git clone https://code.diode.computer/demo/b/Renfield.git
git clone https://code.diode.computer/demo/b/Seward.git
```

From the pcb repository, with the sibling ENG-1429 physical-view changes present:

```sh
bash crates/pcb-corpus/scripts/smoke-demo.sh /tmp/corpus-sources /tmp/demo-smoke
```

This is the actual smoke command used. It captures revisions, layout hashes,
exporter version, provenance, logs, per-stage outcomes and canonical fixtures.
The individual export command is:

```sh
kicad-cli pcb export ipc2581 --version C --units mm --precision 6 \
  -o /tmp/demo-smoke/exports/Demeter.xml /tmp/corpus-sources/Demeter/layout/layout.kicad_pcb
cargo run --quiet -p pcb-corpus --features extract --example extract -- \
  /tmp/demo-smoke/exports/Demeter.xml /tmp/demo-smoke/provenance/demo-demeter.json \
  demo-demeter /tmp/demo-smoke/fixtures/demo-demeter.json.zst
cargo run --quiet -p pcb-corpus -- /tmp/demo-smoke/replay/Demeter \
  /tmp/demo-smoke/fixtures/demo-demeter.json.zst
```

The versioned `fixtures/v1/demo-*.json.zst` records contain exact layout SHA-256,
generated XML SHA-256, repository URL/revision and exporter command. XML includes
export-time metadata, so its hash can differ on regeneration without a geometry
change. Inspect changes; do not accept generated outputs as approved snapshots.
The extracted polygon tolerance is 0.001 mm and curve flattening tolerance is
0.005 mm. Reuse `pcb_corpus::load` and `region(&fixture.substrate,
fixture.tolerance_mm)` for independent prep-component software checks.
