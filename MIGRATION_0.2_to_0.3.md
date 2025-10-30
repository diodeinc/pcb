# Migration Guide: v0.2 → v0.3

## Breaking Changes

Version 0.3.0 splits the monolithic `ipc-2581` crate into two focused crates for better modularity and build performance.

## Crate Split

### Before (0.2.x)
```toml
[dependencies]
ipc-2581 = "0.2"  # Everything in one crate
```

### After (0.3.x)
```toml
[dependencies]
ipc2581 = "0.3"         # Pure parser only (faster builds)
ipc2581-tools = "0.3"   # Export/visualization (includes parser)
```

## Import Changes

### Module Name Change

The crate name changed from `ipc-2581` (with hyphen) to `ipc2581` (no separator).

**Before:**
```rust
use ipc_2581::Ipc2581;
use ipc_2581::html_generator::generate_html;
use ipc_2581::svg_export::export_layer_to_svg;
```

**After:**
```rust
use ipc2581::Ipc2581;
use ipc2581_tools::html_generator::generate_html;
use ipc2581_tools::svg_export::export_layer_to_svg;
```

### Export Modules Moved

All rendering/visualization modules moved to `ipc2581-tools`:

| Module | Old | New |
|--------|-----|-----|
| HTML generation | `ipc_2581::html_generator` | `ipc2581_tools::html_generator` |
| SVG export | `ipc_2581::svg_export` | `ipc2581_tools::svg_export` |
| Board outline | `ipc_2581::board_outline` | `ipc2581_tools::board_outline` |
| Copper layers | `ipc_2581::copper_layer` | `ipc2581_tools::copper_layer` |
| Geometry | `ipc_2581::geometry` | `ipc2581_tools::geometry` |

### Core Types (No Change)

Core parser types remain in the same place (now re-exported by both crates):

```rust
// These work in both crates
use ipc2581::{Ipc2581, Ecad, Layer, Stackup, Component, etc.};

// Or from tools crate (re-exported)
use ipc2581_tools::{Ipc2581, Ecad, Layer, etc.};
```

## API Changes

### Removed Fields

- `StackupLayer.tol_percent` - Removed derived boolean field (use raw tol_plus/tol_minus)

### Changed Field Types (String → Symbol)

Several identifier fields changed from `String` to `Symbol` for performance:

```rust
// Spec fields
spec.material: Option<Symbol>      // was Option<String>
spec.properties: Vec<Symbol>       // was Vec<String>  
spec.color_term: Option<Symbol>    // was Option<String>

// Access via interner
let material_name = doc.resolve(spec.material.unwrap());
```

**Affected types:**
- `Spec`: 3 fields
- `FinishProduct`: 1 field
- `SurfaceFinish`: 1 field  
- `TextualCharacteristic`: 3 fields

**Migration:** Use `doc.resolve(symbol)` to get string values.

### Feature Flags Removed

The `export` feature flag no longer exists - functionality is split into separate crates instead.

**Before:**
```toml
ipc-2581 = { version = "0.2", default-features = false }  # Parser only
ipc-2581 = { version = "0.2" }  # With export feature
```

**After:**
```toml
ipc2581 = "0.3"         # Pure parser (always)
ipc2581-tools = "0.3"   # Export tools (when needed)
```

## Migration Examples

### Example 1: Parser Only

**Before (0.2):**
```toml
[dependencies]
ipc-2581 = { version = "0.2", default-features = false }
```

```rust
use ipc_2581::Ipc2581;
```

**After (0.3):**
```toml
[dependencies]
ipc2581 = "0.3"
```

```rust
use ipc2581::Ipc2581;  // Note: ipc2581 not ipc_2581
```

### Example 2: With Export/Visualization

**Before (0.2):**
```toml
[dependencies]
ipc-2581 = "0.2"  # or features = ["export"]
```

```rust
use ipc_2581::Ipc2581;
use ipc_2581::svg_export::export_layer_to_svg;
use ipc_2581::html_generator::generate_html;
```

**After (0.3):**
```toml
[dependencies]
ipc2581-tools = "0.3"  # Includes parser + tools
```

```rust
use ipc2581::Ipc2581;  // Re-exported by tools
use ipc2581_tools::svg_export::export_layer_to_svg;
use ipc2581_tools::html_generator::generate_html;
```

### Example 3: CLI Tool

**Before (0.2):**
```toml
[dependencies]
ipc-2581 = "0.2"
```

```bash
$ cargo install ipc-2581
$ ipc2581 validate file.xml
```

**After (0.3):**
```toml
[dependencies]
ipc2581-tools = "0.3"
```

```bash
$ cargo install ipc2581-tools
$ ipc2581 validate file.xml  # Binary name unchanged
```

## Build Time Impact

### Pure Parser
```bash
# Before: All dependencies always compiled
$ cargo build  # ~45s (includes skia)

# After: Lightweight parser only
$ cargo build -p ipc2581  # ~5s
```

**9x faster builds** for parser-only use cases!

### With Export
```bash
$ cargo build -p ipc2581-tools  # ~45s (same as before)
```

## Compatibility Notes

### KiCad Compatibility Removed

Version 0.3 removes vendor-specific workarounds for non-compliant IPC-2581 exports:
- No more "SPEC_" prefix stripping
- No more fuzzy SpecRef matching

**Impact:** Files exported by KiCad with incorrect SpecRef naming may not resolve specs correctly.

**Fix:** Use compliant IPC-2581 exporters or file bug reports with vendor tools.

### Arc Tessellation Simplified

UserSpecial arc handling temporarily simplified (arcs rendered as straight lines in parser).

**Impact:** Minimal - arcs in UserSpecial primitives are rare in practice.

**Future:** Proper arc data preservation coming in 0.4 (parser will store Arc structs, tools will tessellate).

## Testing Your Migration

```bash
# Ensure pure parser works
cargo test -p ipc2581

# Ensure tools work
cargo test -p ipc2581-tools

# Ensure your code compiles
cargo check
```

## Questions?

See [MAXIMAL_PURITY.md](../ipc2581/MAXIMAL_PURITY.md) for architecture details.

## License

MIT OR Apache-2.0
