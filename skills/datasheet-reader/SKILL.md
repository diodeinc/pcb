---
name: datasheet-reader
description: Read datasheets and technical PDF documents with `pcb scan`. Use when the user gives a local PDF path or an `http(s)` datasheet/document URL, when a task requires reading, summarizing, extracting information from, or answering questions about a datasheet or technical PDF, or when a KiCad symbol / `.kicad_sym` provides a `Datasheet` property to resolve. Run `pcb scan <input>` in bash, treat stdout as the generated `.md` path, then read that markdown file.
---

# Datasheet Reader

Use this skill when a task depends on a datasheet or technical PDF.

- Input: local `.pdf` path or `http(s)` URL
- Command: `pcb scan <input>`
- Output: stdout is the resolved markdown path
- Next step: read the markdown file, not the raw PDF
- Images are linked from the markdown

## Workflow

1. Run `pcb scan /path/to/file.pdf` or `pcb scan https://...`.
2. Capture the printed markdown path.
3. Read the markdown file and work from that artifact.
4. Follow image links only if the task depends on figures, diagrams, or tables.

## Examples

```bash
pcb scan ./TPS54331.pdf
pcb scan https://www.ti.com/lit/gpn/tca9554
```

## Notes

- Prefer the minimal invocation above. Do not depend on optional flags unless a task explicitly requires them.
- If `pcb scan` fails, report the failure briefly and then choose the best fallback.
