use std::path::Path;

use anyhow::Context;
use ruff_formatter::{IndentStyle, LineWidth};
use ruff_python_formatter::{PyFormatOptions, format_module_source};
use similar::TextDiff;

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

    pub fn check_file(&self, file_path: &Path) -> anyhow::Result<bool> {
        let source = std::fs::read_to_string(file_path)?;
        let formatted = self.format_source(&source)?;
        Ok(source != formatted)
    }

    pub fn format_file(&self, file_path: &Path) -> anyhow::Result<()> {
        let source = std::fs::read_to_string(file_path)?;
        let formatted = self.format_source(&source)?;
        std::fs::write(file_path, formatted)?;
        Ok(())
    }

    pub fn diff_file(&self, file_path: &Path) -> anyhow::Result<String> {
        let source = std::fs::read_to_string(file_path)?;
        let formatted = self.format_source(&source)?;
        let diff = TextDiff::from_lines(source.as_str(), formatted.as_str());
        Ok(format!(
            "{}",
            diff.unified_diff().context_radius(3).header(
                &format!("old/{}", file_path.display()),
                &format!("new/{}", file_path.display())
            )
        ))
    }
}
