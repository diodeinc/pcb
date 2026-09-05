---
name: datasheet-reader
description: Read datasheets and technical PDFs with `pcb scan`.
---

# Datasheet Reader

Run `pcb scan <local.pdf-or-url>`. Stdout reports materialized `PDF:` and
`Markdown:` paths; read the file at the `Markdown:` path. Follow its image
links when the task depends on figures, diagrams, or tables.

Prefer source URLs for package metadata. A cached PDF is not a package
artifact; do not copy it into the repository by default. If scanning fails,
report the failure briefly and use an available PDF-reading fallback.
