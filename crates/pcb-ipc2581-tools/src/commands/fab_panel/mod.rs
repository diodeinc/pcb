use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ipc2581::Ipc2581;
use ipc2581::edit::{Doc, Node};
use ipc2581::types::{Spec, Units};
use pcb_ir::dialects::ipc::{LayoutStepKind, root_step};
use pcb_ir::geom::BBox;

use crate::geometry;
use crate::utils::file as file_utils;

mod packing;
mod xml;

use packing::{MAX_ITEM_COUNT, SOFT_ITEM_LIMIT, Size, pack};

const FAB_PANEL_WIDTH_MM: f64 = 18.0 * 25.4;
const FAB_PANEL_HEIGHT_MM: f64 = 24.0 * 25.4;
const EDGE_RAIL_MM: f64 = 5.0;
const PANEL_GAP_MM: f64 = 5.0;
const MICROMETERS_PER_MM: f64 = 1_000.0;

const USABLE_FAB_PANEL: Size = Size {
    width: 447_200,
    height: 599_600,
};
const PANEL_GAP_UM: u32 = 5_000;

#[derive(Debug)]
struct SourcePanel {
    namespaced_xml: String,
    root_step_name: String,
    bbox: BBox,
    units: Units,
    revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PhysicalStackup {
    attributes: Vec<(String, String)>,
    group_attributes: Vec<Vec<(String, String)>>,
    layers: Vec<PhysicalStackupLayer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PhysicalStackupLayer {
    name: String,
    layer_function: String,
    side: Option<String>,
    polarity: Option<String>,
    span: Option<(Option<String>, Option<String>)>,
    thickness: Option<u64>,
    tol_plus: Option<u64>,
    tol_minus: Option<u64>,
    material: Option<String>,
    dielectric_constant: Option<u64>,
    loss_tangent: Option<u64>,
    layer_number: Option<u32>,
    specs: Vec<SpecSignature>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpecSignature {
    items: Vec<SpecItemSignature>,
    material: Option<String>,
    dielectric_constant: Option<u64>,
    loss_tangent: Option<u64>,
    properties: Vec<String>,
    surface_finish: Option<SurfaceFinishSignature>,
    copper_weight_oz: Option<u64>,
    color_term: Option<String>,
    color_rgb: Option<(u8, u8, u8)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpecItemSignature {
    element: String,
    kind: String,
    item_type: Option<String>,
    comment: Option<String>,
    properties: Vec<SpecPropertySignature>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpecPropertySignature {
    value: Option<u64>,
    text: Option<String>,
    unit: Option<String>,
    plus_tol: Option<u64>,
    minus_tol: Option<u64>,
    tol_percent: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SurfaceFinishSignature {
    finish_type: String,
    comment: Option<String>,
    products: Vec<(String, Option<String>)>,
}

pub fn execute(inputs: &[PathBuf], output: &Path) -> Result<()> {
    if inputs.is_empty() {
        bail!("at least one assembly panel IPC-2581 file is required");
    }
    if inputs.len() > MAX_ITEM_COUNT {
        bail!(
            "at most {MAX_ITEM_COUNT} assembly panels are supported; got {}",
            inputs.len()
        );
    }
    if inputs.len() > SOFT_ITEM_LIMIT {
        eprintln!(
            "warning: packing {} assembly panels exceeds the recommended limit of {SOFT_ITEM_LIMIT}",
            inputs.len()
        );
    }

    let mut source_by_path = HashMap::<PathBuf, usize>::new();
    let mut source_xml = Vec::new();
    let mut occurrences = Vec::with_capacity(inputs.len());
    for input in inputs {
        let canonical = std::fs::canonicalize(input)
            .with_context(|| format!("Failed to resolve input file: {}", input.display()))?;
        let source_index = match source_by_path.get(&canonical) {
            Some(source_index) => *source_index,
            None => {
                let source_index = source_xml.len();
                source_xml.push(file_utils::load_ipc_file(input)?);
                source_by_path.insert(canonical, source_index);
                source_index
            }
        };
        occurrences.push(source_index);
    }

    let generated = create_fab_panel_xml(&source_xml, &occurrences)?;
    if output.as_os_str() == "-" {
        io::stdout().lock().write_all(generated.as_bytes())?;
        eprintln!("✓ Created IPC-2581 fabrication panel on stdout");
    } else {
        file_utils::save_ipc_file(output, &generated)?;
        eprintln!(
            "✓ Created IPC-2581 fabrication panel at {}",
            output.display()
        );
    }
    Ok(())
}

fn create_fab_panel_xml(source_xml: &[String], occurrences: &[usize]) -> Result<String> {
    if occurrences.is_empty() {
        bail!("at least one assembly panel is required");
    }

    let stackups = source_xml
        .iter()
        .enumerate()
        .map(|(source_index, xml)| physical_stackup(xml, source_index))
        .collect::<Result<Vec<_>>>()?;
    let first_stackup = stackups
        .first()
        .context("at least one assembly panel source is required")?;
    for (source_index, stackup) in stackups.iter().enumerate().skip(1) {
        require_identical_stackup(first_stackup, stackup, source_index)?;
    }
    let shared_stackup_layers = first_stackup
        .layers
        .iter()
        .map(|layer| layer.name.clone())
        .collect::<HashSet<_>>();

    let sources = source_xml
        .iter()
        .enumerate()
        .map(|(source_index, xml)| prepare_source_panel(xml, source_index, &shared_stackup_layers))
        .collect::<Result<Vec<_>>>()?;
    let first = sources
        .first()
        .context("at least one assembly panel source is required")?;
    for source in &sources[1..] {
        if source.units != first.units {
            bail!("all assembly panel IPC-2581 files must use the same units");
        }
        if source.revision != first.revision {
            bail!("all assembly panel IPC-2581 files must use the same revision");
        }
    }

    let items = occurrences
        .iter()
        .map(|source_index| {
            let source = sources
                .get(*source_index)
                .with_context(|| format!("invalid assembly panel source index {source_index}"))?;
            Ok(Size {
                width: dimension_um(source.bbox.width())?,
                height: dimension_um(source.bbox.height())?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let placements = pack(&items, USABLE_FAB_PANEL, PANEL_GAP_UM)?;

    xml::write_fab_panel_xml(&sources, occurrences, &placements, &shared_stackup_layers)
}

fn prepare_source_panel(
    xml: &str,
    source_index: usize,
    shared_stackup_layers: &HashSet<String>,
) -> Result<SourcePanel> {
    let ipc = Ipc2581::parse(xml)
        .with_context(|| format!("Failed to parse assembly panel input {}", source_index + 1))?;
    let ecad = ipc.ecad().with_context(|| {
        format!(
            "assembly panel input {} has no ECAD section",
            source_index + 1
        )
    })?;
    let layout = geometry::extract_layout(&ipc).with_context(|| {
        format!(
            "failed to extract layout from assembly panel input {}",
            source_index + 1
        )
    })?;
    let (_, root) = root_step(&layout).with_context(|| {
        format!(
            "assembly panel input {} has no layout root",
            source_index + 1
        )
    })?;
    if root.kind != LayoutStepKind::Panel {
        bail!(
            "assembly panel input {} has a board layout root; expected a board array",
            source_index + 1
        );
    }
    if root.bbox.is_empty() || root.bbox.width() <= 0.0 || root.bbox.height() <= 0.0 {
        bail!(
            "assembly panel input {} has no non-empty root Profile",
            source_index + 1
        );
    }

    let prefix = format!("fab_{source_index}_");
    Ok(SourcePanel {
        namespaced_xml: xml::namespace_source(xml, &prefix, shared_stackup_layers)?,
        root_step_name: format!("{prefix}{}", ipc.resolve(root.source_step_ref)),
        bbox: root.bbox,
        units: ecad.cad_header.units,
        revision: ipc.revision().to_string(),
    })
}

fn physical_stackup(xml: &str, source_index: usize) -> Result<PhysicalStackup> {
    let input_number = source_index + 1;
    let ipc = Ipc2581::parse(xml)
        .with_context(|| format!("Failed to parse assembly panel input {input_number}"))?;
    let ecad = ipc
        .ecad()
        .with_context(|| format!("assembly panel input {input_number} has no ECAD section"))?;
    if ecad.cad_data.stackups.len() != 1 {
        bail!(
            "assembly panel input {input_number} must contain exactly one physical stackup; found {}",
            ecad.cad_data.stackups.len()
        );
    }
    let stackup = &ecad.cad_data.stackups[0];

    let doc = Doc::parse(xml)?;
    let stackup_nodes = doc.find_all("Stackup");
    if stackup_nodes.len() != 1 {
        bail!(
            "assembly panel input {input_number} must contain exactly one Stackup element; found {}",
            stackup_nodes.len()
        );
    }
    let stackup_node = stackup_nodes[0];
    let attributes = sorted_attributes(&doc, stackup_node, &["name"]);
    let group_attributes = doc
        .children(stackup_node)
        .into_iter()
        .filter(|child| doc.name(*child) == "StackupGroup")
        .map(|group| sorted_attributes(&doc, group, &["name"]))
        .collect::<Vec<_>>();

    let layers = stackup
        .layers
        .iter()
        .enumerate()
        .map(|(layer_index, stackup_layer)| {
            let name = ipc.resolve(stackup_layer.layer_ref).to_string();
            let layer = ecad
                .cad_data
                .layers
                .iter()
                .find(|layer| ipc.resolve(layer.name) == name)
                .with_context(|| {
                    format!(
                        "assembly panel input {input_number} stackup layer {} references missing layer '{name}'",
                        layer_index + 1
                    )
                })?;
            let mut specs = layer
                .spec_refs
                .iter()
                .filter_map(|spec_ref| ecad.cad_header.specs.get(spec_ref))
                .map(|spec| spec_signature(&ipc, spec))
                .collect::<Vec<_>>();
            if let Some(spec_ref) = stackup_layer.spec_ref
                && !layer.spec_refs.contains(&spec_ref)
                && let Some(spec) = ecad.cad_header.specs.get(&spec_ref)
            {
                specs.push(spec_signature(&ipc, spec));
            }

            Ok(PhysicalStackupLayer {
                name,
                layer_function: layer.layer_function.as_str().to_string(),
                side: layer.side.map(|side| side.as_str().to_string()),
                polarity: layer.polarity.map(|polarity| format!("{polarity:?}")),
                span: layer.span.map(|span| {
                    (
                        span.from_layer
                            .map(|layer| ipc.resolve(layer).to_string()),
                        span.to_layer.map(|layer| ipc.resolve(layer).to_string()),
                    )
                }),
                thickness: float_bits(stackup_layer.thickness),
                tol_plus: float_bits(stackup_layer.tol_plus),
                tol_minus: float_bits(stackup_layer.tol_minus),
                material: stackup_layer
                    .material
                    .map(|material| ipc.resolve(material).to_string()),
                dielectric_constant: float_bits(stackup_layer.dielectric_constant),
                loss_tangent: float_bits(stackup_layer.loss_tangent),
                layer_number: stackup_layer.layer_number,
                specs,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(PhysicalStackup {
        attributes,
        group_attributes,
        layers,
    })
}

fn require_identical_stackup(
    first: &PhysicalStackup,
    candidate: &PhysicalStackup,
    source_index: usize,
) -> Result<()> {
    let input_number = source_index + 1;
    if candidate.attributes != first.attributes
        || candidate.group_attributes != first.group_attributes
    {
        bail!(
            "assembly panel input {input_number} has stackup attributes that differ from assembly panel input 1"
        );
    }
    if candidate.layers.len() != first.layers.len() {
        bail!(
            "assembly panel input {input_number} has {} stackup layers; assembly panel input 1 has {}",
            candidate.layers.len(),
            first.layers.len()
        );
    }
    for (layer_index, (expected, actual)) in first.layers.iter().zip(&candidate.layers).enumerate()
    {
        if actual.name != expected.name {
            bail!(
                "assembly panel input {input_number} stackup layer {} is '{}'; assembly panel input 1 uses '{}'",
                layer_index + 1,
                actual.name,
                expected.name
            );
        }
        if actual != expected {
            bail!(
                "assembly panel input {input_number} stackup layer {} ('{}') differs from assembly panel input 1",
                layer_index + 1,
                expected.name
            );
        }
    }
    Ok(())
}

fn sorted_attributes(doc: &Doc<'_>, node: Node, excluded: &[&str]) -> Vec<(String, String)> {
    let mut attributes = doc
        .attrs(node)
        .filter(|(name, _)| !excluded.contains(name))
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect::<Vec<_>>();
    attributes.sort();
    attributes
}

fn spec_signature(ipc: &Ipc2581, spec: &Spec) -> SpecSignature {
    SpecSignature {
        items: spec
            .items
            .iter()
            .map(|item| SpecItemSignature {
                element: ipc.resolve(item.element).to_string(),
                kind: format!("{:?}", item.kind),
                item_type: item
                    .item_type
                    .map(|item_type| ipc.resolve(item_type).to_string()),
                comment: item.comment.map(|comment| ipc.resolve(comment).to_string()),
                properties: item
                    .properties
                    .iter()
                    .map(|property| SpecPropertySignature {
                        value: float_bits(property.value),
                        text: property.text.map(|text| ipc.resolve(text).to_string()),
                        unit: property.unit.map(|unit| ipc.resolve(unit).to_string()),
                        plus_tol: float_bits(property.plus_tol),
                        minus_tol: float_bits(property.minus_tol),
                        tol_percent: property.tol_percent,
                    })
                    .collect(),
            })
            .collect(),
        material: spec
            .material
            .map(|material| ipc.resolve(material).to_string()),
        dielectric_constant: float_bits(spec.dielectric_constant),
        loss_tangent: float_bits(spec.loss_tangent),
        properties: spec
            .properties
            .iter()
            .map(|property| ipc.resolve(*property).to_string())
            .collect(),
        surface_finish: spec
            .surface_finish
            .as_ref()
            .map(|finish| SurfaceFinishSignature {
                finish_type: format!("{:?}", finish.finish_type),
                comment: finish
                    .comment
                    .map(|comment| ipc.resolve(comment).to_string()),
                products: finish
                    .products
                    .iter()
                    .map(|product| {
                        (
                            ipc.resolve(product.name).to_string(),
                            product.criteria.map(|criteria| format!("{criteria:?}")),
                        )
                    })
                    .collect(),
            }),
        copper_weight_oz: float_bits(spec.copper_weight_oz),
        color_term: spec
            .color_term
            .map(|color_term| ipc.resolve(color_term).to_string()),
        color_rgb: spec.color_rgb,
    }
}

fn float_bits(value: Option<f64>) -> Option<u64> {
    value.map(f64::to_bits)
}

fn dimension_um(value_mm: f64) -> Result<u32> {
    let value = (value_mm * MICROMETERS_PER_MM).ceil();
    if !value.is_finite() || value <= 0.0 || value > f64::from(u32::MAX) {
        bail!("assembly panel dimension {value_mm} mm is outside the supported range");
    }
    Ok(value as u32)
}

#[cfg(test)]
mod tests;
