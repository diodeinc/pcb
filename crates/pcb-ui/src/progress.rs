use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar as IndicatifBar, ProgressDrawTarget, ProgressStyle};
use once_cell::sync::Lazy;
use std::time::Duration;

use crate::style::Style;

static MULTI: Lazy<MultiProgress> = Lazy::new(MultiProgress::new);

/// Default tick characters for progress bars (includes completion checkmark)
const DEFAULT_TICK_CHARS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏✓";

/// A progress bar for showing determinate progress
pub struct ProgressBar {
    bar: IndicatifBar,
    total: u64,
}

impl ProgressBar {
    /// Create a new progress bar with the given total steps
    pub fn builder(total: u64) -> ProgressBarBuilder {
        ProgressBarBuilder::new(total)
    }

    /// Set the current position
    pub fn set_position(&self, pos: u64) {
        self.bar.set_position(pos);
    }

    /// Increment the position by the given amount
    pub fn inc(&self, delta: u64) {
        self.bar.inc(delta);
    }

    /// Set the message displayed with the progress bar
    pub fn set_message(&self, message: impl Into<String>) {
        self.bar.set_message(message.into());
    }

    /// Get the current position
    pub fn position(&self) -> u64 {
        self.bar.position()
    }

    /// Get the total number of steps
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Calculate and return the percentage complete (0-100)
    pub fn percentage(&self) -> u8 {
        ((self.position() as f64 / self.total as f64) * 100.0) as u8
    }

    /// Finish the progress bar with a success message
    pub fn success(self, message: impl Into<String>) {
        let msg = message.into();
        self.bar
            .finish_with_message(format!("{} {}", "✓".green(), msg));
    }

    /// Finish the progress bar with an error message
    pub fn error(self, message: impl Into<String>) {
        let msg = message.into();
        self.bar
            .finish_with_message(format!("{} {}", "✗".red(), msg));
    }

    /// Finish and clear the progress bar
    pub fn finish(self) {
        self.bar.finish_and_clear();
    }

    /// Finish with a custom message
    pub fn finish_with_message(self, message: impl Into<String>) {
        self.bar.finish_with_message(message.into());
    }

    /// Temporarily hide the progress bar (useful when showing other output)
    pub fn suspend<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        self.bar.set_draw_target(ProgressDrawTarget::hidden());
        let result = f();
        self.bar.set_draw_target(ProgressDrawTarget::stderr());
        self.bar.tick();
        result
    }
}

/// Builder for creating customized progress bars
pub struct ProgressBarBuilder {
    total: u64,
    message: Option<String>,
    style: Style,
    template: Option<String>,
    progress_chars: String,
    tick_chars: String,
    tick_interval: Option<Duration>,
    hidden: bool,
}

impl ProgressBarBuilder {
    fn new(total: u64) -> Self {
        Self {
            total,
            message: None,
            style: Style::Green,
            template: None,
            progress_chars: "=> ".to_string(),
            tick_chars: DEFAULT_TICK_CHARS.to_string(),
            tick_interval: Some(Duration::from_millis(100)),
            hidden: false,
        }
    }

    /// Set the initial message
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }

    /// Set the progress bar style/color
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Set a custom template (advanced usage)
    /// Default: "|{bar:40.green/gray}| {spinner:.green} [{pos}/{len}] {msg}"
    pub fn template(mut self, template: impl Into<String>) -> Self {
        self.template = Some(template.into());
        self
    }

    /// Set the progress characters (default: "=> ")
    pub fn progress_chars(mut self, chars: impl Into<String>) -> Self {
        self.progress_chars = chars.into();
        self
    }

    /// Set the tick characters for the spinner
    pub fn tick_chars(mut self, chars: impl Into<String>) -> Self {
        self.tick_chars = chars.into();
        self
    }

    /// Set the tick interval (None to disable ticking)
    pub fn tick_interval(mut self, interval: Option<Duration>) -> Self {
        self.tick_interval = interval;
        self
    }

    /// Hide the progress bar (useful for non-interactive environments)
    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    /// Start the progress bar
    pub fn start(self) -> ProgressBar {
        let bar = MULTI.add(IndicatifBar::new(self.total));

        let template = self.template.unwrap_or_else(|| {
            match self.style {
                Style::Green => "|{bar:40.green/gray}| {spinner:.green} [{pos}/{len}] {msg}",
                Style::Yellow => "|{bar:40.yellow/gray}| {spinner:.yellow} [{pos}/{len}] {msg}",
                Style::Red => "|{bar:40.red/gray}| {spinner:.red} [{pos}/{len}] {msg}",
                Style::Blue => "|{bar:40.blue/gray}| {spinner:.blue} [{pos}/{len}] {msg}",
                Style::Cyan => "|{bar:40.cyan/gray}| {spinner:.cyan} [{pos}/{len}] {msg}",
                Style::Default => "|{bar:40.white/gray}| {spinner} [{pos}/{len}] {msg}",
            }
            .to_string()
        });

        bar.set_style(
            ProgressStyle::default_bar()
                .template(&template)
                .unwrap()
                .progress_chars(&self.progress_chars)
                .tick_chars(&self.tick_chars),
        );

        if let Some(message) = self.message {
            bar.set_message(message);
        }

        if let Some(interval) = self.tick_interval {
            bar.enable_steady_tick(interval);
        }

        if self.hidden {
            bar.set_draw_target(ProgressDrawTarget::hidden());
        }

        ProgressBar {
            bar,
            total: self.total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_bar_creation() {
        let pb = ProgressBar::builder(100).message("Testing").start();
        assert_eq!(pb.total(), 100);
        assert_eq!(pb.position(), 0);
        pb.finish();
    }

    #[test]
    fn test_progress_bar_increment() {
        let pb = ProgressBar::builder(100).start();
        pb.inc(25);
        assert_eq!(pb.position(), 25);
        assert_eq!(pb.percentage(), 25);
        pb.finish();
    }

    #[test]
    fn test_progress_bar_builder() {
        let pb = ProgressBar::builder(50)
            .message("Custom progress")
            .style(Style::Blue)
            .progress_chars("#>-")
            .hidden(true)
            .start();
        pb.set_position(25);
        assert_eq!(pb.percentage(), 50);
        pb.success("Complete!");
    }
}
