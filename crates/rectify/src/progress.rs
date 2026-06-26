//! Progress-bar plumbing shared by `bench` and `audit`. Wraps `indicatif` so
//! callers get a consistent look-and-feel and so both commands disable the
//! bar in the same set of situations (non-TTY stderr, `jsonl` output, or
//! `RUST_LOG` being set — where log lines would otherwise scramble the bar's
//! in-place redraw).

use std::time::Duration;

use indicatif::{ProgressBar, ProgressStyle};

const TICK_INTERVAL_MS: u64 = 120;

/// Build a progress bar for a batch of `total` items. Returns a hidden bar
/// (no-op updates, no draw) when we shouldn't render one, so callers can
/// `.inc(1)` unconditionally.
///
/// `label` is the short activity verb shown at the bar's left edge — e.g.
/// "audit" or "bench". The caller is expected to call `.inc(1)` per item
/// and `.finish_and_clear()` (or `.finish_with_message(...)`) at the end.
pub fn batch_bar(total: u64, label: &str, disable: bool) -> ProgressBar {
    if disable || !should_render() || total == 0 {
        return ProgressBar::hidden();
    }
    let bar = ProgressBar::new(total);
    let template = format!(
        "{label:<6} {{bar:40.cyan/blue}} {{pos:>5}}/{{len}} ({{percent}}%) {{elapsed_precise}} eta {{eta_precise}} {{msg}}",
    );
    let style = ProgressStyle::with_template(&template)
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("=>-");
    bar.set_style(style);
    bar.enable_steady_tick(Duration::from_millis(TICK_INTERVAL_MS));
    bar
}

/// Show the bar only when stderr is a TTY and `RUST_LOG` is unset.
fn should_render() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
        && !matches!(std::env::var_os("RUST_LOG"), Some(v) if !v.is_empty())
}
