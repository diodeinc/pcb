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
//! folded-A7 tile's corner tooling, the fixture bed's mate lands on the
//! bottom copper (the S13 constellation by default, see [`bed`]; its
//! mate-detect pairs pre-shorted), and full-sheet GND pours. Pogo
//! placement and routing are later, panel-specific passes. A single
//! singulated board can stand in for the panel ([`single`]): the
//! generator places it on the bed and the DUT plate nests it.

// Use pipe-safe replacements for standard printing macros in CLI output paths.
#[macro_use(eprintln)]
extern crate anstream;

pub mod bed;
pub mod contacts;
pub mod dsn;
pub mod emit;
pub mod guard;
pub mod panel;
pub mod pattern;
pub mod plan;
pub mod plate;
pub mod pogo;
pub mod repair;
pub mod single;
pub mod stitch;

use std::path::Path;

use anyhow::{Context, Result};

pub use bed::Bed;
pub use emit::Layers;

/// What the interposer tests: a board-array panel, or one singulated
/// board in a nest ([`single`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Panel,
    SingleBoard,
}

/// Generate the interposer board for a panel file, returning the
/// `.kicad_pcb` and `.kicad_pro` sources, and the `.kicad_dru` source on
/// a bed with high voltage whose stackup holds per-item clearances. When the panel carries ICT
/// contacts, the board is populated from the fixture plan: pogo pads on
/// the top face for every tested contact, and nets bound on both ends —
/// the unrouted airwires are the routing pass's specification.
/// `interposer_id` is the number the bed's ID straps encode, for beds that
/// read one.
pub fn generate(
    panel_xml: &Path,
    mode: Mode,
    layers: Layers,
    bed: Bed,
    interposer_id: Option<u16>,
) -> Result<(String, String, Option<String>)> {
    let Built {
        panel,
        lands,
        plan,
        placement,
        ..
    } = build(panel_xml, mode, bed)?;
    if let Some(placement) = placement {
        eprintln!(
            "note: board placed {} {}° at ({:.2}, {:.2}); interposer spans x {:.1}–{:.1} mm",
            if placement.mirrored {
                "face down (top probed),"
            } else {
                "bottom probed,"
            },
            u32::from(placement.quarter_turns) * 90,
            placement.offset[0],
            placement.offset[1],
            panel.envelope.map_or(0.0, |e| e[0]),
            panel.envelope.map_or(panel.width, |e| e[2]),
        );
    }
    let straps = bed.id_straps(&lands, interposer_id)?;
    if let Some(plan) = &plan
        && let Some(own) = plan.cells.first()
        && own.len() > 1
    {
        eprintln!(
            "note: each board needs {} bed cells; testing {} of {} boards",
            own.len(),
            plan.tested.len(),
            plan.boards_total
        );
    }
    if let Some(plan) = &plan
        && !plan.dropped.is_empty()
    {
        let names: Vec<String> = plan
            .dropped
            .iter()
            .map(|&contact| {
                let contact = &plan.contacts[contact];
                format!("B{}.{} ({})", contact.board, contact.refdes, contact.ict)
            })
            .collect();
        eprintln!(
            "warning: {} contacts have no free land of their role and are not wired: {}",
            names.len(),
            names.join(", ")
        );
    }
    Ok((
        emit::board(&panel, &lands, bed, plan.as_ref(), &straps, layers),
        emit::project(bed, plan.as_ref(), layers),
        emit::rules(bed, layers),
    ))
}

/// Compute the fixture map for a panel file: which boards one insertion
/// tests and which mate land carries which contact, as JSON.
pub fn fixture_map(panel_xml: &Path, mode: Mode, bed: Bed) -> Result<String> {
    let Built {
        panel,
        lands,
        plan,
        placement,
        ..
    } = build(panel_xml, mode, bed)?;
    let plan = plan.context("panel has no ICT contacts to plan")?;
    Ok(plan::to_json(&plan, &panel, &lands, placement.as_ref()))
}

/// Generate the DUT plate for a panel file, as OpenSCAD source: the
/// printed slab between the interposer and the DUT panel (see [`plate`]).
/// A single board always sits in a nest, whatever `locators` asks.
pub fn dut_plate(
    panel_xml: &Path,
    mode: Mode,
    bed: Bed,
    locators: plate::Locators,
) -> Result<String> {
    let Built {
        panel,
        plan,
        boards,
        ..
    } = build(panel_xml, mode, bed)?;
    let plan = plan.context("panel has no ICT contacts to plan")?;
    let locators = match mode {
        Mode::Panel => locators,
        Mode::SingleBoard => plate::Locators::Nest,
    };
    plate::plate(&panel, bed, &plan, &boards, locators)
}

/// Fixed-feature disks a pogo pad must stay out of: tooling holes (the
/// A7 tile's corners can sit under the board field on large sheets) and
/// top-face fiducial mask openings, each grown by the pogo pad radius
/// plus clearance. A board with a contact in one is left untested; a
/// contact in the bed's alignment or screw keep-outs fails the plan
/// instead ([`plan::plan`]).
fn pogo_keepouts(panel: &panel::Panel) -> Vec<([f64; 2], f64)> {
    let pogo = pogo::COURTYARD_RADIUS_MM;
    let clear = 0.5;
    panel
        .holes
        .iter()
        .map(|(at, dia)| (*at, dia / 2.0 + pogo + clear))
        .chain(panel.fids_top.iter().map(|at| (*at, 1.0 + pogo + clear)))
        .collect()
}

struct Built {
    panel: panel::Panel,
    lands: Vec<pattern::Land>,
    plan: Option<plan::Plan>,
    /// Board bounding boxes in the sheet frame.
    boards: Vec<[f64; 4]>,
    placement: Option<single::Placement>,
}

fn build(panel_xml: &Path, mode: Mode, bed: Bed) -> Result<Built> {
    let content = std::fs::read_to_string(panel_xml)
        .with_context(|| format!("read panel {}", panel_xml.display()))?;
    let ipc = ipc2581::Ipc2581::parse(&content).context("parse IPC-2581 panel")?;
    let (panel, contacts, boards, placement) = match mode {
        Mode::Panel => {
            let panel = bed.fit(panel::extract(&ipc)?);
            let contacts = contacts::extract_contacts(&ipc, panel.height)?;
            let boards = contacts::board_outlines(&ipc, panel.height)?;
            (panel, contacts, boards, None)
        }
        Mode::SingleBoard => {
            let placed = single::place(&ipc, bed)?;
            (
                placed.panel,
                placed.contacts,
                vec![placed.board],
                Some(placed.placement),
            )
        }
    };
    let lands = bed.lands(panel.width, panel.height)?;
    let plan = if contacts.is_empty() {
        None
    } else {
        Some(plan::plan(
            contacts,
            &lands,
            &pogo_keepouts(&panel),
            panel.envelope,
            bed,
        )?)
    };
    Ok(Built {
        panel,
        lands,
        plan,
        boards,
        placement,
    })
}
