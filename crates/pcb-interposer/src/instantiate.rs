//! Panel instantiation: pack ≤8 boards on A5/A6/A7 and transform contacts.

use crate::types::{BoardId, Contact, ContactId};

#[derive(Debug, Clone, Copy)]
pub struct Sheet {
    pub name: &'static str,
    pub w: f64,
    pub h: f64,
}

pub const A7: Sheet = Sheet {
    name: "A7",
    w: 74.0,
    h: 105.0,
};
pub const A6: Sheet = Sheet {
    name: "A6",
    w: 105.0,
    h: 148.0,
};
pub const A5: Sheet = Sheet {
    name: "A5",
    w: 148.0,
    h: 210.0,
};

pub fn sheet_by_name(name: &str) -> Option<Sheet> {
    match name.to_ascii_uppercase().as_str() {
        "A7" => Some(A7),
        "A6" => Some(A6),
        "A5" => Some(A5),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct Placement {
    pub board: BoardId,
    pub origin: [f64; 2],
}

/// Pack as many copies as fit, capped at 8, with 5 mm rails and 3 mm gaps.
pub fn pack(sheet: Sheet, board_w: f64, board_h: f64, max_boards: u32) -> Vec<Placement> {
    let rail = 5.0;
    let gap = 3.0;
    let usable_w = sheet.w - 2.0 * rail;
    let usable_h = sheet.h - 2.0 * rail;
    let cell_w = board_w + gap;
    let cell_h = board_h + gap;
    if cell_w <= 0.0 || cell_h <= 0.0 {
        return Vec::new();
    }
    let cols = ((usable_w + gap) / cell_w).floor() as u32;
    let rows = ((usable_h + gap) / cell_h).floor() as u32;
    let n = (cols * rows).min(max_boards).min(8);
    let mut out = Vec::new();
    for i in 0..n {
        let c = i % cols;
        let r = i / cols;
        out.push(Placement {
            board: BoardId(i),
            origin: [rail + c as f64 * cell_w, rail + r as f64 * cell_h],
        });
    }
    if out.is_empty() {
        out.push(Placement {
            board: BoardId(0),
            origin: [rail, rail],
        });
    }
    out
}

/// Shift KiCad-absolute contacts so the board bbox origin is (0,0).
pub fn localize(contacts: &mut [Contact], origin: [f64; 2]) {
    for c in contacts {
        c.xy[0] -= origin[0];
        c.xy[1] -= origin[1];
    }
}

/// Clone DUT-local contacts into each placement. `src` uses board-local XY.
pub fn instantiate(src: &[Contact], placements: &[Placement]) -> Vec<Contact> {
    let mut out = Vec::new();
    let mut next = 0u32;
    for place in placements {
        for c in src {
            out.push(Contact {
                id: ContactId(next),
                board: place.board,
                xy: [c.xy[0] + place.origin[0], c.xy[1] + place.origin[1]],
                ict: c.ict,
                path: format!("B{}/{}", place.board.0, c.path),
                package: c.package.clone(),
                side: c.side.clone(),
            });
            next += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a5_packs_up_to_eight() {
        let p = pack(A5, 30.0, 20.0, 8);
        assert!(p.len() >= 2);
        assert!(p.len() <= 8);
    }
}
