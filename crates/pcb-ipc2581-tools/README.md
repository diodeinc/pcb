# pcb-ipc2581

**Note:** This is a library crate providing IPC-2581 functionality. The CLI is integrated into the main `pcb` tool as the `ipc2581` subcommand.

CLI tool for inspecting IPC-2581 PCB data files.

## Commands

### `info <file>`

Board summary: dimensions, components, layers, drills, thickness.

```bash
pcb ipc2581 info board.xml
pcb ipc2581 info board.xml --units mil --format json
```

### `bom <file>`

Extract bill of materials with manufacturer/MPN alternatives.

```bash
pcb ipc2581 bom board.xml
pcb ipc2581 bom board.xml --format json
```

Shows primary MPN/manufacturer and alternatives in separate column.

### `edit bom <file> --rules <rules.json>`

Add manufacturer/MPN alternatives to BOM entries via AVL section.

```bash
pcb ipc2581 edit bom board.xml --rules alternatives.json
pcb ipc2581 edit bom board.xml.zst --rules alternatives.json --output enriched.xml
```

**Rules format:**
```json
[
  {
    "key": {"Path": ["C1", "C2"]},
    "offers": [
      {"manufacturer": "Murata", "manufacturer_pn": "GRM155R71C104KA88D", "rank": 1},
      {"manufacturer": "Samsung", "manufacturer_pn": "CL05A104KA5NNNC", "rank": 2}
    ]
  },
  {
    "key": {"Mpn": "SOME_DESIGN_MPN"},
    "offers": [
      {"manufacturer": "TI", "manufacturer_pn": "TPS563201DDCR"}
    ]
  }
]
```

**Features:**
- Merges with existing AVL data (preserves unmatched entries)
- New rules override existing for same (MPN, manufacturer) pair
- Optional `rank` field (1 = highest priority, IPC-2581 spec)
- Ranked entries sorted before unranked
- First entry gets `chosen="true"` flag
- Running multiple times is safe (idempotent)

**Matching keys:**
- `{"Mpn": "..."}` - Match by part number in BOM characteristics
- `{"Path": ["R1", "R2"]}` - Match by reference designator
- `{"Generic": {...}}` - Match by component specs (capacitance, resistance, etc.)

**Generic matching example:**
```json
{
  "key": {
    "Generic": {
      "package": "0402",
      "component_type": "Capacitor",
      "capacitance": {"value": "100e-9", "unit": "F", "tolerance": "0.1"}
    }
  },
  "offers": [
    {"manufacturer": "Murata", "manufacturer_pn": "GRM155R71C104KA88D", "rank": 1}
  ]
}
```

### Planned

- `stackup` - Layer stack with material properties
- `layers` - List all layers with filtering
- `components` - Component listing with search
- `nets` - Logical nets with connectivity info
- `drills` - Drill histogram and statistics
- `geometry` - Board outline and cutouts
- `validate` - File validation and checks

## Options

- `--format <text|json>` - Output format (default: text)
- `--units <mm|mil|inch>` - Unit preference (default: mm, info command only)
- Respects `NO_COLOR` environment variable
