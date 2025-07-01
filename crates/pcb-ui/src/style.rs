use colored::{ColoredString, Colorize};

/// Predefined styles for UI components
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Default style (no color)
    Default,
    /// Green style (for success)
    Green,
    /// Yellow style (for warnings)
    Yellow,
    /// Red style (for errors)
    Red,
    /// Blue style (for info)
    Blue,
    /// Cyan style (for highlights)
    Cyan,
}

/// Extension trait for applying consistent styles to text
pub trait StyledText {
    /// Apply success styling (green with checkmark)
    fn success(self) -> String;

    /// Apply error styling (red with cross)
    fn error(self) -> String;

    /// Apply warning styling (yellow with exclamation)
    fn warning(self) -> String;

    /// Apply info styling (blue)
    fn info(self) -> String;

    /// Apply the specified style
    fn with_style(self, style: Style) -> ColoredString;
}

impl<T: AsRef<str>> StyledText for T {
    fn success(self) -> String {
        format!("{} {}", "✓".green(), self.as_ref().green())
    }

    fn error(self) -> String {
        format!("{} {}", "✗".red(), self.as_ref().red())
    }

    fn warning(self) -> String {
        format!("{} {}", "!".yellow(), self.as_ref().yellow())
    }

    fn info(self) -> String {
        format!("{} {}", "ℹ".blue(), self.as_ref().blue())
    }

    fn with_style(self, style: Style) -> ColoredString {
        let text = self.as_ref();
        match style {
            Style::Default => text.normal(),
            Style::Green => text.green(),
            Style::Yellow => text.yellow(),
            Style::Red => text.red(),
            Style::Blue => text.blue(),
            Style::Cyan => text.cyan(),
        }
    }
}

/// Common status icons used across the UI
pub mod icons {
    use colored::Colorize;

    /// Success checkmark (green)
    pub fn success() -> String {
        "✓".green().to_string()
    }

    /// Error cross (red)
    pub fn error() -> String {
        "✗".red().to_string()
    }

    /// Warning exclamation (yellow)
    pub fn warning() -> String {
        "!".yellow().to_string()
    }

    /// Info icon (blue)
    pub fn info() -> String {
        "ℹ".blue().to_string()
    }

    /// Bullet point
    pub fn bullet() -> &'static str {
        "•"
    }

    /// Arrow right
    pub fn arrow() -> &'static str {
        "→"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_styled_text() {
        let text = "Hello";

        // These should compile and not panic
        let _ = text.success();
        let _ = text.error();
        let _ = text.warning();
        let _ = text.info();
        let _ = text.with_style(Style::Cyan);
    }
}
