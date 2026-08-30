//! Report estimated panel bow and twist.

#[cfg(feature = "cli")]
use std::path::Path;

#[cfg(feature = "cli")]
use anyhow::{Context, Result};

#[cfg(feature = "cli")]
use crate::ipc2581::Ipc2581;
#[cfg(feature = "cli")]
use crate::warp;
use crate::warp::WarpAnalysis;

/// Bow beyond which IPC-6012 rejects a board carrying surface-mount parts.
const SURFACE_MOUNT_LIMIT_PERCENT: f64 = 0.75;

#[cfg(feature = "cli")]
pub fn execute(file: &Path, report: Option<&Path>) -> Result<()> {
    let xml = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read {}", file.display()))?;
    let ipc = Ipc2581::parse(&xml).context("failed to parse IPC-2581 file")?;
    let analysis = warp::analyze(&ipc)?;

    for line in summary_lines(&analysis) {
        println!("{line}");
    }

    if let Some(path) = report {
        std::fs::write(path, super::warp_report::render(&analysis))
            .with_context(|| format!("failed to write {}", path.display()))?;
        println!("✓ Field report written to {}", path.display());
    }
    Ok(())
}

/// One line per copper layer, then the estimate and what it rests on.
pub fn summary_lines(analysis: &WarpAnalysis) -> Vec<String> {
    let warp = &analysis.warp;
    let mut lines = analysis
        .layers
        .iter()
        .zip(analysis.stack.conductor_weights())
        .map(|(layer, conductor)| {
            format!(
                "{:>8}: copper {:.3}, lever arm {:+.4} mm",
                layer.layer_name, layer.mean, conductor.lever_arm_mm,
            )
        })
        .collect::<Vec<_>>();

    lines.push(format!(
        "stack {:.2} mm over {:.0} x {:.0} mm",
        analysis.stack.total_thickness_mm(),
        analysis.bounds.width(),
        analysis.bounds.height(),
    ));
    lines.push(format!(
        "estimated bow {:.3} mm ({:.3} %), IPC-6012 allows {SURFACE_MOUNT_LIMIT_PERCENT} % \
         for surface mount",
        warp.bow_mm, warp.bow_percent,
    ));
    lines.push(format!(
        "estimated twist {:.3} mm ({:.3} %)",
        warp.twist_mm, warp.twist_percent,
    ));
    lines.push(format!(
        "modelled from the stackup and copper distribution at a {:.0} K drop, not measured",
        analysis.temperature_drop_k,
    ));
    lines
}
