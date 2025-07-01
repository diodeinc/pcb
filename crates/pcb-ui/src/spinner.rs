use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use once_cell::sync::Lazy;
use std::time::Duration;

use crate::style::Style;

static MULTI: Lazy<MultiProgress> = Lazy::new(MultiProgress::new);

/// Default spinner tick characters (same as used in CLI)
const DEFAULT_TICK_CHARS: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";

/// A spinner for showing indeterminate progress
pub struct Spinner {
    progress_bar: ProgressBar,
}

impl Spinner {
    /// Create a new spinner with the given message
    pub fn builder(message: impl Into<String>) -> SpinnerBuilder {
        SpinnerBuilder::new(message)
    }

    /// Update the spinner message
    pub fn set_message(&self, message: impl Into<String>) {
        self.progress_bar.set_message(message.into());
    }

    /// Finish the spinner with a success message
    pub fn success(self, message: impl Into<String>) {
        let msg = message.into();
        self.progress_bar
            .finish_with_message(format!("{} {}", "✓".green(), msg));
    }

    /// Finish the spinner with an error message
    pub fn error(self, message: impl Into<String>) {
        let msg = message.into();
        self.progress_bar
            .finish_with_message(format!("{} {}", "✗".red(), msg));
    }

    /// Finish the spinner with a warning message
    pub fn warning(self, message: impl Into<String>) {
        let msg = message.into();
        self.progress_bar
            .finish_with_message(format!("{} {}", "!".yellow(), msg));
    }

    /// Finish and clear the spinner
    pub fn finish(self) {
        self.progress_bar.finish_and_clear();
    }

    /// Finish with a custom message (no icon)
    pub fn finish_with_message(self, message: impl Into<String>) {
        self.progress_bar.finish_with_message(message.into());
    }

    /// Temporarily hide the spinner (useful when prompting for input)
    pub fn suspend<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        self.progress_bar
            .set_draw_target(ProgressDrawTarget::hidden());
        let result = f();
        self.progress_bar
            .set_draw_target(ProgressDrawTarget::stderr());
        self.progress_bar.tick();
        result
    }
}

/// Builder for creating customized spinners
pub struct SpinnerBuilder {
    message: String,
    tick_chars: String,
    tick_interval: Duration,
    style: Style,
    hidden: bool,
}

impl SpinnerBuilder {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            tick_chars: DEFAULT_TICK_CHARS.to_string(),
            tick_interval: Duration::from_millis(100),
            style: Style::Green,
            hidden: false,
        }
    }

    /// Set custom tick characters for the spinner animation
    pub fn tick_chars(mut self, chars: impl Into<String>) -> Self {
        self.tick_chars = chars.into();
        self
    }

    /// Set the tick interval (default: 100ms)
    pub fn tick_interval(mut self, interval: Duration) -> Self {
        self.tick_interval = interval;
        self
    }

    /// Set the spinner style/color
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Hide the spinner (useful for non-interactive environments)
    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = hidden;
        self
    }

    /// Start the spinner
    pub fn start(self) -> Spinner {
        let progress_bar = MULTI.add(ProgressBar::new_spinner());

        let template = match self.style {
            Style::Green => "{spinner:.green} {msg}",
            Style::Yellow => "{spinner:.yellow} {msg}",
            Style::Red => "{spinner:.red} {msg}",
            Style::Blue => "{spinner:.blue} {msg}",
            Style::Cyan => "{spinner:.cyan} {msg}",
            Style::Default => "{spinner} {msg}",
        };

        progress_bar.set_style(
            ProgressStyle::default_spinner()
                .template(template)
                .unwrap()
                .tick_chars(&self.tick_chars),
        );

        progress_bar.set_message(self.message);
        progress_bar.enable_steady_tick(self.tick_interval);

        if self.hidden {
            progress_bar.set_draw_target(ProgressDrawTarget::hidden());
        }

        Spinner { progress_bar }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spinner_creation() {
        let spinner = Spinner::builder("Testing").start();
        spinner.finish();
    }

    #[test]
    fn test_spinner_builder() {
        let spinner = Spinner::builder("Custom spinner")
            .tick_chars("◐◓◑◒")
            .style(Style::Blue)
            .tick_interval(Duration::from_millis(200))
            .start();
        spinner.success("Done!");
    }
}
