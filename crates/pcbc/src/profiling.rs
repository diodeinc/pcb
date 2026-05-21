//! Build profiling infrastructure.
//!
//! When `--profile <path>` is passed, this module sets up a Chrome-compatible
//! trace output that can be visualized in `chrome://tracing` or https://ui.perfetto.dev/

use std::path::PathBuf;
use tracing_subscriber::prelude::*;

/// Guard that flushes the trace file when dropped.
/// Must be held for the duration of the profiled operation.
pub struct ProfileGuard {
    _guard: tracing_chrome::FlushGuard,
}

/// Initialize profiling and return a guard that must be held until profiling should stop.
///
/// When the guard is dropped, the trace file is flushed and closed.
///
/// Returns `None` if `output_path` is `None`.
pub fn init(output_path: Option<PathBuf>) -> Option<ProfileGuard> {
    let output_path = output_path?;

    let (chrome_layer, guard) = tracing_chrome::ChromeLayerBuilder::new()
        .file(output_path)
        .include_args(true)
        .build();

    // Use try_init to avoid panicking if another subscriber is already set
    let _ = tracing_subscriber::registry().with(chrome_layer).try_init();

    Some(ProfileGuard { _guard: guard })
}
