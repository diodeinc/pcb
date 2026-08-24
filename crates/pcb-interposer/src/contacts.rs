//! Extract a panel's ICT contacts, instantiated per board.
//!
//! The board-array file carries the board's components once (in the
//! repeated board step) and the placement grid as step repeats; the BOM
//! carries each component's `Ict` role; only `TestPoint_ICT`-footprint
//! components qualify. This joins the three into one
//! contact list in the interposer's Y-down sheet frame — one entry per
//! fixture-facing test point per board instance.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use ipc2581::Ipc2581;

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
    /// Position in the Y-down sheet frame.
    pub xy: [f64; 2],
}

/// Parse an `ict` role name into the fixture vocabulary. Low-speed debug
/// roles collapse into [`Role::Ls`]; unknown names are `None`.
fn parse_role(name: &str) -> Option<Role> {
    Some(match name {
        "usb_dp" => Role::UsbDp,
        "usb_dm" => Role::UsbDm,
        "vusb" => Role::Vusb,
        "vtarget" => Role::Vtarget,
        "gnd" => Role::Gnd,
        "ls" | "swdio" | "swclk" | "nrst" | "swo" => Role::Ls,
        _ => return None,
    })
}

/// Extract every board instance's ICT contacts. `sheet_height` is the
/// panel height used for the Y flip (from [`crate::panel::extract`]).
pub fn extract_contacts(ipc: &Ipc2581, sheet_height: f64) -> Result<Vec<PanelContact>> {
    let ecad = ipc.ecad().context("panel has no ECAD section")?;
    let steps = &ecad.cad_data.steps;
    let array = crate::panel::primary_step(ipc, steps).context("panel has no step")?;

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

    let roles = bom_roles(ipc);
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
                    xy: [x, sheet_height - y],
                });
            }
        }
    }
    if unknown > 0 {
        eprintln!("warning: skipped {unknown} contacts with unrecognized ict roles");
    }
    Ok(contacts)
}

/// Designator → (`Ict` role name, zen path) from the BOM.
fn bom_roles(ipc: &Ipc2581) -> BTreeMap<String, (String, String)> {
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
        for ref_des in &item.ref_des_list {
            if !is_ict_package(ipc.resolve(ref_des.package_ref)) {
                continue;
            }
            let refdes = ipc.resolve(ref_des.name).to_string();
            if !refdes.is_empty() {
                roles.insert(refdes, (ict.clone(), path.clone()));
            }
        }
    }
    roles
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
