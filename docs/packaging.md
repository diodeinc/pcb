# PCB Toolchain Packaging V2

## Overview

Proposes per-package versioning in monorepos, ahead-of-time dependency resolution with lockfiles, and semantic versioning support. Draws from Go modules for Git-based distribution and Cargo for manifest structure. Enables parallel dependency downloads, reproducible builds, and gradual migrations across major versions.

## Motivation

**Coarse-Grained Versioning.** Repository-level versioning forces all-or-nothing upgrades. The component registry contains hundreds of parts from different manufacturers (`ti/tps54331`, `analog/ltc3115`, `infineon/mosfets`) all sharing one repository version. A bug fix in the definition of `ti/tps54331` currently requires upgrading the entire registry to `v0.2.28`, potentially pulling in breaking changes or unstable updates from unrelated components like `analog/ltc3115`.

**Lazy Resolution.** Dependencies resolve during evaluation. Evaluator hits `load("@registry/ti/tps54331.zen", ...)`, stops, fetches, resumes. Serial fetching kills cold build performance. Can't determine complete dependency graph without running evaluation. Can't parallelize downloads. CI building multiple boards in parallel can't share discovery work or pre-fetch dependencies.

**No Reproducible Builds.** No lockfiles. Weave boards often depend on mutable references like branches (`@stdlib:main`) or moving tags. A build that passes today might fail tomorrow if the branch is updated, breaking CI unexpectedly. Even with specific tags (`v0.2.13`), there is no cryptographic verification, so tags can be moved or deleted. A developer checking out the project months later has no guarantee they are building with the exact same dependency code.

**Performance.** Cold build of WV0002 serially fetches stdlib@v0.3.2 and registry@v0.3.1 during evaluation. With 50 dependencies, this serialization is substantial. Ahead-of-time resolution would fetch all packages in parallel before evaluation starts.

## Goals

- Per-package versioning in monorepos (version `ti/tps54331` independent from `analog/ltc3115`)
- Ahead-of-time resolution (parse manifests, build graph, fetch parallel, then evaluate)
- Multiple major versions in one build (stdlib v1 and v2 coexist during migration)
- Reproducible builds (`pcb.sum` lockfile with cryptographic hashes)
- Foundation for caching proxy (like Go's module proxy)

**Non-Goals:** No centralized registry. No complex constraint solving beyond semver within major versions. No code signing (hash verification detects tampering, Git handles authentication).

## Current System

Three mutually exclusive config modes: `[workspace]`, `[module]`, `[board]`. All support `[packages]` section mapping aliases to load specs.

Weave workspace root:
```toml
[workspace]
name = "test"

[packages]
registry = "@github/diodeinc/registry:v0.1.1"
stdlib = "@github/diodeinc/stdlib:v0.2.8"

[access]
allow = ["*@weaverobots.com"]
```

Board WV0002 overrides versions:
```toml
[board]
name = "WV0002"
description = "Power Regulator Board"

[packages]
registry = "@github/diodeinc/registry:v0.3.1"
stdlib = "@github/diodeinc/stdlib:v0.3.2"
```

Load syntax embeds version in import path:
```python
load("@stdlib/properties.zen", "Layout")
load("@stdlib/board_config.zen", "Board", "CopperWeight")

SwitchingReg = Module("@registry/common/regulator-switching.zen")
Capacitor = Module("@stdlib/generics/Capacitor.zen")
```

Within stdlib, relative paths:
```python
load("../config.zen", "config_unit")
load("../units.zen", "Capacitance", "Resistance")
```

Evaluator processes sequentially. First load resolves `@stdlib/properties.zen` through alias, fetches `github.com/diodeinc/stdlib@v0.3.2`, loads file. Repeats for each load. Builds dependency graph incrementally without global view.

**No Version Resolution.** The system performs no version resolution or deduplication. If `Module A` depends on `stdlib@0.3.2` and `Module B` depends on `stdlib@0.3.3`, the build system downloads and evaluates **both** versions. This results in:
1.  Bloated vendor directories containing every transitive dependency version encountered.
2.  Runtime type incompatibilities (a `Net` from `stdlib@0.3.2` might not match `stdlib@0.3.3`).
3.  Confusion when debugging which version of a function is actually running.

Weave vendor directory shows result:
```
vendor/github.com/diodeinc/stdlib/v0.2.13/
vendor/github.com/diodeinc/stdlib/v0.2.26/
vendor/github.com/diodeinc/stdlib/v0.3.1/
vendor/github.com/diodeinc/stdlib/v0.3.2/
```

Multiple versions, no coordination.

## Proposed Design

### Workspace Configuration

All V2 packages require a `[workspace]` section (even standalone packages):

```toml
[workspace]
resolver = "2"
pcb-version = "0.3"

[package]

[board]
name = "WV0002"
path = "WV0002.zen"
description = "Power Regulator Board"
```

**`resolver`** determines the packaging system version ("2" for V2). Required at workspace root to opt-in existing V1 workspaces. For new projects without V1-only constructs (`[packages]` or `[module]`), V2 is the default.

**`pcb-version`** specifies the minimum compatible toolchain release series (e.g. `0.3` covers all `0.3.x` releases). It indicates breaking changes in the language or standard library that require a newer compiler.

The package **version** itself is not in the manifest - it is derived from the Git tag, following Go's decentralized model where version control is the source of truth.

For multi-package workspaces, the `[workspace]` section also defines the base package path used to infer member package paths (see Multi-Package Workspaces below).

The optional `[board]` section specifies a `.zen` file that can be built as a standalone board with `pcb build`. Packages with or without `[board]` can be used as reusable modules via `Module()`.

### Dependencies

Full repository URLs as keys:

```toml
[dependencies]
"github.com/diodeinc/stdlib" = "0.3.2"
"github.com/diodeinc/registry/reference/ti/tps54331" = "1.0.0"
```

Version formats:
- Exact: `"0.3.2"`
- Major+minor: `"0.3"` (latest 0.3.x)
- Major: `"0"` (latest 0.x.x)
- Caret: `"^0.3.2"` (≥0.3.2, <0.4.0)
- Branch: `{ branch = "main" }` (resolves to pseudo-version)
- Revision: `{ rev = "a1b2c3d4" }` (resolves to pseudo-version)
- Path: `{ path = "../external-lib", version = "0.3.2" }` (for external local development only)

**Workspace Member Resolution:**

Workspace members are automatically discovered and used instead of fetching from Git. Package paths are inferred from the workspace repository and optional subpath:

```toml
# Workspace root pcb.toml
[workspace]
repository = "github.com/myorg/registry"
members = ["reference/*"]
```

Member packages at `reference/ti/tps54331/` automatically get the inferred path `github.com/myorg/registry/reference/ti/tps54331`. No manual configuration needed.

For nested workspaces within monorepos:

```toml
# Workspace at hardware/boards/ in monorepo
[workspace]
repository = "github.com/mycompany/monorepo"
path = "hardware/boards"
members = ["*"]
```

Member at `power/` gets path `github.com/mycompany/monorepo/hardware/boards/power`.

- **No `path` needed for workspace-internal dependencies** - just declare the normal dependency and the workspace member is automatically used
- **Workspace members always shadow Git** - if the package is in the workspace, it's always used regardless of version
- **Path dependencies are for external local development** - use them only when developing a package outside the workspace:
  ```toml
  [dependencies]
  "github.com/external/stdlib" = { path = "../stdlib", version = "0.3.2" }
  ```

**Pseudo-Versions:** Branches and revisions are resolved to pseudo-versions with commit hashes for reproducibility. Format: `v<base>-0.<timestamp>-<commit>` where base is the latest reachable Git tag incremented by one patch version. Example: commit after `v0.3.14` becomes `v0.3.15-0.20251120004415-137e2dcabc28`. Pseudo-versions participate in MVS and can win over older tags.

Full URLs eliminate ambiguity. No confusion about package origin.

### Asset Packages (Raw Git Repositories)

Dependencies that contain only assets (e.g., KiCad symbols, footprints, 3D models) and lack a `pcb.toml` manifest are supported as **Asset Packages**.

Asset packages are declared in a separate `[assets]` section to make the distinction explicit:

*   **Declaration:** Must be declared in `[assets]` (not `[dependencies]`).
*   **Validation:** System enforces that assets do NOT have `pcb.toml` manifests.
*   **Resolution:** Leaf nodes - no transitive dependencies, no MVS participation.
*   **Versioning:** Version/ref string used literally as git tag (no v-prefix logic or semver parsing).
*   **Lockfile:** `pcb.sum` records only the content hash. The manifest hash line is omitted.

```toml
[dependencies]
"github.com/diodeinc/stdlib" = "0.3.2"

[assets]
"gitlab.com/kicad/libraries/kicad-symbols" = "9.0.3"
"gitlab.com/kicad/libraries/kicad-footprints" = "9.0.3"
```

Supported formats:
- Simple ref: `"9.0.3"` (used as-is, no v-prefix added)
- Branch: `{ branch = "main" }` (resolved to commit hash in lockfile)
- Revision: `{ rev = "a1b2c3d4" }` (commit hash)

### Multi-Package Workspaces

Multi-package workspaces define a repository and optional subpath used to infer member package paths:

```toml
# Workspace root pcb.toml
[workspace]
resolver = "2"
pcb-version = "0.3"
repository = "github.com/diodeinc/registry"
members = ["reference/*"]
```

Member packages are discovered automatically and their import paths are inferred from the repository + subpath (if present) + relative directory:

```
github.com/diodeinc/registry/
  pcb.toml (workspace config)
  reference/
    ti/
      tps54331/
        pcb.toml → inferred path: github.com/diodeinc/registry/reference/ti/tps54331
        tps54331.zen
    analog/
      ltc3115/
        pcb.toml → inferred path: github.com/diodeinc/registry/reference/analog/ltc3115
        ltc3115.zen
```

### Git Tag Convention

Following Go: root packages use `v<version>`, nested packages use `<path>/v<version>`.

Tags for workspace members:
- `reference/ti/tps54331/v1.0.0`
- `reference/analog/ltc3115/v1.5.0`

Each package has independent version namespaces.

Tags for standalone packages:
- `v0.3.2` (e.g., stdlib at repository root)

### Dependency Resolution

Semantic versioning with Minimal Version Selection within semver families. Multiple families allowed to coexist.

**Semver Families:** For 0.x versions, the minor version acts as the major version:
- `0.2.x` and `0.3.x` are different families (breaking changes)
- For `1.x+`, standard major version grouping

**Algorithm:**

1. **Seed:** Discover members, preseed from lockfile, resolve branches/revs to pseudo-versions, initialize MVS state
2. **Discovery:** Iteratively fetch manifests (blob-filtered clones), add transitive deps, monotonically upgrade versions until fixed point
3. **Build Closure:** DFS from workspace roots using final selected versions (filters Phase 1 obsolete fetches)
4. **Fetch:** Download full repos (shallow clones), compute hashes, create `.pcbcache` markers
5. **Lock:** Merge hashes into `pcb.sum` (accumulate, never delete)

**Precedence Order:**
For each module path, version selection follows:
1. Workspace `[patch]` (ultimate authority - applied during graph construction)
2. Lockfile pin (future - stores resolved pseudo-versions)
3. Workspace/member requirements
4. Transitive requirements (lowest priority)

**Example:**
```
Workspace members:
  WV0001: stdlib = "0.2.13"
  WV0002: stdlib = "0.3.2", ti/tps54331 = "1.0.0"
  WV0003: stdlib = "0.3.1"

Phase 0 - Seed:
  selected[("stdlib", "v0.2")] = 0.2.13
  selected[("stdlib", "v0.3")] = 0.3.2    (0.3.1 < 0.3.2, not added)
  selected[("ti/tps54331", "v1")] = 1.0.0
  workQ = [("stdlib", "v0.2"), ("stdlib", "v0.3"), ("ti/tps54331", "v1")]

Phase 1 - Discovery:
  Fetch stdlib@0.2.13: no remote deps
  Fetch stdlib@0.3.2: no remote deps  
  Fetch ti/tps54331@1.0.0: requires stdlib = "0.3.0"
    → selected[("stdlib", "v0.3")] = 0.3.2 (already higher, no upgrade)
  Fixed point reached

Phase 2 - Build Closure:
  buildSet = { stdlib@0.2.13, stdlib@0.3.2, ti/tps54331@1.0.0 }

Final: Both stdlib@0.2.13 and stdlib@0.3.2 in build (different families)
```

Deterministic and predictable. Manually computable by finding max in each semver family. Pseudo-versions from branches participate in MVS and can win over tags.

Semver compatibility guarantees justify simple greedy selection. Within a family, later releases are backward compatible. Major version bumps (or 0.x minor bumps) are breaking changes.

### Load Syntax

**Full URLs are strongly preferred.** This design adopts the Go philosophy that "the import path is the package identity":

1.  **Unambiguous Origin**: `github.com/diodeinc/stdlib` tells you exactly who owns the code and where it lives. While aliases like `@stdlib` work (auto-generated from dependencies), the full URL is always canonical.
2.  **Decentralized Namespace**: URLs are globally unique. There is no central registry handing out short names or gatekeeping the "standard" names. Any user can publish `github.com/user/utils` without conflict.
3.  **Zero-Config Readability**: A developer reading the code knows the dependencies without needing to see the build configuration.

```python
load("github.com/diodeinc/stdlib/properties.zen", "Layout")
load("github.com/diodeinc/stdlib/board_config.zen", "Board")

# Registry imports are explicit about manufacturer and part
SwitchingReg = Module("github.com/diodeinc/registry/reference/ti/tps54331/tps54331.zen")
```

**Auto-Generated Aliases**

Aliases are automatically generated from the last path segment of dependencies and assets. This provides convenient shortcuts without manual configuration.

```python
# With this in pcb.toml:
# [dependencies]
# "github.com/diodeinc/stdlib" = "0.4.4"
# "github.com/diodeinc/registry/reference/XAL7070-562MEx" = "0.1.0"
# [assets]
# "gitlab.com/kicad/libraries/kicad-symbols" = "9.0.3"

# These aliases are automatically available:
load("@stdlib/properties.zen", "Layout")  # → github.com/diodeinc/stdlib
load("@XAL7070-562MEx/XAL7070-562MEx.zen", "XAL7070")  # → github.com/.../XAL7070-562MEx
Symbol("@kicad-symbols/Device.kicad_sym:R")  # → gitlab.com/.../kicad-symbols
```

Aliases are only generated for unique last path segments (no collisions). If two dependencies end with the same name, neither gets an alias.

Longest prefix matching: `github.com/diodeinc/stdlib` declared at `0.3.2` matches loading `github.com/diodeinc/stdlib/generics/Capacitor.zen`. Don't declare every subdirectory separately.

Relative paths within packages still work:
```python
load("./config.zen", "config_unit")
load("./units.zen", "Capacitance")
```

### Auto-Discovery

During `pcb build`, the toolchain automatically scans `.zen` files and adds missing dependencies to `pcb.toml`. This runs before dependency resolution (Phase -1).

**Resolution Order:**
1.  **Known aliases**: Common aliases like `@stdlib`, `@kicad-symbols` are resolved to known URLs and default versions
2.  **Workspace members**: URL imports matching workspace member paths use the member's published version
3.  **Lockfile entries**: If a matching entry exists in `pcb.sum`, use that version (fast path, no git)
4.  **Remote discovery**: Probe git tags to find the package and its latest version (slow path, cached per repo)

**Mechanism:**
1.  AST parser extracts import paths from `load()` statements and string literals (for `Module()`, `Symbol()`, etc.)
2.  For each import, tries resolution in the order above
3.  Updates `pcb.toml` with new dependencies
4.  Also corrects versions for workspace member dependencies to match their published versions

**Example Output:**
```bash
$ pcb build
V2 Dependency Resolution
Workspace root: /path/to/workspace

Phase -1: Auto-detecting dependencies from .zen files
  Auto-added 3 dependencies across 2 package(s)
  Discovered 1 remote package(s) via git tags
  Corrected 2 workspace member version(s)
```

Unknown aliases or unresolvable URLs are reported as warnings but don't fail the build.

### Workspace

Coordinate packages in monorepo:

```toml
[workspace]
resolver = "2"
repository = "github.com/myorg/myrepo"
members = ["boards/*", "components/*"]
default-board = "WV0002"

[access]
allow = ["*@weaverobots.com"]

[vendor]
directory = "vendor"
match = ["*"]
```

**Default member patterns:** If `members` is not specified, the default patterns are:
- `components/*`
- `reference/*`
- `modules/*`
- `boards/*`

Each member package declares its own dependencies:
```toml
[package]
pcb-version = "0.3"

[board]
name = "WV0002"
path = "WV0002.zen"

[dependencies]
"github.com/diodeinc/stdlib" = "0.3"
"github.com/diodeinc/registry/reference/ti/tps54331" = "1.0"
```

MVS automatically coordinates versions across the workspace. If WV0002 depends on `stdlib@0.3.2` and WV0003 depends on `stdlib@0.3.4`, MVS selects `0.3.4` (highest within the v0.3 family) for both.

### Patches

Patches provide Go-style `replace` semantics for local development. They override module sources **during graph construction** (not after MVS), allowing the patched module's dependencies to participate in resolution.

**Scope:** Workspace root only. Patches in package-level pcb.toml files are forbidden.

```toml
[workspace]
members = ["boards/*"]

[patch]
# Local path override (most common)
"github.com/diodeinc/stdlib" = { path = "../stdlib" }

# Git revision override
"github.com/diodeinc/registry/reference/ti/tps54331" = { git = "https://github.com/me/fork", rev = "abcd1234" }
```

**Semantics:**
- Patches are applied **during graph construction**, before MVS completes
- The patched module's `pcb.toml` is loaded, and its dependencies feed into MVS
- Version identity remains (e.g., stdlib@0.3.13), but code comes from patched source
- Ultimate authority: patches override all version constraints

**Use case:** Develop changes spanning multiple repos without creating temporary tags or modifying lockfiles. Remove patches when done to return to normal resolution.

### Canonical Package Format

Deterministic GNU tar with normalized metadata:
- `mtime=0`, `uid=0`, `gid=0`, `mode=0644`
- Username/groupname set to empty strings
- Lexicographic ordering of entries
- Regular files only (directories implicit from paths)
- Respects `.gitignore` via the `ignore` crate
- Filters internal markers (`.pcbcache`)
- Excludes nested packages (subdirectories with their own `pcb.toml`)

Streamed directly to BLAKE3 hasher (no buffering). Same commit produces identical hash regardless of machine or remote.

Debug: `pcb package <dir> [-o output.tar] [-v]`

The `-v` flag shows all files that would be included in the package.

### Lockfile

`pcb.sum` uses Go's format (accumulate, never auto-delete):
```
github.com/diodeinc/stdlib v0.3.2 h1:sL5Wum7w69ati4f0ExSvRMgfk8kD8MoW0neD6yS94Yo=
github.com/diodeinc/stdlib v0.3.2/pcb.toml h1:abc123def456...
```

Two lines per dependency: content hash (canonical tar BLAKE3) and manifest hash (pcb.toml BLAKE3). Asset packages get content hash only. Pseudo-versions include full commit hash. Cache markers (`~/.pcb/cache/{path}/{version}/.pcbcache`) store both hashes and avoid re-computation. Generated on first build, merged on updates, verified on subsequent builds. Commit to version control.

### Vendoring

Vendoring allows checking dependencies into source control for hermetic builds (zero network dependence) or auditing.

**Configuration:**
Controlled via top-level `[vendor]` in `pcb.toml`. Typically defined at the workspace root.

```toml
[vendor]
directory = "vendor"
# List of package prefixes to vendor. Empty list or "*" vendors everything.
match = ["github.com/diodeinc/registry/reference/ti"]
```

**Command:**
`pcb vendor` populates the `vendor/` directory in the workspace root based strictly on `pcb.sum` versions and the configuration. It does not evaluate `.zen` files.

```bash
$ pcb vendor
# Populates vendor/github.com/diodeinc/registry/reference/ti/...
```

**Resolution Priority:**
1.  **Workspace Vendor:** `vendor/` (if present and checksum matches `pcb.sum`)
2.  **Global Cache:** `~/.cache/pcb/git/`
3.  **Network:** Fetch from upstream

This enables workflows where specific critical dependencies are committed to the repo while others remain external.

### Standalone Scripts

Standalone scripts are single-file designs that declare their own dependencies inline. By convention, they include a shebang to be directly executable.

```python
#!/usr/bin/env pcb build
#
# ```pcb
# [package]
# pcb-version = "0.3"
#
# [dependencies]
# "github.com/diodeinc/stdlib" = "0.3"
# ```

load("github.com/diodeinc/stdlib/units.zen", "Voltage")
load("github.com/diodeinc/stdlib/generics/Capacitor.zen", "Capacitor")

v3v3 = Power("3v3")
gnd = Ground("gnd")

Capacitor(name="C1", P1=v3v3, P2=gnd, value="10uF")
```

Parser extracts comment block as inline manifest. It supports the full `pcb.toml` specification, but typically only `pcb-version` and `[dependencies]` are used.

**Reproducibility:**
To ensure consistent execution across machines, a lockfile is automatically generated in a hidden subdirectory relative to the script: `.pcb/<script_name>.zen.sum`.

For a script named `experiment.zen`, the lockfile is `experiment.zen` -> `.pcb/experiment.zen.sum`. This maintains the same strict versioning guarantees as full workspace projects.

## Case Study: Weave

### Current Structure

```
weave/
  pcb.toml                               # Workspace
  boards/
    WV0001/pcb.toml, WV0001.zen
    WV0002/pcb.toml, WV0002.zen
    WV0003/pcb.toml, WV0003.zen
  components/XT30PB/...
  graphics/logos/...
  vendor/
    github.com/diodeinc/stdlib/v0.2.13/
    github.com/diodeinc/stdlib/v0.3.2/
    github.com/diodeinc/registry/v0.3.1/
```

Workspace root defines defaults, boards override:
```toml
[workspace]
name = "test"
[packages]
registry = "@github/diodeinc/registry:v0.1.1"
stdlib = "@github/diodeinc/stdlib:v0.2.8"
```

WV0002 overrides:
```toml
[board]
name = "WV0002"
[packages]
registry = "@github/diodeinc/registry:v0.3.1"
stdlib = "@github/diodeinc/stdlib:v0.3.2"
```

Load syntax:
```python
load("@stdlib/properties.zen", "Layout")
load("@stdlib/board_config.zen", "Board", "CopperWeight")
load("@stdlib/interfaces.zen", "Power", "Ground", "Can", "Usb2")

SwitchingReg = Module("@registry/common/regulator-switching.zen")
LedIndicator = Module("@registry/modules/basic/LedIndicator.zen")
Capacitor = Module("@stdlib/generics/Capacitor.zen")
```

### Proposed Structure

```
weave/
  pcb.toml                               # Workspace (new format)
  pcb.sum                                # Lockfile (new)
  boards/
    WV0002/pcb.toml, WV0002.zen
  components/XT30PB/...
  graphics/logos/...
```

Workspace root:
```toml
[workspace]
resolver = "2"
pcb-version = "0.3"
members = ["boards/*"]
default-board = "WV0002"

[access]
allow = ["*@weaverobots.com"]
```

WV0002 declares dependencies:
```toml
[workspace]
resolver = "2"
pcb-version = "0.3"

[package]

[board]
name = "WV0002"
path = "WV0002.zen"
description = "Power Regulator Board"

[dependencies]
"github.com/diodeinc/stdlib" = "0.3"
"github.com/diodeinc/registry/reference/ti/tps54331" = "1.0"
"github.com/diodeinc/registry/reference/analog/ltc3115" = "1.5"
```

Load statements support auto-generated aliases or full URLs:
```python
load("@stdlib/properties.zen", "Layout")  # Auto-alias from "github.com/diodeinc/stdlib"
# or
load("github.com/diodeinc/stdlib/properties.zen", "Layout")  # Full canonical URL
```

Lockfile generated first build:
```
github.com/diodeinc/stdlib v0.3.2 h1:abc123...
github.com/diodeinc/stdlib v0.3.2/pcb.toml h1:def456...
github.com/diodeinc/registry/reference/ti/tps54331 v1.0.0 h1:ghi789...
github.com/diodeinc/registry/reference/ti/tps54331 v1.0.0/pcb.toml h1:jkl012...
github.com/diodeinc/registry/reference/analog/ltc3115 v1.5.0 h1:mno345...
github.com/diodeinc/registry/reference/analog/ltc3115 v1.5.0/pcb.toml h1:pqr678...
```

## Workflows

### Initial Build

```bash
$ cd weave
$ pcb build boards/WV0002

Resolving dependencies...
  Fetching github.com/diodeinc/stdlib@v0.3.2
  Fetching github.com/diodeinc/registry/reference/ti/tps54331@v1.0.0
  Fetching github.com/diodeinc/registry/reference/analog/ltc3115@v1.5.0
  Verifying checksums...

Updating pcb.sum...
  Added github.com/diodeinc/stdlib v0.3.2
  Added github.com/diodeinc/registry/reference/ti/tps54331 v1.0.0
  Added github.com/diodeinc/registry/reference/analog/ltc3115 v1.5.0

Evaluating WV0002.zen...
Building board WV0002...
Build succeeded.
```

Lockfile generated automatically. Subsequent builds verify against lockfile:

```bash
$ pcb build boards/WV0002

Resolving dependencies...
  github.com/diodeinc/stdlib@v0.3.2 (cached)
  github.com/diodeinc/registry/reference/ti/tps54331@v1.0.0 (cached)
  github.com/diodeinc/registry/reference/analog/ltc3115@v1.5.0 (cached)
  Verifying checksums... ✓

Evaluating WV0002.zen...
Build succeeded.
```

### Upgrading Dependencies

Workspace-level upgrade:
```bash
$ pcb upgrade --dependencies

Checking for updates...
  github.com/diodeinc/stdlib: 0.3.2 → 0.3.4 available
  github.com/diodeinc/registry/reference/ti/tps54331: 1.0.0 → 1.0.1 available
  github.com/diodeinc/registry/reference/analog/ltc3115: 1.5.0 → 1.5.2 available

Upgrade to latest minor versions? [Y/n]: y

Updating pcb.toml...
  "github.com/diodeinc/stdlib" = "0.3.4"
  "github.com/diodeinc/registry/reference/ti/tps54331" = "1.0.1"
  "github.com/diodeinc/registry/reference/analog/ltc3115" = "1.5.2"

Resolving dependencies...
  Fetching github.com/diodeinc/stdlib@v0.3.4
  Fetching github.com/diodeinc/registry/reference/ti/tps54331@v1.0.1
  Fetching github.com/diodeinc/registry/reference/analog/ltc3115@v1.5.2

Building all workspace boards...
  WV0001... ✓
  WV0002... ✓
  WV0003... ✓
  WV0004... ✓
  WV0005... ✓

All boards built successfully.
Updating pcb.sum...

Changes: pcb.toml, pcb.sum
Commit to lock versions.
```

Specific dependency:
```bash
$ pcb upgrade --dependencies github.com/diodeinc/stdlib

Current: 0.3.2
Available:
  0.3.3 (2024-10-15)
  0.3.4 (2024-11-01)

Select [0.3.4]: 0.3.4

Updating pcb.toml...
Resolving dependencies...
Testing builds... ✓
Done. Commit changes.
```

### Local Development

Developing changes spanning stdlib and weave:

```bash
$ cd ~/src/weave
$ cat >> pcb.toml << 'EOF'

[patch]
"github.com/diodeinc/stdlib" = { path = "../stdlib" }
EOF

$ pcb build boards/WV0002

Resolving dependencies...
  github.com/diodeinc/stdlib@v0.3.2 → ../stdlib (patched)
  github.com/diodeinc/registry/reference/ti/tps54331@v1.0.0 (cached)
  github.com/diodeinc/registry/reference/analog/ltc3115@v1.5.0 (cached)

Evaluating WV0002.zen...
Build succeeded.
```

Changes in `../stdlib` reflected immediately. No temp tags, no lockfile modifications. Remove patch when done.

### Adding Dependencies

```bash
$ cd boards/WV0002
$ cat >> pcb.toml << 'EOF'

[dependencies]
"github.com/diodeinc/stdlib" = "0.3"
"github.com/diodeinc/registry/reference/ti/tps54331" = "1.0"
"github.com/diodeinc/registry/reference/analog/ltc3115" = "1.5"
"github.com/externalorg/custom-sensors" = "1.0"
EOF

$ pcb build

Resolving dependencies...
  Fetching github.com/externalorg/custom-sensors@v1.0.0
  Verifying checksums...

Updating pcb.sum...
  Added github.com/externalorg/custom-sensors v1.0.0

Evaluating WV0002.zen...
Build succeeded.
```

New dependency and hash added to `pcb.sum`. Commit both files.

### Multiple Major Versions

Gradual migration when stdlib releases breaking v1.0.0:

```toml
# WV0001 (unmigrated)
[dependencies]
"github.com/diodeinc/stdlib" = "0.3"

# WV0002 (migrated)
[dependencies]
"github.com/diodeinc/stdlib" = "1.0"
```

Build includes both `stdlib@0.3.x` and `stdlib@1.0.x`. WV0002 uses new API, WV0001 uses old. MVS resolves each family independently. Migrate board-by-board over weeks/months.

## Design Comparisons

### Go Modules

**Adopted:**
- Repository URLs as package identifiers (no naming conflicts)
- `v<version>` and `<path>/v<version>` tag convention
- Minimal Version Selection (deterministic, predictable)
- go.sum format (content + manifest hashes)

**Diverged:**
- Multiple major versions allowed (Go forbids)
- Interpreted language with isolated contexts makes this feasible

### Cargo

**Adopted:**
- Manifest structure (`[package]`, `[board]` like `[[bin]]`)
- Workspace for monorepo coordination
- `[patch]` for local development

**Diverged:**
- No dependency inheritance (each package declares its own deps, MVS coordinates versions)
- MVS instead of constraint solver (simpler, sufficient for smaller ecosystem)
- Git-based, no registry

## Implementation

### Resolution Phases

`pcb build` runs the following phases for V2 workspaces:

#### Phase 1: Auto-Discovery

Scans `.zen` files for imports and automatically adds missing dependencies to `pcb.toml` files.

- Parses AST to extract import paths from `load()` statements and string literals
- Resolves imports via: known aliases → workspace members → lockfile → remote discovery
- Updates `pcb.toml` files with new dependencies
- Corrects versions for workspace member dependencies

This enables a "code-first" workflow where developers write imports and the toolchain manages manifests.

#### Phase 2: Seed MVS State

Initializes Minimal Version Selection state from workspace package dependencies.

- Discovers workspace member packages via glob patterns
- Preseeds from lockfile entries (avoids git operations for known versions)
- Resolves branch/rev specs to pseudo-versions
- Validates patches are only at workspace root

#### Phase 3: Parallel Dependency Resolution

Wave-based parallel fetching with MVS:

1. **Collect Wave**: Gather packages from work queue not yet fetched
2. **Parallel Fetch**: Fetch manifests using sparse checkout with `--filter=blob:none`
3. **Process Results**: Add transitive dependencies to MVS state (monotonic upgrade)
4. **Repeat**: Until work queue is empty (fixed point reached)

Each wave can fetch multiple packages in parallel. MVS updates are done sequentially to maintain determinism.

#### Phase 4: Build Closure

Computes the minimal set of packages needed for the build.

- DFS from workspace package dependencies using final selected versions
- Filters out obsolete fetches (versions that were upgraded during resolution)
- Produces a cargo-tree style dependency visualization

#### Phase 5: Asset Fetching

Fetches asset repositories (KiCad libraries, etc.).

- Collects assets from workspace packages and transitive manifests
- Fetches via sparse checkout (assets are leaf nodes, no transitive deps)
- Computes content hashes for lockfile

#### Phase 6: Lockfile Update

Updates `pcb.sum` with cryptographic hashes.

- Merges new entries with existing lockfile (accumulate, never delete)
- Verifies cached entries against lockfile hashes
- Only writes to disk if new entries were added

The entire resolution is deterministic, monotonic (no backtracking), and incremental via lockfile preseeding and cache markers.

### Git Operations

**Cache Structure:** `~/.pcb/cache/{full-module-path}/{version}/` contains package contents directly at root (no nested path redundancy) + `.pcbcache` marker storing content and manifest hashes for verification. Temp bare repos in `~/.pcb/cache/temp/` for pseudo-version generation.

Examples:
- Root package: `~/.pcb/cache/github.com/diodeinc/stdlib/0.3.2/`
- Nested package: `~/.pcb/cache/github.com/diodeinc/registry/reference/ti/tps54331/1.0.0/`
- Asset package: `~/.pcb/cache/gitlab.com/kicad/libraries/kicad-symbols/9.0.3/`

Note: Version directories use the version string without `v` prefix (e.g., `0.3.2` not `v0.3.2`).

**Repository Boundary Detection:**
- **GitHub:** 3-segment split: `github.com/user/repo` is the repository, everything after is subpath
  - Example: `github.com/diodeinc/registry/components/2N7002` → repo: `github.com/diodeinc/registry`, subpath: `components/2N7002`
- **GitLab:** Entire path is the repository (supports nested groups natively)
  - Example: `gitlab.com/kicad/libraries/kicad-symbols` → repo: `gitlab.com/kicad/libraries/kicad-symbols`

**Tag Resolution:** 
- Root packages: `refs/tags/v{version}` (e.g., `v0.3.2`)
- Nested packages: `refs/tags/{subpath}/v{version}` (e.g., `components/2N7002/v1.0.0`)
- Fallback for non-prefixed tags: tries `refs/tags/{version}` or `refs/tags/{subpath}/{version}` (KiCad compatibility)

**Sparse-Checkout for Nested Packages:**
1. Clone with `--filter=blob:none` (promisor remote with `remote.origin.promisor=true`)
2. Configure sparse-checkout to fetch only the subpath
3. Use `git reset --hard FETCH_HEAD` to materialize blobs (required for filtered clones)
4. Move contents from `subpath_dir/` to cache root, delete empty parent dirs (eliminates path redundancy)

**Fetch Strategy:**
1. Try HTTPS with v-prefixed tag
2. On failure, switch remote to SSH and retry
3. If SSH fails and not a pseudo-version, try without v-prefix (handles KiCad-style tags)

**Pseudo-versions:** `git ls-remote` → `git describe` → `v<base+1>-0.<timestamp>-<commit>` (40-char hash)

**Optimizations:** Sparse-checkout (bandwidth), .pcbcache markers (skip re-hash), lockfile preseeding (skip git ls-remote), .no-manifest markers (skip asset package re-fetch)

### Load Resolution

1. Expand built-in aliases (`@stdlib` → `github.com/diodeinc/stdlib`)
2. Longest prefix match against declared dependencies
3. Resolve version/location (apply patches if present)
4. Load from cache

Requires Starlark evaluation hook. Trie structure for efficient prefix matching.

### Backward Compatibility

Old LoadSpec syntax still works:
```python
load("@github/diodeinc/stdlib:v0.3.2/units.zen", "Voltage")
```

System extracts repo/version/path, creates implicit dependency. Deprecation warning:
```
Warning: Old-style load syntax in WV0002.zen:19
Replace with: load("github.com/diodeinc/stdlib/units.zen", ...)
And declare: "github.com/diodeinc/stdlib" = "0.3.2"
Run 'pcb migrate' to update project.
```

### Migration Tool

The `pcb migrate` command converts V1 workspaces to V2 format:

```bash
$ pcb migrate [PATHS]

Step 1: Detecting workspace root
  Workspace root: /path/to/workspace

Step 2: Detecting git repository
  Repository: github.com/myorg/myrepo

Step 3: Converting pcb.toml files to V2
  Converted 5 pcb.toml files

Step 4: Discovering .zen files
  Found 12 .zen files

Step 5: Running codemods on .zen files
  ✓ WV0002.zen
  ✓ WV0003.zen
  ...

✓ Migration complete
  Review changes with: git diff
  Run build to verify: pcb build
```

**Codemods Applied:**

1. **RemoveDirectoryLoads** - Removes directory `load()` statements (V2 requires file paths)
2. **WorkspacePaths** - Converts paths like `./components/foo.zen` to full URLs
3. **EscapePaths** - Escapes special characters in paths
4. **AliasExpansion** - Expands `@alias/path` to full URLs
5. **PathCorrection** - Fixes relative paths to use workspace-relative URLs

**Manifest Conversion:**

- Adds `resolver = "2"` to workspace root
- Adds `repository` field from git remote
- Converts `[packages]` to `[dependencies]`
- Removes V1-only fields (`[module]`, `[board]` from workspace root)

Conservative migration - preserves behavior while modernizing syntax. Review with `git diff` before committing.

## Adoption

**Phase 1 - Compatibility:** Both syntaxes work. Old LoadSpecs create implicit dependencies. Mixed usage supported. Incremental adoption.

**Phase 2 - Migration:** `pcb migrate` available. Docs/examples use new syntax. Old syntax still works.

**Phase 3 - Deprecation:** Warnings (not errors) on old syntax. Appear in CI.

**Phase 4 - Removal:** Future edition removes old syntax support. Projects stay on older toolchain until ready.

## Future Work

### Module Proxy

While Git repositories work for distribution, a dedicated module proxy (similar to `GOPROXY`) optimizes reliability and performance.

-   **Performance:** Proxies serve optimized `.zip` or `.tar.gz` archives, which are significantly faster to download than full Git clones or even shallow fetches.
-   **Reliability:** A proxy insulates the ecosystem from GitHub outages or deleted repositories. "Left-pad" scenarios are prevented because the proxy caches content indefinitely.
-   **Privacy:** Corporations can run private proxies (like Artifactory/Athens) to cache internal modules and audit external dependencies.

Mechanism: `pcb` tool checks `PCB_PROXY` environment variable. If set, it requests metadata and archives from the proxy API instead of speaking Git protocol to the origin.

### Bundled Standard Library

Currently, `stdlib` is treated as just another external dependency. Future versions should bundle the standard library with the `pcb` toolchain, matching the approach of virtually every major language (Go, Rust, Python).

**Reasons:**

1.  **Tight Coupling & Stability:** Language features often require matching standard library support (e.g., a new intrinsic type in the runtime needs a corresponding wrapper in `stdlib`). Versioning them together guarantees compatibility and prevents "compiler too new for stdlib" errors.
2.  **Zero-Latency Start:** Users can build basic designs immediately after installing the toolchain without network requests. "Hello World" should not require a `git clone`.

**Implementation:**
The `stdlib` would be embedded in the `pcb` binary or distributed in an adjacent `lib/` directory. Imports might simplify to `load("std/units.zen", ...)` or remain virtualized URLs that resolve to local storage.

## Publishing & Release Workflow

### `pcb publish`

Discovers dirty/unpublished packages in a V2 workspace and publishes them by creating annotated git tags with content and manifest hashes.

```bash
# Publish all dirty packages in workspace
$ pcb publish

# Skip preflight checks (uncommitted changes, branch verification)
$ pcb publish --force

# Publish from a specific path
$ pcb publish ./path/to/workspace
```

### Dirty Detection

A package is considered **dirty** (needs publishing) if any of the following are true:

1. **Unpublished** - No matching version tag exists
2. **Uncommitted changes** - `git status` shows modifications in the package directory
3. **Legacy tag** - Published tag exists but lacks hash annotations (pre-V2 tag)
4. **Content changed** - Current content hash differs from the hash in the tag annotation
5. **Manifest changed** - Current `pcb.toml` hash differs from the hash in the tag annotation

Use `pcb info` to see the current dirty state of all packages in a workspace.

### Tag Annotation Format

Published tags are annotated with content and manifest hashes in `pcb.sum` format:

```
github.com/diodeinc/registry/reference/ti/tps54331 v1.0.0 h1:mIGycQL5u80O2Jx/...=
github.com/diodeinc/registry/reference/ti/tps54331 v1.0.0/pcb.toml h1:rxNJufX5oaa...=
```

This embeds verification hashes directly in the git tag, enabling:
- Dirty detection by comparing current vs published hashes
- Integrity verification without fetching package contents
- Reproducible builds via cryptographic content addressing

### Versioning Strategy

Version numbers are computed automatically:

- **Unpublished packages**: Start at `0.1.0`
- **Published 0.x packages**: Bump minor version (`0.3.2` → `0.4.0`)
- **Published 1.x+ packages**: Bump major version (`1.2.3` → `2.0.0`)

This follows the semver family convention where 0.x minor bumps are breaking changes.

### Multi-Wave Publishing

Packages with inter-dependencies are published in **waves** based on the dependency DAG:

1. **Wave 1**: Packages with no dirty dependencies are tagged first
2. **Dependency Bump**: After each wave, packages that depend on just-published packages have their `pcb.toml` updated with the new versions and a commit is created
3. **Wave 2+**: Now-dirty dependents become publishable
4. **Continue** until all packages are published

```bash
$ pcb publish
✓ on main @ abc1234

Wave 1:
2 package(s) to publish:
  reference/ti/tps54331: 0.1.0 (initial) [reference/ti/tps54331/v0.1.0]
  reference/analog/ltc3115: 0.1.0 (initial) [reference/analog/ltc3115/v0.1.0]
  patching: boards/power-supply/pcb.toml

Wave 2:
1 package(s) to publish:
  boards/power-supply: 0.1.0 (initial) [boards/power-supply/v0.1.0]

Push main branch and 3 tag(s) to origin? [y/N]:
```

All tags are created locally first, then pushed in a single batch after confirmation. If you decline, all local tags are deleted and any commits are reset.

### Workflow Steps

1. **Pre-flight Checks:**
   - Working directory is clean (no uncommitted changes)
   - Current branch is `main`
   - Branch is tracking a remote

2. **Discovery:**
   - Detects V2 workspace and all member packages (excludes boards)
   - Computes content hashes for dirty detection
   - Fetches tag annotations in batch for efficient comparison

3. **Wave Publishing:**
   - Identifies publishable packages (no dirty dependencies)
   - Computes next versions and tag names
   - Creates annotated tags locally with hash annotations
   - Updates `pcb.toml` of dependents and commits changes
   - Repeats until no more packages can be published

4. **Confirmation & Push:**
   - Displays summary of tags and commits
   - Prompts for confirmation before pushing
   - Pushes main branch (if commits were made) and all tags
   - If declined, rolls back: deletes local tags and resets to initial commit

### Board Packages

Packages with a `[board]` section are currently excluded from `pcb publish`. Board publishing support is planned for a future release.

### `pcb info`

Displays workspace and package information for V2 workspaces:

```bash
$ pcb info
Workspace
Root: /path/to/workspace
Repository: github.com/myorg/registry
Toolchain: pcb >= 0.3

Boards (2)
  WV0002 (v1.0.0) - boards/WV0002/WV0002.zen
    Power Regulator Board

Packages (3)
  tps54331 (v0.2.0) reference/ti/tps54331 (2 deps)
  ltc3115 (v0.1.0)* reference/analog/ltc3115 (1 deps)
  interfaces (unpublished) common/interfaces
```

The `*` indicator shows packages with unpublished changes (dirty state).

Options:
- `--format json` - Output as JSON for tooling integration

---

## Future Work

### Remaining V2 Tasks

- Board publishing support in `pcb publish`
- `pcb tidy` command to remove unused lockfile entries
- `pcb upgrade` command to upgrade dependencies to latest versions
- Offline mode improvements (`--offline` flag for air-gapped builds)
