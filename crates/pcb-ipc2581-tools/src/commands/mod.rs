use anyhow::{Result, bail};

pub mod board_array;
pub mod board_array_auto;
pub mod bom;
#[cfg(feature = "cli")]
pub mod bom_edit;
pub mod cpl;
pub mod dfm;
pub mod fab_panel;
pub(crate) mod fabrication;
pub mod html_export;
pub mod ict;
pub mod info;
pub mod outline;
#[cfg(feature = "cli")]
pub mod render;
pub mod view;
pub mod warp;
pub mod warp_report;

/// A generated panel's IPC-2581 with its optional per-layer copper-balance
/// accounting.
#[derive(Debug, Clone)]
pub struct PanelCreation {
    pub xml: String,
    pub copper_balance: Option<crate::copper_balance::CopperBalanceReport>,
}

#[cfg(feature = "cli")]
impl PanelCreation {
    /// Report the balance, then write the panel to `output` or stdout.
    fn write(&self, output: &std::path::Path, what: &str) -> Result<()> {
        for line in self.copper_balance.iter().flat_map(|r| r.summary_lines()) {
            eprintln!("  {line}");
        }
        if output.as_os_str() == "-" {
            pcb_ui::write_stdout(|stdout| stdout.write_all(self.xml.as_bytes()))?;
            eprintln!("✓ Created IPC-2581 {what} on stdout");
        } else {
            crate::utils::file::save_ipc_file(output, &self.xml)?;
            eprintln!("✓ Created IPC-2581 {what} at {}", output.display());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EdgeInsetsMm {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl EdgeInsetsMm {
    pub const fn new(top: f64, right: f64, bottom: f64, left: f64) -> Self {
        Self {
            top,
            right,
            bottom,
            left,
        }
    }

    pub const fn all(value: f64) -> Self {
        Self::new(value, value, value, value)
    }

    pub fn from_css_shorthand(values: &[f64]) -> Result<Self> {
        Self::from_css_shorthand_named("board margin", values)
    }

    pub fn from_css_shorthand_named(name: &'static str, values: &[f64]) -> Result<Self> {
        match values {
            [all] => Ok(Self::all(*all)),
            [vertical, horizontal] => Ok(Self::new(*vertical, *horizontal, *vertical, *horizontal)),
            [top, horizontal, bottom] => Ok(Self::new(*top, *horizontal, *bottom, *horizontal)),
            [top, right, bottom, left] => Ok(Self::new(*top, *right, *bottom, *left)),
            _ => bail!("{name} expects 1 to 4 values"),
        }
    }

    /// Every side with its name, in CSS order.
    pub(crate) fn sides(self) -> [(&'static str, f64); 4] {
        [
            ("top", self.top),
            ("right", self.right),
            ("bottom", self.bottom),
            ("left", self.left),
        ]
    }

    pub(crate) fn horizontal_sum(self) -> f64 {
        self.left + self.right
    }

    pub(crate) fn vertical_sum(self) -> f64 {
        self.top + self.bottom
    }
}
