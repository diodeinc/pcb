# Package URI Migration — Open Issues

Tracking document for remaining issues from the `package://` URI migration.

## Bug 2: `write_footprint_library_table` silently skips non-URI footprints

**File:** `crates/pcb-layout/src/lib.rs` lines 753–758

The function resolves each component's `footprint` attribute via `resolve_package_uri`. If the URI resolution fails (e.g. `package_roots` is empty or the package isn't registered), it silently `continue`s — dropping the component from the fp-lib-table entirely.

```rust
let resolved_fp = match schematic.resolve_package_uri(fp_attr) {
    Ok(abs) => abs.to_string_lossy().into_owned(),
    Err(_) => continue,  // silently drops this component
};
```

**Impact:** If `package_roots` is misconfigured or incomplete, footprint libraries are silently missing from the generated `fp-lib-table`, causing KiCad to not find footprints. No warning or error is emitted.

**Suggested fix:** Log a warning or emit a diagnostic when a footprint URI can't be resolved, so users know which components are affected.

## Bug 3: Test workspaces with empty `package_roots`

Several test resource directories have no `pcb.toml` or only a bare workspace `pcb.toml` with no board/package member declarations. This means the test directory itself isn't registered as a package root, so all `package://` URI resolution against it fails.

**Affected test directories** (no pcb.toml or missing board/package members):
- `crates/pcb-layout/tests/resources/complex/`
- `crates/pcb-layout/tests/resources/component_side_sync/`
- `crates/pcb-layout/tests/resources/graphics/`
- `crates/pcb-layout/tests/resources/tracks/`
- `crates/pcb-layout/tests/resources/zones/`
- `crates/pcb-layout/tests/resources/netclass_assignment/`

These tests use stdlib's `Layout()` function (which internally calls `Path()`), so the layout_path becomes a `package://` URI. But since the test directory isn't a registered package, `resolve_package_uri` fails.

**Suggested fix:** Add proper `pcb.toml` files with board declarations to these test directories so that `package_roots` gets populated correctly during test evaluation.

## Bug 4: Release test — missing `layout_path` and manufacturing CPL

**Files:**
- `crates/pcb/tests/snapshots/release__publish_full.snap.new`
- `crates/pcb/tests/snapshots/tag__publish_board_simple_workspace.snap.new`

After converting `add_property("layout_path", ...)` to `Layout()`, the release test snapshots need to be verified:

1. **`layout_path` in netlist JSON** — The old convert.rs HACK prepended the module directory to `layout_path` for non-root modules. That hack was removed. With `Layout()` using `Path()`, the value should now be a `package://` URI. Need to verify this resolves correctly in the release/publish pipeline.

2. **Manufacturing CPL missing** — The `release__publish_full` snapshot lost all `manufacturing/cpl.csv` entries (component placement data). This happens when `process_layout` returns `None` (layout dir not resolved), which means the PCB file never gets created, so there's nothing to export positions from. This should be fixed once `package_roots` is properly populated.
