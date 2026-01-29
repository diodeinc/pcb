use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use super::IpcAccessor;
use ipc2581::types::{Step};

// MM diameter to integer mils conversion factor
const MM_TO_MILS: f64 = 39_370.0; // 1mm = 39_370.0 mils
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hole {
    /// Hole diameter after plating
    pub hole_diameter_mm: f64,
    /// Number of identical holes
    pub count: u32,
}

impl Hole {
    pub fn new(hole_diameter_mm: f64, count: u32) -> Self {
        Self {
            hole_diameter_mm,
            count,
        }
    }
}

impl<'a> IpcAccessor<'a> {
    pub fn holes(&self) -> Option<Vec<Hole>> {
        let step = self.first_step()?;
        Some(extract_vias_from_step(&step))
    }
}

fn extract_vias_from_step(step: &Step) -> Vec<Hole> {
    let mut counts: HashMap<i32, u32> = HashMap::new();

    for layer_feature in &step.layer_features {
        for set in &layer_feature.sets {
            for hole in &set.holes {
                let diameter_mils = (hole.diameter * MM_TO_MILS) as i32;
                *counts.entry(diameter_mils).or_insert(0) += 1;
            }
        }
    }

    counts
        .into_iter()
        .map(|(diameter_mils, count)| Hole {
            hole_diameter_mm: diameter_mils as f64 / MM_TO_MILS,
            count,
        })
        .collect()
}