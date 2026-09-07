# Mockingbird antenna regression

`antenna.kicad_pcb` wraps the exact E1 footprint extracted from Mockingbird
Feather's saved layout in a minimal two-layer, 1.6 mm board with a rectangular
outline. The footprint copper, drilled ground pad, feed pad, placement,
rotation, and properties are unchanged. It intentionally has **no** net-tie
annotation, matching the failing original input.

`mockingbird-antenna.xml` was exported with KiCad 10.0.6:

```sh
kicad-cli pcb export ipc2581 --output mockingbird-antenna.xml \
  --bom-col-int-id Path --bom-col-mfg-pn Mpn --bom-col-mfg Manufacturer \
  antenna.kicad_pcb
```

The fixture retains the real exported user primitive (renumbered `UPOLY_1` in
this minimal board), rotation 270°, location (168.90, -100.439392), both pad
nets, BOM-excluded component, drill, and stackup. Export timestamps are not
significant. Unit tests add the standard IPC-2581C `NetShort` representation
of the native `net_tie_pad_groups "1,2"` declaration, then exercise both
`standard` and `jlcpcb-1oz`, including negative cases. No custom properties
or BOM characteristics authorize a short.

This fixture is not an RF validation or a complete Mockingbird board check.
