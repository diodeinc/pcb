use std::collections::BTreeMap;

use ipc2581::types::LayerFunction;
use pcb_ir::dialects::ipc::{FeatureKind, PlatingKind, View};
use serde::{Deserialize, Serialize};

use super::IpcAccessor;
use crate::geometry;

type GeometryDocument =
    pcb_ir::dialects::ipc::Document<ipc2581::Symbol, ipc2581::types::LayerFunction>;

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
    /// Get board-local drill hole statistics with per-type distribution.
    pub fn board_drill_stats(&self) -> Option<DrillStats> {
        self.drill_stats_for_view(View::Board)
    }

    /// Get array-local drill hole statistics, excluding repeated board drills.
    pub fn board_array_drill_stats(&self) -> Option<DrillStats> {
        self.drill_stats_for_view(View::ArrayLocal)
    }

    /// Get flattened board-array drill statistics, including repeated board drills
    /// and array-local drill features.
    pub fn board_array_flattened_drill_stats(&self) -> Option<DrillStats> {
        self.drill_stats_for_view(View::ArrayFlattened)
    }

    fn drill_stats_for_view(&self, view: View) -> Option<DrillStats> {
        let ecad = self.ecad()?;
        let mut collector = DrillStatsCollector::default();
        let mut has_drill_layer = false;

        for layer in &ecad.cad_data.layers {
            if layer.layer_function != LayerFunction::Drill {
                continue;
            }
            has_drill_layer = true;
            let layer_name = self.ipc.resolve(layer.name);
            let Ok(doc) = geometry::extract_layer_for_view(self.ipc, layer_name, view) else {
                continue;
            };
            collect_drill_info(&doc, &mut collector);
        }

        has_drill_layer.then(|| collector.finish())
    }
}

#[derive(Default)]
struct DrillStatsCollector {
    by_type: BTreeMap<DrillHoleType, BTreeMap<i32, (f64, usize)>>,
    total_holes: usize,
    all_diameters: std::collections::HashSet<i32>,
}

impl DrillStatsCollector {
    fn add_hole(&mut self, diameter_mm: f64, hole_type: DrillHoleType) {
        self.total_holes += 1;
        let diameter_mils = (diameter_mm * 39370.0) as i32;
        self.all_diameters.insert(diameter_mils);
        let entry = self
            .by_type
            .entry(hole_type)
            .or_default()
            .entry(diameter_mils)
            .or_insert((diameter_mm, 0));
        entry.1 += 1;
    }

    fn finish(self) -> DrillStats {
        let distribution = self
            .by_type
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
            total_holes: self.total_holes,
            unique_sizes: self.all_diameters.len(),
            distribution,
        }
    }
}

/// Collect drill holes grouped by plating type, with per-diameter counts.
fn collect_drill_info(doc: &GeometryDocument, collector: &mut DrillStatsCollector) {
    for layer in &doc.layers {
        for feature in layer.features.slice(&doc.features) {
            if let Some((diameter_mm, hole_type)) = drill_hole(feature) {
                collector.add_hole(diameter_mm, hole_type);
            }
        }
    }
}

fn drill_hole(
    feature: &pcb_ir::dialects::ipc::Feature<ipc2581::Symbol>,
) -> Option<(f64, DrillHoleType)> {
    if !feature.is_drill_like()
        || feature.kind != FeatureKind::Hole
        || feature.outer_diameter <= 0.0
    {
        return None;
    }

    Some((
        feature.outer_diameter,
        drill_hole_type(feature.intent.plating),
    ))
}

fn drill_hole_type(plating: PlatingKind) -> DrillHoleType {
    match plating {
        PlatingKind::Via | PlatingKind::ViaCapped => DrillHoleType::Via,
        PlatingKind::Plated => DrillHoleType::Plated,
        PlatingKind::NonPlated | PlatingKind::None | PlatingKind::Unknown => {
            DrillHoleType::NonPlated
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drill_stats_are_scoped_by_layout_view() {
        let ipc = ipc2581::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="array"/>
    <LayerRef name="Drill"/>
    <LayerRef name="Board_Array_Drill"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
      <Layer name="Board_Array_Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <LayerFeature layerRef="Drill">
          <Set>
            <Hole name="board_via" diameter="0.3" platingStatus="VIA" x="2" y="2"/>
          </Set>
        </LayerFeature>
      </Step>
      <Step name="array" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="25" y="0"/>
            <PolyStepSegment x="25" y="15"/>
            <PolyStepSegment x="0" y="15"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="5" y="5" nx="2" ny="1" dx="12" dy="0"/>
        <LayerFeature layerRef="Board_Array_Drill">
          <Set>
            <Hole name="array_tooling" diameter="2.0" platingStatus="NONPLATED" x="3" y="3"/>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let accessor = IpcAccessor::new(&ipc);

        let board = accessor.board_drill_stats().unwrap();
        assert_eq!(board.total_holes, 1);
        assert_eq!(board.distribution[0].hole_type, DrillHoleType::Via);

        let array = accessor.board_array_drill_stats().unwrap();
        assert_eq!(array.total_holes, 1);
        assert_eq!(array.distribution[0].hole_type, DrillHoleType::NonPlated);

        let flattened = accessor.board_array_flattened_drill_stats().unwrap();
        assert_eq!(flattened.total_holes, 3);
        assert_eq!(
            flattened
                .distribution
                .iter()
                .find(|dist| dist.hole_type == DrillHoleType::Via)
                .unwrap()
                .total,
            2
        );
        assert_eq!(
            flattened
                .distribution
                .iter()
                .find(|dist| dist.hole_type == DrillHoleType::NonPlated)
                .unwrap()
                .total,
            1
        );
    }
}
