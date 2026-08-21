//! The committed S11 mate constellation.
//!
//! The interposer's bottom face carries a fixed land pattern the fixture's
//! A7-sized connector plate mates against — a hardware contract, identical
//! on every interposer. S11 places structures around all four edges of the
//! folded A7 tile: per band, `[LS-array, kit, kit, LS-array]` with
//! symmetric gaps. A *kit* is a 2×3 block carrying one board's USB pair,
//! power, and ground (inner column Vt Vt Gnd, outer Dp Dm Vusb); an
//! *LS-array* is a 2×4 block of low-speed lands with a ground seeded in.
//! The trailing array of the bottom band is a spare all-GND block. Only
//! 2×3 and 2×4 blocks at 2.54 mm pitch appear — standard pogo-array
//! connectors.
//!
//! Totals: 16 blocks, 112 lands — 8 USB pairs, 16 Vtarget, 8 Vusb, 48
//! low-speed, and 24 ground.

/// A7 tile dimensions, portrait.
pub const A7_W: f64 = 74.0;
pub const A7_H: f64 = 105.0;
/// Keep-out from the tile edges.
pub const MARGIN: f64 = 5.0;
/// The standard 0.1" pogo-array pitch.
pub const PITCH_254: f64 = 2.54;

/// Electrical role of one mate land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    UsbDp,
    UsbDm,
    Vusb,
    Vtarget,
    Gnd,
    Ls,
}

impl Role {
    /// The `ict` vocabulary name for this role.
    pub fn name(self) -> &'static str {
        match self {
            Role::UsbDp => "usb_dp",
            Role::UsbDm => "usb_dm",
            Role::Vusb => "vusb",
            Role::Vtarget => "vtarget",
            Role::Gnd => "gnd",
            Role::Ls => "ls",
        }
    }
}

/// One land of the constellation, in the sheet frame (millimeters, Y
/// down, origin at the sheet corner the A7 tile folds onto).
#[derive(Debug, Clone, Copy)]
pub struct Land {
    pub xy: [f64; 2],
    pub role: Role,
    /// Which 2×3/2×4 connector block the land belongs to (0..16).
    pub block: u32,
}

/// Dimensions of the A7 mate region at the origin corner of a standard
/// A-series sheet, by the ISO fold rule: each halving cuts the sheet's
/// long side, so the A7 descendant alternates orientation per fold. A7 →
/// 74×105, A6 → 105×74, A5 → 74×105, A4 → 105×74. The fold decides only
/// the orientation; the returned dimensions are the exact A7 tile — the
/// fixture-plate contract — even when repeated halving leaves a
/// remainder (297 mm halves to 74.25).
pub fn mate_dims(sheet_w: f64, sheet_h: f64) -> (f64, f64) {
    let (mut w, mut h) = (sheet_w, sheet_h);
    while w * h > A7_W * A7_H * 1.05 {
        if w >= h {
            w /= 2.0;
        } else {
            h /= 2.0;
        }
    }
    if w >= h { (A7_H, A7_W) } else { (A7_W, A7_H) }
}

/// The S11 constellation oriented for a sheet: generated in the canonical
/// portrait 74×105 frame, then rotated 90° (never mirrored — the mate is
/// a rigid contract) when the sheet's folded A7 tile is landscape.
pub fn oriented_s11(sheet_w: f64, sheet_h: f64) -> Vec<Land> {
    let (mw, mh) = mate_dims(sheet_w, sheet_h);
    let mut lands = s11();
    if mw > mh {
        for land in &mut lands {
            land.xy = [land.xy[1], A7_W - land.xy[0]];
        }
    }
    lands
}

/// One structure: rows of (inner, outer) roles, walked along the band.
type Rows = Vec<(Role, Role)>;

fn kit() -> Rows {
    vec![
        (Role::Vtarget, Role::UsbDp),
        (Role::Vtarget, Role::UsbDm),
        (Role::Gnd, Role::Vusb),
    ]
}

fn ls_array(extra_gnd: usize) -> Rows {
    (0..4)
        .map(|row| {
            let inner = match row {
                3 if extra_gnd > 0 => Role::Gnd,
                2 if extra_gnd > 1 => Role::Gnd,
                _ => Role::Ls,
            };
            (inner, Role::Ls)
        })
        .collect()
}

/// The canonical portrait-frame S11 pattern.
fn s11() -> Vec<Land> {
    let gnd_array: Rows = (0..4).map(|_| (Role::Gnd, Role::Gnd)).collect();

    // Per band, walking the band axis: [array, kit, kit, array]. Bands in
    // order: right, top, left, bottom. The spare all-GND array is the
    // trailing array of the bottom band (the quiet origin corner).
    let band_structs: [[Rows; 4]; 4] = [
        [ls_array(2), kit(), kit(), ls_array(1)],
        [ls_array(1), kit(), kit(), ls_array(1)],
        [ls_array(1), kit(), kit(), ls_array(1)],
        [ls_array(1), kit(), kit(), gnd_array],
    ];
    // (along_y, band span start..end, outer row coordinate, inward sign);
    // the inner row sits one pitch further inward.
    let bands = [
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

    let mut lands = Vec::new();
    let mut block = 0u32;
    for (band, (along_y, b0, b1, outer, inward)) in bands.into_iter().enumerate() {
        let structs = &band_structs[band];
        let extents: Vec<f64> = structs
            .iter()
            .map(|rows| (rows.len() - 1) as f64 * PITCH_254)
            .collect();
        let total: f64 = extents.iter().sum();
        // Symmetric spacing: equal gaps between structures, equal margins.
        let gap = ((b1 - b0) - total) / 5.0;
        let mut pos = b0 + gap;
        for (rows, extent) in structs.iter().zip(&extents) {
            for (row, (inner, outer_role)) in rows.iter().enumerate() {
                for (role, is_outer) in [(*inner, false), (*outer_role, true)] {
                    let cross = if is_outer {
                        outer
                    } else {
                        outer + inward * PITCH_254
                    };
                    let along = pos + row as f64 * PITCH_254;
                    let xy = if along_y {
                        [cross, along]
                    } else {
                        [along, cross]
                    };
                    lands.push(Land { xy, role, block });
                }
            }
            pos += extent + gap;
            block += 1;
        }
    }
    lands
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role_count(lands: &[Land], role: Role) -> usize {
        lands.iter().filter(|land| land.role == role).count()
    }

    #[test]
    fn s11_land_budget() {
        let lands = s11();
        assert_eq!(lands.len(), 112);
        assert_eq!(role_count(&lands, Role::UsbDp), 8);
        assert_eq!(role_count(&lands, Role::UsbDm), 8);
        assert_eq!(role_count(&lands, Role::Vusb), 8);
        assert_eq!(role_count(&lands, Role::Vtarget), 16);
        assert_eq!(role_count(&lands, Role::Gnd), 24);
        assert_eq!(role_count(&lands, Role::Ls), 48);
    }

    #[test]
    fn s11_uses_the_254_pitch_and_only_2x3_or_2x4_blocks() {
        let lands = s11();
        // Nearest-neighbor distance is exactly one pitch for every land.
        for a in &lands {
            let nearest = lands
                .iter()
                .filter(|b| (b.xy[0] - a.xy[0]).abs() > 1e-9 || (b.xy[1] - a.xy[1]).abs() > 1e-9)
                .map(|b| ((b.xy[0] - a.xy[0]).powi(2) + (b.xy[1] - a.xy[1]).powi(2)).sqrt())
                .fold(f64::INFINITY, f64::min);
            assert!((nearest - PITCH_254).abs() < 1e-6, "land at {:?}", a.xy);
        }
        // Connected components under one-pitch adjacency are 2×3 or 2×4.
        let mut seen = vec![false; lands.len()];
        let mut sizes = Vec::new();
        for start in 0..lands.len() {
            if seen[start] {
                continue;
            }
            let mut stack = vec![start];
            let mut size = 0;
            seen[start] = true;
            while let Some(i) = stack.pop() {
                size += 1;
                for (j, other) in lands.iter().enumerate() {
                    if seen[j] {
                        continue;
                    }
                    let d = ((other.xy[0] - lands[i].xy[0]).powi(2)
                        + (other.xy[1] - lands[i].xy[1]).powi(2))
                    .sqrt();
                    if d < PITCH_254 * 1.1 {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
            sizes.push(size);
        }
        assert_eq!(sizes.len(), 16);
        assert!(sizes.iter().all(|size| *size == 6 || *size == 8));
    }

    #[test]
    fn s11_respects_the_tile_margin() {
        for land in s11() {
            assert!(land.xy[0] >= MARGIN - 1e-9 && land.xy[0] <= A7_W - MARGIN + 1e-9);
            assert!(land.xy[1] >= MARGIN - 1e-9 && land.xy[1] <= A7_H - MARGIN + 1e-9);
        }
    }

    #[test]
    fn fold_orientation_rotates_landscape_mates() {
        // A7 and A5 sheets fold to a portrait tile: canonical frame.
        for (w, h) in [(74.0, 105.0), (148.0, 210.0)] {
            assert_eq!(mate_dims(w, h), (74.0, 105.0));
            let lands = oriented_s11(w, h);
            assert!(lands.iter().all(|l| l.xy[0] <= A7_W));
        }
        // A6 and A4 fold to a landscape tile: rotated, never mirrored.
        // A4's 297 mm side halves to 74.25, but the tile stays the exact
        // A7 contract.
        assert_eq!(mate_dims(105.0, 148.0), (105.0, 74.0));
        assert_eq!(mate_dims(210.0, 297.0), (105.0, 74.0));
        // A rotated sheet rotates its whole fold chain with it.
        assert_eq!(mate_dims(297.0, 210.0), (74.0, 105.0));
        assert_eq!(mate_dims(148.0, 105.0), (74.0, 105.0));
        let lands = oriented_s11(105.0, 148.0);
        assert_eq!(lands.len(), 112);
        assert!(lands.iter().all(|l| l.xy[0] <= A7_H && l.xy[1] <= A7_W));
        // A rigid rotation preserves the land budget per role.
        assert_eq!(lands.iter().filter(|l| l.role == Role::Gnd).count(), 24);
    }
}
