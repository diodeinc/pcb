use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use super::IpcAccessor;
use ipc2581::types::{
    PlatingStatus, StandardPrimitive, UserPrimitive, UserShapeType,
};

// MM diameter to integer mils conversion factor (for dedup hashing)
const MM_TO_MILS: f64 = 39_370.0; // 1mm = 39_370.0 mils

/// Round mm value to 4 decimal places for clean JSON output (avoids quantization noise).
fn round_mm(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HoleType {
    Through,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hole {
    /// Hole diameter (drill/finished) in mm
    pub hole_diameter_mm: f64,
    /// Pad diameter in mm (for circular pads only), if present
    pub shape_diameter_mm: Option<f64>,
    /// Only supported type for now is Through, but can be extended to other via types in the future.
    pub hole_type: HoleType,
    /// Number of identical holes/vias
    pub count: u32,
    /// Plating status of the hole
    pub plating_status: PlatingStatus,
}

impl Hole {
    pub fn new(
        hole_diameter_mm: f64,
        shape_diameter_mm: Option<f64>,
        via_type: HoleType,
        count: u32,
        plating_status: PlatingStatus,
    ) -> Self {
        Self {
            hole_diameter_mm,
            shape_diameter_mm,
            hole_type: via_type,
            count,
            plating_status,
        }
    }
}

impl<'a> IpcAccessor<'a> {
    pub fn holes(&self) -> Option<Vec<Hole>> {
        Some(extract_holes_from_step(&self))
    }
}


#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
struct HoleKey {
    hole_diameter_mils: i32,
    /// Pad diameter in mils (circular pads only) for grouping
    shape_diameter_mils: Option<i32>,
    plating_status: PlatingStatus,
}

/// Resolve a primitive ref Symbol to pad diameter (mm). Returns Some only for circular pads.
fn primitive_diameter_mm(content: &ipc2581::Content, prim_ref: ipc2581::Symbol) -> Option<f64> {
    for entry in &content.dictionary_standard.entries {
        if entry.id == prim_ref {
            return match &entry.primitive {
                StandardPrimitive::Circle(styled) => Some(round_mm(styled.shape.diameter)),
                _ => None,
            };
        }
    }
    for entry in &content.dictionary_user.entries {
        if entry.id == prim_ref {
            let UserPrimitive::UserSpecial(us) = &entry.primitive;
            for shape in &us.shapes {
                return match &shape.shape {
                    UserShapeType::Circle(c) => Some(round_mm(c.diameter)),
                    _ => None,
                };
            }
            return None;
        }
    }
    None
}

// Quantize coordinates to microns for stable deduplication across layers
const MM_TO_MICRONS: f64 = 1000.0;

fn extract_holes_from_step(ipc: &IpcAccessor) -> Vec<Hole> {
    let step = ipc.first_step().unwrap();

    let padstack_def_map = step.padstack_defs.iter().fold(HashMap::new(), |mut acc, ps| {
        acc.insert(ps.name, ps);
        acc
    });

    let content = ipc.ipc().content();
    let mut unique_positions: HashMap<HoleKey, HashSet<(i64, i64)>> = HashMap::new();

    for layer_feature in &step.layer_features {
        for set in &layer_feature.sets {
            let shape_diameter_mm = set.geometry.and_then(|geom_symb| {
                padstack_def_map.get(&geom_symb).and_then(|ps| {
                    ps.pad_defs.first().and_then(|pad_def| {
                        pad_def
                            .user_primitive_ref
                            .and_then(|sym| primitive_diameter_mm(content, sym))
                            .or_else(|| {
                                pad_def
                                    .standard_primitive_ref
                                    .and_then(|sym| primitive_diameter_mm(content, sym))
                            })
                    })
                })
            });
            let shape_diameter_mils = shape_diameter_mm.map(|d| (d * MM_TO_MILS) as i32);

            for hole in &set.holes {
                let hole_diameter_mils = (hole.diameter * MM_TO_MILS) as i32;
                let x_microns = (hole.x * MM_TO_MICRONS).round() as i64;
                let y_microns = (hole.y * MM_TO_MICRONS).round() as i64;
                let key = HoleKey {
                    hole_diameter_mils,
                    shape_diameter_mils,
                    plating_status: hole.plating_status,
                };
                unique_positions
                    .entry(key)
                    .or_default()
                    .insert((x_microns, y_microns));
            }
        }
    }

    unique_positions
        .into_iter()
        .map(|(key, positions)| {
            let count = positions.len() as u32;
            let hole_diameter_mm = round_mm(key.hole_diameter_mils as f64 / MM_TO_MILS);
            let shape_diameter_mm = key
                .shape_diameter_mils
                .map(|mils| round_mm(mils as f64 / MM_TO_MILS));
            Hole {
                hole_diameter_mm,
                shape_diameter_mm,
                hole_type: HoleType::Through,
                count,
                plating_status: key.plating_status,
            }
        })
        .collect()
}
