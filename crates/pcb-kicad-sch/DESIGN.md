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
equivalent schematic except to refresh netlist-derived symbol assembly flags,
keep generated module sheets' pins in step with their modules, and prune
obsolete generated module sheets as described below.
Otherwise, it makes the smallest repair needed to restore equivalence.

KiCad power symbols are adopted by their exact effective net name and
connectivity. They do not require Zener identity metadata or a particular
library definition. A net symbol specified by Zener is the preferred way to
create a missing name driver; it is not a canonical representation imposed on
an existing equivalent schematic.

Generation may give a new module instance its own child sheet and file. Later
applications preserve user reorganizations rather than restoring that layout.
A generated sheet has one pin for each net that leaves its module, under one
of the net's port names; a net used only inside the module stays local to its
page. Full reconciliation keeps a generated sheet's pins in step as nets start
or stop leaving the module and as ports are renamed or removed. A pin is pcb's
while its id is the generated id for its own name; a pin the user renamed or
wired on the parent page, and the pins of a sheet whose module still has
symbols on the parent page, are the user's. pcb removes its own pins that the
interface no longer has, with the label or net symbol it placed at each on
the parent page and, once the net no longer leaves the module, the parent
page's local labels for the net; a label or symbol the user attached at the
pin stays. The module page's hierarchical labels for a
removed pin become the page's own driver for the net, or go when the port is
gone. A newly crossing port gets a pin and parent driver where the pin, its
label and any growth of the sheet clear the page's other items, and the module
page's local labels for its net become hierarchical. Existing layout never
moves: a pin that does not fit is left to the connectivity repair, which joins
the net as it does across a sheet the user organized.
Full reconciliation removes an obsolete generated subtree only when its page
filenames, page UUIDs and incoming sheet UUIDs still identify the generated
module paths and parent-relative filenames, and it contains no live managed
symbols, linked modules, drawings, native documentation symbols or
user-organized pages. Root pages are always retained. This narrow cleanup also
removes the child files and relationship metadata; renamed or relocated pages
and sheets containing live content remain authoritative. Orphan power symbols,
labels, wires, buses and bus entries alone do not retain obsolete pages. Scoped
issue repairs do not prune pages.

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
