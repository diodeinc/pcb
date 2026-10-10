# KiCad 10 schematic fixtures

These fixtures come from the KiCad source QA data at revision `3aaf95cc32`
(`10.0.3-16-g3aaf95cc32`):

- `shared1.kicad_sch`: `qa/data/eeschema/issue23403/shared1.kicad_sch`
- `unsupported-items.kicad_sch`: `qa/data/eeschema/issue24201/issue24201.kicad_sch`
- `issue24201/`: the complete two-sheet `qa/data/eeschema/issue24201`
  schematic hierarchy, paired with a minimal project marker for the loader test

`resaved-child-sheet.kicad_sch` is a `pcb apply schematic` child sheet, given a title block, after a
KiCad 10.0.7 save (`kicad-cli sch upgrade --force`); it pins KiCad's exact output order.

`item-kinds.kicad_sch` is a hand-written page with a title block, bezier,
text box, bus, bus entry, image, label, netclass flag, rule area and group
after the same KiCad 10.0.7 save; it pins how KiCad writes each item kind.

Both files declare `generator_version "10.0"`. They test semantic round trips
through the supported model while retaining unmodeled S-expressions.

The complete issue 24201 project exercises hierarchical sheet pins and labels,
junctions, multi-segment wires, symbols, and no-connect markers in a real KiCad
10 project.
