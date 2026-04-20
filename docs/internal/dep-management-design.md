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
2. **Resolution happens at `pcb add` time, not `pcb build` time.** Eval
   is a lookup, not a graph walk. `pcb build` against a complete
   `pcb.toml` does no MVS.
3. **MVS for selection.** Minimal Version Selection gives us
   reproducibility without a lockfile and high-fidelity builds — you
   only get version bumps when someone in your graph explicitly raised
   a floor.
4. **Preserve autodep + autovendor.** The existing UX where `pcb build`
   "just works" even if you `load()` a new URL stays. The messy work is
   factored into `pcb add`.
5. **`pcb build` is pure read.** All world-changing work lives in
   `pcb add`.

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
```

Rules:

- `[dependencies]` is human-curated intent — what this package
  directly `load()`s. Always exactly = the set of module roots
  referenced from this package's source (see "scan semantics" below).
- `[dependencies.indirect]` is tool-managed — the transitive closure
  MVS pulls in. Humans do not edit it.
- Versions are always **exact, fully-resolved** strings. No ranges, no
  wildcards.
- Together, both tables comprise the complete build list for this
  package.
- `pcb add` writes both tables canonically (sorted, formatted).

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
single package. `pcb add` keeps these entries in sync with the
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

The CLI accepts `@<sha>` and `@<branch>` as resolution inputs; `pcb
add` resolves them to pseudo-versions and writes the pseudo-version
into `pcb.toml`. Only the three forms above ever appear in the
manifest.

### Major versions

We do **not** use Go's `/v2` path convention. One path per module,
across all majors. MVS picks one version; cross-major incompatibility
manifests as a `pcb add` warning (see "major-version spread" below)
and, if the selected version is actually incompatible with some
consumer, as an eval-time error. Trade-off: simpler than path
versioning, less safe. Pre-1.0 ecosystem doesn't feel the pain yet.

### MVS, briefly

For each module in the transitive graph, take the **maximum** of all
declared minimum versions (floors). That's the selected version.
Deterministic, no backtracking, no lockfile needed for reproducibility.
See [`go-modules-research.md`](./go-modules-research.md) for depth.

---

## Commands

Three verbs: `pcb add`, `pcb build`, `pcb info`.

### `pcb add`

Owns all mutation between source and a build-ready local state.
Build-ready means:

1. `pcb.toml` is authoritative (full transitive closure written).
2. `~/.pcb/cache` contains every entry in `pcb.toml`.
3. `vendor/` contains every vendored entry.

Forms:

```
pcb add                  reconcile pcb.toml with sources (add + remove),
                         run MVS, hydrate cache, write vendor/
pcb add <url>            add / update one dep to latest compat
pcb add <url>@1.2.3      pin one dep
pcb add <url>@latest     upgrade one dep to latest
pcb add <url>@<sha>      set one dep to a pseudo-version (resolved)
pcb add <url>@<branch>   set one dep to a pseudo-version (resolved)
pcb add -u               raise floors on all deps
pcb add -u <url>         raise floor on one dep

pcb add --locked         skip phase 1; hydrate cache + vendor from
                         existing pcb.toml (reproducibility mode)
```

No `@none`. Removal is implicit: bare `pcb add` drops entries whose
URLs are no longer `load()`'d anywhere in this package's source.

Context-aware scoping:

- Inside a member directory → operates on that package only.
- At workspace root → operates on every member.

"Raise floors" = MVS terminology. Each `require` entry is a minimum
version floor; the selected version is the maximum floor across the
graph. `-u` bumps floors to latest.

### `pcb build`

```
pcb build                 full (pcb add + eval)
pcb build --locked        no pcb.toml mutation; rehydrate cache if needed
pcb build --offline       no network at all; local stores only
```

`pcb build` builds everything under cwd that's a workspace member.
Optional positional path narrows the scope. Same as today. Works on
any package type (component, module, board); board-specific commands
like `layout` / `route` require a `[board]` table.

`--locked` and `--offline` behavior on missing deps: if source
references a module root not in `pcb.toml`, error hard. The user must
run `pcb add` first.

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
pcb add               = 1 + 2 + 3
pcb add --locked      = 2 + 3       (skip resolve)

pcb build             = 1 + 2 + 3 + 4
pcb build --locked    = 2 + 4       (rehydrate cache only; no vendor write)
pcb build --offline   = 4           (pure local read)
```

`--locked` = "don't mutate `pcb.toml`."
`--offline` = "don't touch the network."

### Key invariant

**`pcb build` never writes to `vendor/`.** Vendor population is
strictly `pcb add`'s job. `pcb build --locked` may rehydrate the cache
for non-vendored deps (since not everything lives in `vendor/`) but
does not touch the vendor directory.

CI reproducibility pattern: `pcb add --locked && pcb build --offline`
— hydrate from the committed manifest, then prove eval works with
zero network.

### Read resolution order (eval)

`vendor/` → `~/.pcb/cache` → (network if `--locked`) → error.

---

## Behavior matrix

| Command | Mutates `pcb.toml` | Runs MVS | Network | Cache | Vendor |
|---|---|---|---|---|---|
| `pcb add` | add + remove | yes (full sync) | yes | read/write | write |
| `pcb add --locked` | no | no | rehydrate | read/write | write |
| `pcb add <url>` | add/update one | yes | yes | read/write | write |
| `pcb add <url>@1.2.3` | pin one | yes | yes | read/write | write |
| `pcb add -u [<url>]` | raise floor(s) | yes | yes | read/write | write |
| `pcb build` | via `pcb add` | via `pcb add` | yes | read/write | read + write |
| `pcb build --locked` | no | no | rehydrate | read/write | read only |
| `pcb build --offline` | no | no | no | read | read only |

---

## `pcb add` (bare): precise steps

1. Locate the workspace root (walk up from cwd); identify this
   package (and other members if scope is workspace-wide).
2. Glob `members`; for each in-scope package, collect every
   `load(...)` / `Module(...)` URL from its `.zen` files.
3. Filter:
   - Drop `@stdlib/...` (hardcoded alias, not a dep).
   - Drop relative paths (intra-package navigation).
   - Drop URLs under `[workspace].repository` (intra-workspace;
     resolved locally).
4. For each remaining URL, apply longest-prefix module-root inference
   to determine the module that owns it. That's the direct dependency
   set.
5. For each URL in `[dependencies]` whose module is no longer in the
   set, drop it.
6. For each URL not in `[dependencies]`, look up the latest non-
   prerelease version from the registry.
7. Run MVS across all direct requirements, recursively reading each
   dep's `pcb.toml`:
   - If a dep manifest has `[dependencies.indirect]`, read
     `[dependencies]` + `[dependencies.indirect]`, take the union
     (one-hop, pruned MVS).
   - If a dep manifest does not have `[dependencies.indirect]`, walk
     the transitive graph. Per-dep fallback, not global.
   - On major-version spread for any module (floors spanning majors):
     warn on stderr, still resolve to max.
8. Hydrate `~/.pcb/cache` for every entry in the new build list
   (atomic tmpdir + rename per entry).
9. Materialize `vendor/` for entries matching the workspace vendor
   glob.
10. Write `pcb.toml` canonically, including:
    - Updated `[dependencies]` and `[dependencies.indirect]`.
11. Print a stderr summary of changes:
    ```
    + github.com/baz/qux 2.1.0 (new)
    ↑ github.com/foo/bar 1.2.0 → 1.5.0 (required by github.com/baz/qux)
    - github.com/old/dep 0.4.2 (unused)
    ⚠ major-version spread on github.com/some/lib (1.5.0 vs 2.0.0)
    ```

`pcb add` is atomic: resolution errors or network failures abort the
whole command with no writes to `pcb.toml`, cache, or vendor. Either
everything lands consistently or nothing changes.

A file lock (`flock` on `pcb.toml`) serializes concurrent `pcb add`
invocations in the same workspace. Cache writes across workspaces are
safe via atomic tmpdir-rename because registry versions are immutable.

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

What counts as "source" for bare `pcb add` reconciliation:

- Every `.zen` file reachable from a workspace member's glob. Files
  outside `members` are ignored.
- Within a package, **relative paths** for intra-package references
  (`Module("src/Foo.zen")`).
- Across packages in the same workspace, **canonical URLs**
  (`Module("github.com/dioderobot/demo/components/.../Foo.zen")`);
  `pcb add` identifies these via `[workspace].repository` and resolves
  them locally.
- `@stdlib/...` is hardcoded in the pcb binary — never a dependency,
  never in `pcb.toml`.

Module-root inference: given `load("github.com/foo/bar/sub/thing.zen")`
and a registry, `pcb add` walks prefixes (longest first) until it
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
| `go get <mod>[@ver]` | `pcb add <url>[@ver]` |
| `go mod tidy` | `pcb add` (bare) |
| `go get -u ./...` | `pcb add -u` |
| `go build` (pre-1.16 `-mod=mod` default) | `pcb build` |
| `go build -mod=readonly` | `pcb build --locked` |
| `go build -mod=vendor` | `pcb build --offline` |
| `go list -m all` | `pcb info` |
| `go.work` | workspace `pcb.toml` |
| `go.mod` | package `pcb.toml` |
Deliberate divergences:

- **One verb (`pcb add`) instead of two.** Go split `go get` /
  `go mod tidy` for historical reasons. Bare `pcb add` does the tidy
  job; `pcb add <url>` does the get job.
- **Auto-vendor is on by default.** Go requires explicit
  `go mod vendor`. Hardware workflows benefit more from always-local
  bills of materials.
- **`pcb build` auto-runs `pcb add`.** Go stopped doing this at 1.16.
  We keep the autodep UX because it's valuable and `--locked` /
  `--offline` give a clean opt-out.
- **No `/v2` path versioning.** One module path across all majors.
  Simpler, less safe. Revisit if we ever feel the pain.

---

## Phased rollout

To minimize disruption, the rollout should happen in two distinct
stages:

### Stage 1: additive plumbing only

- Add the new manifest shape needed by MVS v2, especially package-level
  metadata and `[dependencies.indirect]`.
- Introduce `pcb add` as a **self-contained package-level command**.
- `pcb add` ignores `pcb.sum` entirely.
- `pcb add` performs Go-style MVS and hydrates `pcb.toml` only:
  direct deps, indirect deps, version updates, and unused-dep removal.
- Running `pcb add` at workspace root is only a thin sequential loop
  over member packages. There is no separate workspace-level resolver.
- Existing `pcb build` / eval behavior stays unchanged in this stage.
  The old resolver simply ignores `[dependencies.indirect]`.

### Stage 2: downstream adoption

- Teach `pcb build` and related flows to consume the hydrated manifest
  as the primary resolution source.
- Gradually reduce reliance on `pcb.sum` for semantic resolution.
- Keep legacy recursive fallback for older package manifests until the
  ecosystem has broadly migrated to complete manifests.

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
- **`pcb add --offline`.** Nice-to-have for verifying the manifest
  against the cache without network. Not MVP.
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
  during `pcb add` prompts authors to migrate. Format TBD.
