use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use super::IpcAccessor;
use ipc2581::types::{PlatingStatus, Step};

// MM diameter to integer mils conversion factor
const MM_TO_MILS: f64 = 39_370.0; // 1mm = 39_370.0 mils


#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ViaType {
    Through,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Via {
    /// Hole diameter after plating
    pub hole_diameter_mm: f64,
    /// Only supported type for now is Through, but can be extended to other via types in the future.
    pub via_type: ViaType,
    /// Number of identical vias
    pub count: u32,
}

impl Via {
    pub fn new(hole_diameter_mm: f64, via_type: ViaType, count: u32) -> Self {
        Self {
            hole_diameter_mm,
            via_type,
            count,
        }
    }
}

impl<'a> IpcAccessor<'a> {
    pub fn vias(&self) -> Option<Vec<Via>> {
        let step = self.first_step()?;
        Some(extract_vias_from_step(&step))
    }
}

fn extract_vias_from_step(step: &Step) -> Vec<Via> {
    let mut counts: HashMap<i32, u32> = HashMap::new();

    for layer_feature in &step.layer_features {
        for set in &layer_feature.sets {
            for hole in &set.holes {
                if hole.plating_status != PlatingStatus::Via {
                    continue;
                }

                let diameter_mils = (hole.diameter * MM_TO_MILS) as i32;
                *counts.entry(diameter_mils).or_insert(0) += 1;
            }
        }
    }

    counts
        .into_iter()
        .map(|(diameter_mils, count)| Via {
            hole_diameter_mm: diameter_mils as f64 / MM_TO_MILS,
            via_type: ViaType::Through,
            count,
        })
        .collect()
}
