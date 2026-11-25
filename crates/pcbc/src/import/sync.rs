//! Sync the copied source layout once, so `pcb layout` after import is a no-op.

use super::*;
use anstream::eprintln;
use anyhow::{Context, Result};
use pcb_sch::{InstanceKind, Schematic};
use pcb_sexpr::{PatchSet, Sexpr};
use starlark::errors::EvalSeverity;
use std::collections::BTreeMap;

pub(super) fn sync_imported_layout(board: &MaterializedBoard, schematic: &Schematic) -> Result<()> {
    let Some(pcb_path) = &board.layout_kicad_pcb else {
        return Ok(());
    };
    write_footprint_ids(pcb_path, schematic)?;

    let mut diagnostics = pcb_zen_core::Diagnostics::default();
    pcb_layout::process_layout(
        schematic,
        pcb_layout::LayoutOptions {
            check: false,
            sync_footprints: false,
        },
        &mut diagnostics,
    )
    .context("Failed to sync the imported layout")?;
    let findings = diagnostics
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            matches!(
                diagnostic.severity,
                EvalSeverity::Error | EvalSeverity::Warning
            )
        })
        .collect::<Vec<_>>();
    for diagnostic in findings.iter().take(5) {
        eprintln!("Warning: layout sync: {}", diagnostic.innermost().body);
    }
    if findings.len() > 5 {
        eprintln!(
            "Warning: layout sync: {} more finding(s)",
            findings.len() - 5
        );
    }
    eprintln!("Synced imported layout {}", pcb_path.display());
    Ok(())
}

/// Give each footprint the ID layout sync assigns to its component. Sync replaces a footprint
/// whose ID changed with its library definition, which import generated from only one of the
/// package's placed copies; with matching IDs every placed copy is kept as it is.
fn write_footprint_ids(pcb_path: &Path, schematic: &Schematic) -> Result<()> {
    let mut fpid_by_path = BTreeMap::new();
    for (instance_ref, instance) in &schematic.instances {
        if instance.kind == InstanceKind::Component {
            let (fpid, _) = schematic
                .kicad_footprint(instance)
                .with_context(|| format!("component {instance_ref}"))?;
            fpid_by_path.insert(instance_ref.instance_path.join("."), fpid);
        }
    }

    let text = std::fs::read_to_string(pcb_path)
        .with_context(|| format!("Failed to read {}", pcb_path.display()))?;
    let root = pcb_sexpr::parse(&text).map_err(|error| anyhow::anyhow!(error))?;
    let mut patches = PatchSet::default();
    for footprint in root
        .as_list()
        .into_iter()
        .flatten()
        .filter_map(Sexpr::as_list)
    {
        if footprint.first().and_then(Sexpr::as_sym) != Some("footprint") {
            continue;
        }
        let path = footprint
            .iter()
            .filter_map(Sexpr::as_list)
            .find_map(|item| {
                (item.first().and_then(Sexpr::as_sym) == Some("property")
                    && item.get(1).and_then(Sexpr::as_str) == Some("Path"))
                .then(|| item.get(2).and_then(Sexpr::as_str))
                .flatten()
            });
        let (Some(fpid), Some(current)) = (
            path.and_then(|path| fpid_by_path.get(path)),
            footprint.get(1),
        ) else {
            continue;
        };
        if current.as_str() != Some(fpid.as_str()) {
            patches.replace_string(current.span, fpid);
        }
    }
    if patches.is_empty() {
        return Ok(());
    }
    let mut out = Vec::new();
    patches.write_to(&text, &mut out)?;
    std::fs::write(pcb_path, out).with_context(|| format!("Failed to write {}", pcb_path.display()))
}
