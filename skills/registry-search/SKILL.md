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
| Candidate source directory | `pcb list -m -json <module-url>@<version>` |

Use functional queries for functional needs and MPN/manufacturer queries for
named parts. Read the candidate's source in the reported `dir` for its public
API; do not infer IO/config names from a search snippet.

Prefer a reusable module or reference circuit that matches the actual need,
then a component with the required support circuitry, or a primitive when only
the raw part is needed. Compare electrical fit, package, pinout, sourcing, and
public API. Use `preferred-parts` when choosing a concrete MPN. Ask only about
material unresolved tradeoffs.

Instantiate the chosen `.zen` entrypoint directly in the consuming design, for
example `Module("code.diode.computer/diode/registry/components/<Manufacturer>/<NAME>/<NAME>.zen")`,
and follow `zener-language` for dependencies; do not hand-edit `pcb.toml`.

Discover or clone other boards only when the user explicitly asks; use
`mcp__diode__list_boards` and clone outside the current checkout.

If no suitable result exists or a candidate needs a package, API, or circuit
fix, use `librarian` within a registry-authoring task. From board or spec work,
prepare a `librarian-dispatch` request instead of patching reusable packages
inline.
