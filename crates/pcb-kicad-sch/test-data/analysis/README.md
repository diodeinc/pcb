# Schematic analysis fixture

`simple.zen` is a minimal two-resistor design with three nets. The KiCad project
was generated from it with the Diode schematic editor exporter at Diode revision
`4c0d4c6ad7`:

```sh
just editor run-sch export-kicad /absolute/path/to/simple.zen
```

The schematic declares the KiCad 10 format version `20260306`. KiCad 10.0.2
successfully exported its netlist after conversion. KiCad 10 also exported the
same schematic with no external `.kicad_sym` file or `sym-lib-table`; the
schematic's embedded `lib_symbols` definitions are sufficient for its existing
symbols. The placed-symbol UUIDs were normalized to the shared schematic/layout
identity rule: UUID v5 with `NAMESPACE_URL` over the canonical component path.

The integration-test harness accepts this project directory, a Zener entrypoint,
and a KiCad project path. It compiles the complete Zener project, parses the real
KiCad files, and independently snapshots each source's reduced connectivity
graph before snapshotting the resulting analysis. Focused failure cases edit the
parsed semantic document; for example, the disconnected-net case removes one
known wire by UUID. The same fixture also enforces the schematic/layout UUID
contract.
