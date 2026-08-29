use terminal_size::terminal_size as get_size;
use unicode_width::UnicodeWidthChar;

/// Terminal dimensions
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub width: u16,
    pub height: u16,
}

impl TerminalSize {
    /// Get the current terminal size
    pub fn current() -> Option<Self> {
        get_size().map(|(w, h)| Self {
            width: w.0,
            height: h.0,
        })
    }

    /// Get the terminal width, with a fallback default
    pub fn width_or_default(default: u16) -> u16 {
        Self::current().map(|s| s.width).unwrap_or(default)
    }

    /// Get the terminal height, with a fallback default
    pub fn height_or_default(default: u16) -> u16 {
        Self::current().map(|s| s.height).unwrap_or(default)
    }
}

/// Get the current terminal size
pub fn get_terminal_size() -> Option<TerminalSize> {
    TerminalSize::current()
}

/// Clear the current line
pub fn clear_line() {
    print!("\r\x1b[K");
}

/// Calculate the display width of a string, accounting for Unicode characters
pub fn text_width(text: &str) -> usize {
    text.chars()
        .map(|c| UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// Truncate text to fit within the specified width, adding ellipsis if needed
pub fn truncate_text(text: &str, max_width: usize) -> String {
    if max_width < 3 {
        return ".".repeat(max_width.min(text.len()));
    }

    let width = text_width(text);
    if width <= max_width {
        return text.to_string();
    }

    let ellipsis = "...";
    let target_width = max_width - ellipsis.len();
    let mut result = String::new();
    let mut current_width = 0;

    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width + ch_width > target_width {
            break;
        }
        result.push(ch);
        current_width += ch_width;
    }

    result.push_str(ellipsis);
    result
}

/// Pad text to the specified width
pub fn pad_text(text: &str, width: usize, align: Alignment) -> String {
    let text_width = text_width(text);
    if text_width >= width {
        return text.to_string();
    }

    let padding = width - text_width;
    match align {
        Alignment::Left => format!("{}{}", text, " ".repeat(padding)),
        Alignment::Right => format!("{}{}", " ".repeat(padding), text),
        Alignment::Center => {
            let left_pad = padding / 2;
            let right_pad = padding - left_pad;
            format!("{}{}{}", " ".repeat(left_pad), text, " ".repeat(right_pad))
        }
    }
}

/// Text alignment options
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Right,
    Center,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_width() {
        assert_eq!(text_width("hello"), 5);
        assert_eq!(text_width("café"), 4);
        assert_eq!(text_width("你好"), 4); // Chinese characters are typically width 2
    }

    #[test]
    fn test_truncate_text() {
        assert_eq!(truncate_text("hello world", 20), "hello world");
        assert_eq!(truncate_text("hello world", 8), "hello...");
        assert_eq!(truncate_text("hello", 3), "...");
        assert_eq!(truncate_text("hello", 2), "..");
        assert_eq!(truncate_text("hello", 1), ".");
    }

    #[test]
    fn test_pad_text() {
        assert_eq!(pad_text("hi", 5, Alignment::Left), "hi   ");
        assert_eq!(pad_text("hi", 5, Alignment::Right), "   hi");
        assert_eq!(pad_text("hi", 5, Alignment::Center), " hi  ");
        assert_eq!(pad_text("hello", 5, Alignment::Left), "hello");
    }
}
