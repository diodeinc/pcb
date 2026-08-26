//! Pogo-pin test-fixture interposer generation.
//!
//! An interposer is a PCB the same size as an assembly panel that sits
//! between the panel and the test fixture: pogo pins on its top face reach
//! the panel's ICT test points, and a fixed land constellation on its
//! bottom face mates the fixture's A7-sized connector plate.
//!
//! This crate generates the deterministic part of that board from a
//! generated assembly-panel IPC-2581 file (`pcbc ipc2581 board-array
//! create` output): the panel's exact outline, tooling holes, and global
//! fiducials — inherited, so they cannot drift from the panel — plus the
//! folded-A7 tile's corner tooling, the S13 mate constellation on the
//! bottom copper (its corner mate-detect pairs pre-shorted), and
//! full-sheet GND pours. Pogo placement and routing are later,
//! panel-specific passes.

pub mod contacts;
pub mod emit;
pub mod panel;
pub mod pattern;
pub mod plan;
pub mod pogo;
pub mod stitch;

use std::path::Path;

use anyhow::{Context, Result};

/// Generate the interposer board for a panel file, returning the
/// `.kicad_pcb` and `.kicad_pro` sources. When the panel carries ICT
/// contacts, the board is populated from the fixture plan: pogo pads on
/// the top face for every tested contact, and nets bound on both ends —
/// the unrouted airwires are the routing pass's specification.
pub fn generate(panel_xml: &Path) -> Result<(String, String)> {
    let (panel, lands, plan) = build(panel_xml)?;
    Ok((emit::board(&panel, &lands, plan.as_ref()), emit::project()))
}

/// Compute the fixture map for a panel file: which boards one insertion
/// tests and which mate land carries which contact, as JSON.
pub fn fixture_map(panel_xml: &Path) -> Result<String> {
    let (panel, lands, plan) = build(panel_xml)?;
    let plan = plan.context("panel has no ICT contacts to plan")?;
    Ok(plan::to_json(&plan, &panel, &lands))
}

/// Fixed-feature disks a pogo pad must stay out of: tooling holes (the
/// A7 tile's corners can sit under the board field on large sheets) and
/// top-face fiducial mask openings, each grown by the pogo pad radius
/// plus clearance.
fn pogo_keepouts(panel: &panel::Panel) -> Vec<([f64; 2], f64)> {
    // The vendored 806 pogo footprint's courtyard radius: the cap is
    // wider than the solder land, so the courtyard governs interference.
    let pogo = 1.5;
    let clear = 0.5;
    panel
        .holes
        .iter()
        .map(|(at, dia)| (*at, dia / 2.0 + pogo + clear))
        .chain(panel.fids_top.iter().map(|at| (*at, 1.0 + pogo + clear)))
        .collect()
}

fn build(panel_xml: &Path) -> Result<(panel::Panel, Vec<pattern::Land>, Option<plan::Plan>)> {
    let content = std::fs::read_to_string(panel_xml)
        .with_context(|| format!("read panel {}", panel_xml.display()))?;
    let ipc = ipc2581::Ipc2581::parse(&content).context("parse IPC-2581 panel")?;
    let panel = panel::extract(&ipc)?;
    let lands = pattern::oriented_s13(panel.width, panel.height);
    let contacts = contacts::extract_contacts(&ipc, panel.height)?;
    let plan = if contacts.is_empty() {
        None
    } else {
        Some(plan::plan(contacts, &lands, &pogo_keepouts(&panel))?)
    };
    Ok((panel, lands, plan))
}
