//! Extract a panel's ICT contacts, instantiated per board.
//!
//! The board-array file carries the board's components once (in the
//! repeated board step) and the placement grid as step repeats; the BOM
//! carries each component's `Ict` role. A component with a role but not on
//! the `TestPoint_ICT` footprint draws a warning, since its pad may be too
//! small for a pogo, and is probed only under [`Footprints::Any`]. This
//! joins the three into one
//! contact list in the interposer's Y-down sheet frame — one entry per
//! fixture-facing test point per board instance.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use ipc2581::Ipc2581;
use ipc2581::types::{Component, LayerFunction, Side, Step, StepRepeat};

use crate::pattern::Role;

/// One ICT contact of one board instance.
#[derive(Debug, Clone)]
pub struct PanelContact {
    /// Board instance index, in grid order.
    pub board: u32,
    pub refdes: String,
    /// The component's zen `Path`, when the export carries one.
    pub path: String,
    pub role: Role,
    /// The `ict` name the role was parsed from (`swclk`, `ls`, …).
    pub ict: String,
    /// Position in the Y-down sheet frame.
    pub xy: [f64; 2],
}

/// Parse an `ict` role name into the fixture vocabulary. Low-speed debug
/// roles collapse into [`Role::Ls`] (the contact keeps its `ict` name for
/// beds that pin them); unknown names are `None`.
pub(crate) fn parse_role(name: &str) -> Option<Role> {
    Some(match name {
        "usb_dp" => Role::UsbDp,
        "usb_dm" => Role::UsbDm,
        "vusb" => Role::Vusb,
        "vtarget" => Role::Vtarget,
        "gnd" => Role::Gnd,
        "ls" | "swdio" | "swclk" | "nrst" | "swo" => Role::Ls,
        "analog" => Role::Analog,
        _ => return None,
    })
}

/// Extract every board instance's ICT contacts. `sheet_height` is the
/// panel height used for the Y flip (from [`crate::panel::extract`]).
/// The panel's board grid: the array's repeat of the board cell, the
/// cell's constant offset of the board step, and the board step.
fn board_grid(ipc: &Ipc2581) -> Result<(&StepRepeat, &StepRepeat, &Step)> {
    let ecad = ipc.ecad().context("panel has no ECAD section")?;
    let steps = &ecad.cad_data.steps;
    let array = pcb_ir::import::ipc2581::primary_step(ipc, steps).context("panel has no step")?;

    // The generator's fixed shape: array step → grid repeat of the board
    // cell → the cell places the board step at a constant offset.
    let find = |name: ipc2581::Symbol| steps.iter().find(|step| step.name == name);
    let mut grid = None;
    for repeat in &array.step_repeats {
        let Some(cell) = find(repeat.step_ref) else {
            continue;
        };
        for inner in &cell.step_repeats {
            let Some(board_step) = find(inner.step_ref) else {
                continue;
            };
            if !board_step.components.is_empty() {
                grid = Some((repeat, inner, board_step));
            }
        }
    }
    let Some((grid, offset, board_step)) = grid else {
        bail!("panel has no repeated board step with components");
    };
    if offset.angle.abs() > 1e-6 || grid.angle.abs() > 1e-6 {
        bail!("rotated board cells are not supported");
    }
    Ok((grid, offset, board_step))
}

/// Every board instance's bounding box `[x0, y0, x1, y1]` in the Y-down
/// sheet frame, in grid order.
pub fn board_outlines(ipc: &Ipc2581, sheet_height: f64) -> Result<Vec<[f64; 4]>> {
    let (grid, offset, board_step) = board_grid(ipc)?;
    let profile = board_step
        .profile
        .as_ref()
        .context("board step has no profile")?;
    let points = profile.polygon.points();
    let x0 = points.iter().map(|p| p.x).fold(f64::MAX, f64::min);
    let x1 = points.iter().map(|p| p.x).fold(f64::MIN, f64::max);
    let y0 = points.iter().map(|p| p.y).fold(f64::MAX, f64::min);
    let y1 = points.iter().map(|p| p.y).fold(f64::MIN, f64::max);
    let mut boards = Vec::new();
    for j in 0..grid.ny.max(1) {
        for i in 0..grid.nx.max(1) {
            let dx = offset.x + grid.x + i as f64 * grid.dx;
            let dy = offset.y + grid.y + j as f64 * grid.dy;
            boards.push([
                x0 + dx,
                sheet_height - (y1 + dy),
                x1 + dx,
                sheet_height - (y0 + dy),
            ]);
        }
    }
    Ok(boards)
}

pub fn extract_contacts(
    ipc: &Ipc2581,
    sheet_height: f64,
    footprints: Footprints,
) -> Result<Vec<PanelContact>> {
    let (grid, offset, board_step) = board_grid(ipc)?;

    let roles = bom_roles(ipc, footprints);
    let mut contacts = Vec::new();
    let mut unknown = 0usize;
    for j in 0..grid.ny.max(1) {
        for i in 0..grid.nx.max(1) {
            let board = j * grid.nx.max(1) + i;
            for component in &board_step.components {
                let Some(ref_des) = component.ref_des else {
                    continue;
                };
                let refdes = ipc.resolve(ref_des).to_string();
                let Some((ict, path)) = roles.get(&refdes) else {
                    continue;
                };
                let Some(role) = parse_role(ict) else {
                    unknown += 1;
                    continue;
                };
                let x = component.location.x + offset.x + grid.x + i as f64 * grid.dx;
                let y = component.location.y + offset.y + grid.y + j as f64 * grid.dy;
                contacts.push(PanelContact {
                    board,
                    refdes: refdes.clone(),
                    path: path.clone(),
                    role,
                    ict: ict.clone(),
                    xy: [x, sheet_height - y],
                });
            }
        }
    }
    if unknown > 0 {
        eprintln!("warning: skipped {unknown} contacts with unrecognized ict roles");
    }
    // The bed probes the panel's bottom face; a panel laid face down would
    // need its tooling mirrored too.
    let top: Vec<&str> = board_step
        .components
        .iter()
        .filter(|component| component_side(ipc, component) == Some(Side::Top))
        .filter_map(|component| component.ref_des.map(|r| ipc.resolve(r)))
        .filter(|refdes| contacts.iter().any(|contact| contact.refdes == *refdes))
        .collect();
    if !top.is_empty() {
        bail!(
            "the panel's ICT contacts {} are on its top face; a panel fixture probes the bottom \
             face (test one board face down with --single-board)",
            top.join(", ")
        );
    }
    Ok(contacts)
}

/// The copper face a component is placed on, when its layer names one.
pub(crate) fn component_side(ipc: &Ipc2581, component: &Component) -> Option<Side> {
    let ecad = ipc.ecad()?;
    ecad.cad_data
        .layers
        .iter()
        .find(|layer| {
            layer.name == component.layer_ref && layer.layer_function == LayerFunction::Conductor
        })
        .and_then(|layer| layer.side)
}

/// Which footprints of test points with an `Ict` role get a pogo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Footprints {
    /// Only the TestPoint ICT variant's footprint; others are skipped.
    #[default]
    IctOnly,
    /// Any footprint.
    Any,
}

/// Designator → (`Ict` role name, zen path) from the BOM, for the test
/// points `footprints` admits. Each test point with a role on another
/// footprint draws a warning either way.
pub(crate) fn bom_roles(
    ipc: &Ipc2581,
    footprints: Footprints,
) -> BTreeMap<String, (String, String)> {
    let mut roles = BTreeMap::new();
    let Some(bom) = ipc.bom() else {
        return roles;
    };
    for item in &bom.items {
        let Some(chars) = item.characteristics.as_ref() else {
            continue;
        };
        let mut ict = None;
        let mut path = String::new();
        for textual in &chars.textuals {
            let (Some(name), Some(value)) = (textual.name, textual.value) else {
                continue;
            };
            let name = ipc.resolve(name);
            if name.eq_ignore_ascii_case("ict") {
                ict = Some(ipc.resolve(value).to_string());
            } else if name.eq_ignore_ascii_case("path") {
                path = ipc.resolve(value).to_string();
            }
        }
        let Some(ict) = ict else { continue };
        for ref_des in item.reference_designators() {
            let refdes = ipc.resolve(ref_des.name).to_string();
            if refdes.is_empty() {
                continue;
            }
            let package = ref_des.package_ref.map(|package| ipc.resolve(package));
            if !package.is_some_and(is_ict_package) {
                // Loudly either way: a pad not sized for a pogo may be
                // missed once tolerances stack up, or crowd its neighbours.
                if first_warning(&refdes, package) {
                    let action = match footprints {
                        Footprints::IctOnly => {
                            "not probed (pass --probe-non-ict-footprints to probe it)"
                        }
                        Footprints::Any => {
                            "probing it anyway, but its pad may be too small for a pogo"
                        }
                    };
                    eprintln!(
                        "\x1b[1;31mWARNING:\x1b[0m {refdes} has ict role `{ict}` but footprint \
                         `{}`, not the TestPoint ICT variant; {action}",
                        package.unwrap_or("none")
                    );
                }
                if footprints == Footprints::IctOnly {
                    continue;
                }
            }
            roles.insert(refdes, (ict.clone(), path.clone()));
        }
    }
    roles
}

/// Whether this is the first footprint warning for `refdes`: one run
/// builds the interposer, its plate and its map from the same export, and
/// each would otherwise repeat every warning.
fn first_warning(refdes: &str, package: Option<&str>) -> bool {
    use std::sync::{Mutex, OnceLock};
    static WARNED: OnceLock<Mutex<std::collections::BTreeSet<(String, String)>>> = OnceLock::new();
    WARNED
        .get_or_init(Default::default)
        .lock()
        .map(|mut warned| warned.insert((refdes.to_string(), package.unwrap_or("").to_string())))
        .unwrap_or(true)
}

/// The `TestPoint` ICT variant's footprint, allowing the `_<n>` suffix
/// board-array creation appends when deduplicating package names.
/// Mirrors `pcb_ipc2581_tools::commands::ict::is_ict_package`.
fn is_ict_package(name: &str) -> bool {
    const FOOTPRINT: &str = "TestPoint_ICT";
    match name.strip_prefix(FOOTPRINT) {
        Some("") => true,
        Some(rest) => rest
            .strip_prefix('_')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
        None => false,
    }
}
