


> **PCB tooling by [Diode Computers, Inc.](https://diode.computer/)**
> 
> A comprehensive Rust-based toolchain for PCB design using the Zener language (.zen files)

## Overview

The PCB toolchain is a modern, Starlark-based PCB design system that provides:
- **Zener Language**: Starlark-based DSL for PCB design specification
- **KiCad Integration**: Seamless integration with KiCad EDA tools
- **CLI Tools**: Complete command-line interface for PCB workflows
- **IDE Support**: Language Server Protocol (LSP) for editor integration
- **Cross-platform**: Windows, macOS, and Linux support

## Installation

```bash
# From source
git clone https://github.com/diodeinc/pcb.git
cd pcb
./install.sh
```

**Requirements**: KiCad 9.x

## CLI Interface

### Main Commands

#### Build
```bash
pcb build [PATHS...]     # Build .zen files
pcb b [PATHS...]         # Alias
```

#### Layout Generation
```bash
pcb layout [OPTIONS] [PATHS...]  # Generate PCB layouts
pcb l [OPTIONS] [PATHS...]       # Alias

# Options:
--no-open                # Skip opening layout file
-s, --select            # Always prompt for layout selection
```

#### Formatting
```bash
pcb fmt [OPTIONS] [PATHS...]  # Format .zen/.star files

# Options:
--check                  # Check formatting without modifying
--diff                   # Show diffs instead of writing
```

#### Clean
```bash
pcb clean [OPTIONS]      # Clean generated files

# Options:
-f, --force             # Remove all generated files
--keep-cache            # Keep remote package cache
```

#### Open
```bash
pcb open [PATHS...]      # Open PCB layouts
pcb o [PATHS...]         # Alias
```

#### LSP Server
```bash
pcb lsp                  # Start Language Server Protocol server
```

## Core Libraries

### pcb-zen-core

**Purpose**: Foundation library for Zener language runtime

```rust
use pcb_zen_core::{
    EvalContext, EvalOutput, InputMap, InputValue,
    Diagnostic, WithDiagnostics, DiagnosticError,
    FileProvider, DefaultFileProvider, InMemoryFileProvider,
    LoadResolver, CompoundLoadResolver, WorkspaceLoadResolver,
    Bundle, BundleManifest, BundleMetadata,
    file_extensions
};

// Core evaluation
let ctx = EvalContext::new();
let result = ctx.eval();

// File extensions
use pcb_zen_core::file_extensions::{
    STARLARK_EXTENSIONS, // ["star", "zen"]
    KICAD_SYMBOL_EXTENSION, // "kicad_sym"
    is_starlark_file
};
```

**Features**:
- `default`: `["native"]`
- `native`: File system access, HTTP, compression
- `wasm`: WASM-compatible random generation

### pcb-zen

**Purpose**: High-level interface for Zener evaluation

```rust
use pcb_zen::{
    run, create_eval_context, lsp, lsp_with_eager,
    render_diagnostic, Bundle, BundleMetadata,
    Diagnostic, WithDiagnostics, EvalSeverity
};

// Basic evaluation
let result = run(Path::new("design.zen"));
if result.is_success() {
    let schematic = result.output.unwrap();
    // Use schematic...
}

// Custom context
let ctx = create_eval_context(Path::new("/workspace"));
let result = ctx.eval();

// LSP server
lsp()?; // Start LSP server
lsp_with_eager(true)?; // With eager evaluation
```

**Load Resolution**:
- `@package:tag/path` - Remote packages
- `@github/user/repo:rev/path` - GitHub repositories  
- `@gitlab/user/repo:rev/path` - GitLab repositories
- `//path/to/file` - Workspace-relative
- `./relative/path` - Relative paths

## EDA Libraries

### pcb-eda

**Purpose**: Electronic component and symbol management

```rust
use pcb_eda::{Symbol, SymbolLibrary, Part, Pin};

// Symbol structure
struct Symbol {
    name: String,
    footprint: String,
    in_bom: bool,
    pins: Vec<Pin>,
    datasheet: Option<String>,
    manufacturer: Option<String>,
    mpn: Option<String>,
    distributors: HashMap<String, Part>,
    description: Option<String>,
    properties: HashMap<String, String>,
}

// Usage
let symbol = Symbol::from_file(Path::new("component.kicad_sym"))?;
let library = SymbolLibrary::from_file(Path::new("library.kicad_sym"))?;
let resistor = library.get_symbol("R_0603")?;
```

### pcb-sch

**Purpose**: Schematic representation and netlist generation

```rust
use pcb_sch::{
    Schematic, Instance, Net, InstanceKind, NetKind,
    InstanceRef, ModuleRef, AttributeValue,
    kicad_netlist::to_kicad_netlist,
    kicad_schematic::to_kicad_schematic
};

// Core structures
struct Schematic {
    instances: HashMap<InstanceRef, Instance>,
    nets: HashMap<String, Net>,
    root_ref: Option<InstanceRef>,
}

enum InstanceKind {
    Module, Component, Interface, Port, Pin
}

enum NetKind {
    Normal, Ground, Power
}

// Usage
let netlist = to_kicad_netlist(&schematic);
let kicad_sch = to_kicad_schematic(&schematic, &output_path)?;
```

### pcb-layout

**Purpose**: PCB layout generation and management

```rust
use pcb_layout::{
    process_layout, LayoutResult, LayoutError,
    utils::{extract_layout_path, get_layout_paths}
};

// Layout generation
struct LayoutResult {
    source_file: PathBuf,
    layout_dir: PathBuf,
    pcb_file: PathBuf,
    netlist_file: PathBuf,
    snapshot_file: PathBuf,
    log_file: PathBuf,
    created: bool, // true if new, false if updated
}

// Usage
let result = process_layout(&schematic, Path::new("design.zen"))?;
println!("PCB file: {}", result.pcb_file.display());
```

## Utility Libraries

### pcb-kicad

**Purpose**: KiCad file format integration and Python scripting

```rust
use pcb_kicad::PythonScriptBuilder;

// KiCad Python integration
let script = PythonScriptBuilder::new()
    .add_site_packages_path()
    .build();
```

**Environment Variables**:
- `KICAD_PYTHON_INTERPRETER`: Custom Python path
- `KICAD_PYTHON_SITE_PACKAGES`: Custom site-packages
- `KICAD_CLI`: Custom KiCad CLI path
- `KICAD_SYMBOL_DIR`: Custom symbol directory

### pcb-sexpr

**Purpose**: S-expression parsing for KiCad formats

```rust
use pcb_sexpr::Sexpr;

// S-expression types
enum Sexpr {
    Symbol(String),    // Unquoted identifier
    String(String),    // Quoted text
    List(Vec<Sexpr>),  // List of expressions
}

// Usage
let expr = Sexpr::symbol("component");
let list = Sexpr::list(vec![expr]);
let parsed = Sexpr::parse("(symbol \"value\")");
```

### pcb-ui

**Purpose**: Terminal UI components for CLI tools

```rust
use pcb_ui::{Spinner, Style, Progress};

// Spinner usage
let spinner = Spinner::builder("Processing...")
    .start();
// ... do work ...
spinner.success("Done!");

// Progress bars and styling
let progress = Progress::new(100);
progress.set_position(50);
```

### pcb-buildifier

**Purpose**: Code formatting for .zen/.star files

```rust
use pcb_buildifier::{
    format_file, format_string, check_format,
    BuildifierError
};

// Format files
format_file(Path::new("design.zen"))?;
let formatted = format_string("load('//lib.star', 'component')");
let is_formatted = check_format(Path::new("design.zen"))?;
```

### pcb-starlark-lsp

**Purpose**: Language Server Protocol implementation

```rust
use pcb_starlark_lsp::{
    server::StarlarkLanguageServer,
    completion::CompletionProvider,
    definition::DefinitionProvider
};

// LSP server components
let server = StarlarkLanguageServer::new();
server.run()?;
```

### pcb-command-runner

**Purpose**: Command execution with output capture

```rust
use pcb_command_runner::{CommandRunner, CommandOutput};

// Command execution
struct CommandOutput {
    raw_output: Vec<u8>,    // With ANSI sequences
    plain_output: Vec<u8>,  // ANSI stripped
    success: bool,
}

let runner = CommandRunner::new();
let output = runner.run("kicad-cli", &["--version"])?;
```

## Workspace Configuration

### Cargo.toml Dependencies

```toml
[dependencies]
# Core libraries
pcb-zen = "0.2.0-dev"
pcb-zen-core = "0.2.0-dev"

# EDA libraries
pcb-eda = "0.2.0-dev"
pcb-sch = "0.2.0-dev"
pcb-layout = "0.2.0-dev"

# Utility libraries
pcb-kicad = "0.2.0-dev"
pcb-sexpr = "0.2.0-dev"
pcb-ui = "0.2.0-dev"
pcb-buildifier = "0.2.0-dev"
pcb-starlark-lsp = "0.2.0-dev"
pcb-command-runner = "0.2.0-dev"
```

### Workspace Structure

```
pcb/
├── crates/
│   ├── pcb/                 # Main CLI binary
│   ├── pcb-zen/             # High-level Zener interface
│   ├── pcb-zen-core/        # Core Zener runtime
│   ├── pcb-eda/             # EDA component management
│   ├── pcb-sch/             # Schematic representation
│   ├── pcb-layout/          # Layout generation
│   ├── pcb-kicad/           # KiCad integration
│   ├── pcb-sexpr/           # S-expression parsing
│   ├── pcb-ui/              # Terminal UI components
│   ├── pcb-buildifier/      # Code formatting
│   ├── pcb-starlark-lsp/    # LSP server
│   └── pcb-command-runner/  # Command execution
├── vscode/                  # VS Code extension
└── docs/                    # Documentation
```

## Example Usage

### Basic PCB Design Workflow

```rust
use pcb_zen::run;
use pcb_layout::process_layout;
use std::path::Path;

// 1. Evaluate Zener design
let result = run(Path::new("blinky.zen"));
if !result.is_success() {
    for diagnostic in result.diagnostics {
        eprintln!("{}", diagnostic);
    }
    return;
}

// 2. Generate PCB layout
let schematic = result.output.unwrap();
let layout_result = process_layout(&schematic, Path::new("blinky.zen"))?;

println!("PCB generated: {}", layout_result.pcb_file.display());
```

### Custom Load Resolution

```rust
use pcb_zen_core::{
    EvalContext, CompoundLoadResolver,
    WorkspaceLoadResolver, RelativeLoadResolver
};

let resolver = CompoundLoadResolver::new(vec![
    Box::new(WorkspaceLoadResolver::new(workspace_root)),
    Box::new(RelativeLoadResolver),
]);

let ctx = EvalContext::new()
    .with_load_resolver(resolver)
    .with_file_provider(file_provider);
```

### Bundle Creation

```rust
use pcb_zen::bundle::create_bundle;

// Create self-contained bundle
create_bundle(
    Path::new("design.zen"),
    Path::new("output.bundle")
)?;
```

## Key Features

- **Starlark-based DSL**: Familiar Python-like syntax for PCB design
- **KiCad Integration**: Seamless workflow with industry-standard EDA tools
- **Hierarchical Design**: Support for complex, modular PCB designs
- **Package Management**: Remote package loading from GitHub/GitLab
- **IDE Support**: Full LSP implementation for modern editors
- **Cross-platform**: Native support for Windows, macOS, and Linux
- **Bundle System**: Self-contained deployment packages
- **Rich CLI**: Comprehensive command-line interface with progress feedback
- **Extensible**: Plugin architecture via external subcommands

## License

MIT License - See LICENSE file for details

---

*Generated for PCB Toolchain v0.2.0-dev*
