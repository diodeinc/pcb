---
name: zener-language
description: Canonical Zener HDL semantics, package rules, manifests, and high-value stdlib APIs. Covers `Module()`, `io()`, `config()`, imports, `pcb.toml`, `pcb.sum`, stdlib interfaces and units, and package APIs.
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
3. `pcb doc --package <package>` shows the public API (types, io, config) of installed or registry packages. It returns type-level listings, not field-level details — for interface field names, check the stdlib reference above or read the source in `.pcb/stdlib/`.
4. For broader toolchain semantics, consult `~/.pcb/docs/spec.md` and `~/.pcb/docs/packages.md`.
5. Check exact semantics before editing when the code touches unfamiliar syntax, manifests, imports, stdlib APIs, or package interfaces.
6. Never invent syntax, stdlib modules, interfaces, fields, or package APIs.
