# KiCad schematic reconciliation

## Compatibility invariant

`pcb apply` accepts any KiCad schematic hierarchy whose connectivity is
equivalent to the evaluated Zener netlist. It does not require a particular
sheet structure, label strategy, wire layout, item order, or placement policy.
The only required reconciliation metadata is the Zener component path and
symbol-slot identity stored on each managed symbol. Deterministic UUIDs on
generated labels, wires, sheets, and other items do not grant continued
ownership of those items.

Analysis is pure and interprets the schematic as KiCad does. Reconciliation
must preserve a netlist-equivalent schematic without changing its files. When
the netlist changes, the repair planner can use the managed symbol identities
to make the smallest repair needed to restore equivalence. It must not treat
tool-generated geometry or hierarchy as a canonical form.

KiCad power symbols are adopted by their exact effective net name and
connectivity. They do not require Zener identity metadata or a particular
library definition. A net symbol specified by Zener is the preferred way to
create a missing name driver; it is not a canonical representation imposed on
an existing equivalent schematic.

Generation can use a prescriptive layout to produce a useful initial project.
In particular, a newly materialized module instance can receive its own child
sheet and `.kicad_sch` file. This is an initialization policy only. After the
user edits or reorganizes the project in KiCad, later applications must operate
on the resulting valid KiCad structure rather than reconstructing the initial
structure.

## Shared core

`reconcile::plan_reconciliation` is the single semantic entrypoint for both
`pcb apply` and interactive editors. It accepts typed in-memory inputs, returns
exact reversible document edits, and verifies the resulting document before it
returns. It does not read or write files.

The `pcbc` persistence adapter resolves linked project paths, converts the
verified typed result into minimal source patches, writes them atomically,
reloads the project, and verifies it again. It must not make additional
semantic decisions.

Physical connectivity includes item provenance so an editor can associate an
analysis result with the symbols, wires, labels, junctions, and sheet pins that
formed it. Symbol pin placement and visual bounds are also public read-only
queries.

`pcb-kicad-sch` contains no filesystem operations. It owns parsing,
serialization, connectivity reduction, analysis, reconciliation planning,
in-memory plan application, and pure source patching. Project discovery, file
loading, atomic writes, and rollback belong to the calling application. The
normal crate build is therefore usable in WASM without a separate feature
mode.

## Repair model

Every repair, whether `pcb apply` or an interactive editor action, is one
pipeline with different inputs: PCB inspects, PCB decides, a realizer draws,
PCB verifies.

- **Inspect** (`analysis::inspect_schematic`) reduces the document and the
  netlist to one connectivity graph and reports issues with the exact items
  that formed them. It is the only connectivity model; consumers never
  recompute which net a wire belongs to.
- **Decide** (`plan_connectivity_repair`) turns selected issues, plus any
  items the consumer has chosen to remove, into a `ConnectivityRepairIntent`:
  items to remove, symbols that must leave an invalid overlap, nets to
  reconnect, and the driver kind each of those nets needs on each page. Label
  scope changes what KiCad's netlister computes, so PCB chooses it. A short or
  unexpected connection is cut by a minimum node cut over the physical
  adjacency graph, verified through the reducer; whole-island teardown is the
  last resort when no finite cut exists.
- **Realize** applies the intent's removals and relocations, then only adds
  geometry. PCB's realizer places one driver per island with a one-bend stub;
  an editor may use its own router instead.
- **Verify** (`verify_repair`) accepts a realization only if every selected
  issue is gone, no issue appeared, and nothing outside the intent changed.

Two invariants follow. A schematic equivalent to its netlist is never touched.
Otherwise the repair is the least change that restores equivalence: PCB's
removals are minimal, realizers add the least they can, and everything else is
preserved exactly.
