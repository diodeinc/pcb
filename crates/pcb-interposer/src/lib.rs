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
//! generator places it on the bed and the upper shroud nests it. On
//! Switchyard's bench the fixture's printed upper shroud, the lower
//! shroud's relief map and the hardware list come from [`shroud`].

// Use pipe-safe replacements for standard printing macros in CLI output paths.
#[macro_use(eprintln)]
extern crate anstream;

pub mod bed;
pub mod contacts;
pub mod dsn;
pub mod emit;
pub mod guard;
pub mod panel;
pub mod parts;
pub mod pattern;
pub mod plan;
pub mod pogo;
pub mod repair;
pub mod shroud;
pub mod single;
pub mod stitch;

use std::path::Path;

use anyhow::{Context, Result};

pub use bed::Bed;
pub use contacts::Footprints;
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
/// read one. `footprints` picks which role-tagged test points get a pogo.
pub fn generate(
    panel_xml: &Path,
    mode: Mode,
    footprints: Footprints,
    layers: Layers,
    bed: Bed,
    interposer_id: Option<u16>,
) -> Result<(String, String, Option<String>)> {
    let Built {
        panel,
        lands,
        plan,
        placement,
        fixture,
        ..
    } = build(panel_xml, mode, footprints, bed, layers)?;
    if let Some(fixture) = &fixture {
        for issue in &fixture.issues {
            eprintln!("\x1b[1;31mWARNING:\x1b[0m {issue}");
        }
    }
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
pub fn fixture_map(
    panel_xml: &Path,
    mode: Mode,
    footprints: Footprints,
    layers: Layers,
    bed: Bed,
) -> Result<String> {
    let Built {
        panel,
        lands,
        plan,
        placement,
        ..
    } = build(panel_xml, mode, footprints, bed, layers)?;
    let plan = plan.context("panel has no ICT contacts to plan")?;
    Ok(plan::to_json(&plan, &panel, &lands, placement.as_ref()))
}

/// The bench fixture's printed and bought parts for a panel file, on a
/// bed with stack dowels ([`shroud`]).
pub struct FixtureFiles {
    /// The upper shroud, as OpenSCAD source.
    pub shroud: String,
    /// The lower shroud's relief map, as JSON.
    pub relief: String,
    /// The fixture BOM and stack report, as Markdown.
    pub bom: String,
}

/// Generate the bench fixture's upper shroud, lower-shroud relief map and
/// BOM for a panel file.
pub fn fixture(
    panel_xml: &Path,
    mode: Mode,
    footprints: Footprints,
    layers: Layers,
    bed: Bed,
) -> Result<FixtureFiles> {
    let Built {
        panel,
        plan,
        boards,
        fixture,
        ..
    } = build(panel_xml, mode, footprints, bed, layers)?;
    let plan = plan.context("panel has no ICT contacts to plan")?;
    let fixture =
        fixture.with_context(|| format!("the {bed:?} bed has no stack dowels for a shroud"))?;
    let shroud = shroud::scad(&shroud::Shroud {
        panel: &panel,
        bed,
        plan: &plan,
        boards: &boards,
        parts: &fixture.parts,
        locating: &fixture.locating,
        stack: &fixture.stack,
        dut: fixture.dut,
    })?;
    Ok(FixtureFiles {
        shroud,
        relief: shroud::relief_map(&fixture.stack, &fixture.locating),
        bom: shroud::bom(
            &fixture.stack,
            &fixture.locating,
            plan.bindings.len(),
            &fixture.issues,
        ),
    })
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
    fixture: Option<Fixture>,
}

/// The bench fixture around the interposer, on a bed with stack dowels.
struct Fixture {
    stack: shroud::Stack,
    parts: Vec<parts::Part>,
    locating: shroud::Locating,
    /// The DUT's thickness.
    dut: f64,
    issues: Vec<String>,
}

/// The DUT's finished thickness, from its stackup.
fn dut_thickness(ipc: &ipc2581::Ipc2581) -> f64 {
    ipc.ecad()
        .and_then(|ecad| ecad.cad_data.stackups.first())
        .and_then(|stackup| stackup.overall_thickness)
        .unwrap_or(1.6)
}

fn build(
    panel_xml: &Path,
    mode: Mode,
    footprints: Footprints,
    bed: Bed,
    layers: Layers,
) -> Result<Built> {
    let content = std::fs::read_to_string(panel_xml)
        .with_context(|| format!("read panel {}", panel_xml.display()))?;
    let ipc = ipc2581::Ipc2581::parse(&content).context("parse IPC-2581 panel")?;
    let (mut panel, contacts, boards, placement, tooling, parts) = match mode {
        Mode::Panel => {
            let raw = panel::extract(&ipc)?;
            let tooling = raw.holes.clone();
            let panel = bed.fit(raw);
            let contacts = contacts::extract_contacts(&ipc, panel.height, footprints)?;
            let boards = contacts::board_outlines(&ipc, panel.height)?;
            let (step, offsets) = contacts::board_instances(&ipc)?;
            let height = panel.height;
            let mut parts = Vec::new();
            for (board, [dx, dy]) in offsets.into_iter().enumerate() {
                let place = move |[x, y]: [f64; 2]| [x + dx, height - (y + dy)];
                parts.extend(parts::probed_parts(
                    &ipc,
                    step,
                    ipc2581::types::Side::Bottom,
                    &[(board as u32, &place)],
                )?);
            }
            (panel, contacts, boards, None, tooling, parts)
        }
        Mode::SingleBoard => {
            let placed = single::place(&ipc, bed, footprints)?;
            let placement = placed.placement;
            let place = move |p: [f64; 2]| placement.apply(p);
            let side = if placement.mirrored {
                ipc2581::types::Side::Top
            } else {
                ipc2581::types::Side::Bottom
            };
            let parts =
                parts::probed_parts(&ipc, parts::component_step(&ipc)?, side, &[(0, &place)])?;
            (
                placed.panel,
                placed.contacts,
                vec![placed.board],
                Some(placed.placement),
                Vec::new(),
                parts,
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
    let fixture = match (&plan, bed.alignment().is_empty()) {
        (Some(plan), false) => {
            let dut = dut_thickness(&ipc);
            let stack = shroud::stack(emit::thickness(layers), dut)?;
            let pogos: Vec<[f64; 2]> = plan
                .bindings
                .iter()
                .map(|binding| plan.contacts[binding.contact].xy)
                .collect();
            let footprint = shroud::footprint(&panel, bed);
            let locating = match mode {
                Mode::SingleBoard => shroud::Locating::Nest,
                Mode::Panel => {
                    shroud::choose_pins(&tooling, footprint, bed, &pogos, &parts, &lands)
                }
            };
            let mut issues = shroud::part_conflicts(bed, &parts);
            match &locating {
                shroud::Locating::Pins(pins) => {
                    if stack.pin.is_none() {
                        issues.push(
                            "no stock DUT pin reaches the DUT before the pogos do".to_string(),
                        );
                    }
                    // The pins reach down into the interposer.
                    for pin in pins {
                        panel.holes.push((pin.at, shroud::PIN_RELIEF_MM));
                    }
                }
                shroud::Locating::Missing(why) => {
                    issues.push(format!(
                        "no pair of the panel's tooling holes can take the DUT's Ø2 locating \
                         pins; choose tooling positions by hand:\n    {}",
                        why.join("\n    ")
                    ));
                }
                shroud::Locating::Nest => {}
            }
            Some(Fixture {
                stack,
                parts,
                locating,
                dut,
                issues,
            })
        }
        _ => None,
    };
    Ok(Built {
        panel,
        lands,
        plan,
        boards,
        placement,
        fixture,
    })
}
