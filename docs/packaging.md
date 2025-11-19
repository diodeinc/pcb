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

### Package Configuration

Unified `[package]` section replaces `[module]` and `[board]`:

```toml
[package]
pcb-version = "0.3"

[board]
name = "WV0002"
path = "WV0002.zen"
description = "Power Regulator Board"
```

`pcb-version` specifies the minimum compatible toolchain release series (e.g. `0.3` covers all `0.3.x` releases). It is used to indicate breaking changes in the language or standard library that require a newer compiler. No package name/version in manifest - derived from repository URL and Git tag following Go's model.

The optional `[board]` section specifies a `.zen` file that can be built as a standalone board with `pcb build`. Packages with or without `[board]` can be used as reusable modules via `Module()`.

### Dependencies

Full repository URLs as keys, no aliases:

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
- Branch: `{ branch = "main" }`
- Revision: `{ rev = "a1b2c3d4" }`
- Path: `{ path = "../local" }`

Full URLs eliminate ambiguity. No confusion about package origin.

### Asset Packages (Raw Git Repositories)

Dependencies that contain only assets (e.g., KiCad symbols, footprints, 3D models) and lack a `pcb.toml` manifest are supported as **Asset Packages**.

*   **Declaration:** Declared in `[dependencies]` like any other package.
*   **Resolution:** Treated as leaf nodes in the dependency graph (cannot have transitive dependencies).
*   **Lockfile:** `pcb.sum` records only the content hash. The manifest hash line is omitted.

```toml
[dependencies]
"gitlab.com/kicad/libraries/kicad-symbols" = "v7.0.0"
```

### Git Tag Convention

Following Go: root packages use `v<version>`, nested packages use `<path>/v<version>`.

Registry restructured for per-component versioning:
```
github.com/diodeinc/registry/
  reference/
    ti/
      tps54331/
        pcb.toml
        tps54331.zen
    analog/
      ltc3115/
        pcb.toml
        ltc3115.zen
```

Tags: `reference/ti/tps54331/v1.0.0`, `reference/analog/ltc3115/v1.5.0`. Independent version namespaces.

Stdlib remains a single package:
```
github.com/diodeinc/stdlib/
  pcb.toml
  board_config.zen
  interfaces.zen
  units.zen
  generics/
```

Tags: `v0.3.2`. Breaking changes in any part of stdlib require a major version bump for the whole package. This simplicity is acceptable for the core standard library, unlike the registry which must scale to thousands of independent components.

### Dependency Resolution

Semantic versioning with Minimal Version Selection within major versions. Multiple major versions allowed to coexist.

Algorithm:
1. Parse all `pcb.toml` files recursively
2. Group by `(url, major_version)` tuples
3. Select max version per group

Example from weave:
```
Workspace: stdlib@0.2.8
WV0001:    stdlib@0.2.13
WV0002:    stdlib@0.3.2
WV0003:    stdlib@0.3.1
```

Groups: `0.2.x` → select `0.2.13`, `0.3.x` → select `0.3.2`. Final build includes both `stdlib@0.2.13` and `stdlib@0.3.2`.

Deterministic and predictable. Manually computable by finding max in each major version group.

Semver compatibility guarantees justify simple greedy selection. Within major version, later releases are backward compatible. Major version bumps are breaking changes, so `1.8.0` can't satisfy `2.0.0` requirement.

### Load Syntax

**Full URLs are strongly preferred.** This design adopts the Go philosophy that "the import path is the package identity":

1.  **Unambiguous Origin**: `github.com/diodeinc/stdlib` tells you exactly who owns the code and where it lives. Aliases like `@utils` are ambiguous and require cross-referencing a configuration file to understand the dependency.
2.  **Decentralized Namespace**: URLs are globally unique. There is no central registry handing out short names or gatekeeping the "standard" names. Any user can publish `github.com/user/utils` without conflict.
3.  **Zero-Config Readability**: A developer reading the code knows the dependencies without needing to see the build configuration.

```python
load("github.com/diodeinc/stdlib/properties.zen", "Layout")
load("github.com/diodeinc/stdlib/board_config.zen", "Board")

# Registry imports are explicit about manufacturer and part
SwitchingReg = Module("github.com/diodeinc/registry/reference/ti/tps54331/tps54331.zen")
```

**Legacy Aliases (Discouraged)**

Aliases are supported primarily for backward compatibility and migration. New projects should avoid them.

```python
# Discouraged: requires alias mapping in pcb.toml or built-in knowledge
load("@stdlib/properties.zen", "Layout")

# Aliases also work in asset paths (passed to Component/Symbol), but use full URLs where possible:
# Symbol("@kicad-symbols/Device.kicad_sym:R")
```

Three built-in aliases remain for convenience during transition: `@stdlib`, `@kicad-symbols`, `@kicad-footprints`. These expand to their full URLs before resolution.

Longest prefix matching: `github.com/diodeinc/stdlib` declared at `0.3.2` matches loading `github.com/diodeinc/stdlib/generics/Capacitor.zen`. Don't declare every subdirectory separately.

Relative paths within packages still work:
```python
load("./config.zen", "config_unit")
load("./units.zen", "Capacitance")
```

### Auto-Discovery

Similar to Go, if a `load()` statement references a URL that is not declared in `pcb.toml`, the toolchain automatically attempts to resolve and add the dependency.

**Mechanism:**
1.  Parser extracts the import path: `github.com/user/repo/sub/module.zen`.
2.  If no matching dependency is found, it probes path prefixes to identify the repository root (e.g., `github.com/user/repo`).
3.  Fetches the latest stable release (semver).
4.  Updates `pcb.toml` with the new dependency and `pcb.sum` with the hashes.

**Example:**
User writes code without editing config:
```python
load("github.com/analog/lib/filter.zen", "Filter")
```

Running `pcb build`:
```bash
$ pcb build
Resolving dependencies...
  Auto-resolving github.com/analog/lib... found v1.2.0
  Adding dependency to pcb.toml
  Fetching github.com/analog/lib@v1.2.0...
```

This enables a "code-first" workflow where manifests are managed primarily by the tool.

### Workspace

Coordinate versions across packages in monorepo:

```toml
[workspace]
members = ["boards/*"]
default-board = "WV0002"
allow = ["*@weaverobots.com"]

[workspace.dependencies]
"github.com/diodeinc/stdlib" = "0.3"
"github.com/diodeinc/registry/reference/ti/tps54331" = "1.0"

[vendor]
directory = "vendor"
match = ["*"]
```

Member packages inherit:
```toml
[package]
pcb-version = "0.3"

[board]
name = "WV0002"
path = "WV0002.zen"

[dependencies]
"github.com/diodeinc/stdlib" = { workspace = true }
"github.com/diodeinc/registry/reference/ti/tps54331" = { workspace = true }
```

Override when needed:
```toml
[dependencies]
"github.com/diodeinc/stdlib" = { workspace = true }
"github.com/diodeinc/registry/reference/ti/tps54331" = "1.0.2"  # Override
```

Solves version coordination visible in current weave where different boards use incompatible versions with no central policy.

### Patches

Local development across repos without modifying configs or creating temp tags:

```toml
[workspace]
members = ["boards/*"]

[patch]
"github.com/diodeinc/stdlib" = { path = "../stdlib" }
"github.com/diodeinc/registry/reference/ti/tps54331" = { path = "../registry-ti-local" }
```

Workspace-root only. Affects all members. Redirects resolution temporarily. Different from path dependencies - patches override existing declarations, path deps are permanent intra-workspace references.

### Canonical Package Format

Deterministic USTAR tarball for cryptographic verification:
- Regular files and directories only (no symlinks, devices)
- Relative paths, forward slashes, lexicographic order
- Normalized metadata: mtime=0, uid=0, gid=0, uname="", gname=""
- File mode: 0644, directory mode: 0755
- End with two 512-byte zero blocks

Generation: checkout at tag, extract package path if nested, filter `.git` and ignored files, create normalized tarball. SHA-256 hash of tarball is content identifier. Identical package content produces identical hash across platforms.

### Lockfile

`pcb.sum` records cryptographic hashes:
```
github.com/diodeinc/stdlib v0.3.2 h1:abc123...
github.com/diodeinc/stdlib v0.3.2/pcb.toml h1:def456...
github.com/diodeinc/registry/reference/ti/tps54331 v1.0.0 h1:ghi789...
github.com/diodeinc/registry/reference/ti/tps54331 v1.0.0/pcb.toml h1:jkl012...
gitlab.com/kicad/libraries/kicad-symbols v7.0.0 h1:xyz789...
```

Two lines per standard dependency: full content hash and manifest-only hash. Asset packages (no `pcb.toml`) have only the content hash line. Manifest hash enables lightweight verification - fetch manifests, verify hashes, reconstruct graph, check if lockfile valid before downloading full content.

Generated on first build when absent. Updated automatically when dependencies change. Hash mismatch fails build with security warning. Must be committed to version control. `pcb.sum` is per-workspace and resides at the workspace root.

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
members = ["boards/*"]
default-board = "WV0002"

allow = ["*@weaverobots.com"]

[workspace.dependencies]
"github.com/diodeinc/stdlib" = "0.3"
"github.com/diodeinc/registry/reference/ti/tps54331" = "1.0"
"github.com/diodeinc/registry/reference/analog/ltc3115" = "1.5"
```

WV0002 inherits:
```toml
[package]
pcb-version = "0.3"

[board]
name = "WV0002"
path = "WV0002.zen"
description = "Power Regulator Board"

[dependencies]
"github.com/diodeinc/stdlib" = { workspace = true }
"github.com/diodeinc/registry/reference/ti/tps54331" = { workspace = true }
"github.com/diodeinc/registry/reference/analog/ltc3115" = { workspace = true }
```

Load statements backward compatible (aliases work) or use full URLs:
```python
load("@stdlib/properties.zen", "Layout")
# or
load("github.com/diodeinc/stdlib/properties.zen", "Layout")
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
"github.com/diodeinc/stdlib" = { workspace = true }
"github.com/diodeinc/registry/reference/ti/tps54331" = { workspace = true }
"github.com/diodeinc/registry/reference/analog/ltc3115" = { workspace = true }
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
# Workspace default
[workspace.dependencies]
"github.com/diodeinc/stdlib" = "0.3"

# WV0002 migrated
[dependencies]
"github.com/diodeinc/stdlib" = "1.0"
```

Build includes both `stdlib@0.3.x` and `stdlib@1.0.x`. WV0002 uses new API, other boards use old. Migrate board-by-board over weeks/months.

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
- Manifest structure (`[package]`, `[[board]]` like `[[bin]]`)
- Workspace with `{ workspace = true }` inheritance
- `[patch]` for local development

**Diverged:**
- MVS instead of constraint solver (simpler, sufficient for smaller ecosystem)
- Git-based, no registry

## Implementation

### Resolution Phase

Runs before evaluation:
1. Parse all `pcb.toml` files
2. Build complete dependency graph
3. Group by `(url, major_version)`
4. Select max version per group (MVS)
5. Verify against `pcb.sum`
6. Fetch missing packages
7. Compute canonical hashes
8. Update `pcb.sum`

Enables parallel fetching, reproducible builds.

### Git Operations

Cache structure:
```
~/.cache/pcb/git/
  github.com/
    diodeinc/
      stdlib/
        v0.3.2/
        v0.3.4/
      registry/
        ti/v1.0.0/
        analog/v1.5.0/
```

Shallow clones where possible. Sparse checkouts for monorepo packages (fetch only relevant subdirectory). Conservative cache invalidation - once verified, packages trusted.

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

```bash
$ pcb migrate

Analyzing workspace...
Found 6 boards
Detected dependencies:
  stdlib: v0.2.13, v0.3.1, v0.3.2
  registry: v0.1.28, v0.3.1, v0.3.8

Proposed workspace dependencies:
  "github.com/diodeinc/stdlib" = "0.3"  (satisfies 0.3.x boards)
  "github.com/diodeinc/registry/reference/ti/tps54331" = "1.0"
  "github.com/diodeinc/registry/reference/analog/ltc3115" = "1.5"
  "github.com/diodeinc/registry/..." = "..."

Board WV0001 uses stdlib@0.2.13 (incompatible with 0.3)
Keep multiple major versions in build? [Y/n]: y

Generating new pcb.toml files...
Rewriting load statements (optional)...
Creating pcb.sum lockfile...

Migration complete. Review with: git diff
```

Conservative - preserves behavior, modernizes syntax. Clean git diff.

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

### Current Limitations

The current workflow separates tagging and releasing into two phases: `pcb tag` (local build, git tag, push) triggers CI, which then runs `pcb release` (upload to storage). This split is brittle and board-specific. We need a unified command for both packages and boards that handles the entire lifecycle atomically.

### Proposed: `pcb publish`

A single command to validate, build, tag, and publish artifacts for both packages and boards.

```bash
# Publish a package (from root of repo)
$ pcb publish -v v1.0.0

# Publish a specific board
$ pcb publish -b WV0002 -v v1.0.0

# Publish a specific sub-package (monorepo)
$ pcb publish -p registry/reference/ti/tps54331 -v v1.0.0

# Dry run (validate and build only, skip tagging/upload)
$ pcb publish --dry-run -v v1.0.0
```

### Workflow Steps

1.  **Pre-flight Checks:**
    *   Working directory is clean (no uncommitted changes).
    *   Current branch is `main` (or matches config).
    *   Upstream is synchronized (no unpushed local commits).
    *   Version tag does not already exist locally or remotely.

2.  **Validation & Build:**
    *   **Package:** Runs `pcb build` (dry run) to verify dependency resolution and syntax. Runs tests.
    *   **Board:** Runs full layout generation and DRC/ERC checks.
    *   **Monorepo Package:** Verifies the isolated sub-package can resolve its own dependencies.

3.  **Packaging:**
    *   Creates the canonical artifact (tarball for packages, release bundle for boards).
    *   Computes checksums.
    *   *Stop here if `--dry-run` is set.*

4.  **Publishing (Atomic-ish):**
    *   **Git Tag:** Creates and pushes the git tag (e.g., `v1.0.0` or `reference/ti/tps54331/v1.0.0`).
    *   **Artifact Upload:** Uploads the release bundle to the storage server / registry.

By doing everything locally, we remove the CI dependency for the "release" step, treating CI purely as a verification gate for PRs. The publisher's machine (or a specialized release bot) is the source of truth.
