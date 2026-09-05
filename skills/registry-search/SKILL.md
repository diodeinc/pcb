---
name: registry-search
description: Find reusable registry modules and component packages for a board or specification.
---

# Registry Search

Find suitable prepared content before authoring a new reusable package.

| Need | Command |
| --- | --- |
| Reusable circuit or entrypoint | `pcb search -m registry:modules <query> -f json` |
| Concrete MPN, footprint, availability, or package behind a symbol | `pcb search -m registry:components <query> -f json` |
| Candidate public API and source root | `pcb doc --package <module-url>@<version>` |

Use functional queries for functional needs and MPN/manufacturer queries for
named parts. If docs are incomplete, inspect the reported source path or tree;
do not infer IO/config names from a search snippet.

Prefer a reusable module or reference circuit that matches the actual need,
then a component with the required support circuitry, or a primitive when only
the raw part is needed. Compare electrical fit, package, pinout, sourcing, and
public API. Use `preferred-parts` when choosing a concrete MPN. Ask only about
material unresolved tradeoffs.

Instantiate the chosen `.zen` entrypoint directly in the consuming design.
For example:

```zen
PartModule = Module("code.diode.computer/diode/registry/components/<Manufacturer>/<NAME>/<NAME>.zen")
```

Follow `zener-language` for dependencies and validation; do not hand-edit
`pcb.toml` to add the dependency.

If no suitable result exists or a candidate needs a package/API/circuit fix,
use `librarian` within a registry-authoring task. From board or spec work,
prepare a `librarian-dispatch` request instead of patching reusable packages
inline.
