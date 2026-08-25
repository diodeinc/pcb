//! The committed S13 mate constellation.
//!
//! The interposer's bottom face carries a fixed land pattern the fixture's
//! A7-sized connector plate mates against — a hardware contract, identical
//! on every interposer. Per the fixture-bed design rules, S13 uses only
//! 2×3 blocks (one connector part), every block is *pure* — power + GND
//! or signals + GND, never both — every block carries at least one GND,
//! and every power land has a GND land beside it in its own block.
//!
//! Three block flavors:
//! - **USB** (signal): inner column GND GND LS, outer column D+ D− LS —
//!   each pair member has its return beside it.
//! - **PWR** (power): inner column all GND, outer column VUSB VT VT —
//!   every power pin individually paired with a ground.
//! - **LS** (signal): five low-speed lands with GND seeded at opposite
//!   corners.
//!
//! The vertical bands carry a USB and a PWR block per board (eight
//! boards); the horizontal bands carry the LS arrays. Totals: 24 blocks,
//! 144 lands — 8 USB pairs, 16 Vtarget, 8 Vusb, 48 low-speed, 56 ground.

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

    fn is_power(self) -> bool {
        matches!(self, Role::Vusb | Role::Vtarget)
    }
}

/// One land of the constellation, in the sheet frame (millimeters, Y
/// down, origin at the sheet corner the A7 tile folds onto).
#[derive(Debug, Clone, Copy)]
pub struct Land {
    pub xy: [f64; 2],
    pub role: Role,
    /// Which 2×3 connector block the land belongs to (0..24).
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

/// The S13 constellation oriented for a sheet: generated in the canonical
/// portrait 74×105 frame, then rotated 90° (never mirrored — the mate is
/// a rigid contract) when the sheet's folded A7 tile is landscape.
pub fn oriented_s13(sheet_w: f64, sheet_h: f64) -> Vec<Land> {
    let (mw, mh) = mate_dims(sheet_w, sheet_h);
    let mut lands = s13();
    if mw > mh {
        for land in &mut lands {
            land.xy = [land.xy[1], A7_W - land.xy[0]];
        }
    }
    lands
}

/// One block: rows of (inner, outer) roles, walked along the band.
type Rows = Vec<(Role, Role)>;

fn usb_block() -> Rows {
    vec![
        (Role::Gnd, Role::UsbDp),
        (Role::Gnd, Role::UsbDm),
        (Role::Ls, Role::Ls),
    ]
}

fn pwr_block() -> Rows {
    vec![
        (Role::Gnd, Role::Vusb),
        (Role::Gnd, Role::Vtarget),
        (Role::Gnd, Role::Vtarget),
    ]
}

fn ls_block() -> Rows {
    vec![
        (Role::Gnd, Role::Ls),
        (Role::Ls, Role::Ls),
        (Role::Ls, Role::Gnd),
    ]
}

/// The canonical portrait-frame S13 pattern.
fn s13() -> Vec<Land> {
    // The vertical bands each carry four boards' USB+PWR block pairs;
    // the horizontal bands carry four LS arrays each.
    let board_band: Vec<Rows> = (0..4).flat_map(|_| [usb_block(), pwr_block()]).collect();
    let ls_band: Vec<Rows> = (0..4).map(|_| ls_block()).collect();
    let band_structs: [Vec<Rows>; 4] = [board_band.clone(), ls_band.clone(), board_band, ls_band];
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
        // Symmetric spacing: equal gaps between blocks, equal margins.
        let gap = ((b1 - b0) - total) / (structs.len() + 1) as f64;
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
    fn s13_land_budget() {
        let lands = s13();
        assert_eq!(lands.len(), 144);
        assert_eq!(role_count(&lands, Role::UsbDp), 8);
        assert_eq!(role_count(&lands, Role::UsbDm), 8);
        assert_eq!(role_count(&lands, Role::Vusb), 8);
        assert_eq!(role_count(&lands, Role::Vtarget), 16);
        assert_eq!(role_count(&lands, Role::Gnd), 56);
        assert_eq!(role_count(&lands, Role::Ls), 48);
    }

    #[test]
    fn s13_blocks_are_pure_2x3_with_grounds() {
        let lands = s13();
        let blocks = lands.iter().map(|l| l.block).max().unwrap() + 1;
        assert_eq!(blocks, 24);
        for block in 0..blocks {
            let members: Vec<&Land> = lands.iter().filter(|l| l.block == block).collect();
            // Every block is exactly 2×3.
            assert_eq!(members.len(), 6, "block {block}");
            // At least one ground per block.
            let gnds = members.iter().filter(|l| l.role == Role::Gnd).count();
            assert!(gnds >= 1, "block {block} has no ground");
            // Pure: power and signals never share a block.
            let power = members.iter().filter(|l| l.role.is_power()).count();
            let signals = members
                .iter()
                .filter(|l| !l.role.is_power() && l.role != Role::Gnd)
                .count();
            assert!(
                power == 0 || signals == 0,
                "block {block} mixes power and signals"
            );
            // Every power pin is paired with a ground in its own block.
            assert!(gnds >= power, "block {block} under-grounds its power");
        }
        // Constellation-wide, grounds cover every power pin.
        let power = lands.iter().filter(|l| l.role.is_power()).count();
        assert!(role_count(&lands, Role::Gnd) >= power);
    }

    #[test]
    fn s13_uses_the_254_pitch_and_only_2x3_blocks() {
        let lands = s13();
        // Nearest-neighbor distance is exactly one pitch for every land.
        for a in &lands {
            let nearest = lands
                .iter()
                .filter(|b| (b.xy[0] - a.xy[0]).abs() > 1e-9 || (b.xy[1] - a.xy[1]).abs() > 1e-9)
                .map(|b| ((b.xy[0] - a.xy[0]).powi(2) + (b.xy[1] - a.xy[1]).powi(2)).sqrt())
                .fold(f64::INFINITY, f64::min);
            assert!((nearest - PITCH_254).abs() < 1e-6, "land at {:?}", a.xy);
        }
        // Connected components under one-pitch adjacency are all 2×3.
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
        assert_eq!(sizes.len(), 24);
        assert!(sizes.iter().all(|size| *size == 6));
    }

    #[test]
    fn s13_respects_the_tile_margin() {
        for land in s13() {
            assert!(land.xy[0] >= MARGIN - 1e-9 && land.xy[0] <= A7_W - MARGIN + 1e-9);
            assert!(land.xy[1] >= MARGIN - 1e-9 && land.xy[1] <= A7_H - MARGIN + 1e-9);
        }
    }

    #[test]
    fn fold_orientation_rotates_landscape_mates() {
        // A7 and A5 sheets fold to a portrait tile: canonical frame.
        for (w, h) in [(74.0, 105.0), (148.0, 210.0)] {
            assert_eq!(mate_dims(w, h), (74.0, 105.0));
            let lands = oriented_s13(w, h);
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
        let lands = oriented_s13(105.0, 148.0);
        assert_eq!(lands.len(), 144);
        assert!(lands.iter().all(|l| l.xy[0] <= A7_H && l.xy[1] <= A7_W));
        // A rigid rotation preserves the land budget per role.
        assert_eq!(lands.iter().filter(|l| l.role == Role::Gnd).count(), 56);
    }
}
