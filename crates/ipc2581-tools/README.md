# ipc2581-tools

**Visualization and export tools for IPC-2581** - SVG rendering, HTML reports, and CLI utilities.

## Overview

`ipc2581-tools` provides high-quality rendering and export functionality for IPC-2581 documents parsed by the [`ipc2581`](../ipc2581) crate.

## Features

- 🎨 **SVG Export**: Render PCB layers to high-quality SVG
- 📄 **HTML Reports**: Interactive stackup documentation
- 📐 **Board Outline**: Extract and render board outlines
- ⚡ **Copper Layers**: Accurate copper feature rendering with tessellation
- 🛠️ **CLI Tool**: `ipc2581` binary for command-line processing

## Installation

```toml
[dependencies]
ipc2581-tools = "0.3"  # Includes ipc2581 parser automatically
```

## Usage

### Rust API

```rust
use bumpalo::Bump;
use ipc2581::{Ipc2581, LayerFunction};
use ipc2581_tools::svg_export::export_layer_to_svg;
use ipc2581_tools::html_generator::generate_html;

// Parse document (using re-exported ipc2581)
let arena = Bump::new();
let doc = Ipc2581::parse_file(&arena, "design.xml")?;

// Export copper layer to SVG
let svg = export_layer_to_svg(
    &doc,
    "F.Cu",
    ipc2581_tools::svg_export::DrawingParams::default()
)?;
std::fs::write("copper.svg", svg)?;

// Generate HTML report
let html = generate_html(&doc)?;
std::fs::write("report.html", html)?;
```

### CLI Tool

The crate includes the `ipc2581` binary for command-line use:

```bash
# Validate IPC-2581 file
ipc2581 validate design.xml

# Export layer to SVG
ipc2581 export design.xml --layer F.Cu --output copper.svg

# Generate HTML report
ipc2581 html design.xml --output report.html

# Inspect stackup
ipc2581 stackup design.xml
```

## Modules

- **`svg_export`**: Multi-stage rendering pipeline for SVG output
- **`html_generator`**: Interactive HTML stackup documentation
- **`board_outline`**: Board edge extraction
- **`copper_layer`**: Copper feature collection and rendering
- **`geometry`**: Geometric utilities (arc creation, tessellation)

## Dependencies

This crate includes heavier dependencies for rendering:

- `ipc2581` (pure parser)
- `skia-safe` - 2D graphics rendering
- `lyon` - Polygon tessellation
- `svg` - SVG generation
- `kurbo` - 2D geometry
- `minijinja` - HTML templates
- `geo` - Geometric operations

**Build time:** ~45 seconds (clean build with skia compilation)

For pure parsing without these dependencies, use `ipc2581` directly.

## Architecture

```
Rendering Pipeline:
  IPC-2581 XML
       ↓ (ipc2581 parser)
  Rust Data Structures
       ↓ (svg_export stages)
  Stage 0: Context setup
  Stage 1: Feature resolution
  Stage 2: Tessellation
  Stage 3: Path conversion
  Stage 4: SVG generation
       ↓
  SVG Output
```

## Performance

- **Skia rendering**: GPU-accelerated path operations
- **Lyon tessellation**: Efficient polygon triangulation
- **Multi-stage pipeline**: Modular, testable rendering
- **Caching**: Board context and feature resolution cached

## License

MIT OR Apache-2.0
