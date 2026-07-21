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
| `edit bom` | Add approved alternatives to BOM entries. |

Run `pcb ipc2581 <command> --help` for arguments and output options.

`edit bom` modifies the input file when `--output` is omitted. Specify an output
path when the source document must remain unchanged.

```bash
cargo test -p pcb-ipc2581-tools
```
