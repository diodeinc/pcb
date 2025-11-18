use ipc2581::types::{PolyStep, Step};
use serde::{Deserialize, Serialize};

use super::IpcAccessor;
use crate::utils::Length;

/// Board physical dimensions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardDimensions {
    pub width: Length,
    pub height: Length,
}

impl BoardDimensions {
    pub fn new(width_mm: f64, height_mm: f64) -> Self {
        Self {
            width: Length::from_mm(width_mm),
            height: Length::from_mm(height_mm),
        }
    }

    // Legacy accessors for backward compatibility
    pub fn width_mm(&self) -> f64 {
        self.width.mm()
    }

    pub fn height_mm(&self) -> f64 {
        self.height.mm()
    }

    pub fn width_inch(&self) -> f64 {
        self.width.inch()
    }

    pub fn height_inch(&self) -> f64 {
        self.height.inch()
    }
}

/// Board stackup information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackupInfo {
    pub thickness: Option<Length>,
    pub layer_count: usize,
}

impl StackupInfo {
    // Legacy accessor for backward compatibility
    pub fn overall_thickness_mm(&self) -> Option<f64> {
        self.thickness.map(|t| t.mm())
    }
}

impl<'a> IpcAccessor<'a> {
    /// Extract board physical dimensions from the profile outline
    ///
    /// Calculates the bounding box of the board profile polygon.
    /// Returns None if no ECAD section, no step, or no profile exists.
    ///
    /// Note: Currently only checks arc endpoints, not the actual arc path.
    /// For curved boards, this may slightly underestimate dimensions.
    pub fn board_dimensions(&self) -> Option<BoardDimensions> {
        let step = self.first_step()?;
        calculate_board_dimensions(step)
    }

    /// Extract stackup information (thickness and layer count)
    pub fn stackup_info(&self) -> Option<StackupInfo> {
        let ecad = self.ecad()?;
        let stackup = ecad.cad_data.stackups.first()?;

        Some(StackupInfo {
            thickness: stackup.overall_thickness.map(Length::from),
            layer_count: stackup.layers.len(),
        })
    }
}

/// Calculate board dimensions from profile polygon
///
/// TODO: Make arc-aware - currently only checks arc endpoints, not the actual arc path
/// For curves, we should calculate the true bounding box including arc bulge
fn calculate_board_dimensions(step: &Step) -> Option<BoardDimensions> {
    let profile = step.profile.as_ref()?;

    let mut min_x = f64::MAX;
    let mut max_x = f64::MIN;
    let mut min_y = f64::MAX;
    let mut max_y = f64::MIN;

    // Check main outline starting point
    min_x = min_x.min(profile.polygon.begin.x);
    max_x = max_x.max(profile.polygon.begin.x);
    min_y = min_y.min(profile.polygon.begin.y);
    max_y = max_y.max(profile.polygon.begin.y);

    // Process all polygon steps
    for step in &profile.polygon.steps {
        match step {
            PolyStep::Segment(seg) => {
                min_x = min_x.min(seg.point.x);
                max_x = max_x.max(seg.point.x);
                min_y = min_y.min(seg.point.y);
                max_y = max_y.max(seg.point.y);
            }
            PolyStep::Curve(curve) => {
                // TODO: Calculate actual arc bounding box, not just endpoints
                min_x = min_x.min(curve.point.x);
                max_x = max_x.max(curve.point.x);
                min_y = min_y.min(curve.point.y);
                max_y = max_y.max(curve.point.y);
            }
        }
    }

    let width = max_x - min_x;
    let height = max_y - min_y;

    if width > 0.0 && height > 0.0 {
        Some(BoardDimensions::new(width, height))
    } else {
        None
    }
}
