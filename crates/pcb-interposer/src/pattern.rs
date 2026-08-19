//! A7 mate constellation generators (S1, S2, S3).

use crate::types::{Kind, MatePin, MatePinId, Shape, Slot, SlotId};

pub const A7_W: f64 = 74.0;
pub const A7_H: f64 = 105.0;
pub const MARGIN: f64 = 5.0;
pub const PITCH_254: f64 = 2.54;

/// Dimensions of the A7 mate region at the origin corner of a sheet, by the
/// ISO fold rule: each halving cuts the sheet's long side, so the A7
/// descendant alternates orientation per fold. A7 → 74×105, A6 → 105×74,
/// A5 → 74×105, A4 → 105×74.
pub fn mate_dims(sheet_w: f64, sheet_h: f64) -> (f64, f64) {
    let (mut w, mut h) = (sheet_w, sheet_h);
    while w * h > A7_W * A7_H * 1.02 {
        if w >= h {
            w /= 2.0;
        } else {
            h /= 2.0;
        }
    }
    (w, h)
}

/// Rotate a canonically generated (74×105) pattern into the sheet's folded
/// mate orientation. A proper 90° rotation — the mate is a rigid contract,
/// never mirrored. Of the two fold-valid rotations we commit to the one
/// that puts the long band along the sheet-edge strip; it scored strictly
/// better than the fold-line side on the corpus (boards cover the whole
/// sheet, so the quiet margin beats "funnel-facing").
pub fn orient_pattern(pattern: &mut Pattern, sheet_w: f64, sheet_h: f64) {
    let (mw, mh) = mate_dims(sheet_w, sheet_h);
    if mw > mh {
        for p in &mut pattern.pins {
            p.xy = [p.xy[1], A7_W - p.xy[0]];
        }
    }
    let _ = mh;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    S1,
    S2,
    S3,
    /// USB/power on the A7 edges that face the rest of the panel; wide LS streets.
    S8,
    /// Per-board kits placed on the A7 perimeter in the boards' angular order.
    S9,
    /// Ring: every land in a double-row band along the two funnel-facing A7
    /// edges. No core to clog; every pad is at most one pad deep.
    S10,
    /// Perimeter: structures around all four mate edges, 9 mm apart.
    S11,
    /// Cluster grid: 3×5 structure sites across the mate, wide streets.
    S12,
}

impl PatternKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::S1 => "S1",
            Self::S2 => "S2",
            Self::S3 => "S3",
            Self::S8 => "S8",
            Self::S9 => "S9",
            Self::S10 => "S10",
            Self::S11 => "S11",
            Self::S12 => "S12",
        }
    }

    pub fn all() -> [PatternKind; 8] {
        [
            Self::S1,
            Self::S2,
            Self::S3,
            Self::S8,
            Self::S9,
            Self::S10,
            Self::S11,
            Self::S12,
        ]
    }

    /// Placement strategies we iterate: the tight L-band and the two
    /// roomy-spacing layouts.
    pub fn eval() -> [PatternKind; 3] {
        [Self::S10, Self::S11, Self::S12]
    }
}

#[derive(Debug, Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    pub pitch: f64,
    pub pins: Vec<MatePin>,
    pub slots: Vec<Slot>,
    /// Pin groups per physical pogo-array connector (2×3 or 2×4 blocks).
    pub arrays: Vec<Vec<MatePinId>>,
    pub n_arrays: usize,
    pub unused_pins: usize,
}

/// Group pins into their physical pogo-array connectors: connected
/// components under orthogonal-neighbor adjacency (≤ 1.1 × pitch). Every
/// generator keeps ≥ 3 mm between structures, so components are exact.
fn derive_arrays(pins: &[MatePin], pitch: f64) -> Vec<Vec<MatePinId>> {
    let thr = pitch * 1.1;
    let mut comp: Vec<Option<usize>> = vec![None; pins.len()];
    let mut arrays: Vec<Vec<MatePinId>> = Vec::new();
    for i in 0..pins.len() {
        if comp[i].is_some() {
            continue;
        }
        let id = arrays.len();
        arrays.push(Vec::new());
        let mut stack = vec![i];
        comp[i] = Some(id);
        while let Some(k) = stack.pop() {
            arrays[id].push(pins[k].id);
            for j in 0..pins.len() {
                if comp[j].is_none() && crate::types::dist(pins[k].xy, pins[j].xy) <= thr {
                    comp[j] = Some(id);
                    stack.push(j);
                }
            }
        }
    }
    arrays
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

/// Every committed constellation is built from exactly two connector
/// formats at one 2.54 mm pitch: 2×3 kits and 2×4 LS arrays. A structure
/// is a list of rows along its long axis; each row is (inner fn, outer fn),
/// where "outer" faces away from the mate interior.
#[derive(Clone, Copy, PartialEq)]
enum F {
    Dp,
    Dm,
    Vusb,
    Vt,
    Gnd,
    Ls,
}

fn fkey(f: F) -> u8 {
    match f {
        F::Dp => 0,
        F::Dm => 1,
        F::Vusb => 2,
        F::Vt => 3,
        F::Gnd => 4,
        F::Ls => 5,
    }
}

/// 8 kits and 7 LS arrays, interleaved K L K L … K. One LS array carries
/// two GND so the 16-land GND budget is met (8 kit + 8 array GND).
fn structure_list() -> Vec<Vec<(F, F)>> {
    let kit = vec![(F::Vt, F::Dp), (F::Vt, F::Dm), (F::Gnd, F::Vusb)];
    let ls_arr = |extra_gnd: usize| -> Vec<(F, F)> {
        (0..4)
            .map(|r| {
                let inner = if r == 3 && extra_gnd > 0 {
                    F::Gnd
                } else {
                    F::Ls
                };
                let inner = if r == 2 && extra_gnd > 1 {
                    F::Gnd
                } else {
                    inner
                };
                (inner, F::Ls)
            })
            .collect()
    };
    (0..15)
        .map(|i| {
            if i % 2 == 0 {
                kit.clone()
            } else {
                ls_arr(if i == 1 { 2 } else { 1 })
            }
        })
        .collect()
}

/// Emit one structure's pins and slots. `place(row, is_outer)` gives each
/// pad's XY.
fn emit_structure(
    rows: &[(F, F)],
    place: impl Fn(usize, bool) -> [f64; 2],
    pins: &mut Vec<MatePin>,
    slots: &mut Vec<Slot>,
    next_pin: &mut u32,
    next_slot: &mut u32,
) {
    let mut by_fn: std::collections::HashMap<u8, Vec<MatePinId>> = Default::default();
    for (r, (inner, outer)) in rows.iter().enumerate() {
        for (f, is_outer) in [(*inner, false), (*outer, true)] {
            let id = MatePinId(*next_pin);
            *next_pin += 1;
            pins.push(MatePin {
                id,
                xy: place(r, is_outer),
            });
            by_fn.entry(fkey(f)).or_default().push(id);
        }
    }
    if let (Some(dp), Some(dm)) = (
        by_fn.get(&0).and_then(|v| v.first()),
        by_fn.get(&1).and_then(|v| v.first()),
    ) {
        push_ordered_pair(slots, *dp, *dm, next_slot);
    }
    if let Some([vt0, vt1]) = by_fn.get(&3).map(|v| v.as_slice()) {
        push_vt_bank(slots, *vt0, *vt1, next_slot);
    }
    for (key, kind) in [(2u8, Kind::Vusb), (4, Kind::Gnd), (5, Kind::Ls)] {
        if let Some(ids) = by_fn.get(&key) {
            for id in ids {
                let sid = SlotId(*next_slot);
                *next_slot += 1;
                slots.push(Slot {
                    id: sid,
                    kind,
                    shape: Shape::Unit,
                    pins: vec![*id],
                });
            }
        }
    }
}

/// A double-row band the walker fills: structures run along it at `start..end`
/// with the outer pad row at `outer` and the inner row at `inner`.
struct Band {
    along_y: bool,
    start: f64,
    end: f64,
    outer: f64,
    inner: f64,
}

/// Fill bands in order with the 15 structures separated by `gap`.
fn generate_banded(kind: PatternKind, pitch: f64, bands: &[Band], gap: f64) -> Pattern {
    let mut next_pin = 0u32;
    let mut next_slot = 0u32;
    let mut pins = Vec::new();
    let mut slots = Vec::new();
    let structures = structure_list();

    let mut bi = 0usize;
    let mut pos = bands[0].start;
    for rows in &structures {
        let extent = (rows.len() - 1) as f64 * pitch;
        while bi < bands.len() && pos + extent > bands[bi].end + 1e-6 {
            bi += 1;
            if bi < bands.len() {
                pos = bands[bi].start;
            }
        }
        assert!(bi < bands.len(), "{kind:?} band overflow at pitch {pitch}");
        let band = &bands[bi];
        emit_structure(
            rows,
            |r, is_outer| {
                let cross = if is_outer { band.outer } else { band.inner };
                let along = pos + r as f64 * pitch;
                if band.along_y {
                    [cross, along]
                } else {
                    [along, cross]
                }
            },
            &mut pins,
            &mut slots,
            &mut next_pin,
            &mut next_slot,
        );
        pos += extent + gap;
    }

    Pattern {
        kind,
        pitch,
        arrays: derive_arrays(&pins, pitch),
        n_arrays: structures.len(),
        pins,
        slots,
        unused_pins: 0,
    }
}

/// S10: the tight L-band along the +X and +Y edges (3.2 mm between
/// connectors — the densest legal packing of the 15 structures).
fn generate_ring(pitch: f64) -> Pattern {
    let right_outer_x = A7_W - MARGIN - 1.5; // 67.5
    let top_outer_y = A7_H - MARGIN - 2.5; // 97.5
    let bands = [
        Band {
            along_y: true,
            start: MARGIN + 1.5,
            end: A7_H - MARGIN - 1.0,
            outer: right_outer_x,
            inner: right_outer_x - pitch,
        },
        Band {
            along_y: false,
            start: MARGIN + 1.5,
            end: right_outer_x - pitch - 4.0,
            outer: top_outer_y,
            inner: top_outer_y - pitch,
        },
    ];
    generate_banded(PatternKind::S10, pitch, &bands, 3.2)
}

/// S11: the symmetric perimeter ring. Four structures on each of the four
/// mate edges, every band mirror-ordered `[A K K A]` (array, kit, kit,
/// array) and centered along its edge. That is 8 kits + 8 arrays; the
/// seven LS arrays carry 48 LS + 8 GND, and the sixteenth structure is a
/// spare all-GND 2×4 — eight extra pour-connected lands that buy the
/// symmetry (more pads on the mate than pogos on the base is fine).
fn generate_perimeter(pitch: f64) -> Pattern {
    let mut next_pin = 0u32;
    let mut next_slot = 0u32;
    let mut pins = Vec::new();
    let mut slots = Vec::new();

    let kit = vec![(F::Vt, F::Dp), (F::Vt, F::Dm), (F::Gnd, F::Vusb)];
    let ls_arr = |extra_gnd: usize| -> Vec<(F, F)> {
        (0..4)
            .map(|r| {
                let inner = if r == 3 && extra_gnd > 0 {
                    F::Gnd
                } else {
                    F::Ls
                };
                let inner = if r == 2 && extra_gnd > 1 {
                    F::Gnd
                } else {
                    inner
                };
                (inner, F::Ls)
            })
            .collect()
    };
    let gnd_arr: Vec<(F, F)> = (0..4).map(|_| (F::Gnd, F::Gnd)).collect();

    // Per band, walking the band axis: [array, kit, kit, array].
    // Bands in order: right, top, left, bottom. The spare all-GND array is
    // the trailing array of the bottom band (the quiet origin corner).
    let band_structs: [[Vec<(F, F)>; 4]; 4] = [
        [ls_arr(2), kit.clone(), kit.clone(), ls_arr(1)],
        [ls_arr(1), kit.clone(), kit.clone(), ls_arr(1)],
        [ls_arr(1), kit.clone(), kit.clone(), ls_arr(1)],
        [ls_arr(1), kit.clone(), kit.clone(), gnd_arr],
    ];
    let bands = [
        // (along_y, band span start..end, outer coord; inner is one pitch in)
        (
            true,
            MARGIN + 1.5,
            A7_H - MARGIN - 1.5,
            A7_W - MARGIN - 1.5,
            -1.0,
        ),
        (false, 13.0, A7_W - 13.0, A7_H - MARGIN - 2.5, -1.0),
        (true, MARGIN + 1.5, A7_H - MARGIN - 1.5, MARGIN + 1.5, 1.0),
        (false, 13.0, A7_W - 13.0, MARGIN + 2.5, 1.0),
    ];
    for (bi, (along_y, b0, b1, outer, inward)) in bands.into_iter().enumerate() {
        let structs = &band_structs[bi];
        let extents: Vec<f64> = structs
            .iter()
            .map(|r| (r.len() - 1) as f64 * pitch)
            .collect();
        let total: f64 = extents.iter().sum();
        // Symmetric spacing: equal gaps between structures, equal margins.
        let gap = ((b1 - b0) - total) / 5.0;
        let mut pos = b0 + gap;
        for (rows, extent) in structs.iter().zip(&extents) {
            emit_structure(
                rows,
                |r, is_outer| {
                    let cross = if is_outer {
                        outer
                    } else {
                        outer + inward * pitch
                    };
                    let along = pos + r as f64 * pitch;
                    if along_y {
                        [cross, along]
                    } else {
                        [along, cross]
                    }
                },
                &mut pins,
                &mut slots,
                &mut next_pin,
                &mut next_slot,
            );
            pos += extent + gap;
        }
    }

    Pattern {
        kind: PatternKind::S11,
        pitch,
        arrays: derive_arrays(&pins, pitch),
        n_arrays: 16,
        pins,
        slots,
        unused_pins: 8,
    }
}

/// S12: a grid of clusters — 3 columns × 5 rows of structures across the
/// mate interior with ≥ 11 mm streets in every direction.
fn generate_cluster_grid(pitch: f64) -> Pattern {
    let mut next_pin = 0u32;
    let mut next_slot = 0u32;
    let mut pins = Vec::new();
    let mut slots = Vec::new();
    let structures = structure_list();
    let cols = [12.0, 34.0, 56.0];
    let rows_y = [9.0, 28.0, 47.0, 66.0, 85.0];
    let cx_mate = A7_W / 2.0;
    for (si, rows) in structures.iter().enumerate() {
        let x0 = cols[si % 3];
        let y0 = rows_y[si / 3];
        // The DP/DM column faces away from the mate interior.
        let (inner_x, outer_x) = if x0 + pitch / 2.0 < cx_mate {
            (x0 + pitch, x0)
        } else {
            (x0, x0 + pitch)
        };
        emit_structure(
            rows,
            |r, is_outer| {
                let x = if is_outer { outer_x } else { inner_x };
                [x, y0 + r as f64 * pitch]
            },
            &mut pins,
            &mut slots,
            &mut next_pin,
            &mut next_slot,
        );
    }
    Pattern {
        kind: PatternKind::S12,
        pitch,
        arrays: derive_arrays(&pins, pitch),
        n_arrays: structures.len(),
        pins,
        slots,
        unused_pins: 0,
    }
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
        arrays: derive_arrays(&pins, pitch),
        pins,
        slots,
        n_arrays,
        unused_pins: 0,
    }
}

pub fn generate_pattern(kind: PatternKind, pitch: f64) -> Pattern {
    match kind {
        PatternKind::S10 => return generate_ring(pitch),
        PatternKind::S11 => return generate_perimeter(pitch),
        PatternKind::S12 => return generate_cluster_grid(pitch),
        _ => {}
    }
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
        PatternKind::S10 | PatternKind::S11 | PatternKind::S12 => unreachable!(),
    }

    let unused = 0; // generators emit exactly the 104-land budget when filled
    Pattern {
        kind,
        pitch,
        arrays: derive_arrays(&pins, pitch),
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
    fn eval_patterns_use_only_2x3_and_2x4_arrays_at_254() {
        for kind in PatternKind::eval() {
            check_constellation_rule(kind);
        }
    }

    fn check_constellation_rule(kind: PatternKind) {
        let p = generate_pattern(kind, PITCH_254);
        // 104 electrical lands; S11 adds a spare all-GND 2×4 for symmetry.
        assert_eq!(p.pins.len(), 104 + p.unused_pins);
        let mut sixes = 0;
        let mut eights = 0;
        for arr in &p.arrays {
            match arr.len() {
                6 => sixes += 1,
                8 => eights += 1,
                n => panic!("array of {n} pins; only 2x3 and 2x4 are allowed"),
            }
        }
        assert_eq!(sixes, 8, "{kind:?} kits");
        assert_eq!(eights, 7 + p.unused_pins / 8, "{kind:?} arrays");
        // Every pin has an orthogonal neighbor exactly one 2.54 mm pitch away.
        for a in &p.pins {
            assert!(p.pins.iter().any(|b| {
                let d = crate::types::dist(a.xy, b.xy);
                (d - PITCH_254).abs() < 1e-6
            }));
        }
        // Pads stay inside the usable mate interior.
        for a in &p.pins {
            assert!(
                a.xy[0] >= MARGIN
                    && a.xy[0] <= A7_W - MARGIN
                    && a.xy[1] >= MARGIN
                    && a.xy[1] <= A7_H - MARGIN,
                "{kind:?} pin outside margin: {:?}",
                a.xy
            );
        }
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
