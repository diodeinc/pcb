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
`pcb apply` and interactive editors. It inspects and solves the complete issue
set as one global problem, then returns only an ordered set of exact
`DocumentPatch` suggestions. Each suggestion is verified against the original
document, mutually compatible with the others, and applicable independently;
coupled edits remain in one patch. `apply_one` supports an editor preview and
`apply_all` produces the globally verified document used by `pcb apply`. Plans
do not retain issue selections or before/after inspection snapshots, and the
planner does not read or write files.

Physical routing is part of this shared core rather than a second editor repair
model. Reconciliation extracts endpoints from physical islands, routes every
eligible net through the shared orthogonal router, preserves existing segments,
materializes deterministic KiCad wires and junctions, and falls back to scoped
labels or net symbols when a physical route is unsuitable. Editors can call
`routing::plan_wire_reroute` for explicit selected-item reroutes without
invoking semantic reconciliation.

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
