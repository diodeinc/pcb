use std::collections::BTreeMap;

use ipc2581::types::{PlatingStatus, Step};
use serde::{Deserialize, Serialize};

use super::IpcAccessor;

/// Drill hole statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrillStats {
    pub total_holes: usize,
    pub unique_sizes: usize,
    /// Per-type distribution: via, plated, non-plated
    pub distribution: Vec<DrillTypeDistribution>,
}

/// Distribution of holes for a single plating type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrillTypeDistribution {
    pub hole_type: DrillHoleType,
    pub total: usize,
    /// Unique diameters sorted ascending, each with count
    pub sizes: Vec<DrillSize>,
}

/// Categorized hole type
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DrillHoleType {
    Via,
    Plated,
    NonPlated,
}

impl DrillHoleType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Via => "Via",
            Self::Plated => "Plated (PTH)",
            Self::NonPlated => "Non-Plated (NPTH)",
        }
    }
}

/// A unique drill size with its count
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrillSize {
    pub diameter_mm: f64,
    pub count: usize,
}

impl<'a> IpcAccessor<'a> {
    /// Get drill hole statistics with per-type distribution
    ///
    /// Returns None if no ECAD section or no steps exist
    pub fn drill_stats(&self) -> Option<DrillStats> {
        let step = self.first_step()?;
        Some(collect_drill_info(step))
    }
}

/// Collect drill holes grouped by plating type, with per-diameter counts
fn collect_drill_info(step: &Step) -> DrillStats {
    // BTreeMap<DrillHoleType, BTreeMap<diameter_mils, count>>
    let mut by_type: BTreeMap<DrillHoleType, BTreeMap<i32, (f64, usize)>> = BTreeMap::new();
    let mut total_holes = 0usize;
    let mut all_diameters = std::collections::HashSet::new();

    for layer_feature in &step.layer_features {
        for set in &layer_feature.sets {
            for hole in &set.holes {
                total_holes += 1;
                let diameter_mils = (hole.diameter * 39370.0) as i32;
                all_diameters.insert(diameter_mils);

                let hole_type = match hole.plating_status {
                    PlatingStatus::Via => DrillHoleType::Via,
                    PlatingStatus::Plated => DrillHoleType::Plated,
                    PlatingStatus::NonPlated => DrillHoleType::NonPlated,
                };

                let entry = by_type
                    .entry(hole_type)
                    .or_default()
                    .entry(diameter_mils)
                    .or_insert((hole.diameter, 0));
                entry.1 += 1;
            }
        }
    }

    let distribution = by_type
        .into_iter()
        .map(|(hole_type, sizes_map)| {
            let mut total = 0usize;
            let sizes: Vec<DrillSize> = sizes_map
                .into_values()
                .map(|(diameter_mm, count)| {
                    total += count;
                    DrillSize { diameter_mm, count }
                })
                .collect();
            DrillTypeDistribution {
                hole_type,
                total,
                sizes,
            }
        })
        .collect();

    DrillStats {
        total_holes,
        unique_sizes: all_diameters.len(),
        distribution,
    }
}
