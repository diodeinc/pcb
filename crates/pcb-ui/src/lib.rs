//! # diode-ui
//!
//! A consistent UI library for Diode PCB tools, providing spinners, progress bars,
//! and other terminal UI components.
//!
//! ## Example
//!
//! ```rust,no_run
//! use pcb_ui::{Spinner, Style};
//!
//! // Create and use a spinner
//! let spinner = Spinner::builder("Processing...").start();
//! // ... do work ...
//! spinner.success("Done!");
//! ```

mod progress;
mod spinner;
mod style;
mod terminal;

pub use progress::{ProgressBar, ProgressBarBuilder};
pub use spinner::{Spinner, SpinnerBuilder};
pub use style::{icons, Style, StyledText};
pub use terminal::{
    clear_line, get_terminal_size, pad_text, truncate_text, Alignment, TerminalSize,
};

// Re-export commonly used items from dependencies
pub use colored::Colorize;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::{
        progress::{ProgressBar, ProgressBarBuilder},
        spinner::{Spinner, SpinnerBuilder},
        style::{Style, StyledText},
        Colorize,
    };
}
