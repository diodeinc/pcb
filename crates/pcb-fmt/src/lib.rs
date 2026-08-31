use anyhow::Context;
use ruff_formatter::{IndentStyle, LineWidth};
use ruff_python_formatter::{PyFormatOptions, format_module_source};

pub struct RuffFormatter {
    options: PyFormatOptions,
}

impl Default for RuffFormatter {
    fn default() -> Self {
        Self {
            options: PyFormatOptions::default()
                .with_line_width(LineWidth::try_from(120).unwrap())
                .with_indent_style(IndentStyle::Space),
        }
    }
}

impl RuffFormatter {
    /// Format source code string directly, returning the formatted code.
    pub fn format_source(&self, source: &str) -> anyhow::Result<String> {
        format_module_source(source, self.options.clone())
            .context("Failed to format source")
            .map(|formatted| formatted.into_code())
    }
}
