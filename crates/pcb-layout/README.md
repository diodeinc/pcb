# PCB Layout Synchronization

This crate implements lens-based synchronization between Zener netlists and KiCad PCB layout files.

## The Core Insight: Asymmetric Lens with Complement

The destination PCB file is a *decorated view* of the source netlist—it contains both derived data (metadata from the netlist) and user-authored data (placement, routing). The key insight is that **the destination decomposes as `D ≅ View ⊕ Complement`**, where View and Complement are disjoint types with separate origins:

```
View       ← always from SOURCE (metadata: reference, value, fpid)
Complement ← always from DEST   (placement: position, rotation, layer)
```

This transforms the synchronization problem from "carefully merge two trees with per-field rules" into "extract, adapt, and recombine pure data structures."

## The Sync Formula

```
sync(s, d) = join(get(s), adapt_complement(get(s), *extract(d)))
```

In English:
1. `get(s)` — Derive fresh View from SOURCE netlist
2. `extract(d)` — Extract existing (View, Complement) from DEST board
3. `adapt_complement(...)` — Adapt Complement to match new View's structure
4. `join(v, c)` — Combine View and Complement into result

## Entity Identity

### Footprints

The primary identifier is the **EntityId**, which combines:
- **EntityPath**: Hierarchical path segments (e.g., `("PowerSupply", "Regulator", "R1")`)
- **FPID**: Footprint ID (e.g., `Resistor_SMD:R_0603_1608Metric`)

Both path AND fpid are part of the identity. An FPID change is treated as a delete + add operation (with position inheritance).

**Note:** Renames (via `moved()` in Zener) are handled in Rust preprocessing before the Python sync runs. The Python lens module receives already-renamed paths.

### Groups

Groups are identified by **EntityPath** only (no fpid). They represent Zener module hierarchy.

### Nets

Nets are identified by **name** (string). The `BoardView.nets` dict is keyed by net name. Routing items (tracks, vias, zones) store `net_name` to reference their net.

## Data Model

### View (SOURCE-authoritative)

| Type | Contents |
|------|----------|
| `FootprintView` | entity_id, reference, value, fpid, dnp, fields |
| `GroupView` | entity_id, member_ids, layout_path |
| `NetView` | name, connections (entity_id, pad) |
| `BoardView` | footprints, groups, nets |

### Complement (DEST-authoritative)

| Type | Contents |
|------|----------|
| `FootprintComplement` | position, orientation, layer, locked, field positions |
| `GroupComplement` | tracks, vias, zones, graphics |
| `BoardComplement` | footprints, groups, board_tracks, board_vias, board_zones |

## Authority Rules

| Entity | Attribute | SOURCE Authority | DEST Authority |
|--------|-----------|------------------|----------------|
| Footprint | reference, value, DNP, fields | ✓ | |
| Footprint | FPID | ✓ | |
| Footprint | position, orientation, layer | | ✓ (after initial placement) |
| Footprint | locked | | ✓ |
| Group | name, membership | ✓ | |
| Group | routing (tracks/vias/zones) | ✓ (new only) | ✓ (existing) |
| Net | name, connections | ✓ | |

## Lens Laws

The implementation guarantees four properties:

1. **View Consistency**: After sync, all metadata matches SOURCE
2. **Complement Preservation**: Existing placements are never overwritten (unless FPID changes)
3. **Idempotence**: `sync(s, sync(s, d)) = sync(s, d)`
4. **Structural Fidelity**: Entities in result match exactly what's in new View

## Module Structure

```
src/scripts/lens/
├── __init__.py          # Package exports
├── types.py             # Core data types (EntityPath, View, Complement, etc.)
├── lens.py              # Core operations: get(), extract(), adapt_complement(), join(), run_lens_sync()
├── kicad_adapter.py     # KiCad-specific extraction and application
├── hierplace.py         # Pure geometry HierPlace algorithm
├── changeset.py         # SyncChangeset and serialization utilities
├── oplog.py             # OpLog for operation logging
└── tests/
    ├── test_types.py
    ├── test_lens.py
    ├── test_scenarios.py
    ├── test_hierplace.py    # Placement algorithm tests
    ├── test_properties.py   # Property-based tests (Hypothesis)
    ├── test_stateful.py     # Stateful machine tests
    ├── test_changeset.py    # Changeset serialization tests
    └── strategies.py        # Hypothesis generators
```

## Sync Output

All sync information is logged to the log file at INFO level. The log contains:

### Lens State (OLD/NEW)

Before and after state of View and Complement, prefixed with `OLD` or `NEW`:

```
INFO: OLD FPV path=R1 ref=R1 value=10k fpid=Resistor_SMD:R_0402_1005Metric
INFO: OLD FPC path=R1 x=100000 y=50000 orient=0 layer=F.Cu
INFO: NEW FPV path=R1 ref=R1 value=10k fpid=Resistor_SMD:R_0603_1608Metric
INFO: NEW FPC path=R1 x=100000 y=50000 orient=0 layer=F.Cu
```

### Changeset

Planned changes before application, prefixed with `CHANGESET`:

```
INFO: CHANGESET FP_ADD path=M1.R1 ref=R1 fpid=Resistor_SMD:R_0603 value=10k
INFO: CHANGESET FP_REMOVE path=M1.C1 fpid=Capacitor_SMD:C_0402
INFO: CHANGESET GR_ADD path=M1 members=2
```

| Command | Description |
|---------|-------------|
| `FP_ADD` | New footprint to create |
| `FP_REMOVE` | Footprint to delete (includes position for inheritance) |
| `GR_ADD` | New group to create |
| `GR_REMOVE` | Group to delete |

### OpLog

Actual operations performed during `apply_changeset`, prefixed with `OPLOG`:

```
INFO: OPLOG FP_ADD path=M1.R1 ref=R1 fpid=Resistor_SMD:R_0603 value=10k x=0 y=0
INFO: OPLOG GR_ADD path=M1
INFO: OPLOG GR_MEMBER path=M1 members=[M1.R1, M1.C1]
INFO: OPLOG PLACE_FP path=M1.R1 x=150000000 y=100000000
```

| Command | Description |
|---------|-------------|
| `NET_ADD` | Net created |
| `GR_REMOVE` | Group deleted (with item count) |
| `FP_REMOVE` | Footprint deleted |
| `FP_ADD` | Footprint created |
| `GR_ADD` | Group created |
| `FRAG_IGNORED` | Nested fragment ignored (ancestor is authoritative) |
| `FRAG_TRACK` | Track duplicated from fragment |
| `FRAG_VIA` | Via duplicated from fragment |
| `FRAG_ZONE` | Zone duplicated from fragment |
| `FRAG_GRAPHIC` | Graphic duplicated from fragment |
| `PLACE_FP` | Footprint positioned by HierPlace |
| `PLACE_GR` | Fragment group positioned as rigid block |
| `PLACE_FP_INHERIT` | Footprint inherited position from FPID change |
| `PLACE_FP_FRAGMENT` | Footprint positioned from fragment layout |
| `PLACE_FP_ORPHAN` | Footprint not in fragment, packed near fragment bbox |

## Application Pipeline

The `apply_changeset()` function applies operations in phases:

```
Phase 1: Deletions
  ├── GR-REMOVE (delete group + all contents)
  └── FP-REMOVE (delete standalone footprints)

Phase 2: Additions
  ├── FP-ADD (create at origin)
  └── GR-ADD (create empty group)

Phase 3: View updates
  └── Update metadata on existing footprints

Phase 4: Group membership rebuild
  └── Rebuild parent→child relationships

Phase 5: Pad-to-net assignments
  └── Connect pads to their nets

Phase 6: HierPlace positioning
  ├── Position inheritance (FPID changes)
  ├── Fragment positions (PLACE_FP_FRAGMENT)
  ├── Orphan packing (PLACE_FP_ORPHAN)
  ├── Hierarchical layout (bottom-up packing)
  └── Fragment group moves (PLACE_GR)

Phase 7: Fragment routing
  └── Apply tracks/vias/zones from layout fragments (offset by group move)
```

## HierPlace Algorithm

New footprints and groups are positioned using a hierarchical packing algorithm. The core motivation is: **related footprints should be close together**.

### Placement Rules

#### Rule A — Fragment Dominance (Top-Most Wins)

A group **F is an authoritative fragment** iff:
- F has a **successfully loaded fragment**, AND
- **No ancestor** of F has a successfully loaded fragment

In other words: the first loaded fragment encountered from the root downward wins. Nested fragments are ignored.

#### Rule B — Authoritative Fragment Behavior

For each authoritative fragment F:
1. **Place covered footprints**: Apply fragment positions for footprints the fragment specifies
2. **Pack orphans**: HierPlace all descendant footprints NOT in the fragment near the fragment's bounding box
3. **Emit one rigid block**: The entire subtree of F becomes a single rigid block for higher-level placement

**No other placement runs inside F.** Child groups do not do their own placement—they are handled entirely at the authoritative fragment level.

#### Rule C — Non-Fragment Placement

Groups not inside an authoritative fragment subtree use **pure bottom-up HierPlace**:
- Pack children into blocks
- Pack blocks into parent
- Repeat up to root

A fragment that **fails to load** behaves as "no fragment" and follows this rule.

#### Rule D — Root Integration

At root level, pack together:
- Rigid blocks from authoritative fragments (Rule B)
- Rigid blocks from non-fragment groups (Rule C)
- Existing board content serves as obstacles/anchors

### Packing Algorithm

The `pack_at_origin()` function implements corner-based bin packing:

1. Sort items by area (largest first) for deterministic placement
2. Place first item at origin
3. For each subsequent item, try placement at corners of already-placed items
4. Choose the placement that minimizes: `width + height + |width - height|` (prefers square layouts)
5. After packing at origin, translate cluster to final position

### Guardrails

- **Authoritative fragments list**: Log all authoritative fragments at start of HierPlace: `INFO: Authoritative fragments: ['M1', 'PS_12V', ...]`
- **Ignored nested fragment warning**: When a child fragment is ignored because an ancestor is authoritative, emit: `WARNING: Fragment at 'child/path' ignored because ancestor 'parent/path' is authoritative.`
- **Orphan tracking**: Log orphan footprints with `PLACE_FP_ORPHAN` so users can see what wasn't in the fragment.
- **Deterministic ordering**: Always sort by `str(entity_id.path)` when iterating to ensure reproducible layouts.

## Fragment Handling

Groups with `layout_path` load positioning and routing from a fragment file:

1. **Positions**: Fragment footprint positions are applied first, then the entire group is moved as a rigid block during HierPlace
2. **Routing offset**: Fragment routing (tracks, vias, zones, graphics) is duplicated and offset by the same delta as the group move
3. **Net Remapping**: Fragment nets (e.g., `VCC`) are remapped to board nets (e.g., `Power.VCC`) using pad→net mapping
4. **Multiple Instances**: Same fragment can be used by multiple groups; each gets its own copy

## Testing

### Python Unit Tests

Test the lens sync logic (pure Python, no KiCad required):

```bash
# All lens tests
uv run pytest crates/pcb-layout/src/scripts/lens/tests/ -v

# Specific test files:
uv run pytest crates/pcb-layout/src/scripts/lens/tests/test_scenarios.py -v  # Core sync scenarios
uv run pytest crates/pcb-layout/src/scripts/lens/tests/test_properties.py -v # Property-based (Hypothesis)
uv run pytest crates/pcb-layout/src/scripts/lens/tests/test_stateful.py -v   # Stateful machine tests
uv run pytest crates/pcb-layout/src/scripts/lens/tests/test_hierplace.py -v  # Placement algorithm
uv run pytest crates/pcb-layout/src/scripts/lens/tests/test_changeset.py -v  # Changeset tests
```

### Rust Integration Tests

Test end-to-end layout generation (requires KiCad):

```bash
# All layout tests
cargo test --package pcb-layout

# Specific test files:
cargo test --package pcb-layout layout_generation  # Main layout scenarios
cargo test --package pcb-layout fpid_change        # FPID change handling
cargo test --package pcb-layout moved              # moved() path handling
```

These tests generate snapshots in `tests/snapshots/`:
- `.layout.json.snap` — Final board state (footprints, groups, nets)
- `.log.snap` — Sync log (lens state, changeset, oplog)

Use `cargo insta review` to review snapshot changes.

### Comparing Layout Snapshots

Use `compare_layout_snapshots.py` to distinguish structural vs positional diffs:

```bash
# Compare current file against main branch
python crates/pcb-layout/src/scripts/compare_layout_snapshots.py main HEAD path/to/snapshot.snap

# Compare all layout snapshots between branches
python crates/pcb-layout/src/scripts/compare_layout_snapshots.py main HEAD --all

# Verbose output (show all position changes)
python crates/pcb-layout/src/scripts/compare_layout_snapshots.py main HEAD --all -v
```

The tool categorizes differences into:
- **STRUCTURAL**: Changes to footprints, groups, nets, references (breaks correctness)
- **POSITION**: Only x/y coordinate changes (expected with placement algorithm changes)

## Usage

```python
from lens import run_lens_sync

result = run_lens_sync(
    netlist=netlist,
    kicad_board=board,
    pcbnew=pcbnew,
    board_path=board_path,
    footprint_lib_map=footprint_lib_map,
    groups_registry=groups_registry,
)
```
