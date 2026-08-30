# DFM report bundle contract, version 1

This contract defines the portable DFM exchange format. The
[JSON report reference](dfm.md#json-report) defines diagnostics, topology, and
native geometry. Bundle, report, and scene versions are independent; all
currently use integer `1`. Bundles contain data for external viewers, with no
HTML report or executable viewer assets.

## Producing a bundle

```sh
pcb ipc dfm check panel.xml --pdk standard --layout-target board-array \
  --format bundle --output panel.dfm.tar.zst

pcb dfm MyBoard.zen --pdk standard \
  --format bundle --output MyBoard.dfm.tar.zst
```

`--format bundle` requires an output file and always includes native vector
geometry. The recommended suffix is `.dfm.tar.zst`; the file contents, not its
name, identify the format. `--format json` remains the default.
`--include-geometry` is redundant but permitted with bundle output.

Completed failing checks write a complete bundle before returning nonzero.
Preparation failures produce an incomplete report with any captured sources.
Output is replaced atomically: I/O, serialization, or size-limit failures leave
any previous artifact untouched, even when writing an incomplete report.
Callers must check exit status. See [CLI behavior](dfm.md#cli), including
rejection of source aliases and KiCad board destinations before output.

## Archive members

There is no enclosing directory. These are the only version 1 member paths:

| Path | Media type | When present |
| --- | --- | --- |
| `manifest.json` | `application/json` | Always, first member |
| `report.json` | `application/json` | Always, second member |
| `source/design.ipc2581.xml` | `application/xml` | Required for a complete report |
| `source/pdk.toml` | `application/toml` | Required for a complete report |
| `source/waivers.toml` | `application/toml` | Required when `--waivers` was supplied, even if zero waivers applied |

The remaining members appear in the table's order, omitting absent sources.
Incomplete reports may contain none, some, or all three sources, depending on
where preparation failed; their presence does not mean they parsed successfully.
No other members are permitted in v1, including raster previews, separate SVG,
HTML, scripts, fonts, or network references.

The archive is POSIX ustar: regular files only, 512-byte record padding, and two
zero end records. Header metadata is fixed: mode `0644`, uid/gid/mtime `0`, empty
owner/group names. No directory entries, symlinks, hard links, device entries,
sparse files, PAX records, GNU long-name records, or duplicate paths are allowed.

Compression is a single standard Zstandard frame with a declared frame content
size, a content checksum, no dictionary, and a window no larger than 8 MiB. No
concatenated or skippable frames or nonzero trailing archive data are allowed.
The content size includes the entire TAR stream, headers and padding included.

### Manifest

The UTF-8 JSON manifest has this shape:

```ts
type DfmBundleManifest = {
  format: 'pcb-dfm-report';
  schema_version: 1;
  report: 'report.json';
  files: Array<{
    path: 'report.json' | 'source/design.ipc2581.xml' |
          'source/pdk.toml' | 'source/waivers.toml';
    media_type: 'application/json' | 'application/xml' | 'application/toml';
    size_bytes: number; // nonnegative safe integer, uncompressed payload only
    sha256: string;    // exactly 64 lowercase hexadecimal characters
  }>;
};
```

`files` inventories every other member exactly once, in archive order. It does
not inventory or hash `manifest.json` itself. Each media type must match the
table. Sizes and hashes apply to the exact member bytes, including whitespace
and a final newline if present. Validate membership, size, and SHA-256 before
interpreting a report. Checksums detect corruption; they are not authentication
or a signature from a fabricator.

### Source identity

The XML member contains the exact UTF-8 text passed to the IPC parser, after
decompressing a `.xml.zst` input when necessary. Do not reserialize XML or change
line endings. Built-in PDKs are included as their exact resolved TOML bytes.
Waivers are captured from the bytes actually read for the check, not reread
afterward. For `pcb dfm`, the temporary exported IPC XML is captured before its
temporary directory is removed.

The diagnostic JSON retains its existing provenance semantics:

- `report.input.sha256` and `size_bytes` identify the original on-disk IPC input
  bytes. For uncompressed XML, these also match the XML archive member. For a
  `.xml.zst` source, the manifest hashes the decoded XML while the report hashes
  the original compressed input; those hashes are intentionally different.
- `report.pdk.sha256` and, when present, `report.waivers.sha256` match the
  corresponding archived TOML bytes.
- Report `path` fields are descriptive source labels. They can be absolute,
  `builtin:standard`, or a no-longer-existing temporary path. Never treat them
  as archive paths, fetch URLs, or files to open on the consumer's machine.
  The manifest's fixed paths locate the portable artifacts.

The bundle can contain private board data, local path labels, component names,
nets, and waiver reasons. A file picker or drop action means local inspection;
it does not authorize uploading these contents to a backend or telemetry.

## Report requirements

A complete `report.json` has report schema version `1`, a `pass` or `fail`
verdict, and a [native scene](dfm.md#native-scene) with `schema_version: 1`,
`bounds`, and `passes`, even if all configured checks are nonspatial. Spatial
sites must have every required scene pass and native evidence operand.
The [finding](dfm.md#findings), [coordinate](dfm.md#coordinates-and-topology),
count, waiver, and rendering invariants in the JSON reference also apply here.

`report.json` is the scene authority. Render its producer-supplied native SVG
and evidence; do not infer geometry from messages or screenshots, rerun DFM,
or reconstruct the verdict through a second XML geometry engine. The bundled
XML is available for inspection and future IR workflows.

Handle [`verdict: "incomplete"`](dfm.md#incomplete-reports) before requiring
complete-report fields. It carries the error and available source labels, with
no `summary`, `rules`, `findings`, or `scene`. It is never a pass or a skipped
check.

## Reader limits and safety

Producer and consumer enforce these inclusive v1 limits. MiB means 1,048,576
bytes; KiB means 1,024 bytes.

| Resource | Limit |
| --- | --- |
| Compressed bundle | 64 MiB |
| Complete expanded TAR stream | 256 MiB |
| `manifest.json` | 64 KiB |
| `report.json` | 128 MiB |
| XML | 128 MiB |
| PDK | 1 MiB |
| Waivers | 8 MiB |
| Archive members | 5 |
| Zstandard window | 8 MiB |

Preflight the frame header and enforce actual expanded-byte limits while
decoding, before an unbounded allocation. Reject unknown frame sizes, excessive
windows, unsupported dictionaries, trailing frames, truncation, checksum
failures, and a decoded size that differs from the declaration. Parse in a
worker so cancellation and malformed input do not block the admin UI.

Never extract an uploaded archive onto a server filesystem. Validate TAR
checksums, type flags, declared lengths, record boundaries, fixed member paths,
end markers, and the manifest. Reject absolute paths, backslashes, `.`/`..`,
duplicate names, unlisted or missing members, and unknown member paths; do not
normalize them into an accepted path.

All JSON and SVG are untrusted even when hashes match. Validate finite numbers,
ordered bounds, supported coordinate frames, unique IDs, references, and schema
versions. Parse SVG into an inert allowlisted tree; never inject uploaded markup
as HTML. Reject scripts, event handlers, foreign objects, styles, entities,
external resources, and nonlocal fragment references. Bound JSON/geometry
complexity and SVG reference traversal independently of byte size. Show a clear
unsupported/invalid report error when safe rendering is unavailable.

Do not log payloads, keep hidden copies after replacement, or silently persist
uploads. Terminate workers, release buffers/object URLs, and ignore stale async
results when another file replaces the current load.

## Evolution and reproducibility

Unknown optional object fields can be ignored. Existing field meanings, units,
ID/waiver semantics, and rendering constructions follow the
[JSON evolution rules](dfm.md#schema-evolution). Unknown required semantics
must produce an explicit unsupported state. New archive member kinds, breaking
structure, or larger resource profiles require a new bundle version.

With the same input bytes, source labels, options, and `SOURCE_DATE_EPOCH`, bundle
bytes are reproducible. That timestamp also controls waiver expiry. TAR metadata
never comes from the local filesystem. A `.zen` workflow's newly exported input
and temporary source label may differ between runs; this is not a promise of
determinism across a separate layout/export operation.
