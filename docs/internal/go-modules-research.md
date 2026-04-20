# Go Modules: Research & Fact-Finding

This document is a fact-finding pass on Go's modern module system, aimed at
informing a redesign of pcb's `pcb.toml` / `pcb.sum` dependency management.
No design decisions are proposed here — only how Go works today and why.

Sources:

- Filippo Valsorda, *"go.sum: the Go checksum database lock file"* — https://words.filippo.io/gosum/
- *The Go Modules Reference* — https://go.dev/ref/mod
- *Managing dependencies* (Go docs) — https://go.dev/doc/modules/managing-dependencies
- *Using Go Modules* (Go blog) — https://go.dev/blog/using-go-modules
- Russ Cox, *"Minimal Version Selection"* — https://research.swtch.com/vgo-mvs

---

## 1. The headline claim: `go.sum` is NOT part of resolution

This is the single most important fact, and it's the one we'd been muddling.

From Filippo's post (paraphrased):

> `go.sum` is only a local cache for the Go Checksum Database. It's a map of
> module versions to their cryptographic hashes. It has **zero semantic
> effects on version resolution**.

Concretely:

- `go.sum` does **not** participate in MVS.
- `go.sum` does **not** tell you which versions a module actually uses.
- `go.sum` can (and usually does) contain entries for versions that are
  *not* in the selected build list — e.g. older versions that appeared
  during resolution but got superseded, versions of modules that were
  consulted but ultimately unused, etc.
- Parsing `go.sum` to reconstruct a dependency graph is **wrong**.
- `go.sum` exists only for cryptographic verification: when the Go command
  downloads a module, it checks the hash against `go.sum` (and, on first
  sight, against the public checksum database `sum.golang.org`).

So: `go.mod` is the ground truth for "what will be built." `go.sum` is a
tamper-evident seal on the bytes that get downloaded.

The mental model: **`go.mod` is manifest + lockfile. `go.sum` is a
security artifact.**

This is the direction we want `pcb.sum` to move: checksums only, no role
in MVS, no role in resolution.

---

## 2. What `go.mod` actually contains (post-1.17)

This is the other big thing we were getting wrong. `go.mod` in modern Go
is **not** just the direct dependencies — it contains the full set of
modules needed to build the main module.

### Directives

```go
module example.com/my/thing

go 1.22

require (
    example.com/direct-dep v1.2.3
)

require (
    example.com/transitive-a v1.0.0 // indirect
    example.com/transitive-b v2.4.1 // indirect
)

exclude example.com/bad v1.2.3
replace  example.com/foo v1.0.0 => example.com/fork-of-foo v1.0.1
replace  example.com/local => ../local
retract  [v1.9.0, v1.9.5]
```

- `require` — declares a *minimum* version of a module. This is the atom
  MVS operates on.
- `// indirect` — a comment marking a `require` line that the main module
  doesn't import directly but that's needed by some dependency.
- `exclude` — main-module-only; removes a specific version from MVS
  consideration (MVS will pick the next higher version required).
- `replace` — main-module-only; substitutes a module (or a specific
  version of a module) with something else (another module, a fork, a
  local path). Replace directives in dependencies are **ignored** — only
  the main module's replacements apply. This is deliberate: "who controls
  your build" is you.
- `retract` — the module author marks published versions as broken so
  tooling skips them.

### The 1.17 change — this is the critical shift

Before Go 1.17, `go.mod` listed only direct dependencies. To compute the
build list, the Go command had to walk the full transitive graph by
fetching every dependency's `go.mod`.

Starting with Go 1.17, **`go.mod` contains an explicit `require` line for
every module needed to build any package or test in the main module** —
direct and indirect. Indirect ones get the `// indirect` comment.

Consequences:

1. **`go.mod` is self-describing.** You can compute the build list from
   the main module's `go.mod` alone, without network access, without
   fetching any other module's `go.mod`. This is what we don't have
   today.
2. **Module graph pruning.** If all modules in the graph declare
   `go 1.17+`, MVS only walks immediate requirements of each dep, not
   their full transitive graphs. The main module's `go.mod` already
   pins everything that matters.
3. **Lazy module loading.** `go build` loads only the main `go.mod` and
   fetches additional `go.mod` files on demand when imports can't be
   resolved from what's already listed.
4. `go.mod` gets bigger — often much bigger — but the payoff is that the
   manifest is complete and builds are faster and hermetic-ish.

This is the model we want `pcb.toml` (or some successor) to move toward:
**the manifest contains the full resolved set, so resolution at build
time is just a lookup, not a graph walk.**

---

## 3. Minimal Version Selection (MVS), in one page

MVS is Russ Cox's algorithm. Its goals, in rough priority order:

1. **Reproducibility without a separate lockfile.** Given the same
   `go.mod` files across the graph, the build list is a deterministic
   function of them.
2. **High fidelity to what module authors tested.** You get the versions
   the authors *asked for*, not the newest versions available.
3. **Predictability.** No backtracking, no SAT-solver surprises, no
   "why did cargo pick this?".
4. **Polynomial-time, easy to explain.**

### The algorithm (build list)

For each module in the transitive graph, collect every version that any
`require` line asks for. For each module, take the **maximum** of those
minimums. That's the build list.

"Select the minimum version that satisfies all the maximum requirements."
Or more simply: *of all the floors requested, take the highest floor.*

Example:
- Main requires A v1.2 and B v1.1.
- A v1.2 requires C v1.3.
- B v1.1 requires C v1.4.
- Build list: A v1.2, B v1.1, **C v1.4** (the higher of the two floors).

### Why this avoids the npm/cargo footgun

npm, pip, cargo ask "what's the newest version that satisfies the
constraints?" That means a fresh `npm install` today can pick versions
that didn't exist when the library was last tested — *low fidelity*.
Lockfiles exist to freeze that choice, but libraries (not applications)
typically don't ship lockfiles, so downstream users get the drift back.

MVS asks "what's the oldest version everyone is willing to accept?" That
version is, by construction, one that *someone explicitly asked for* —
it was in some module's `go.mod`, which means it was at least
compile-tested at some point by that module's author. You only get a
newer version of C if some module in your graph actually bumped their
minimum.

The practical effect: **the build list moves forward only when someone
with skin in the game bumps a requirement.** No surprise upgrades on
Tuesday morning because upstream published v2.4.1.

### Other MVS operations

- **Upgrade all** (`go get -u ./...`): replace each requirement with the
  latest version, then recompute.
- **Upgrade one** (`go get foo@v1.5`): add/raise the floor for foo,
  recompute. Crucially, doing this may *raise* other floors but never
  lowers them — no accidental downgrades.
- **Downgrade one** (`go get foo@v1.2`): walk the graph backwards and
  remove/lower requirements that forced foo ≥ v1.2. Downgrades can
  *only* lower floors, never add new ones.

### Properties

- **No lockfile needed for reproducibility.** The build list is
  determined by the `go.mod` files in the graph.
- **No backtracking.** The problem is reducible to 2-SAT / Horn-SAT; it's
  linear-time.
- **Only the main module can `exclude` or `replace`.** Libraries cannot
  impose build-shaping constraints on their users. This prevents the
  Kubernetes-style "you must use this ancient yaml.v2" problem.

---

## 4. The user-facing workflow

Your instinct is correct: `go.mod` is **designed to be machine-written**.
Manual editing happens but is rare and mostly for `replace` and
`exclude`.

### The main commands

| Command | What it does | When to use it |
|---|---|---|
| `go mod init <path>` | Create a new `go.mod`. | Once, at project start. |
| `go get <mod>[@version]` | Add a module, or change its version. Updates `go.mod` and `go.sum`. May raise floors on other modules. | Adding a new dep, pinning, upgrading, downgrading. |
| `go get <mod>@none` | Remove a module from `require`. | Drop a dep. |
| `go get <mod>@latest` | Update to latest released version. | Routine upgrades. |
| `go get -u ./...` | Upgrade all transitive deps to their latest minor/patch. | Periodic "upgrade everything." |
| `go mod tidy` | Reconcile `go.mod` + `go.sum` against the actual imports in the source tree. Adds missing, removes unused, canonicalizes. | After any edit that changes imports. Also what CI typically checks. |
| `go mod download` | Pre-populate the module cache. | Caching, CI warmup. |
| `go mod vendor` | Write all deps into `vendor/`. | Offline builds, some corp setups. |
| `go list -m all` | Show the full build list. | Inspecting what will actually be built. |
| `go list -m -u all` | Same, but annotate available upgrades. | "What's out of date?" |

### `go get` is the primary "add" command

Idiomatic answer to your question: yes, `go get <mod>` is how you add a
dep. It does the resolution, writes to `go.mod`, writes hashes to
`go.sum`, and may adjust other requirements as MVS dictates.

There is no separate `go add`. (There was historical talk of renaming,
but `go get` is the command.) There is no `go update` — upgrades are
`go get <mod>@latest` or `go get -u ./...`.

### What happens if you just add an import and hit build?

Two cases, depending on the `-mod` setting. Since Go 1.16, the default
is `-mod=readonly`:

- **`-mod=readonly` (default)**: `go build` fails if the import would
  require editing `go.mod`. The error message tells you to run `go get`
  or `go mod tidy`. This is the modern, recommended mode — it keeps
  `go.mod` under explicit user control.
- **`-mod=mod`**: `go build` will itself resolve the missing module and
  edit `go.mod`/`go.sum` in place. This used to be the default
  pre-1.16. Some blog posts still describe this behavior — it's accurate
  history but not the modern default.

So the modern answer is: **adding an `import` line alone does not update
`go.mod`; you get an error pointing you at `go get` or `go mod tidy`.**

### `go get` vs `go mod tidy` — how they divide labor

- `go get` is *version-oriented*: "I want this module at this version."
  It reconciles `go.mod` to satisfy that demand, running MVS.
- `go mod tidy` is *import-oriented*: "make `go.mod` match the
  `import` statements in the source tree." It adds missing requires for
  imports that aren't yet listed, and removes requires for modules that
  aren't imported anywhere anymore. It walks *all* build tags and
  platforms, which is why it's slower.

A reasonable mental model:

- Adding / changing / pinning a specific dep → `go get`.
- "My code changed, make `go.mod` match reality" → `go mod tidy`.
- CI gates often run `go mod tidy && git diff --exit-code` to enforce
  that committed `go.mod` is in sync with the source.

### Is manual editing a thing?

Yes, but rare. The honest list of when people edit by hand:

- Adding/adjusting `replace` directives for local development (`replace
  example.com/foo => ../foo`).
- Adding `exclude` for a known-broken version.
- Resolving merge conflicts in `go.mod`.
- Bumping the `go 1.X` line.

After any manual edit, `go mod tidy` is the standard follow-up.

---

## 5. `go.sum`: what it is, what it isn't

Restating for clarity, because this is where we want to land:

### What `go.sum` is

- A text file with one line per `(module, version, hash-kind)`, e.g.:
  ```
  example.com/foo v1.2.3 h1:abcd...=
  example.com/foo v1.2.3/go.mod h1:wxyz...=
  ```
- A local cache of entries from the public Go checksum database
  (`sum.golang.org`).
- Consulted by the Go command every time it downloads a module: the
  downloaded bytes are hashed and compared to the `go.sum` entry. If
  they differ, the build fails with a security error.
- Updated by `go get` / `go mod tidy` whenever a new `(module, version)`
  pair enters the picture.

### What `go.sum` is **not**

- **Not a lockfile in the npm/cargo sense.** It does not pin versions.
  It does not determine the build list.
- **Not a dependency graph.** It typically contains more entries than
  the current build list — past resolutions leave sediment.
- **Not consulted by MVS.** MVS only looks at `go.mod` files.
- **Not the source of truth for "what version is used."** That's
  `go.mod` (modern, post-1.17) or `go list -m all` at runtime.

### Two hashes per module: `h1:` lines

For each `(module, version)` there are up to two lines:

1. `path version h1:…` — hash of the zipped module content.
2. `path version/go.mod h1:…` — hash of just that module's `go.mod`
   file.

The second exists because with lazy loading, the Go command sometimes
needs to read a dep's `go.mod` without downloading the full module.
Those reads still need to be verifiable.

### Role of `sum.golang.org`

The first time anyone in the world fetches a `(module, version)`, the
Go proxy asks the checksum DB what the canonical hash is. The DB is
append-only and transparency-logged (Merkle tree, à la Certificate
Transparency). `go.sum` is the local-project crystallization of those
answers. If a malicious proxy or a compromised module author tries to
serve different bytes later, it mismatches `go.sum` and the build fails.

Key mental model: **the security guarantee is about *what bytes*, not
*what version*. Version selection is `go.mod`'s job.**

---

## 6. Why Go's model fits what pcb wants

Listing the properties we want to inherit, without yet proposing how:

1. **Manifest is self-describing.** Build list is computable from the
   manifest alone. We don't have this today; `pcb.toml` is direct-only.
2. **Resolution happens at "add" time, not "build" time.** The user runs
   an explicit command (`go get`) that does the MVS computation and
   writes results back. Builds are fast and deterministic because they
   don't resolve.
3. **The sum file is a security artifact only.** Not load-bearing for
   resolution. Safe to regenerate; safe to diff; safe to audit.
4. **MVS gives reproducibility without a lockfile.** High-fidelity
   builds. No "surprise upgrade" class of bug.
5. **`replace` / `exclude` are main-module-only.** Libraries can't
   impose constraints on downstream.
6. **Tooling owns the manifest.** Manual editing is possible but rare;
   idiomatic flow is through commands.

---

## 7. Open questions to resolve in the design phase

Explicitly not answering these here — flagging them for later.

- Do we want a direct analogue of `go mod tidy` (reconcile manifest to
  source imports), or is pcb's import model different enough that the
  equivalent looks different?
- How do we represent the full transitive set in `pcb.toml` in a way
  that stays readable? (`// indirect` works for Go but TOML is
  structured differently.)
- Does pcb want MVS specifically, or some variant? MVS's "only upgrade
  when someone bumps a floor" property is especially valuable for
  hardware, where surprise upgrades are scarier than in software.
- How do `replace` / local-path overrides look for a hardware library
  ecosystem? Especially for local board-vendor kits under development.
- What's the pcb analogue of `sum.golang.org`? Do we want a central
  checksum authority, or is local-only `pcb.sum` sufficient for now?
- Migration: how do existing projects with a resolution-participating
  `pcb.sum` move to a security-only `pcb.sum`?
- Workspaces / multi-module repos (`go.work`) — is there a pcb analogue,
  and do we need to design for it now?
