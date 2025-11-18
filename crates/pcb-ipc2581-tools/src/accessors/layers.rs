use ipc2581::types::{Layer, LayerFunction};
use serde::{Deserialize, Serialize};

use super::IpcAccessor;

/// Layer count statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayerStats {
    pub copper_count: usize,
    pub total_count: usize,
}

impl LayerStats {
    pub fn new(copper_count: usize, total_count: usize) -> Self {
        Self {
            copper_count,
            total_count,
        }
    }
}

/// Net statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetStats {
    pub count: usize,
}

impl NetStats {
    pub fn new(count: usize) -> Self {
        Self { count }
    }
}

impl<'a> IpcAccessor<'a> {
    /// Get layer statistics (copper count and total count)
    ///
    /// Returns None if no ECAD section exists
    pub fn layer_stats(&self) -> Option<LayerStats> {
        let ecad = self.ecad()?;

        let copper_count = count_layers_by_function(
            &ecad.cad_data.layers,
            &[
                LayerFunction::Conductor,
                LayerFunction::Signal,
                LayerFunction::Plane,
            ],
        );

        Some(LayerStats::new(copper_count, ecad.cad_data.layers.len()))
    }

    /// Get net statistics
    ///
    /// Returns None if no ECAD section or no steps exist
    pub fn net_stats(&self) -> Option<NetStats> {
        let step = self.first_step()?;
        Some(NetStats::new(step.logical_nets.len()))
    }
}

/// Count layers by specific functions
fn count_layers_by_function(layers: &[Layer], functions: &[LayerFunction]) -> usize {
    layers
        .iter()
        .filter(|layer| functions.contains(&layer.layer_function))
        .count()
}
