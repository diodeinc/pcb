# pcb-ipc2581-tools

`pcb-ipc2581-tools` implements the `pcb ipc2581` command group. The `pcb ipc`
alias provides the same commands.

| Command | Purpose |
|---|---|
| `info` | Report board, layer, drill, and stackup metadata. |
| `bom` | Export the bill of materials. |
| `cpl` | Export component placement data. |
| `html` | Export an HTML board summary. |
| `outline` | Export a KiCad-compatible DXF outline. |
| `render` | Render one layer as terminal graphics, SVG, or PNG. |
| `dfm` | Check generated Gerber geometry for narrow features. |
| `gerber` | Export fabrication layers and drill files. |
| `view` | Export a filtered IPC-2581 document. |
| `board-array create` | Create a rectangular board array. |
| `fab-panel create` | Tile assembly panels into a supported fabrication panel size. |
| `edit bom` | Add approved alternatives to BOM entries. |

Run `pcb ipc2581 <command> --help` for arguments and output options.

`edit bom` modifies the input file when `--output` is omitted. Specify an output
path when the source document must remain unchanged.

`fab-panel create` supports the common 12 by 18, 16 by 18, 18 by 24, and
21 by 24 inch fabrication panel sizes through `--panel-size`. The default is
18 by 24 inches:

```bash
pcb ipc2581 fab-panel create \
  --panel-size 16x18 \
  --output fabrication-panel.xml \
  assembly-a.xml assembly-a.xml assembly-b.xml
```

The command uses a 5 mm edge rail and a 5 mm gap between assembly panels. All
inputs must have identical physical stackups. The first input provides the fab
panel stackup and canonical physical layer definitions. Repeat an input path to
request more than one copy. The command supports up to 32 assembly panels and
fails without writing an output when it cannot find a layout.

```bash
cargo test -p pcb-ipc2581-tools
```
