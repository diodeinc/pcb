//! A7 mate constellation generators (S1, S2, S3).

use crate::types::{Kind, MatePin, MatePinId, Shape, Slot, SlotId};

pub const A7_W: f64 = 74.0;
pub const A7_H: f64 = 105.0;
pub const MARGIN: f64 = 5.0;
pub const PITCH_254: f64 = 2.54;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    S1,
    S2,
    S3,
    /// USB/power on the A7 edges that face the rest of the panel; wide LS streets.
    S8,
    /// Per-board kits placed on the A7 perimeter in the boards' angular order.
    S9,
}

impl PatternKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::S1 => "S1",
            Self::S2 => "S2",
            Self::S3 => "S3",
            Self::S8 => "S8",
            Self::S9 => "S9",
        }
    }

    pub fn all() -> [PatternKind; 5] {
        [Self::S1, Self::S2, Self::S3, Self::S8, Self::S9]
    }

    /// Placement strategies we iterate for the high-coverage POC.
    pub fn eval() -> [PatternKind; 3] {
        [Self::S8, Self::S9, Self::S2]
    }
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    pub pitch: f64,
    pub pins: Vec<MatePin>,
    pub slots: Vec<Slot>,
    pub n_arrays: usize,
    pub unused_pins: usize,
}

/// 2×n pogo array at `origin`, row-major, pitch along +X then +Y.
fn array_pins(origin: [f64; 2], n: usize, pitch: f64, next: &mut u32) -> Vec<MatePin> {
    let cols = 2;
    let mut pins = Vec::with_capacity(n);
    for i in 0..n {
        let col = i % cols;
        let row = i / cols;
        let id = MatePinId(*next);
        *next += 1;
        pins.push(MatePin {
            id,
            xy: [
                origin[0] + col as f64 * pitch,
                origin[1] + row as f64 * pitch,
            ],
        });
    }
    pins
}

fn push_unit_slots(slots: &mut Vec<Slot>, pins: &[MatePin], kind: Kind, next_slot: &mut u32) {
    for p in pins {
        let id = SlotId(*next_slot);
        *next_slot += 1;
        slots.push(Slot {
            id,
            kind,
            shape: Shape::Unit,
            pins: vec![p.id],
        });
    }
}

fn push_ordered_pair(slots: &mut Vec<Slot>, a: MatePinId, b: MatePinId, next_slot: &mut u32) {
    let id = SlotId(*next_slot);
    *next_slot += 1;
    slots.push(Slot {
        id,
        kind: Kind::UsbHs,
        shape: Shape::Ordered { n: 2 },
        pins: vec![a, b],
    });
}

fn push_vt_bank(slots: &mut Vec<Slot>, a: MatePinId, b: MatePinId, next_slot: &mut u32) {
    let id = SlotId(*next_slot);
    *next_slot += 1;
    slots.push(Slot {
        id,
        kind: Kind::Vtarget,
        shape: Shape::Unordered { n: 2 },
        pins: vec![a, b],
    });
}

fn a7_center() -> [f64; 2] {
    [A7_W * 0.5, A7_H * 0.5]
}

fn angle_of(p: [f64; 2]) -> f64 {
    let c = a7_center();
    (p[1] - c[1]).atan2(p[0] - c[0])
}

/// Perimeter kit sites facing the rest of the panel (+X then +Y).
fn perimeter_sites() -> Vec<[f64; 2]> {
    let mut s = Vec::new();
    // Right edge, walking +Y (A5/A6 enter from +X).
    for i in 0..5 {
        s.push([A7_W - MARGIN - 6.0, MARGIN + 10.0 + i as f64 * 15.0]);
    }
    // Top edge, walking +X.
    for i in 0..3 {
        s.push([MARGIN + 12.0 + i as f64 * 16.0, A7_H - MARGIN - 8.0]);
    }
    s
}

/// S8/S9: edge-facing USB+power kits, wide-street LS field.
pub fn generate_pattern_at(kind: PatternKind, pitch: f64, centroids: &[[f64; 2]]) -> Pattern {
    if !matches!(kind, PatternKind::S8 | PatternKind::S9) {
        return generate_pattern(kind, pitch);
    }
    let mut next_pin = 0u32;
    let mut next_slot = 0u32;
    let mut pins = Vec::new();
    let mut slots = Vec::new();
    let mut n_arrays = 0usize;

    let mut sites = perimeter_sites();
    if kind == PatternKind::S9 && !centroids.is_empty() {
        let mut order: Vec<usize> = (0..centroids.len().min(8)).collect();
        order.sort_by(|a, b| {
            angle_of(centroids[*a])
                .partial_cmp(&angle_of(centroids[*b]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let raw = perimeter_sites();
        let mut placed = Vec::new();
        for (k, _) in order.into_iter().enumerate() {
            placed.push(raw[k % raw.len()]);
        }
        while placed.len() < 8 {
            placed.push(raw[placed.len() % raw.len()]);
        }
        sites = placed;
    }

    // 8 kits of (DP, DM, VT, VT, VUSB, GND) at perimeter sites.
    for origin in sites.iter().take(8) {
        let block = array_pins(*origin, 6, pitch, &mut next_pin);
        push_ordered_pair(&mut slots, block[0].id, block[1].id, &mut next_slot);
        push_vt_bank(&mut slots, block[2].id, block[3].id, &mut next_slot);
        push_unit_slots(
            &mut slots,
            std::slice::from_ref(&block[4]),
            Kind::Vusb,
            &mut next_slot,
        );
        push_unit_slots(
            &mut slots,
            std::slice::from_ref(&block[5]),
            Kind::Gnd,
            &mut next_slot,
        );
        pins.extend(block);
        n_arrays += 1;
    }

    // LS field in the A7 core with ~5 mm streets.
    let street = 5.0;
    let mut x = MARGIN + 4.0;
    let y_ls = MARGIN + 8.0;
    for _ in 0..6 {
        let block = array_pins([x, y_ls], 8, pitch, &mut next_pin);
        push_unit_slots(&mut slots, &block, Kind::Ls, &mut next_slot);
        pins.extend(block);
        n_arrays += 1;
        x += 2.0 * pitch + street;
    }
    // leftover 8 GND in a 2×4 at the origin corner (away from the funnel).
    let block = array_pins(
        [MARGIN + 4.0, MARGIN + 4.0 + 5.0 * pitch],
        8,
        pitch,
        &mut next_pin,
    );
    push_unit_slots(&mut slots, &block, Kind::Gnd, &mut next_slot);
    pins.extend(block);
    n_arrays += 1;

    Pattern {
        kind,
        pitch,
        pins,
        slots,
        n_arrays,
        unused_pins: 0,
    }
}

pub fn generate_pattern(kind: PatternKind, pitch: f64) -> Pattern {
    let mut next_pin = 0u32;
    let mut next_slot = 0u32;
    let mut pins = Vec::new();
    let mut slots = Vec::new();
    let mut n_arrays = 0usize;

    // Usable A7 interior, origin-corner mate.
    let x0 = MARGIN + 3.0;
    let y0 = MARGIN + 3.0;
    let block_dx = 2.0 * pitch + 3.0;

    match kind {
        PatternKind::S1 => {
            // 6×8 LS, 2×8 USB, 2×8 Vtarget, 1×8 VUSB, 2×8 GND = 13 arrays.
            let mut x = x0;
            let y_ls = y0 + 6.0 * pitch;
            for _ in 0..6 {
                let block = array_pins([x, y_ls], 8, pitch, &mut next_pin);
                push_unit_slots(&mut slots, &block, Kind::Ls, &mut next_slot);
                pins.extend(block);
                n_arrays += 1;
                x += block_dx;
            }
            x = x0;
            let y_usb = y0;
            for _ in 0..2 {
                let block = array_pins([x, y_usb], 8, pitch, &mut next_pin);
                for pair in block.chunks(2) {
                    push_ordered_pair(&mut slots, pair[0].id, pair[1].id, &mut next_slot);
                }
                pins.extend(block);
                n_arrays += 1;
                x += block_dx;
            }
            x = x0 + 2.0 * block_dx;
            for _ in 0..2 {
                let block = array_pins([x, y_usb], 8, pitch, &mut next_pin);
                for pair in block.chunks(2) {
                    push_vt_bank(&mut slots, pair[0].id, pair[1].id, &mut next_slot);
                }
                pins.extend(block);
                n_arrays += 1;
                x += block_dx;
            }
            let block = array_pins([x0 + 4.0 * block_dx, y_usb], 8, pitch, &mut next_pin);
            push_unit_slots(&mut slots, &block, Kind::Vusb, &mut next_slot);
            pins.extend(block);
            n_arrays += 1;
            x = x0 + 5.0 * block_dx;
            for _ in 0..2 {
                let block = array_pins([x, y0 + 12.0], 8, pitch, &mut next_pin);
                push_unit_slots(&mut slots, &block, Kind::Gnd, &mut next_slot);
                pins.extend(block);
                n_arrays += 1;
                x += block_dx;
            }
        }
        PatternKind::S2 => {
            // 8× (DP, DM, GND, GND) + 6×8 LS + 2×8 VT + 1×8 VUSB.
            let mut x = x0;
            for _ in 0..8 {
                let block = array_pins([x, y0], 4, pitch, &mut next_pin);
                push_ordered_pair(&mut slots, block[0].id, block[1].id, &mut next_slot);
                push_unit_slots(&mut slots, &block[2..], Kind::Gnd, &mut next_slot);
                pins.extend(block);
                n_arrays += 1;
                x += block_dx;
            }
            x = x0;
            let y_ls = y0 + 4.0 * pitch;
            for _ in 0..6 {
                let block = array_pins([x, y_ls], 8, pitch, &mut next_pin);
                push_unit_slots(&mut slots, &block, Kind::Ls, &mut next_slot);
                pins.extend(block);
                n_arrays += 1;
                x += block_dx;
            }
            x = x0;
            let y_pwr = y0 + 10.0 * pitch;
            for _ in 0..2 {
                let block = array_pins([x, y_pwr], 8, pitch, &mut next_pin);
                for pair in block.chunks(2) {
                    push_vt_bank(&mut slots, pair[0].id, pair[1].id, &mut next_slot);
                }
                pins.extend(block);
                n_arrays += 1;
                x += block_dx;
            }
            let block = array_pins([x, y_pwr], 8, pitch, &mut next_pin);
            push_unit_slots(&mut slots, &block, Kind::Vusb, &mut next_slot);
            pins.extend(block);
            n_arrays += 1;
        }
        PatternKind::S3 => {
            // 8 board kits of 6: DP DM VT VT VUSB GND + 6×8 LS + 2×4 leftover GND.
            let mut x = x0;
            for _ in 0..8 {
                let block = array_pins([x, y0], 6, pitch, &mut next_pin);
                push_ordered_pair(&mut slots, block[0].id, block[1].id, &mut next_slot);
                push_vt_bank(&mut slots, block[2].id, block[3].id, &mut next_slot);
                push_unit_slots(
                    &mut slots,
                    std::slice::from_ref(&block[4]),
                    Kind::Vusb,
                    &mut next_slot,
                );
                push_unit_slots(
                    &mut slots,
                    std::slice::from_ref(&block[5]),
                    Kind::Gnd,
                    &mut next_slot,
                );
                pins.extend(block);
                n_arrays += 1;
                x += block_dx;
            }
            x = x0;
            let y_ls = y0 + 5.0 * pitch;
            for _ in 0..6 {
                let block = array_pins([x, y_ls], 8, pitch, &mut next_pin);
                push_unit_slots(&mut slots, &block, Kind::Ls, &mut next_slot);
                pins.extend(block);
                n_arrays += 1;
                x += block_dx;
            }
            x = x0;
            let y_g = y0 + 11.0 * pitch;
            for _ in 0..2 {
                let block = array_pins([x, y_g], 4, pitch, &mut next_pin);
                push_unit_slots(&mut slots, &block, Kind::Gnd, &mut next_slot);
                pins.extend(block);
                n_arrays += 1;
                x += block_dx;
            }
        }
        PatternKind::S8 | PatternKind::S9 => {
            // Filled by generate_pattern_at — fallback to edge-facing S8.
            return generate_pattern_at(kind, pitch, &[]);
        }
    }

    let unused = 0; // generators emit exactly the 104-land budget when filled
    Pattern {
        kind,
        pitch,
        pins,
        slots,
        n_arrays,
        unused_pins: unused,
    }
}

pub fn attach_pattern(problem: &mut crate::types::Problem, pattern: &Pattern) {
    for p in &pattern.pins {
        problem.pins.insert(p.id, p.clone());
    }
    for s in &pattern.slots {
        problem.slots.insert(s.id, s.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Kind;

    #[test]
    fn s1_has_land_budget() {
        let p = generate_pattern(PatternKind::S1, PITCH_254);
        assert_eq!(p.pins.len(), 104);
        assert_eq!(p.n_arrays, 13);
        let ls = p.slots.iter().filter(|s| s.kind == Kind::Ls).count();
        assert_eq!(ls, 48);
        let usb = p.slots.iter().filter(|s| s.kind == Kind::UsbHs).count();
        assert_eq!(usb, 8);
    }

    #[test]
    fn s8_kits_sit_on_funnel_edges() {
        let p = generate_pattern(PatternKind::S8, PITCH_254);
        assert_eq!(p.slots.iter().filter(|s| s.kind == Kind::UsbHs).count(), 8);
        assert_eq!(p.slots.iter().filter(|s| s.kind == Kind::Ls).count(), 48);
        // USB pins should hug the +X or +Y side of the A7, not the origin core.
        let usb_pins: Vec<_> = p
            .slots
            .iter()
            .filter(|s| s.kind == Kind::UsbHs)
            .flat_map(|s| s.pins.iter())
            .map(|id| p.pins.iter().find(|pin| pin.id == *id).unwrap().xy)
            .collect();
        let on_edge = usb_pins
            .iter()
            .filter(|xy| xy[0] > 50.0 || xy[1] > 80.0)
            .count();
        assert!(on_edge >= 8, "USB should face the panel funnel");
    }

    #[test]
    fn s3_has_eight_kits() {
        let p = generate_pattern(PatternKind::S3, PITCH_254);
        assert_eq!(p.slots.iter().filter(|s| s.kind == Kind::UsbHs).count(), 8);
        assert_eq!(
            p.slots.iter().filter(|s| s.kind == Kind::Vtarget).count(),
            8
        );
        assert_eq!(p.slots.iter().filter(|s| s.kind == Kind::Vusb).count(), 8);
        assert!(p.pins.iter().all(|pin| {
            pin.xy[0] >= MARGIN && pin.xy[1] >= MARGIN && pin.xy[0] <= A7_W - MARGIN
        }));
    }
}
