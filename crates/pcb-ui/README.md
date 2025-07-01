# diode-ui

A consistent UI library for Diode PCB tools, providing spinners, progress bars, and other terminal UI components.

## Features

- **Spinners** - Indeterminate progress indicators with customizable animations
- **Progress Bars** - Determinate progress indicators with percentage tracking
- **Styled Text** - Consistent styling for success, error, warning, and info messages
- **Terminal Utilities** - Terminal size detection and text manipulation helpers

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
diode-ui = { path = "../diode-ui" }
```

## API Overview

### Spinners

Simple spinner with default settings:

```rust
use diode_ui::Spinner;

let spinner = Spinner::builder("Processing...").start();
// ... do work ...
spinner.success("Done!");
```

Customized spinner:

```rust
use diode_ui::{Spinner, Style};
use std::time::Duration;

let spinner = Spinner::new("Loading")
    .style(Style::Blue)
    .tick_chars("◐◓◑◒")
    .tick_interval(Duration::from_millis(200))
    .start();

// Update message while running
spinner.set_message("Still loading...");

// Different completion methods:
spinner.success("Loaded!");     // ✓ Loaded! (green)
spinner.error("Failed!");       // ✗ Failed! (red)
spinner.warning("Careful!");    // ! Careful! (yellow)
spinner.finish();               // Just clears
```

### Progress Bars

Basic progress bar:

```rust
use diode_ui::ProgressBar;

let pb = ProgressBar::new(100).message("Downloading").start();
for i in 0..=100 {
    pb.set_position(i);
    // ... do work ...
}
pb.success("Complete!");
```

Customized progress bar:

```rust
let pb = ProgressBar::new(total_steps)
    .message("Building")
    .style(Style::Yellow)
    .progress_chars("#>-")
    .start();

// Update during operation
pb.inc(1);  // Increment by 1
pb.set_message(format!("Building ({}/{})", pb.position(), pb.total()));

// Check progress
let percentage = pb.percentage(); // 0-100
```

### Styled Text

Apply consistent styling to text:

```rust
use diode_ui::prelude::*;

println!("{}", "Success!".success());      // ✓ Success! (green)
println!("{}", "Error!".error());          // ✗ Error! (red)
println!("{}", "Warning!".warning());      // ! Warning! (yellow)
println!("{}", "Info".info());             // ℹ Info (blue)

// Or use specific styles
use diode_ui::{Style, StyledText};
println!("{}", "Custom".with_style(Style::Cyan));
```

### Terminal Utilities

```rust
use diode_ui::terminal;

// Get terminal size
if let Some(size) = terminal::get_terminal_size() {
    println!("Terminal: {}x{}", size.width, size.height);
}

// Text manipulation
let truncated = terminal::truncate_text("Long text here", 10); // "Long te..."
let padded = terminal::pad_text("Hi", 10, terminal::Alignment::Center); // "    Hi    "

// Calculate display width (handles Unicode)
let width = terminal::text_width("Hello 世界"); // Accounts for wide chars
```

### Convenience Functions

```rust
use diode_ui::{loading, progress_bar};

// Quick spinner
let spinner = loading("Processing...");

// Quick progress bar
let pb = progress_bar(100, "Downloading");
```

### Suspending for User Input

Both spinners and progress bars can be temporarily hidden:

```rust
let spinner = Spinner::new("Working...").start();

// Temporarily hide for user input
let answer = spinner.suspend(|| {
    // Prompt user for input here
    println!("Continue? (y/n)");
    // ... read input ...
});

spinner.finish();
```

## Design Principles

1. **Consistent Visual Style** - All UI elements follow the same visual patterns as the CLI
2. **Builder Pattern** - Flexible configuration through method chaining
3. **Ergonomic Defaults** - Works great out of the box with sensible defaults
4. **Non-blocking** - Progress indicators run on separate threads via `indicatif`
5. **Terminal-aware** - Handles terminal size and capabilities gracefully

## Examples

Run the demo to see all features:

```bash
cargo run --example demo
```
