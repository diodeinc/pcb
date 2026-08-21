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
//! folded-A7 tile's corner tooling, the S11 mate constellation on the
//! bottom copper, and a full-sheet bottom GND pour. Pogo placement and
//! routing are later, panel-specific passes.

pub mod contacts;
pub mod emit;
pub mod panel;
pub mod pattern;
pub mod plan;

use std::path::Path;

use anyhow::{Context, Result};

/// Generate the interposer board for a panel file, returning the
/// `.kicad_pcb` and `.kicad_pro` sources.
pub fn generate(panel_xml: &Path) -> Result<(String, String)> {
    let ipc = parse(panel_xml)?;
    let panel = panel::extract(&ipc)?;
    let lands = pattern::oriented_s11(panel.width, panel.height);
    Ok((emit::board(&panel, &lands), emit::project()))
}

/// Compute the fixture map for a panel file: which boards one insertion
/// tests and which mate land carries which contact, as JSON.
pub fn fixture_map(panel_xml: &Path) -> Result<String> {
    let ipc = parse(panel_xml)?;
    let panel = panel::extract(&ipc)?;
    let lands = pattern::oriented_s11(panel.width, panel.height);
    let contacts = contacts::extract_contacts(&ipc, panel.height)?;
    let plan = plan::plan(contacts, &lands)?;
    Ok(plan::to_json(&plan, &panel, &lands))
}

fn parse(panel_xml: &Path) -> Result<ipc2581::Ipc2581> {
    let content = std::fs::read_to_string(panel_xml)
        .with_context(|| format!("read panel {}", panel_xml.display()))?;
    ipc2581::Ipc2581::parse(&content).context("parse IPC-2581 panel")
}
