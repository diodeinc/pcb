# pcb Dependency Management: Design

Status: draft
Companion doc: [`go-modules-research.md`](./go-modules-research.md)

This document proposes a redesign of pcb's dependency management to
align with modern (Go 1.17+) Go modules. It assumes the research doc
as background.

---

## Goals

1. **`pcb.toml` is self-describing.** Each package's manifest contains
   the full resolved set — direct and transitive — with exact
   versions. Given `pcb.toml`, anyone can know exactly what the build
   will consume without walking a graph or consulting a sum file.
2. **Resolution happens at `pcb mod sync` / `pcb mod add` time, not `pcb build` time.** Eval
   is a lookup, not a graph walk. `pcb build` against a complete
   `pcb.toml` does no MVS.
3. **MVS for selection.** Minimal Version Selection gives us
   reproducibility without a lockfile and high-fidelity builds — you
   only get version bumps when someone in your graph explicitly raised
   a floor.
4. **Keep mutation explicit.** Dependency-graph mutation happens in
   `pcb mod sync` / `pcb mod add`, not implicitly during `pcb build`.
5. **`pcb build` is pure read.** All world-changing work lives in
   `pcb mod sync` / `pcb mod add`.

---

## `pcb.toml` shape

Two flavors of manifest, corresponding to Go's `go.work` / `go.mod`
split:

- **Workspace manifest** (root of a repo): declares members and
  workspace-wide config. Analogous to `go.work`. Does not carry
  dependencies.
- **Package manifest** (inside a member directory): declares that
  member's dependencies and participates in MVS. Analogous to `go.mod`.
  Each member has one.

Each member has its own `pcb.toml` today; the root holds workspace
config. We keep that structure.

### Workspace manifest

```toml
[workspace]
repository = "github.com/dioderobot/demo"
members = ["components/**", "reference/*", "modules/*", "boards/*"]
vendor = ["github.com/diodeinc/registry/**"]
```

No `[dependencies]` at the workspace level. `vendor = [...]` is a glob
list: deps whose module paths match any pattern are materialized into
`vendor/`; others stay cache-only. Existing behavior, unchanged.

### Package manifest

```toml
[dependencies]
"github.com/diodeinc/registry/reference/BMI270x" = "0.6.2"
"github.com/diodeinc/registry/reference/LM5163Q1" = "0.6.0"

[dependencies.indirect]
"github.com/diodeinc/registry/components/BMI270"        = "0.3.2"
"github.com/diodeinc/registry/components/LM5163QDDARQ1" = "0.3.2"
"github.com/diodeinc/registry/components/SRN6045-470M"  = "0.3.2"
"github.com/diodeinc/registry/modules/LedIndicator@0.8" = "0.8.0"
```

Rules:

- `[dependencies]` is human-curated intent — what this package
  directly `load()`s. Always exactly = the set of module roots
  referenced from this package's source (see "scan semantics" below).
- `[dependencies.indirect]` is tool-managed — the transitive closure
  MVS pulls in. Humans do not edit it.
- `[dependencies]` keys stay **bare module paths**. This package can
  choose at most one direct compatibility lane per module path.
- `[dependencies.indirect]` may include **lane-qualified** keys of the
  form `<module>@<lane>` when the full resolved closure contains an
  additional compatibility lane for that same module path.
- Versions are always **exact, fully-resolved** strings. No ranges, no
  wildcards.
- Together, both tables comprise the complete build list for this
  package. After `pcb mod sync`, the root eval package's `pcb.toml` is the
  complete source of truth for exact version resolution across the
  whole tree.
- `pcb mod sync` writes both tables canonically (sorted, formatted).

### Compatibility lanes

Source code continues to import bare module paths:

```python
Module("github.com/acme/foo/Foo.zen")
```

Compatibility lanes live in manifests, not in source imports.
Lane strings are derived from versions via a compatibility-lane
function; examples in this doc use lanes like `0.1`, `0.8`, `1`, and
`2`.

Example:

```toml
[dependencies]
"github.com/acme/foo" = "0.1.1"

[dependencies.indirect]
"github.com/acme/foo@0.8" = "0.8.3"
```

Semantics:

- If package `P` imports bare module path `foo`, then `P` must have a
  direct dependency entry for bare `foo` in `[dependencies]`.
- The direct dependency version in `[dependencies]` chooses `P`'s lane
  for `foo`.
- `P`'s own imports always resolve their lane through `P`'s direct
  `[dependencies]`, never through `[dependencies.indirect]`.
- Once the lane is known, the exact resolved version comes from the
  root eval package's fully-hydrated closure in `pcb.toml`, whether
  that `(module, lane)` entry is represented as direct or indirect.
- `[dependencies.indirect]` exists to capture the rest of the resolved
  closure, including additional lanes needed elsewhere in the graph.
- This keeps the important invariant: after `pcb mod sync`, the root eval
  package's `pcb.toml` completely describes the full dependency
  resolution state.

### Package version

Packages do **not** declare their own version in `pcb.toml`. A
package's version is determined by its git tag at publish time. Same
as Go. Consumer `[dependencies]` entries are the only place versions
appear.

### Intra-workspace deps

Within a workspace, sibling-member references (e.g. `boards/DM0003`
loading from `components/Nexperia/PUSB3F96X` in the same repo) resolve
to the sibling's local source on disk — the declared version in
`[dependencies]` is ignored for local builds. Matches Go's `go.work`
semantics. Already how pcb works today (see
`crates/pcb-zen-core/src/resolution.rs:164-190`).

The declared version still matters for downstream consumers of a
single package. `pcb mod sync` keeps these entries in sync with the
registry's latest published version of the sibling. Same behavior as
today.

### Future extensions (reserved, not built)

```toml
[replace]
"github.com/foo/bar" = { path = "../bar" }

[exclude]
"github.com/foo/bar" = ["1.2.4"]
```

As in Go, `replace` / `exclude` would be main-module-only.

---

## Version strings

Accepted forms:

- **Semver releases** — `"1.2.3"`, `"0.6.2"`. Ordered per semver 2.0.0.
- **Semver prereleases** — `"1.2.3-rc.1"`. Ordered per semver. Never
  auto-selected as "latest."
- **Pseudo-versions** — Go-style, for untagged commits, e.g.
  `"0.0.0-20260417150300-abcdef123456"`. Format:
  `<base>-<UTC-timestamp>-<12-char-commit-hash>`. Sort below any
  release at `<base>`.

Dependency resolution can consume existing `rev` / `branch` detailed
dependency specs and resolve them to pseudo-versions. `pcb mod add`
accepts `@latest`, exact semver versions, and direct `@<sha>` /
`@<branch>` selectors. Only the three forms above should appear in
hydrated manifests.

### Compatibility lane identity

MVS v2 selects versions per **lane-qualified module identity**, not
just per bare module path.

Conceptually:

```text
(module_path, lane) -> selected_version
```

Direct dependencies keep bare keys in `[dependencies]`; the direct
dependency version chooses the importing package's lane for that bare
path. Additional selected lanes for the same bare path are captured in
`[dependencies.indirect]` as `module@lane`.

This is a deliberate divergence from Go's `/v2` path convention.
Instead of putting compatibility identity in source import strings, pcb
keeps source imports bare and records compatibility lanes in
`pcb.toml`.

### MVS, briefly

For each lane-qualified module identity in the transitive graph, take
the **maximum** of all declared minimum versions (floors). That's the
selected version for that `(module_path, lane)` pair. Deterministic, no
backtracking, no lockfile needed for reproducibility. See
[`go-modules-research.md`](./go-modules-research.md) for depth.

---

## Commands

Primary verbs: `pcb mod sync`, `pcb mod add`, `pcb build`, `pcb info`.
Top-level aliases exist for the common mutations: `pcb sync` aliases
`pcb mod sync`, and `pcb add` aliases `pcb mod add`.

Debug/audit helpers:

- `pcb mod download [url@version]` pre-populates the package cache
  without rewriting manifests or vendor state.
- `pcb mod graph` prints a Go-style edge list for the package graph.
- `pcb mod why <url>` prints one shortest reason path for a dependency.
- `pcb mod resolve [path]` prints the frozen MVS v2 resolution table
  that eval can consume.

### `pcb mod sync`

Owns all mutation between source and a build-ready local state.
Build-ready means:

1. `pcb.toml` is authoritative (full transitive closure written).
2. `~/.pcb/cache` contains every entry in `pcb.toml`.
3. `vendor/` contains every vendored entry.

Forms:

```
pcb mod sync            reconcile pcb.toml with sources (add + remove),
                        run MVS, hydrate cache, write vendor/
pcb mod sync --offline  verify the hydrated manifest can be materialized
                        from local stores only
pcb sync                alias for pcb mod sync
```

No `@none`. Removal is implicit: bare `pcb mod sync` drops entries whose
URLs are no longer `load()`'d anywhere in this package's source.

Context-aware scoping:

- Inside a member directory → operates on that package only.
- At workspace root → operates on every member.

### `pcb mod add`

Package-scoped only:

- Inside a member directory -> operates on that package.
- At workspace root -> error; `pcb mod add` does not fan out across members.

```
pcb mod add <url>            add / update one dep to latest compat
pcb mod add <url>@1.2.3      pin one dep
pcb mod add <url>@latest     upgrade one dep to latest
pcb mod add <url>@<sha>      pin one dep to a pseudo-version
pcb mod add <url>@<branch>   pin one dep to that branch head pseudo-version
pcb mod add -u [<url>]       upgrade one existing direct dep, or all direct deps
pcb add <url>[@ver]          alias for pcb mod add <url>[@ver]
```

Examples:

- `pcb mod add github.com/acme/foo@latest` updates that direct
  dependency to the latest compatible version, then re-runs MVS and
  rewrites the hydrated manifest.
- `pcb mod add github.com/acme/foo@1.2.3` pins that direct dependency
  to `1.2.3`, then re-runs MVS and rewrites the hydrated manifest.
- `pcb mod add -u` upgrades every direct remote dependency in the
  current package to the latest compatible release, then runs the same
  sync pipeline.

"Raise floors" = MVS terminology. Each dependency entry is a minimum
version floor; the selected version is the maximum floor across the
graph.

### `pcb mod download`

Cache-only:

```
pcb mod download                  download every selected remote dep in
                                  the hydrated package closure
pcb mod download <url>@1.2.3      download one exact package version
```

No manifest writeback and no vendor reconciliation. This is for CI
cache warmup and offline preparation.

### `pcb build`

```
pcb build                 frozen build; no pcb.toml mutation
pcb build --offline       no network at all; local stores only
```

`pcb build` builds everything under cwd that's a workspace member.
Optional positional path narrows the scope. Same as today. Works on
any package type (component, module, board); board-specific commands
like `layout` / `route` require a `[board]` table.

`pcb build` and `pcb build --offline` behavior on missing deps: if source
references a module root not in `pcb.toml`, error hard. The user must
run `pcb mod sync` or `pcb mod add` first.

### `pcb info`

Unchanged. Covers "show me the resolved build list." No separate
`pcb mod list`.

---

## Phases

Four phases cleanly delineate all dep-related work:

1. **Resolve.** Run MVS; write `pcb.toml` (direct + indirect).
2. **Hydrate cache.** Populate `~/.pcb/cache` for every entry in
   `pcb.toml`.
3. **Vendor.** Copy from cache into `vendor/` per the workspace's
   vendor glob.
4. **Eval.** Pure read from local stores.

### Flag mapping

```
pcb mod sync          = 1 + 2 + 3
pcb mod add           = 1 + 2 + 3

pcb build             = 2 + 4       (for hydrated packages: rehydrate cache only; no vendor write)
pcb build --offline   = 4           (pure local read)
```

`--offline` = "don't touch the network."

### Key invariant

**`pcb build` never writes to `vendor/`.** Vendor population is
strictly `pcb mod sync` / `pcb mod add`'s job. Plain `pcb build` may
rehydrate the cache for non-vendored deps (since not everything lives
in `vendor/`) but does not touch the vendor directory.

CI reproducibility pattern: `pcb mod sync && pcb build --offline`
— hydrate from the committed manifest, then prove eval works with
zero network.

### Read resolution order (eval)

`vendor/` → `~/.pcb/cache` → (network unless `--offline`) → error.

Lane selection happens before this lookup:

1. The importing package's direct `[dependencies]` chooses the lane for
   a bare imported module path.
2. The root eval package's fully-hydrated `pcb.toml` provides the exact
   resolved version for that `(module_path, lane)` pair.
3. Eval then reads that concrete version from `vendor/` / cache.

---

## Behavior matrix

| Command | Mutates `pcb.toml` | Runs MVS | Network | Cache | Vendor |
|---|---|---|---|---|---|
| `pcb mod sync` | add + remove | yes (full sync) | yes | read/write | write |
| `pcb mod add <url>` | add/update one | yes | yes | read/write | write |
| `pcb mod add <url>@1.2.3` | pin one | yes | yes | read/write | write |
| `pcb build` on hydrated packages | no | no | rehydrate | read/write | read only |
| `pcb build --offline` on hydrated packages | no | no | no | read | read only |
| `pcb build` on legacy packages | legacy behavior | legacy behavior | legacy behavior | legacy behavior | legacy behavior |

---

## `pcb mod sync`: precise steps

1. Locate the workspace root (walk up from cwd); identify this
   package (and other members if scope is workspace-wide).
2. Glob `members`; for each in-scope package, collect every
   `load(...)` / `Module(...)` URL from its `.zen` files.
3. Filter:
   - Drop `@stdlib/...` (hardcoded alias, not a dep).
   - Drop relative paths that stay inside the same package.
   - Resolve relative paths with longest-prefix package ownership; if
     the target path belongs to another workspace member, record that
     member as a direct workspace dependency.
   - Drop URLs under `[workspace].repository` (intra-workspace;
     resolved locally).
4. For each remaining URL, apply longest-prefix module-root inference
   to determine the module that owns it. That's the direct dependency
   set.
5. For each URL in `[dependencies]` whose module is no longer in the
   set, drop it.
6. For each URL not in `[dependencies]`, look up the latest non-
   prerelease version from the registry.
7. Run MVS across all direct requirements, using lane-qualified module
   identities and recursively reading each dep's `pcb.toml`:
   - For each direct dep `module = version`, derive the direct lane
     from `version` and seed `(module, lane)`.
   - If a dep manifest has `[dependencies.indirect]`, read
     `[dependencies]` + `[dependencies.indirect]`, interpreting bare
     `[dependencies]` keys as that dep package's own direct lane
     choices and lane-qualified indirect keys as additional selected
     lanes. This gives a one-hop, pruned MVS fast-path.
   - If a dep manifest does not have `[dependencies.indirect]`, walk
     the transitive graph. Per-dep fallback, not global.
   - The selected set is conceptually `(module_path, lane) ->
     version`, not just `module_path -> version`.
8. Write `pcb.toml` canonically, including:
   - Updated `[dependencies]` for the package's direct lane choices.
   - Updated `[dependencies.indirect]` for the rest of the resolved
     closure, including any additional lanes for the same module path.
   - A complete root/package-local closure that downstream eval can
     treat as authoritative.
9. Hydrate `~/.pcb/cache` for every entry in the new build list
   (atomic tmpdir + rename per entry).
10. Materialize `vendor/` for entries matching the workspace vendor
   glob.
11. Stay quiet on success by default. With `-v`, print changed
    manifests using workspace-relative paths:
    ```
    pcb: updated boards/WV0001/pcb.toml
    ```

Target hardening: `pcb mod sync` should become atomic across manifest,
cache, and vendor updates, and should serialize concurrent manifest
writes with a file lock. Cache writes already reuse the existing
materialization path, including per-cache-entry locking.

---

## Cache layout

User-global at `~/.pcb/cache`. Same layout as `vendor/`:

```
~/.pcb/cache/github.com/diodeinc/registry/reference/BMI270x/0.6.2/...
```

Write-once, read-many. Never mutated in place after first write (entry
corruption → manual removal). Cross-project sharing is safe because
registry versions are immutable and entries are keyed by
`(module, version)`.

Vendor layout mirrors this, rooted under the workspace's `vendor/`:

```
vendor/github.com/diodeinc/registry/reference/BMI270x/0.6.2/...
```

Unchanged from today.

---

## Source scan semantics

What counts as "source" for bare `pcb mod sync` reconciliation:

- Every `.zen` file reachable from a workspace member's glob. Files
  outside `members` are ignored.
- Within a package, **relative paths** for intra-package references
  (`Module("src/Foo.zen")`).
- Relative paths that leave the current package are resolved by
  longest-prefix package ownership. If the target belongs to another
  workspace member, that member is recorded as a direct dependency.
- Across packages in the same workspace, **canonical URLs**
  (`Module("github.com/dioderobot/demo/components/.../Foo.zen")`);
  `pcb mod sync` identifies these via `[workspace].repository` and resolves
  them locally.
- `@stdlib/...` is hardcoded in the pcb binary — never a dependency,
  never in `pcb.toml`.

Module-root inference: given `load("github.com/foo/bar/sub/thing.zen")`
and a registry, `pcb mod sync` walks prefixes (longest first) until it
finds a real module root. That becomes the key in `[dependencies]`.
Matches Go.

Transitive deps are never discovered by scanning dep source files.
Each dep's `pcb.toml` is authoritative for its own direct deps. If a
dep's manifest is stale or missing, it falls into the legacy-fallback
path.

---

## Relationship to Go

| Go | pcb |
|---|---|
| `go get <mod>[@ver]` | `pcb mod add <url>[@ver]` |
| `go mod tidy` | `pcb mod sync` |
| `go get -u ./...` | `pcb mod add -u` |
| `go mod download` | `pcb mod download` |
| `go build -mod=readonly` | `pcb build` |
| `go build && go mod tidy` (or explicit wrapper) | `pcb sync && pcb build` |
| `go build -mod=vendor` | `pcb build --offline` |
| `go list -m all` | `pcb info` |
| `go.work` | workspace `pcb.toml` |
| `go.mod` | package `pcb.toml` |
Deliberate divergences:

- **Two explicit module verbs.** `pcb mod sync` owns source
  reconciliation; `pcb mod add` owns targeted direct-dependency edits.
- **Auto-vendor is on by default.** Go requires explicit
  `go mod vendor`. Hardware workflows benefit more from always-local
  bills of materials.
- **Compatibility lanes live in `pcb.toml`, not import paths.** Go
  uses `/v2` in module paths; pcb keeps source imports bare and stores
  lane identity in manifests so a hydrated `pcb.toml` can capture the
  full lane-aware closure.

---

## Phased rollout

To minimize disruption, the rollout should happen in two distinct
stages:

### Stage 1: additive plumbing only

Status: implemented.

- Add the new manifest shape needed by MVS v2, especially
  `[dependencies.indirect]` and lane-qualified indirect entries.
- Introduce `pcb mod sync` as a **self-contained package-level command**.
- `pcb mod sync` ignores `pcb.sum` entirely.
- `pcb mod sync` performs lane-aware MVS and hydrates `pcb.toml` only:
  direct deps, indirect deps, exact version updates, and unused-dep
  removal.
- Running `pcb mod sync` at workspace root is only a thin sequential loop
  over member packages. There is no separate workspace-level resolver.
- Existing `pcb build` / eval behavior stays unchanged in this stage.
  The old resolver simply ignores `[dependencies.indirect]`.

### Stage 2: downstream adoption

Status: MVP implemented for hydrated package scopes in the shared
native resolve/eval path (`build`, `bom`, `layout`, `open`, `test`,
`sim`, `route`). Packaging/publishing/release flows still need a
separate audit because some of them invoke the legacy resolver before
attaching MVS v2 tables.

- Teach `pcb build` and related flows to consume the hydrated manifest
  as the primary resolution source.
- Gradually reduce reliance on `pcb.sum` for semantic resolution.
- Keep legacy recursive fallback for older package manifests until the
  ecosystem has broadly migrated to complete manifests.

The current switch is invocation-scoped and conservative: if all target
packages for the requested path have non-empty `[dependencies.indirect]`,
the command uses the frozen MVS v2 resolution map. Otherwise it falls
back to the legacy resolver/eval behavior.

### Compatibility rule during rollout

During the mixed world where some manifests are old and some are new:

- If a dep manifest includes `[dependencies.indirect]`, MVS v2 may
  treat that manifest as authoritative and avoid recursive traversal.
- If a dep manifest does not include `[dependencies.indirect]`, MVS v2
  must assume it is legacy and walk transitively.

This keeps the new resolver correct before universal migration and lets
the new plumbing land without making it immediately load-bearing for
builds.

---

## Non-goals for this pass

- **`pcb.sum`.** Not in scope. May or may not be part of the future
  design.
- **`[replace]` / `[exclude]`.** Shape reserved in the TOML; semantics
  and tooling come later.
- **Checksum database / transparency log** (the `sum.golang.org`
  analogue).
- **`pcb publish` changes.** Client-side check that published packages
  carry complete `[dependencies.indirect]` is deferred.
- **Migration.** How existing projects with resolution-participating
  `pcb.sum` move to the new model — separate design.

---

## Open questions

- Diff hygiene: `[dependencies.indirect]` churn on unrelated upgrades
  is noisy. Go lives with it. Worth exploring if it bites in practice.
- Legacy-fallback stderr note: a quiet one-liner per legacy dep
  during `pcb mod sync` prompts authors to migrate. Format TBD.
