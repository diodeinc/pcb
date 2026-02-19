# Branch To Commit Pinning Spec

## Summary

Branch-only dependencies are mutable and non-reproducible. This spec makes commit identity the source of truth for non-semver dependencies while keeping branch intent for update workflows.

The manifest (`pcb.toml`) should carry the resolved commit (`rev`) for branch-based dependencies. The lockfile (`pcb.sum`) remains an integrity/index artifact, not the primary dependency identity source.

## Goals

- Ensure reproducible dependency resolution by default.
- Keep branch-driven workflows ergonomic for `pcb update`.
- Keep auto-dep conservative: only add deps that can be materialized.

## Non-Goals

- Changing local `path` dependency semantics.
- Changing semver tag dependency semantics.

## Dependency Identity Model

For remote dependencies, valid identity is:

- `version` (semver tag), or
- `rev` (exact commit hash)

`branch` is a tracking hint, not sufficient identity.

Examples:

```toml
[dependencies]
"github.com/org/repo/pkg" = { rev = "2f4c0f8e1c7d..." }
"github.com/org/repo/lib" = { branch = "main", rev = "7a9d1be0a5d4..." }
"github.com/org/repo/tagged" = "0.6.2"
```

## Validity Rules

### Branch-Only Dependencies

A dependency declared with `branch` and no `rev` is unresolved and non-reproducible.

Behavior:

- `--locked`: invalid (hard error)
- `--offline`: invalid (hard error)
- any online resolve path (`auto-dep`, `build`, `update`): resolve to commit and rewrite

### Lockfile Entries

`pcb.sum` entries without concrete commit identity for branch-based deps should not exist.

If encountered, treat the entry as invalid/unusable and require online re-resolution.

## Auto-Dep Behavior

Auto-dep remains best effort and conservative.

For each discovered import URL:

1. Resolve candidate package path/version source:
   - workspace member
   - lockfile hint (only if concrete identity)
   - remote tag/index discovery
   - branch head resolution when applicable
2. Materialize candidate using resolver fetch path.
3. Add to `pcb.toml` only if materialization succeeds.

If any step fails, skip without aborting auto-dep.

### Branch To Commit in Auto-Dep

If a dependency resolves via branch semantics:

- Resolve branch to commit hash online.
- Materialize from that commit.
- Write dependency as `{ branch = "...", rev = "..." }`.

Do not write branch-only dependency entries.

## Update Semantics

### `pcb update`

- `branch + rev`: refresh branch tip, update `rev` when changed.
- `rev` only: keep pinned unless explicitly changed.
- `version`: follow existing semver/tag update rules.

## Migration

On next online resolve (`auto-dep`, `build`, or `update`):

- If `pcb.toml` contains branch-only dependency:
  - resolve to commit
  - rewrite as `branch + rev`
- If resolution fails:
  - keep existing entry
  - emit warning

## Error Handling

Branch-only deps should produce actionable errors in strict contexts:

- "Dependency `<url>` uses `branch` without `rev` and is not reproducible."
- "Run an online resolve (`pcb build` or `pcb update`) to pin branch dependencies to a commit."

## Compatibility Notes

- Semver/tag and path dependencies are unchanged.
- Existing lockfile hash verification remains unchanged.
