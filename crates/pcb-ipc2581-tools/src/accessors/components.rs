use ipc2581::types::{MountType, Step};
use serde::{Deserialize, Serialize};

use super::IpcAccessor;

/// Component statistics broken down by mount type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentStats {
    pub total: usize,
    pub smt: usize,
    pub tht: usize,
    pub other: usize,
}

impl ComponentStats {
    pub fn new(total: usize, smt: usize, tht: usize, other: usize) -> Self {
        Self {
            total,
            smt,
            tht,
            other,
        }
    }
}

impl<'a> IpcAccessor<'a> {
    /// Get component statistics broken down by mount type
    ///
    /// Returns None if no ECAD section or no steps exist
    pub fn component_stats(&self) -> Option<ComponentStats> {
        let step = self.first_step()?;
        Some(count_components_by_mount_type(step))
    }
}

/// Count components by mount type (SMT, THT, Other)
fn count_components_by_mount_type(step: &Step) -> ComponentStats {
    let mut smt_count = 0;
    let mut tht_count = 0;
    let mut other_count = 0;

    for component in &step.components {
        match component.mount_type {
            Some(ref mt) => match mt {
                MountType::Smt => smt_count += 1,
                MountType::Tht => tht_count += 1,
                _ => other_count += 1,
            },
            None => other_count += 1,
        }
    }

    let total = smt_count + tht_count + other_count;
    ComponentStats::new(total, smt_count, tht_count, other_count)
}
