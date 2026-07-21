# PCB layout synchronization

`pcb-layout` synchronizes a Zener netlist with a KiCad PCB file. The netlist
controls circuit metadata and hierarchy. The KiCad file retains user-authored
placement, routing, zones, and graphics.

## Synchronization model

The synchronizer separates each board into two data sets:

- **View** contains source-controlled data derived from the netlist, including
  references, values, footprints, group membership, nets, and connections.
- **Complement** contains destination-controlled data extracted from the KiCad
  board, including placement, orientation, layers, locks, routing, zones, and
  graphics.

Synchronization derives a new view, adapts the existing complement to its
structure, and joins them:

```text
sync(source, destination) =
    join(get(source), adapt(get(source), extract(destination)))
```

This model enforces four invariants:

1. The synchronized metadata matches the current netlist.
2. Existing user placement and routing remain unchanged when their entities
   still exist.
3. Running synchronization twice produces the same result as running it once.
4. The resulting entities match the new netlist exactly.

## Identity and authority

| Entity | Identity | Netlist controls | KiCad controls |
|---|---|---|---|
| Footprint | Hierarchical path and footprint ID | Reference, value, footprint ID, DNP state, fields | Position, orientation, layer, lock state |
| Group | Hierarchical path | Name and membership | Existing routing and graphics |
| Net | Name | Name and pad connections | Existing routed items associated with the name |

A footprint ID change is a removal followed by an addition. The new footprint
inherits the previous position when possible. Zener `moved()` declarations are
resolved before the Python synchronizer runs.

## Synchronization process

The synchronizer applies a changeset in this order:

1. Remove obsolete groups and footprints.
2. Create new footprints and groups.
3. Update source-controlled footprint metadata.
4. Rebuild group membership.
5. Assign pads to nets.
6. Place new entities and restore inherited positions.
7. Copy routing and graphics from reusable layout fragments.

The log records the extracted state, planned changeset, and applied operations.
Use those records to diagnose lost identity, ignored fragments, or unexpected
placement.

## Layout fragments

A group with `layout_path` can load placement, routing, zones, and graphics from
a reusable KiCad fragment. The highest successfully loaded fragment in each
hierarchy branch is authoritative. Descendant fragments in that branch are
ignored and produce a warning.

For an authoritative fragment, synchronization:

1. Applies positions for footprints present in the fragment.
2. Packs descendant footprints missing from the fragment near its bounds.
3. Treats the complete subtree as one rigid block during parent placement.
4. Copies the fragment routing with the same translation applied to the block.
5. Maps fragment net names to board net names through pad connectivity.

A fragment that cannot be loaded does not become authoritative. Its group uses
normal hierarchical placement.

## Hierarchical placement

New footprints and non-fragment groups are packed from the leaves toward the
root. The algorithm sorts items by area, evaluates placements beside previously
placed items, and selects a compact deterministic arrangement. Spacing increases
with hierarchy depth and is capped at 10 mm.

The placement implementation in `src/scripts/lens/hierplace.py` is independent
of KiCad. KiCad extraction and mutation are isolated in `kicad_adapter.py`.

## Source layout

```text
src/scripts/lens/
├── types.py          # View, complement, and identity types
├── lens.py           # Extract, adapt, join, and synchronization logic
├── kicad_adapter.py  # KiCad board input and output
├── hierplace.py      # Deterministic hierarchical placement
├── changeset.py      # Planned synchronization operations
├── oplog.py          # Applied-operation logging
└── tests/            # Unit, property, and stateful tests
```

## Verification

Run the Python tests after changing lens behavior:

```bash
uv run pytest crates/pcb-layout/src/scripts/lens/tests/ -v
```

Run the Rust integration tests after changing layout generation or the Rust and
Python boundary. These tests require KiCad:

```bash
cargo test -p pcb-layout
```

Snapshot changes affect final board state or synchronization logs. Review them
with `cargo insta review`; do not accept them without examining the underlying
behavior change.
