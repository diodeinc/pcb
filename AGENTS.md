# AGENTS.md

This file gives repository-wide instructions to coding agents. Keep it focused on stable, high-value guidance; add a nested `AGENTS.md` only when a subdirectory truly needs different rules.

## Project Commands

- `cargo run -p pcb -- build [PATHS...]` — build and validate `.zen` designs
- `cargo run -p pcb -- fmt [PATHS...]` — format `.zen` files
- `cargo run -p pcb -- layout [PATHS...]` — generate layout files

Never run `cargo insta accept` without explicit user approval.

## Repository Map

- `crates/pcb` is the main CLI crate.
- `crates/pcb-zen` and `crates/pcb-zen-core` implement the Zener language runtime and core semantics.
- `crates/pcb-sch`, `crates/pcb-layout`, and `crates/pcb-kicad` cover schematic, layout, and KiCad integration.
- `stdlib/` contains the Zener standard library.
- `examples/` contains runnable example designs.
- `docs/pages/` contains user-facing documentation, including the language specification in `docs/pages/spec.mdx`.

## Working Rules

- Prefer the smallest correct change over broad refactors.
- Match the existing crate and module boundaries unless a structural change is clearly necessary.
- In `.zen` files, remember that Zener is Starlark-based, not Python: do not use f-strings.
- Avoid editing generated artifacts, vendored code, or snapshot outputs unless the task specifically requires it.
- The project depends on a fork of `starlark-rust` (`diodeinc/starlark-rust`); check that fork when language behavior appears to come from upstream Starlark internals rather than this repository.

## Documentation Rules

- If you change Zener language syntax, built-ins, core types, module/import behavior, type rules, or other user-visible language semantics, update `docs/pages/spec.mdx` in the same change.
- If you change workspace manifests, dependency resolution, or package behavior, update the relevant docs in `docs/pages/`, especially `docs/pages/packages.mdx` when applicable.
- Keep documentation updates concrete and example-driven; do not leave behavior changes documented only in code.

## Verification

- Run the narrowest relevant check first, usually `cargo test -p <crate>` or a focused `cargo run -p pcb -- ...` command.
- Do not run the full workspace verification suite after every small edit.
- Before committing or pushing a meaningful batch of changes, run `cargo fmt`, `cargo clippy -- -D warnings`, and `cargo nextest run --no-fail-fast`.
- If snapshot tests change, call that out clearly in your summary and leave acceptance to the user.

## References

- Start with `README.md` for the product-level overview.
- Use `docs/pages/spec.mdx` for language semantics.
- Use `docs/pages/packages.mdx` for workspace, dependency, and package behavior.
