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

    pub(crate) fn horizontal_sum(self) -> f64 {
        self.left + self.right
    }

    pub(crate) fn vertical_sum(self) -> f64 {
        self.top + self.bottom
    }
}
