use std::collections::{BTreeMap, HashMap};

use ipc2581::types::{LayerFunction, Side};
use pcb_ir::dialects::ipc::{ArtworkScope, FeatureKind, PlatingKind};
use pcb_ir::geom::Polarity;
use pcb_ir::import::ipc2581::{GeometryDocument, ImportedDesign, LayerId, is_copper};
use serde::{Deserialize, Serialize};

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

/// Via holes of one drill size that share an outer-layer land size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViaProfile {
    pub hole_mm: f64,
    /// The via's own land: the narrower extent of the smaller of its top and
    /// bottom pads. Component pads it sits in are ignored. `None` unless both
    /// faces have a land, as for blind, buried and padless vias.
    pub pad_mm: Option<f64>,
    pub count: usize,
}

/// Every via of one board grouped by hole and outer pad size, sorted by hole
/// then pad. A fab's small-via pricing depends on both, not the hole alone.
/// A via's land is a pad at its centre on the padstack its drill names (the
/// hole's Set `geometry`, the pad's `padstackDefRef`), so component pads it
/// sits in are not mistaken for it. Sizes and centres are compared on a 1 µm
/// grid; pads and holes are never in IPC placement groups, so their centres
/// are in layer space.
pub fn via_profiles(imported: &ImportedDesign) -> anyhow::Result<Vec<ViaProfile>> {
    let um = |mm: f64| (mm * 1000.0).round() as i64;
    let mut holes = Vec::new();
    // Smallest pad per centre and padstack on each face; `None` for a face
    // with no copper.
    let mut faces: [Option<HashMap<_, i64>>; 2] = [None, None];
    for (index, layer) in imported.layer_definitions.iter().enumerate() {
        let face = match layer.side {
            Some(Side::Top) if is_copper(layer.layer_function) => Some(0),
            Some(Side::Bottom) if is_copper(layer.layer_function) => Some(1),
            _ => None,
        };
        if face.is_none() && layer.layer_function != LayerFunction::Drill {
            continue;
        }
        let doc = imported.materialize_layer(LayerId(index as u32), ArtworkScope::Board)?;
        for feature in doc
            .layers
            .iter()
            .flat_map(|layer| layer.features.slice(&doc.features))
        {
            let at = (
                um(feature.center.x),
                um(feature.center.y),
                feature.padstack_ref,
            );
            if let Some(face) = face {
                if feature.kind == FeatureKind::Padstack && feature.polarity == Polarity::Dark {
                    let size = um(feature.bbox.width().min(feature.bbox.height()));
                    let pad = faces[face]
                        .get_or_insert_default()
                        .entry(at)
                        .or_insert(size);
                    *pad = (*pad).min(size);
                }
            } else if let Some((diameter, DrillHoleType::Via)) = drill_hole(feature) {
                holes.push((at, um(diameter)));
            }
        }
    }

    let mut profiles = BTreeMap::<_, usize>::new();
    for (at, hole) in holes {
        let [top, bottom] = &faces;
        let pad = top
            .as_ref()
            .and_then(|pads| pads.get(&at))
            .zip(bottom.as_ref().and_then(|pads| pads.get(&at)));
        *profiles
            .entry((hole, pad.map(|(top, bottom)| *top.min(bottom))))
            .or_default() += 1;
    }
    let mm = |um: i64| um as f64 / 1000.0;
    Ok(profiles
        .into_iter()
        .map(|((hole, pad), count)| ViaProfile {
            hole_mm: mm(hole),
            pad_mm: pad.map(mm),
            count,
        })
        .collect())
}

/// Drill hole statistics with per-type distribution for one artwork scope:
/// `Board` counts one board, `ArrayLocal` only the array's own drills.
pub fn drill_stats(
    imported: &ImportedDesign,
    scope: ArtworkScope,
) -> anyhow::Result<Option<DrillStats>> {
    let mut collector = DrillStatsCollector::default();
    let mut has_drill_layer = false;
    for (index, _) in imported
        .layer_definitions
        .iter()
        .enumerate()
        .filter(|(_, layer)| layer.layer_function == LayerFunction::Drill)
    {
        has_drill_layer = true;
        collect_drill_info(
            &imported.materialize_layer(LayerId(index as u32), scope)?,
            &mut collector,
        );
    }
    Ok(has_drill_layer.then(|| collector.finish()))
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

fn drill_hole(feature: &pcb_ir::dialects::ipc::Feature) -> Option<(f64, DrillHoleType)> {
    if !feature.is_drill_like() || feature.kind != FeatureKind::Hole {
        return None;
    }
    let size = feature
        .shape
        .and_then(pcb_ir::dialects::ipc::SimpleShape::hole_size)?;
    (size > 0.0).then_some((size, drill_hole_type(feature.intent.plating)))
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

    /// Two boards in an array; vias `a`-`c` and a plated hole on each. Via
    /// `b` also sits in a narrower component pad, which is not its land.
    fn via_board(bottom_copper: bool) -> ImportedDesign {
        let (bottom_layer, bottom_pads) = if bottom_copper {
            (
                r#"<Layer name="B.Cu" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>"#,
                r#"<LayerFeature layerRef="B.Cu">
          <Set><Pad padstackDefRef="VIA" x="1" y="1"><StandardPrimitiveRef id="C40"/></Pad></Set>
          <Set><Pad padstackDefRef="VIA" x="3" y="1"><StandardPrimitiveRef id="C45"/></Pad></Set>
        </LayerFeature>"#,
            )
        } else {
            ("", "")
        };
        let ipc = ipc2581::Ipc2581::parse(&format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="array"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="C40"><Circle diameter="0.4"/></EntryStandard>
      <EntryStandard id="C45"><Circle diameter="0.45"/></EntryStandard>
      <EntryStandard id="C50"><Circle diameter="0.5"/></EntryStandard>
      <EntryStandard id="R30"><RectCenter width="0.3" height="1"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="F.Cu" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      {bottom_layer}
      <Layer name="Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
        <PadStackDef name="VIA">
          <PadstackHoleDef name="H" diameter="0.2" platingStatus="VIA" plusTol="0" minusTol="0" x="0" y="0"/>
        </PadStackDef>
        <LayerFeature layerRef="F.Cu">
          <Set><Pad padstackDefRef="VIA" x="1" y="1"><StandardPrimitiveRef id="C50"/></Pad></Set>
          <Set><Pad padstackDefRef="VIA" x="3" y="1"><StandardPrimitiveRef id="C45"/></Pad></Set>
          <Set><Pad padstackDefRef="VIA" x="5" y="1"><StandardPrimitiveRef id="C45"/></Pad></Set>
          <Set><Pad x="3" y="1"><StandardPrimitiveRef id="R30"/></Pad></Set>
        </LayerFeature>
        {bottom_pads}
        <LayerFeature layerRef="Drill">
          <Set geometry="VIA"><Hole name="a" diameter="0.2" platingStatus="VIA" x="1" y="1"/></Set>
          <Set geometry="VIA"><Hole name="b" diameter="0.2" platingStatus="VIA" x="3" y="1"/></Set>
          <Set geometry="VIA"><Hole name="c" diameter="0.2" platingStatus="VIA" x="5" y="1"/></Set>
          <Set><Hole name="pth" diameter="0.2" platingStatus="PLATED" x="7" y="1"/></Set>
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
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
        ))
        .unwrap();
        pcb_ir::import::ipc2581::import_design(&ipc, pcb_ir::geom::Resolution::default()).unwrap()
    }

    #[test]
    fn via_profiles_pair_each_hole_with_its_smallest_outer_pad() {
        let profile = |pad_mm, count| ViaProfile {
            hole_mm: 0.2,
            pad_mm,
            count,
        };
        // The smaller bottom land decides `a`; `b` ignores the component pad;
        // `c` has no bottom land; the
        // plated hole is not a via; the array's second board is not counted.
        assert_eq!(
            via_profiles(&via_board(true)).unwrap(),
            vec![
                profile(None, 1),
                profile(Some(0.4), 1),
                profile(Some(0.45), 1)
            ]
        );
        // Without bottom copper no via has a pad on both faces.
        assert_eq!(
            via_profiles(&via_board(false)).unwrap(),
            vec![profile(None, 3)]
        );
    }

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
        let imported =
            pcb_ir::import::ipc2581::import_design(&ipc, pcb_ir::geom::Resolution::default())
                .unwrap();

        let board = drill_stats(&imported, ArtworkScope::Board)
            .unwrap()
            .unwrap();
        assert_eq!(board.total_holes, 1);
        assert_eq!(board.distribution[0].hole_type, DrillHoleType::Via);

        let array = drill_stats(&imported, ArtworkScope::ArrayLocal)
            .unwrap()
            .unwrap();
        assert_eq!(array.total_holes, 1);
        assert_eq!(array.distribution[0].hole_type, DrillHoleType::NonPlated);

        let flattened = drill_stats(&imported, ArtworkScope::ArrayFlattened)
            .unwrap()
            .unwrap();
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
