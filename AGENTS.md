# AGENTS.md

## PCB CLI Subcommands

- `cargo run -p pcbc -- build [PATHS...]` — build and validate Zener designs.
- `cargo run -p pcbc -- test [PATHS...]` — run tests declared in `.zen` files.
- `cargo run -p pcbc -- fmt [PATHS...]` — format `.zen` files.
- `cargo run -p pcbc -- layout --no-open [PATHS...]` — generate/update KiCad layout files without opening KiCad.

## Development Workflow

- `cargo nextest run -p <crate>` — run tests for one crate.
- `cargo test --doc -p <crate>` — run doctests, which nextest does not run.
- `cargo fmt` — format Rust code.
- `uv run pytest crates/pcb-layout/src/scripts/lens/tests/ -v` — run the Python layout-lens tests when changing `crates/pcb-layout/src/scripts/lens/`.

Never run `cargo insta accept` without explicit user approval.

## Where to Look

- Start with `README.md` for product context and user-facing CLI behavior.
- `crates/pcb/src/main.rs` is the shim/version manager; `crates/pcbc/src/main.rs` declares compiler CLI subcommands, with matching files in `crates/pcbc/src/` (`build.rs`, `layout.rs`, `fmt.rs`, `test.rs`, etc.).
- `crates/pcb-zen-core/src/lang/` contains Zener built-ins, core values, type conversion, validation, and evaluation semantics. Check `docs/pages/spec.mdx` when changing user-visible language behavior.
- `crates/pcb-zen/src/` provides higher-level workspace/dependency resolution, package cache, diagnostics plumbing, and LSP integration around `pcb-zen-core`.
- `crates/pcb-sch/src/` is the schematic/netlist data model, BOM helpers, KiCad netlist export, and `# pcb:sch` comment handling.
- `crates/pcb-layout/src/` generates and synchronizes KiCad layouts. The Python lens implementation is in `crates/pcb-layout/src/scripts/lens/`; read `crates/pcb-layout/README.md` before changing sync behavior.
- `crates/pcb-kicad/src/` wraps KiCad tooling and KiCad-specific checks; `crates/pcb-sexpr/src/` handles low-level S-expression parsing/rewriting for KiCad files.
- `crates/pcbc/src/import/` implements `pcb import`; start with `crates/pcbc/src/import/README.md` and `flow.rs` before changing the import pipeline.
- `crates/pcb-eda/` and `crates/pcb-component-gen/` handle external EDA artifacts and generated Zener component modules.
- `crates/pcb-diode-api/` contains API/auth/search/BOM-matching client logic.
- `lib/std/` contains the Zener standard library; `examples/` and `test-workspaces/` are useful runnable designs.
- Use `docs/pages/packages.mdx` for workspace, dependency, and package behavior.

## Working Rules

- Prefer the smallest correct change and match existing crate/module boundaries.
- Avoid adding cross-crate abstractions until a boundary is already shared in the existing code.
- Keep Git history linear: before pushing, fetch and rebase local commits onto `origin/main`, then fast-forward push; never create merge commits.
- In `.zen` files, remember Zener is Starlark-based, not Python: do not use f-strings.
- Language behavior may come from the pinned `diodeinc/starlark-rust` fork in `Cargo.toml`; check that fork when local code does not explain Starlark behavior.

## CLI Output

- Use `anstream`'s `print!`, `println!`, `eprint!`, and `eprintln!` replacements for incidental CLI text so a closed output pipe does not panic.
- For primary, structured, protocol, or binary stdout (such as JSON, CSV, tables, XML, and images), keep renderers fallible over `std::io::Write` and call them through `pcb_ui::write_stdout` at the CLI boundary. It treats only `BrokenPipe` as successful termination and preserves other I/O errors.
- Do not add a process-wide `BrokenPipe` catch or custom printer wrapper; PCB also uses unrelated child-process and network pipes whose failures must remain visible.
- Gate `anstream`, `pcb-ui`, and their macro imports behind the CLI feature in crates that also support non-CLI or WASM builds.

## Documentation Rules

- For user-visible changes, add one succinct `CHANGELOG.md` entry under `Unreleased` per logical change.
- If you change Zener syntax, built-ins, core types, imports/modules, or type rules, update `docs/pages/spec.mdx`.
- If you change workspace manifests, dependency resolution, or package behavior, update the relevant docs in `docs/pages/`, especially `docs/pages/packages.mdx`.
- If you change stdlib APIs or examples, prefer a focused docs/example update over broad documentation churn.

## Verification

- Run the narrowest relevant check first: usually `cargo nextest run -p <crate>`, a focused `cargo run -p pcbc -- ...` command, or the layout-lens pytest command above.
- Do not run full-workspace checks after every small edit.
- If snapshot tests change, call that out and leave snapshot acceptance to the user.
