use std::collections::HashSet;

use ipc2581::types::Step;
use serde::{Deserialize, Serialize};

use super::IpcAccessor;

/// Drill hole statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrillStats {
    pub total_holes: usize,
    pub unique_sizes: usize,
}

impl DrillStats {
    pub fn new(total_holes: usize, unique_sizes: usize) -> Self {
        Self {
            total_holes,
            unique_sizes,
        }
    }
}

impl<'a> IpcAccessor<'a> {
    /// Get drill hole statistics (total count and unique sizes)
    ///
    /// Returns None if no ECAD section or no steps exist
    pub fn drill_stats(&self) -> Option<DrillStats> {
        let step = self.first_step()?;
        Some(count_drill_info(step))
    }
}

/// Count drill holes and unique diameters
fn count_drill_info(step: &Step) -> DrillStats {
    let mut total_holes = 0;
    let mut unique_diameters = HashSet::new();

    for layer_feature in &step.layer_features {
        for set in &layer_feature.sets {
            for hole in &set.holes {
                total_holes += 1;
                // Convert diameter to integer mils to avoid floating point comparison issues
                let diameter_mils = (hole.diameter * 39370.0) as i32;
                unique_diameters.insert(diameter_mils);
            }
        }
    }

    DrillStats::new(total_holes, unique_diameters.len())
}
