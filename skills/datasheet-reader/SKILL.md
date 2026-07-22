---
name: datasheet-reader
description: Read datasheets and technical PDFs with `pcb scan`, including local files, URLs, and KiCad `Datasheet` properties.
---

# Datasheet Reader

Use this skill when a task depends on a datasheet or technical PDF.

- Input: local `.pdf` path or `http(s)` URL
- Command: `pcb scan <input>`
- Output: stdout reports materialized `PDF:` and `Markdown:` paths
- Next step: read the file at the `Markdown:` path, not the raw PDF
- Images are linked from the markdown

## Workflow

1. Run `pcb scan /path/to/file.pdf` or `pcb scan https://...`.
2. Capture the path on the `Markdown:` line.
3. Read the markdown file and work from that artifact.
4. Follow image links only if the task depends on figures, diagrams, or tables.

## Examples

```bash
pcb scan ./TPS54331.pdf
pcb scan https://www.ti.com/lit/gpn/tca9554
```

## Notes

- Prefer a URL when `pcb scan <url>` succeeds. Its cached PDF is not a package artifact; do not copy it into the repository by default.
- Prefer the minimal invocation above. Do not depend on optional flags unless a task explicitly requires them.
- If `pcb scan` fails, report the failure briefly and then choose the best fallback.
