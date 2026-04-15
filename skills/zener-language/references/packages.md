# Packages And Manifests

Files, imports, and packages:

- `@stdlib/...` is implicit and toolchain-managed; do not declare it in `[dependencies]`.
- Remote import paths do not contain versions. Versions live in `pcb.toml`.
- Dependency resolution uses Minimal Version Selection and records exact versions and hashes in `pcb.sum`.
- Commit `pcb.sum` for reproducible builds.

Workspace and package structure:

- Official package types are workspace root, boards, modules, and components.
- Preferred repo organization is:
  - `boards/`: buildable boards
  - `modules/`: reusable subcircuits
  - `components/`: component packages; these may begin as generated device definitions and grow into richer reusable designs around the part
- Root `pcb.toml` holds `[workspace]`; board packages hold `[board]`; reusable packages hold dependencies and optional default `parts`.

`pcb.toml`:

- `pcb.toml` is the manifest for a workspace or package.
- At the workspace root, it defines `[workspace]` metadata and members.
- In board packages, it defines `[board]` and `[dependencies]`.
- In reusable packages, it usually defines `[dependencies]` and may define default `parts`.
- For manifest details, package layout, dependency semantics, and field meanings, read `~/.pcb/docs/packages.md`.

Source of truth:

- For component packages, treat the `.kicad_sym` file as the source of truth for the primitive component interface and pin structure.
- Treat the `.zen` file, `pcb.toml`, and `pcb.sum` as the source of truth for the reusable design built around that component.
