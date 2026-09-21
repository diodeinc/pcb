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

        let copper_count = ecad
            .cad_data
            .layers
            .iter()
            .filter(|layer| crate::layers::is_copper(layer.layer_function))
            .count();

        Some(LayerStats::new(copper_count, ecad.cad_data.layers.len()))
    }

    /// Get net statistics
    ///
    /// Returns None if no ECAD section or no steps exist
    pub fn net_stats(&self) -> Option<NetStats> {
        let step = self.board_step()?;
        Some(NetStats::new(step.logical_nets.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_copper_function_counts_as_a_copper_layer() {
        let ipc = ipc2581::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="L1" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="L2" layerFunction="MIXED" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="L3" layerFunction="PLANE" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="L4" layerFunction="CONDFOIL" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let stats = IpcAccessor::new(&ipc).layer_stats().unwrap();

        assert_eq!((stats.copper_count, stats.total_count), (4, 5));
    }
}
