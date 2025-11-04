# pcb-ipc2581

CLI tool for inspecting IPC-2581 PCB data files.

## Commands

### `info <file>`

Board summary: dimensions, components, layers, drills, thickness.

```bash
pcb-ipc2581 info board.xml
pcb-ipc2581 info board.xml --units mil --format json
```

### `bom <file>`

Extract bill of materials.

```bash
pcb-ipc2581 bom board.xml
pcb-ipc2581 bom board.xml --format json
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
