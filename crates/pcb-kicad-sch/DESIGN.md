# KiCad schematic reconciliation

## Compatibility invariant

`pcb apply` accepts any KiCad schematic hierarchy whose connectivity is
equivalent to the evaluated Zener netlist. It does not require a particular
sheet structure, label strategy, wire layout, item order, or placement policy.
The only required reconciliation metadata is the Zener component path and
symbol-slot identity stored on each managed symbol. Deterministic UUIDs on
generated labels, wires, sheets, and other items do not grant continued
ownership of those items.

Analysis interprets the schematic as KiCad does. Reconciliation preserves an
equivalent schematic except to refresh netlist-derived symbol assembly flags.
Otherwise, it makes the smallest repair needed to restore equivalence.

KiCad power symbols are adopted by their exact effective net name and
connectivity. They do not require Zener identity metadata or a particular
library definition. A net symbol specified by Zener is the preferred way to
create a missing name driver; it is not a canonical representation imposed on
an existing equivalent schematic.

Generation may give a new module instance its own child sheet and file. Later
applications preserve user reorganizations rather than restoring that layout.

## Shared core

`pcb-kicad-sch` owns parsing, serialization, connectivity analysis,
reconciliation, in-memory edits, and source patching. It has no filesystem
operations and supports WASM without a separate feature mode.

- `reconcile::plan_reconciliation` serves `pcb apply` and interactive editors.
  It returns verified, reversible document edits for topology repair.
- `reconcile::sync_netlist_derived_symbol_properties` refreshes assembly flags
  in place without repairing topology.
- Read-only queries expose symbol pin placement, visual bounds, and the source
  items behind connectivity results.

The caller owns project discovery, loading, atomic writes, and rollback.
The `pcbc` adapter converts the verified edits into minimal source patches,
writes them, then reloads and verifies the project. It makes no additional
semantic decisions.

## Repair model

`pcb apply` and interactive repairs share four stages:

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
  geometry. PCB's realizer places one driver per island with a one-bend stub,
  deriving each driver's kind with the same rule the intent reports; an
  editor may use its own router instead.
- **Verify** accepts a realization only if every selected issue is gone, no
  issue appeared, and the result is netlist-equivalent. `pcb apply` verifies
  through reconciliation, which also proves its edits reversible. An external
  realizer calls `verify_connectivity_repair`, which additionally requires
  that nothing outside the intent changed.
