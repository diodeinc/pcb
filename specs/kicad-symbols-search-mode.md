# KiCad Symbols Search Mode

Status: draft

## Purpose

Plan support for a new `pcb search` mode, `kicad:symbols`, backed by a second local SQLite index distinct from the existing registry package index.

This document captures:

- the current registry-index lifecycle and how `pcb search` uses it today
- the current understanding of the KiCad-symbols SQLite artifact produced in `../diode/projects/api/scripts/indexing/kicad-symbols`
- the major differences between the two local indexes
- a proposed implementation plan for adding `kicad:symbols` to both the non-interactive CLI and the interactive TUI

This draft now includes the intended high-level download/update policy for the KiCad-symbols consumer artifact, but it still leaves exact API field names and some UI details open until implementation planning is complete.

## Current Search Architecture

Today `pcb search` has three modes across two data sources:

- `registry:modules`
- `registry:components`
- `web:components`

The first two are backed by the same local registry SQLite database. The third uses online search APIs.

### Current Registry-Backed Modes

Current mode definitions:

- `registry:modules`
  - local SQLite search
  - excludes URLs under `github.com/diodeinc/registry/components`
- `registry:components`
  - local SQLite search
  - includes only URLs under `github.com/diodeinc/registry/components`
- `web:components`
  - online API search

The current code uses a simple mode-to-filter mapping:

- registry modes share one backend and differ only by URL-prefix filtering
- web mode uses a different worker path entirely

## Planned Search Architecture

Planned mode set:

- `registry:modules`
- `registry:components`
- `kicad:symbols`
- `web:components`

Planned data-source split:

- registry SQLite index
  - admin-download-only
  - readable locally once cached
- KiCad symbols SQLite index
  - downloadable by any authenticated user
  - readable locally once cached
- web component APIs
  - online search path

This means the existing assumption that “local SQLite mode” is equivalent to “registry access” must be removed from the design.

## Current Registry Index Lifecycle

### Artifact Identity

Current local cache path:

- `~/.pcb/registry/packages.db`

Current local version sidecar:

- `~/.pcb/registry/packages.db.version`

### Remote Source

Current API metadata endpoint:

- `{DIODE_API_URL or https://api.diode.computer}/api/registry/index`

That endpoint returns metadata including:

- `url`
- `sha256`
- `lastModified`
- `expiresAt`

The actual SQLite payload is then downloaded from `metadata.url`, which the current code and comments treat as an S3-backed object.

### Download / Install Behavior

Current registry download flow:

1. Fetch metadata from `/api/registry/index` using the authenticated bearer token.
2. Download the compressed DB from `metadata.url`.
3. Decompress with zstd.
4. Atomically write the result to `packages.db`.
5. Persist `sha256` to `packages.db.version` in the TUI/background path.

Important current behavior:

- synchronous `RegistryClient::open()` downloads the DB if missing
- TUI worker download path also persists the version sidecar
- the synchronous blocking download path does not currently persist the version sidecar

That means a first-use CLI-triggered download can create `packages.db` without `.db.version`, after which the TUI background updater will still work but will treat the local version as unknown until it rewrites the DB/version pair.

### Access Control / Fallback Behavior

Current TUI preflight logic:

1. If auth is missing or expired, only `web:components` is available.
2. If auth is present and `packages.db` already exists, all current modes are available.
3. If auth is present and `packages.db` is missing, the TUI calls `/api/registry/index` as a capability check.
4. If access is allowed, registry modes are enabled and the initial metadata object is passed into the worker to avoid a duplicate metadata request.
5. If access is forbidden, the TUI falls back to `web:components` only.

Notable consequence:

- cached registry DBs remain usable for non-admin users
- lack of download permission only blocks first acquisition / refresh, not local reads

## Planned Multi-Index Access Control Behavior

The KiCad-symbols index will differ from the registry index:

- registry index:
  - download access is restricted to admins
- KiCad-symbols index:
  - download access is available to any authenticated user

Therefore the future TUI/CLI preflight must reason about each local index independently.

Planned consequence:

- an authenticated non-admin user may have:
  - no registry index access
  - full KiCad-symbols index access
  - full web-component search access

So for non-admin authenticated users, `kicad:symbols` must remain available even when `registry:*` modes are not.

### Update Behavior

Current TUI worker behavior:

- if `packages.db` is missing, initial download is blocking before search becomes active
- once the DB is usable, the worker launches a background metadata check
- update freshness is determined by comparing remote `sha256` to local `.db.version`
- if stale, the worker downloads a replacement DB in the background
- DB reload is detected by file mtime and the open SQLite handle is replaced
- a command-palette action can force an update check / re-download

### Query / Retrieval Behavior

The registry DB is queried locally and read-only.

The search backend combines three ranking sources:

- trigram FTS over identifiers / normalized MPN-like terms
- word FTS over lexical text
- semantic vector search via `sqlite-vec`

Results are merged using Reciprocal Rank Fusion (RRF).

Current result detail flow:

- search list uses lightweight hits
- selected row triggers a secondary detail fetch by package id
- dependencies and dependents are fetched from a relational side table

Current registry-mode UX behavior:

- Enter copies the selected package URL to the clipboard
- registry-mode availability/pricing is fetched online in batch after local search results arrive

## Current Registry Index Shape

From current client usage, the registry DB exposes at least these logical tables/indexes:

- `packages`
  - canonical package rows
- `package_deps`
  - dependency graph edges
- `package_fts_ids`
  - identifier-oriented FTS index
- `package_fts_words`
  - lexical FTS index
- `package_vec`
  - semantic vector index

Fields currently read by `pcb search` include:

- package identity:
  - `id`
  - `url`
  - `version`
  - `package_category`
- component/package metadata:
  - `mpn`
  - `manufacturer`
  - `part_type`
  - `short_description`
  - `detailed_description`
- enrichment:
  - `digikey`
  - `edatasheet`
  - `image`

From the search client’s perspective, this DB is package-centric and URL-centric.

## KiCad Symbols Index Pipeline

Source: `../diode/projects/api/scripts/indexing/kicad-symbols`

The KiCad-symbols pipeline currently has seven phases:

1. `phase0`: discover/filter KiCad symbols from `.kicad_sym`
2. `phase1`: resolve datasheet URLs into cached source PDFs
3. `phase2`: ensure OCR markdown exists for the datasheet PDFs
4. `phase3`: extract search-oriented summary data from OCR
5. `phase4`: optional eDatasheet scaffold extraction
6. `phase5`: validate extracted manufacturer + MPNs via DigiKey
7. `phase6`: build the SQLite search index

The default production path is:

- `phase0 -> phase1 -> phase2 -> phase3 -> phase5 -> phase6`

`phase4` exists but is not part of the default flow.

The phase6 index is built from phase5 survivors only.

## KiCad Symbols Index Lifecycle Today

Current producer-side lifecycle in the sibling project:

- build local DB from the phase5 survivor set
- write local SQLite output
- for full runs without `--limit`, compress with zstd and upload the artifact to S3

Current phase6 defaults:

- local DB path:
  - `${TMPDIR:-/tmp}/kicad-symbols-index.db`
- override:
  - `KICAD_SYMBOLS_INDEX_DB_PATH`
- default S3 key for compressed artifact:
  - `index/kicad-symbols/symbols.db.zst`

Current README also states the compressed DB is uploaded to:

- `s3://components-689688455535/index/kicad-symbols/symbols.db.zst`

This is producer-side lifecycle only. Consumer-side download/install/update behavior inside `pcb search` is still TBD.

## Planned KiCad Symbols Consumer Lifecycle

The KiCad-symbols consumer flow should mirror the registry flow closely.

### Remote Source

The KiCad-symbols index will be discovered from the same API server endpoint family as the registry index:

- the API server returns metadata including a download URL
- the actual payload is a zstd-compressed SQLite database

The important difference is that KiCad symbols has its own endpoint.

Local investigation notes from the current localhost endpoint:

- current route:
  - `GET http://127.0.0.1:3001/api/symbols/kicad/index`
- current response returns:
  - `url`
  - `sha256`
  - `lastModified`
  - `expiresAt`
- the current localhost response uses the same `url` field name as the registry metadata
- the current localhost response now includes a non-empty `sha256`

Implication:

- the current metadata contract is now good enough to support a normal sidecar freshness check
- the client should treat `sha256` as an opaque version string

### Access Model

- any authenticated user can download the KiCad-symbols index
- auth is still required

### Download / Install Behavior

Planned flow:

1. fetch metadata from the API server with bearer auth
2. read the KiCad-symbols artifact URL from its dedicated response field
3. download the compressed SQLite payload
4. decompress with zstd
5. atomically write the local DB file
6. persist a local sidecar version/hash file

### Background Refresh Policy

Planned refresh behavior:

- refresh check should happen in the background on startup only
- not on every query
- not continuously during the session except for the startup-triggered check
- forced/manual refresh behavior can be decided later, but startup refresh is required

### Proposed Local Cache Path

Suggested cache location:

- `~/.pcb/kicad-symbols/symbols.db`

Suggested sidecar version file:

- `~/.pcb/kicad-symbols/symbols.db.version`

Rationale:

- mirrors the existing registry cache layout
- keeps the two SQLite sources clearly separate
- avoids overloading `~/.pcb/registry`
- gives us room for future source-specific metadata alongside the DB

If we later want to generalize local-search caches, we could migrate toward a common pattern like:

- `~/.pcb/search-indexes/registry/packages.db`
- `~/.pcb/search-indexes/kicad-symbols/symbols.db`

but the simpler source-specific directory is a good initial path.

### Sidecar Version / Hash File

A sidecar version/hash file is a small companion file stored next to the downloaded SQLite DB. In the current registry flow, this is the `.db.version` file.

Its purpose is:

- store a compact local marker for “which remote artifact do we have?”
- avoid hashing the entire SQLite file locally on every startup
- allow fast comparison against remote metadata such as:
  - `sha256`
  - version string
  - artifact revision id

Typical behavior:

1. metadata endpoint returns a stable artifact identity such as `sha256`
2. client writes that value to `symbols.db.version`
3. on next startup, client compares local sidecar value to remote metadata value
4. if equal, skip re-download
5. if different or missing, download a replacement DB

Why this is useful:

- cheaper than computing a whole-file hash locally every startup
- decouples freshness checks from SQLite internals
- easy to preserve across atomic DB replacement
- works even if the DB is large

Recommended approach for KiCad-symbols:

- use the remote artifact hash/version as the sidecar contents
- match the registry pattern unless there is a strong reason to diverge

Fallback if the metadata hash is blank:

- store the best available stable freshness token instead
- first choice:
  - non-empty `sha256`
- second choice:
  - `lastModified`
- third choice:
  - explicit server-side version field if added later

## KiCad Symbols SQLite Data Model

Unlike the current registry index, the KiCad-symbols DB is symbol-centric, not package-centric.

Live artifact inspected on 2026-03-18:

- compressed size:
  - about 121 MB
- decompressed size:
  - about 213 MB
- symbol rows:
  - 6,583
- matched MPN rows:
  - 11,719
- `symbol_fts_ids` rows:
  - 6,583
- `symbol_fts_words` rows:
  - 6,583
- `symbol_vec_rowids` rows:
  - 6,583
- rows with image:
  - 6,378
- rows without image:
  - 205
- rows with `phase4_edatasheet`:
  - 0
- rows with non-empty `phase5_digikey`:
  - 6,583

Client plan:

- ignore `phase5_digikey` for now
- if `image` is NULL, do not show an image preview

### Core Table: `symbols`

One canonical row per surviving symbol.

Columns:

- `id INTEGER PRIMARY KEY`
- `symbol_library TEXT NOT NULL`
- `symbol_name TEXT NOT NULL`
- `footprint_library TEXT NOT NULL`
- `footprint_name TEXT NOT NULL`
- `manufacturer TEXT NOT NULL`
- `datasheet_url TEXT`
- `datasheet_sha256 TEXT`
- `datasheet_source TEXT`
- `kicad_description TEXT`
- `kicad_keywords TEXT`
- `kicad_fp_filters TEXT`
- `phase3_description TEXT NOT NULL`
- `phase3_keywords TEXT NOT NULL`
- `phase4_edatasheet TEXT`
- `phase5_digikey TEXT`
- `image BLOB`

Observed semantics:

- `symbol_library` is derived from the `.kicad_sym` relative path basename
- `symbol_name` is the KiCad symbol name
- footprint identity is split from the KiCad `Library:Footprint` reference
- `manufacturer` comes from phase3 extraction and is required by the time a symbol survives phase5
- `phase4_edatasheet` and `phase5_digikey` are compact JSON strings when present
- `image` stores PNG bytes directly on the row
- multi-unit symbols remain in the DB but currently have `image = NULL`

Note:

- the current producer-side `phase6.ts` schema includes `phase5_digikey`
- if product direction is to ignore this field in `pcb search`, we can treat it as non-contractual implementation detail for now and avoid depending on it in the client UX

Additional live-artifact observations:

- every row currently has:
  - `datasheet_url`
  - `datasheet_sha256`
  - `phase3_description`
  - `phase3_keywords`
- almost every row currently has:
  - `kicad_keywords`
- current artifact has `datasheet_source = 'self'` for all rows

Identity constraint:

- unique index on `(symbol_library, symbol_name)`

Secondary index:

- plain index on `manufacturer`

### Child Table: `symbol_mpns`

One row per validated matched MPN for a symbol.

Columns:

- `symbol_id INTEGER NOT NULL REFERENCES symbols(id)`
- `mpn TEXT NOT NULL`
- `mpn_normalized TEXT NOT NULL`

Constraints/indexes:

- primary key `(symbol_id, mpn)`
- secondary index on `mpn_normalized`

Observed semantics:

- only validated/matched phase5 MPNs are inserted
- the normalized form is uppercase alphanumeric with punctuation removed

### Identifier FTS: `symbol_fts_ids`

Virtual table:

- FTS5 with `tokenize = 'trigram'`

Columns:

- `symbol_id UNINDEXED`
- `symbol_library`
- `symbol_name`
- `footprint_library`
- `footprint_name`
- `matched_mpns`

Observed corpus construction:

- all identifier-ish fields are normalized before insertion
- normalization is uppercase alphanumeric only
- `matched_mpns` is a space-joined list of normalized validated MPNs

This index is conceptually closest to the current registry `package_fts_ids`.

### Lexical FTS: `symbol_fts_words`

Virtual table:

- FTS5 with `tokenize = 'unicode61 remove_diacritics 1'`

Columns:

- `symbol_id UNINDEXED`
- `symbol_library`
- `symbol_name`
- `manufacturer`
- `footprint_library`
- `footprint_name`
- `kicad_description`
- `kicad_keywords`
- `kicad_fp_filters`
- `phase3_description`
- `phase3_keywords`

Observed corpus construction:

- combines KiCad-native text from phase0 with OCR-derived phase3 text
- phase3 keywords are flattened to a semicolon-joined string
- this index has richer symbol/text corpus than the identifier index and is likely the main lexical retrieval path

This index is conceptually closest to the current registry `package_fts_words`.

### Semantic Vector Index: `symbol_vec`

Virtual table:

- `vec0` from `sqlite-vec`

Columns:

- `embedding float[EMBEDDING_DIMS]`

Population behavior:

- enabled by default
- disabled with `KICAD_SYMBOLS_PHASE6_ENABLE_EMBEDDINGS=0`
- one embedding row per symbol row
- inserted with `rowid = symbols.id`

Semantic blob synthesis currently includes:

- symbol identity
- manufacturer
- matched MPNs
- footprint identity
- KiCad description
- KiCad keywords
- KiCad footprint filters
- phase3 description
- phase3 keywords

This is the closest analog to the current registry `package_vec`, but the source text composition is different and much more explicitly synthesized.

## KiCad Symbols Search Corpus Details

The phase6 builder prepares three distinct corpora per symbol row:

### Identifier Corpus

Fields used:

- normalized `symbol_library`
- normalized `symbol_name`
- normalized `footprint_library`
- normalized `footprint_name`
- normalized validated matched MPNs

Properties:

- optimized for exact-ish lookup
- punctuation-insensitive
- good for MPNs, library names, footprint ids, and symbol names

### Word Corpus

Fields used:

- raw symbol/library names
- manufacturer
- raw footprint library/name
- KiCad-native:
  - `Description`
  - `ki_keywords`
  - `ki_fp_filters`
- OCR-derived phase3:
  - description
  - keywords

Properties:

- optimized for lexical descriptive search
- likely strongest mode for feature/package/family terms

### Semantic Corpus

Source:

- synthesized multiline semantic blob

Properties:

- blends identity, packaging, KiCad-native text, and OCR-derived summary text
- does not currently depend on phase4 structured eDatasheet data
- does not require phase5 full DigiKey detail payloads, only validated MPNs

## Major Differences From The Registry DB

The new KiCad-symbols DB differs materially from the registry DB.

### Identity Model

Registry DB:

- package-centric
- primary stable identity is registry URL

KiCad-symbols DB:

- symbol-centric
- primary stable identity is effectively `(symbol_library, symbol_name)`

Important live-artifact nuance:

- the DB does not store `relPath`
- the DB does not store `kicadSymPath`
- the original source-library file path has already been collapsed down to `symbol_library`

That means any copied `@kicad-symbols/<path to symbol>` reference must be defined in terms of fields that actually exist in the artifact, not the original `.kicad_sym` path unless the producer adds that field later.

### Corpus Provenance

Registry DB:

- package metadata is already normalized around registry packages

KiCad-symbols DB:

- corpus is assembled from multiple producer-side phases:
  - KiCad symbol metadata
  - datasheet OCR
  - phase3 extraction
  - optional phase4 JSON
  - phase5 validation

### Result Semantics

Registry DB results are directly actionable package references.

KiCad-symbols results are search references to KiCad symbol identities plus attached footprint/manufacturer/MPN metadata. They are not registry package URLs and do not naturally slot into the current “copy selected URL” behavior.

### Availability / Commerce Enrichment

Registry mode today batch-fetches availability after search.

KiCad-symbols rows already carry phase5 validation summaries in `phase5_digikey`, but that is not the same shape as the registry package `digikey` JSON, and there is no current client-side integration for displaying it.

### Detail Shape

Registry detail panels currently assume:

- one selected entity id
- package description
- package relations
- optional product URL / image / JSON blobs

KiCad-symbols can support a detail panel, but the panel model will need to be different:

- symbol identity instead of package URL
- no package dependency/dependent graph
- footprint identity is first-class
- image availability is uneven because multi-unit symbols may have no preview

## Proposed `kicad:symbols` Feature Scope

Add a fourth search mode:

- `kicad:symbols`

This mode should be available in:

- non-interactive CLI search:
  - `pcb search -m kicad:symbols "<query>"`
- interactive TUI mode cycling / startup:
  - `pcb search`
  - `pcb search -m kicad:symbols`

Initial scope should be read/search only. It should not attempt package installation or component generation.

## Proposed UX for `kicad:symbols`

### Non-Interactive CLI

Initial output should include:

- `symbol_library:symbol_name`
- optional manufacturer
- validated matched MPNs
- footprint identity
- short description, preferring:
  - `phase3_description`
  - falling back to `kicad_description` when needed

JSON output should expose the symbol-centric shape rather than trying to masquerade as a registry package.

### Interactive TUI

The TUI should treat `kicad:symbols` as another local-SQLite search backend, but with mode-specific rendering.

Likely list fields:

- line 1:
  - `symbol_library/symbol_name`
- line 2:
  - `manufacturer · first matched MPN or footprint`
- line 3:
  - short description

Likely detail panel fields:

- symbol identity
- footprint library/name
- validated MPN list
- datasheet URL
- KiCad keywords / footprint filters
- optional phase3 / phase5 raw snippets if useful
- optional preview image when present

Current registry-mode Enter behavior should not be reused as-is. For `kicad:symbols`, the most plausible initial behavior is:

- copy the stable symbol URL:
  - `@kicad-symbols/<path to symbol>`

This is now a product requirement, not just a placeholder suggestion.

Based on the live artifact, the spec should define `<path to symbol>` in terms of stored identity.

Recommended initial interpretation:

- `@kicad-symbols/<symbol_library>/<symbol_name>`

with each segment URL-encoded as needed.

Rationale:

- both values exist in the DB
- the pair is the declared unique identity
- it avoids depending on producer-side fields that are not currently stored

We should not define `<path to symbol>` as the original `.kicad_sym` relative path unless the producer starts storing that field in the published artifact.

## Proposed Implementation Strategy

### 1. Introduce Source-Aware Local Index Handling

Current code assumes exactly one local SQLite search source for all local modes.

Refactor to represent local search backends explicitly, for example:

- registry packages backend
- KiCad symbols backend

The TUI worker and non-interactive CLI should dispatch on backend type, not just “requires registry”.

### 2. Split Mode Semantics From Backend Semantics

Today:

- `requires_registry()` is used as a proxy for:
  - local SQLite source
  - availability fetching
  - detail worker type
  - Enter-key behavior
  - mode filtering

That coupling will break once `kicad:symbols` is added.

We should replace it with more explicit mode capabilities such as:

- uses local DB
- supports online availability batch lookup
- supports package relations
- copy action kind
- result renderer kind
- detail model kind

### 3. Add A KiCad Symbols Client

Add a dedicated client for the KiCad-symbols DB with:

- default cache path
- open/open_path behavior
- query parsing + trigram/word/semantic search
- RRF merge, parallel to registry behavior
- detail fetch by symbol id
- result JSON mapping

This client should not reuse registry result structs directly, because the data model is different.

### 4. Add Non-Interactive Search Support

Extend `pcb search` mode enum with `kicad:symbols`.

Non-interactive path should:

- open KiCad-symbols DB
- run symbol search
- render human output
- render symbol-specific JSON output

### 5. Add TUI Support

Add:

- KiCad-symbols mode in the mode enum and cycling
- preflight logic for the new local cache
- a source-specific background worker
- a source-specific detail worker
- mode-specific result list rendering
- mode-specific detail rendering
- same three-way debug panels:
  - identifier/trigram
  - word/lexical
  - semantic

### 6. Defer Download/Update Policy Until Source Is Defined

Do not finalize:

- exact metadata response field names
- whether there is a manual refresh command in addition to startup refresh
- whether we want a shared generalized local-index abstraction immediately or after first integration

## Open Questions

### Distribution / Lifecycle

- What exact metadata response shape will the API return for KiCad-symbols?
- What is the field name for the download URL?
- Will the response also include the same freshness fields as the registry metadata:
  - `sha256`
  - `lastModified`
  - `expiresAt`
- Should there be a manual/forced refresh action in addition to startup refresh?

### Result Semantics

- Should there be a follow-on action beyond copy?

Current answers:

- define `<path to symbol>` as:
  - `<symbol_library>/<symbol_name>`
- no follow-on action beyond copy is defined yet
- ignore `phase5_digikey` entirely for now

### Ranking / Retrieval

- Are embeddings guaranteed to be present in production artifacts?
- Should symbol-name queries prioritize `symbol_fts_ids` more strongly than current registry weighting?

Current answer:

- embeddings are expected to be present in production artifacts

Initial recommendation:

- start with the same broad RRF structure as registry search
- bias the KiCad-symbols mode somewhat more toward `symbol_fts_ids` than registry mode because:
  - symbol/library/footprint identifiers are first-class query targets
  - exact symbol-name and MPN-style lookups are likely common
  - the lexical corpus is richer and noisier than the identifier corpus

I do not want to lock the exact weighting in the spec yet without inspecting a real built artifact and sample queries. Once we have a local DB copy, we should evaluate:

- exact symbol-name query
- near-exact symbol-name query
- MPN query
- footprint/package-family query
- natural-language functional query

and then choose whether to:

- change RRF weights
- change per-index limits
- add query-type heuristics
- or keep parity with the registry ranking

Live-artifact observations supporting this recommendation:

- exact identifier lookup against `symbol_fts_ids` works cleanly for symbol-name queries such as `SN74LVC1G00DBV`
- lexical search against `symbol_fts_words` already works well for descriptive queries such as `buck converter`
- footprint/package text is rich enough that mixed lexical/package queries such as `QFN usb` already return plausible hits

So the current working hypothesis is:

- keep both identifier and lexical search strong
- modestly favor identifiers over the registry default
- retain semantic search and the same debug-panel visibility

### UI

- How should symbols with no image be rendered in the preview/detail area?

Current product answer:

- `kicad:symbols` should participate in pricing/availability fetching just like the other modes
- debug panels should show the same three-source scoring view as registry mode

Clarification on “existing availability worker”:

- today the TUI has a background worker that batch-fetches BOM pricing/availability for search results after local search completes
- for registry-backed results, it keys those requests from `(url, mpn, manufacturer)`
- for web results, it keys them from `(component_id, part_number, manufacturer)`

For `kicad:symbols`, the requirement is that search results should also trigger batch pricing/availability requests, likely keyed by:

- stable result id for the UI cache key
- validated MPN
- manufacturer

This is conceptually the same worker responsibility, though the cache key and displayed result model will need a new source-specific mapping.

On image coverage:

- the current producer README/code says multi-unit symbols can have `image = NULL`
- when `image` is NULL, the client should simply not show an image preview

## Recommended Next Step

Next spec pass should tighten the remaining contract details:

- whether manual refresh exists in addition to startup refresh
- final output JSON shape for `kicad:symbols`

After that, implementation should proceed in this order:

1. backend abstraction cleanup
2. KiCad-symbols client + non-interactive CLI
3. TUI mode integration
4. polish for details/images/copy behavior
