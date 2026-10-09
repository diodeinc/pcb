---
name: registry-search
description: Find reusable registry modules and component packages for a board or specification.
---

# Registry Search

Search the registry before authoring a new reusable package. Read a
candidate's source (`pcb list -m -json <module-url>@<version>` reports its
`dir`) for its public API; do not infer IO or config names from a search result.

Prefer a reusable module that matches the need, then a component with its
support circuitry, then a primitive. Use `preferred-parts` when choosing a
concrete MPN. Instantiate the chosen `.zen` entrypoint with `Module()` and
follow `zener-language` for dependencies; do not hand-edit `pcb.toml`.

Discover or clone other boards only when the user explicitly asks, and clone
them outside the current checkout.

If no suitable package exists or one needs a fix, use `librarian` in a registry
checkout; from board work, prepare a `librarian-dispatch` request.
