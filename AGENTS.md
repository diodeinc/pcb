# AGENTS.md

## Commands

- `cargo run -p pcbc -- <command>` runs the compiler CLI; `pcb` is the shim/version manager.
- `cargo nextest run -p <crate>` runs crate tests; `cargo test --doc -p <crate>` runs doctests separately.
- `uv run pytest crates/pcb-layout/src/scripts/lens/tests/ -v` runs layout-lens tests.

Keep checks scoped to the change. Report changed snapshots; do not accept them without user approval.

## Non-obvious constraints

- Language semantics live in `pcb-zen-core` and the pinned `diodeinc/starlark-rust` fork; workspace/package resolution lives in `pcb-zen`.
- For layout sync changes, see `crates/pcb-layout/README.md`; for import changes, see `crates/pcbc/src/import/README.md`.
- Use `anstream` for incidental CLI output. Structured/binary stdout renderers must remain fallible over `std::io::Write`, called through `pcb_ui::write_stdout`. Do not catch `BrokenPipe` process-wide. Gate CLI dependencies/imports in non-CLI or WASM builds.
- Before pushing, fetch and rebase onto `origin/main`, then fast-forward push; no merge commits.

## Documentation

- Add one succinct `CHANGELOG.md` entry under `Unreleased` per user-visible change.
- Update `docs/pages/spec.mdx` for language changes and `docs/pages/packages.mdx` for workspace/dependency/package changes.
