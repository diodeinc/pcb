---
name: zener-language
description: Canonical Zener HDL semantics, package rules, manifests, and high-value stdlib APIs. Use before non-trivial `.zen` creation, editing, refactoring, or review when the task touches `Module()`, `io()`, `config()`, imports, `pcb.toml`, `pcb.sum`, stdlib interfaces or units, or unfamiliar package APIs. Read this before editing instead of guessing.
---

# Zener Language

Use this skill as the semantics companion to `idiomatic-zener` for non-trivial `.zen` work.

## Workflow

1. Start from nearby workspace code. Prefer the local package's patterns before generic examples.
2. Open only the relevant reference file:
   - `references/language.md` for modules, nets/interfaces, components, `io()`, `config()`, utilities, and tool-managed metadata
   - `references/packages.md` for imports, workspace layout, manifests, dependencies, and `pcb.sum`
   - `references/stdlib.md` for prelude, interfaces, units, checks, utils, properties, and generics
   - `references/examples.md` for example snippets
3. For installed package or registry APIs, run `pcb doc --package <package>` instead of guessing. This skill is the canonical owner of that workflow.
4. For broader toolchain semantics, consult `~/.pcb/docs/spec.md` and `~/.pcb/docs/packages.md`.
5. Check exact semantics before editing when the code touches unfamiliar syntax, manifests, imports, stdlib APIs, or package interfaces.
6. Never invent syntax, stdlib modules, interfaces, fields, or package APIs.

## Notes

- Use this with `idiomatic-zener` for non-trivial `.zen` creation, editing, refactoring, or review.
- Use `reference-design` for vendor and reference-circuit translation work.
- Use `component-search` when the problem is finding or importing a part rather than understanding the language.
