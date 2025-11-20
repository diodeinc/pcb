# V2 Dependency Resolution Algorithm

## Overview

We use Go's MVS (Minimal Version Selection) algorithm adapted for PCB packaging with semver 0.x support. The algorithm maintains a global "highest-required version per module family" and iteratively fetches manifests until reaching a fixed point.

## Key Concepts

### Module Lines (Semver Families)

Dependencies are grouped into **module lines** based on their semver family:

```
ModuleLine = (ModulePath, FamilyId)

FamilyId:
  - For v1.x+:  "v<major>"      (e.g., v1, v2, v3)
  - For v0.x:   "v0.<minor>"    (e.g., v0.2, v0.3)
```

**Examples:**
- `stdlib@0.3.1` → line: `("stdlib", "v0.3")`
- `stdlib@0.3.4` → same line (MVS picks max: 0.3.4)
- `stdlib@1.2.0` → line: `("stdlib", "v1")` (different family, coexists with 0.3)

### Version Normalization

All dependencies must become concrete versions before MVS:

- **Exact versions**: `"0.3.2"` → `Version(0, 3, 2)`
- **Branches**: `{ branch = "main" }` → resolve to commit, create pseudo-version
- **Revisions**: `{ rev = "abc123" }` → resolve to pseudo-version
- **Local paths**: excluded from MVS, handled separately

## Algorithm

### Phase 0: Seed from Workspace

**Input:** Workspace member packages + local path dependencies
**Output:** Initial `selected` map and `workQ`

```rust
selected: HashMap<ModuleLine, Version> = {}
workQ: Queue<ModuleLine> = []

// Process all local packages (workspace members + local path deps)
for pkg in localPackages {
    for dep in pkg.dependencies {
        if dep.is_local_path() {
            continue  // Already handled
        }
        
        version = normalize_version(dep)
        add_requirement(dep.url, version)
    }
}

fn add_requirement(path: String, version: Version) {
    let line = module_line(path, version)
    
    if selected[line].is_none() || version > selected[line] {
        selected[line] = version
        workQ.push(line)  // Need to fetch this version's pcb.toml
    }
}
```

**Example:**
```
BoardA: stdlib = "0.3.2"
BoardB: stdlib = "0.3.4", ti/tps54331 = "1.0.0"

Result:
  selected[("stdlib", "v0.3")] = 0.3.4
  selected[("ti/tps54331", "v1")] = 1.0.0
  workQ = [("stdlib", "v0.3"), ("ti/tps54331", "v1")]
```

### Phase 1: Discovery + MVS Fixed Point

**Core Property:** Monotonically raise versions (never lower). No backtracking.

```rust
manifestCache: HashMap<(ModuleLine, Version), Manifest> = {}

while let Some(line) = workQ.pop() {
    let version = selected[line]
    
    if manifestCache.contains((line, version)) {
        continue  // Already processed this exact version
    }
    
    // 1. Fetch manifest for this exact version
    let manifest = fetch_manifest_from_git(line.path, version)
    manifestCache.insert((line, version), manifest)
    
    // 2. Add its requirements
    for dep in manifest.dependencies {
        if dep.is_local_path() {
            continue
        }
        
        let dep_version = normalize_version(dep)
        add_requirement(dep.path, dep_version)
        // ↑ This may upgrade selected[dep_line] and add to workQ
    }
}
```

**Example Execution:**
```
Iteration 1: Process stdlib@0.3.4
  - Manifest has no remote deps
  - workQ = [("ti/tps54331", "v1")]

Iteration 2: Process ti/tps54331@1.0.0
  - Manifest declares: stdlib = "0.3.1"
  - add_requirement("stdlib", 0.3.1)
    → selected[("stdlib", "v0.3")] = 0.3.4 (already higher)
    → No change, don't re-queue
  - workQ = []

Fixed point reached:
  selected[("stdlib", "v0.3")] = 0.3.4
  selected[("ti/tps54331", "v1")] = 1.0.0
```

### Phase 2: Compute Final Build Closure

Phase 1 may have fetched obsolete versions. Walk the graph using **only** final selected versions.

```rust
buildSet: HashSet<(ModuleLine, Version)> = {}
stack: Vec<ModuleLine> = []

// Seed with remote deps from local packages
for pkg in localPackages {
    for dep in pkg.dependencies {
        if !dep.is_local_path() {
            let version = normalize_version(dep)
            let line = module_line(dep.path, version)
            stack.push(line)
        }
    }
}

// DFS using final selected versions
while let Some(line) = stack.pop() {
    let version = selected[line]
    
    if buildSet.contains((line, version)) {
        continue
    }
    buildSet.insert((line, version))
    
    let manifest = manifestCache[(line, version)]
    
    for dep in manifest.dependencies {
        if !dep.is_local_path() {
            let dep_version = normalize_version(dep)
            let dep_line = module_line(dep.path, dep_version)
            stack.push(dep_line)
        }
    }
}
```

**Output:** `buildSet` contains exactly the modules needed for the build, using MVS-selected versions.

## Properties

### Determinism
- For a given workspace + Git state, the result is always the same
- No search order dependency
- No backtracking or solver heuristics

### Monotonicity
- Versions only increase, never decrease
- `selected[line]` is write-once (or upgrade)

### Termination
- Finite number of versions in all manifests
- Each line upgrades at most N times (N = total unique versions mentioned)

### 0.x Support
- 0.2.x and 0.3.x are different families
- Multiple 0.x families can coexist

### Multi-Major Support
- stdlib v1 and stdlib v2 can coexist
- Enables gradual migration

## Example

### Workspace Structure
```
workspace/
  boards/
    WV0001/
      pcb.toml: stdlib = "0.2.13"
    WV0002/
      pcb.toml: stdlib = "0.3.2", ti/tps54331 = "1.0.0"
    WV0003/
      pcb.toml: stdlib = "0.3.1"
```

### Resolution Steps

**Phase 0 - Seed:**
```
WV0001: stdlib@0.2.13 → selected[("stdlib", "v0.2")] = 0.2.13
WV0002: stdlib@0.3.2  → selected[("stdlib", "v0.3")] = 0.3.2
        ti/tps54331@1.0.0 → selected[("ti/tps54331", "v1")] = 1.0.0
WV0003: stdlib@0.3.1  → selected[("stdlib", "v0.3")] = 0.3.2 (already higher)

workQ = [("stdlib", "v0.2"), ("stdlib", "v0.3"), ("ti/tps54331", "v1")]
```

**Phase 1 - Discovery:**
```
Process stdlib@0.2.13: (no deps)
Process stdlib@0.3.2: (no deps)
Process ti/tps54331@1.0.0:
  Declares: stdlib = "0.3.0"
  selected[("stdlib", "v0.3")] = 0.3.2 (already higher, no change)

Fixed point.
```

**Phase 2 - Build Closure:**
```
buildSet = {
  (("stdlib", "v0.2"), 0.2.13),
  (("stdlib", "v0.3"), 0.3.2),
  (("ti/tps54331", "v1"), 1.0.0)
}
```

**Final Workspace Graph:**
```
stdlib@v0.2.13  (for WV0001)
stdlib@v0.3.2   (for WV0002, WV0003, and ti/tps54331)
ti/tps54331@v1.0.0
```

## Implementation Notes

### Git Operations
- Fetch manifests using: `git clone --depth=1 --branch=v{version}`
- Cache in `~/.cache/pcb/git/{host}/{org}/{repo}/{version}/`
- For branches/revs: resolve to commit hash first, then normalize to pseudo-version

### Pseudo-Versions
- Format: `v0.0.0-{timestamp}-{commit_short}`
- Must be comparable within a family
- Store in lockfile for reproducibility

### Error Handling
- Missing version: error immediately
- Network failures: retry with exponential backoff
- Invalid pcb.toml: error with clear message
- Circular dependencies: detected in Phase 2 (buildSet revisits)

### Performance Optimizations
- Parallel manifest fetching during Phase 1
- Manifest cache persisted to disk
- Skip fetching if high version already selected

## Future Extensions

### Lockfile (pcb.sum)
Store for reproducibility:
```
github.com/diodeinc/stdlib v0.3.2 h1:abc123... (content hash)
github.com/diodeinc/stdlib v0.3.2/pcb.toml h1:def456... (manifest hash)
```

### Vendoring
After resolution, copy dependencies to `vendor/`:
```
vendor/
  github.com/diodeinc/stdlib/v0.3.2/
  github.com/diodeinc/registry/ti/tps54331/v1.0.0/
```

### Policy Enforcement
- Disallow multiple families of same module
- Forbid branches in transitive dependencies
- Require semantic versioning compliance

## References

- [Go Modules: Minimal Version Selection](https://research.swtch.com/vgo-principles)
- [Semantic Versioning 2.0.0](https://semver.org/)
- [Cargo Book: Dependency Resolution](https://doc.rust-lang.org/cargo/reference/resolver.html)
