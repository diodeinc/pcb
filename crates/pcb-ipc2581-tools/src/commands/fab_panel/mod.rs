use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ipc2581::Ipc2581;
use ipc2581::types::Units;
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

    let sources = source_xml
        .iter()
        .enumerate()
        .map(|(source_index, xml)| prepare_source_panel(xml, source_index))
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

    xml::write_fab_panel_xml(&sources, occurrences, &placements)
}

fn prepare_source_panel(xml: &str, source_index: usize) -> Result<SourcePanel> {
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
        namespaced_xml: xml::namespace_source(xml, &prefix)?,
        root_step_name: format!("{prefix}{}", ipc.resolve(root.source_step_ref)),
        bbox: root.bbox,
        units: ecad.cad_header.units,
        revision: ipc.revision().to_string(),
    })
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
