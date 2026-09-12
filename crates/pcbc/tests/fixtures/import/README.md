# Import fixtures

`multiunit_pinmap_split.kicad_sch` is copied unchanged from the public
[KiCad source repository](https://github.com/KiCad/kicad-source-mirror/blob/a8d6201d6bc1739943ea51b3bc18d8d691503539/qa/data/eeschema/spice_netlists/multiunit_pinmap_split/multiunit_pinmap_split.kicad_sch),
revision `a8d6201d6bc1739943ea51b3bc18d8d691503539` (KiCad's GPL-3.0 source tree).
It exercises a three-unit LM358, nine physical net partitions, local labels,
and hidden-pin ground symbols. Tests upgrade a disposable copy with KiCad CLI
and exclude its unsourced simulation parts from the BOM, allowing unsuppressed
electrical build checks. No private designs or customer data are used.
