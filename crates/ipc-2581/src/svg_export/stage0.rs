use super::{BoardContext, Result};
use crate::svg_export::board_context::BoardStats;
use crate::{Ipc2581, LayerFunction, Units};
use std::collections::HashMap;

/// Stage 0: Input Readiness
///
/// Builds BoardContext from parsed IPC-2581 document:
/// - Extract and normalize units
/// - Build dictionaries for quick lookup
/// - Gather validation statistics
/// - Prepare for Stage 1 transform resolution
pub fn build_board_context(doc: &Ipc2581) -> Result<BoardContext> {
    let ecad = doc
        .ecad()
        .ok_or(crate::Ipc2581Error::MissingElement("Ecad"))?;
    let step = ecad
        .cad_data
        .steps
        .first()
        .ok_or(crate::Ipc2581Error::MissingElement("Step"))?;

    // Get board name
    let board_name = doc.resolve(step.name).to_string();

    // Get units and conversion factor
    let units = &ecad.cad_header.units;
    let (original_units, to_mm_factor) = match units {
        Units::Millimeter => ("MILLIMETER".to_string(), 1.0),
        Units::Inch => ("INCH".to_string(), 25.4),
        Units::Micron => ("MICRON".to_string(), 0.001),
        Units::Mils => ("MILS".to_string(), 0.0254),
    };

    // Build padstack dictionary
    let mut padstack_defs = HashMap::new();
    for psd in &step.padstack_defs {
        padstack_defs.insert(psd.name, psd.clone());
    }

    // Build line descriptor dictionary from Content
    let content = doc.content();
    let mut line_descriptors = HashMap::new();
    for entry in &content.dictionary_line_desc.entries {
        line_descriptors.insert(entry.id, entry.line_desc); // LineDesc is Copy
    }

    // Build fill descriptor dictionary
    let mut fill_descriptors = HashMap::new();
    for entry in &content.dictionary_fill_desc.entries {
        fill_descriptors.insert(entry.id, entry.fill_desc); // FillDesc is Copy
    }

    // Build standard primitive dictionary
    let mut standard_primitives = HashMap::new();
    for entry in &content.dictionary_standard.entries {
        standard_primitives.insert(entry.id, entry.primitive.clone());
    }

    // Gather statistics
    let mut stats = BoardStats::new();
    stats.layer_count = ecad.cad_data.layers.len();
    stats.padstack_def_count = step.padstack_defs.len();
    stats.line_desc_count = line_descriptors.len();
    stats.fill_desc_count = fill_descriptors.len();
    stats.standard_primitive_count = standard_primitives.len();

    // Count copper layers
    stats.copper_layer_count = ecad
        .cad_data
        .layers
        .iter()
        .filter(|l| {
            matches!(
                l.layer_function,
                LayerFunction::Conductor | LayerFunction::Plane
            )
        })
        .count();

    // Count drill layers
    stats.drill_layer_count = ecad
        .cad_data
        .layers
        .iter()
        .filter(|l| l.layer_function == LayerFunction::Drill)
        .count();

    // Count features
    for layer_feature in &step.layer_features {
        for set in &layer_feature.sets {
            stats.feature_set_count += 1;
            stats.pad_count += set.pads.len();
            stats.trace_count += set.traces.len();
            stats.hole_count += set.holes.len();
            stats.slot_count += set.slots.len();
        }
    }

    Ok(BoardContext {
        board_name,
        original_units,
        to_mm_factor,
        padstack_defs,
        line_descriptors,
        fill_descriptors,
        standard_primitives,
        stats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bumpalo::Bump;

    #[test]
    fn test_build_context_simple() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <DictionaryColor/>
    <DictionaryLineDesc units="MILLIMETER"/>
    <DictionaryFillDesc units="MILLIMETER"/>
    <DictionaryStandard units="MILLIMETER"/>
    <DictionaryUser units="MILLIMETER"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="TestBoard">
        <Datum x="0.0" y="0.0"/>
      </Step>
      <Layer name="TOP" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
    </CadData>
  </Ecad>
</IPC-2581>"#;

        let arena = Bump::new();
        let doc = Ipc2581::parse(&arena, xml).expect("Failed to parse");
        let ctx = build_board_context(&doc).expect("Failed to build context");

        assert_eq!(ctx.board_name, "TestBoard");
        assert_eq!(ctx.original_units, "MILLIMETER");
        assert_eq!(ctx.to_mm_factor, 1.0);
        assert_eq!(ctx.stats.layer_count, 1);
    }
}
