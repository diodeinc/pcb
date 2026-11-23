//! The fixture beds an interposer can mate.
//!
//! - **S13** ([`crate::pattern`]): the generated 2×3-block constellation on
//!   the folded A7 tile, assigned per signal kind.
//! - **Switchyard rev-1**: the land table of the Switchyard tester as laid
//!   out (`beds/switchyard-rev1.csv`, Switchyard top view, mm, Y down,
//!   origin at the board corner). It covers a landscape A7 sheet origin on
//!   origin, so the table is the sheet frame as is. Eight 14-pin cells,
//!   one per DUT, plus a sense row: two detect pairs, interposer-ID straps
//!   and grounds. Each board takes one whole cell ([`crate::plan`]). The
//!   bench fixture registers the interposer on two stack dowels and
//!   clamps it with five M3 screws ([`Bed::alignment`]), so the interposer
//!   is cut to the bench interposer zone rather than the panel's outline
//!   ([`Bed::fit`]). The 95 × 64 mm conveyor inset is a separate,
//!   production target this bed does not build.

use anyhow::{Context, Result, bail};

use crate::panel::{Outline, Panel};
use crate::pattern::{A7_H, A7_W, Land, Role, oriented_s13};

const SWITCHYARD_REV1: &str = include_str!("../beds/switchyard-rev1.csv");

/// Sense-row blocks: past the eight DUT cells.
const DET0_BLOCK: u32 = 8;
const DET1_BLOCK: u32 = 9;
const SENSE_BLOCK: u32 = 10;

/// The clearances high-voltage copper keeps (IPC-2221 Table 6-1, 51–100
/// V). Bare copper — a high-voltage pad, or any pad facing high-voltage
/// copper — keeps the uncoated external spacing (B2); tracks and tented
/// vias, both under solder mask, only the coated one (B4).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HvClearance {
    pub exposed: f64,
    pub coated: f64,
}

/// A fixture alignment feature: an NPTH hole, or a slot when the drill's
/// two axes differ, with a round copper and pogo keep-out on both faces.
#[derive(Debug, Clone, Copy)]
pub struct Alignment {
    pub name: &'static str,
    pub at: [f64; 2],
    /// Drill extent along x and y.
    pub drill: [f64; 2],
    pub keepout_dia: f64,
    /// A screw through it clamps the DUT plate and interposer to the bed.
    pub clamps: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Bed {
    #[default]
    S13,
    SwitchyardRev1,
}

impl std::str::FromStr for Bed {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "s13" => Ok(Self::S13),
            "switchyard-rev1" => Ok(Self::SwitchyardRev1),
            _ => Err("expected s13 or switchyard-rev1".into()),
        }
    }
}

impl Bed {
    /// The bed's lands in the sheet frame.
    pub fn lands(self, sheet_w: f64, sheet_h: f64) -> Result<Vec<Land>> {
        match self {
            Bed::S13 => Ok(oriented_s13(sheet_w, sheet_h)),
            Bed::SwitchyardRev1 => {
                if (sheet_w - A7_H).abs() > 0.1 || (sheet_h - A7_W).abs() > 0.1 {
                    bail!(
                        "the switchyard-rev1 bed mates a landscape A7 sheet (105 × 74 mm), \
                         not {sheet_w} × {sheet_h} mm"
                    );
                }
                switchyard_rev1()
            }
        }
    }

    /// Alignment features the bed registers the interposer on, beyond the
    /// panel's own tooling. S13 registers on the A7 tile's corner tooling
    /// ([`crate::panel`]) instead.
    pub fn alignment(self) -> Vec<Alignment> {
        match self {
            Bed::S13 => Vec::new(),
            // Switchyard CR-17 and the rev-1 bench stack (mechanical/README.md
            // at f90ab8c): the Ø3 m6 stack dowels locate on DW1 and slide in
            // the DW2 slot along x; five M3 screws through MH1–MH4 and MH7
            // clamp the upper shroud, interposer and lower shroud to the
            // backplate. DW1's +0.05/−0 tolerance is a fab note, not
            // geometry.
            Bed::SwitchyardRev1 => {
                let dowel = |name, at, drill| Alignment {
                    name,
                    at,
                    drill,
                    keepout_dia: 6.0,
                    clamps: false,
                };
                let screw = |name, at| Alignment {
                    name,
                    at,
                    drill: [3.4, 3.4],
                    keepout_dia: 6.0,
                    clamps: true,
                };
                vec![
                    dowel("DW1", [28.5, 31.25], [3.05, 3.05]),
                    dowel("DW2", [76.5, 31.25], [4.0, 3.05]),
                    screw("MH7", [52.5, 31.25]),
                    screw("MH1", [3.2, 3.2]),
                    screw("MH2", [101.8, 3.2]),
                    screw("MH3", [3.2, 70.8]),
                    screw("MH4", [101.8, 70.8]),
                ]
            }
        }
    }

    /// Clearances the bed's high-voltage nets need, when it has any:
    /// Switchyard's VTarget and protected signal lanes (SIG0–5) see up to
    /// 60 V.
    pub fn hv_clearance(self) -> Option<HvClearance> {
        match self {
            Bed::S13 => None,
            Bed::SwitchyardRev1 => Some(HvClearance {
                exposed: 0.6,
                coated: 0.13,
            }),
        }
    }

    /// The nets strapping the bed's interposer-ID lands for interposer
    /// `number`, as (land index, net). Switchyard reads six ID lands, each
    /// strapped to GND (digit 0), ID7 (1) or ID8 (2), or left open (3):
    /// ID1–ID5 carry the number in base 4, most significant first, and
    /// ID6 the check digit (−Σd) mod 4. Number 0 (all GND) is reserved and
    /// all-open means no interposer, so numbers run 1–1023. Beds without ID
    /// lands take no number.
    pub fn id_straps(
        self,
        lands: &[Land],
        number: Option<u16>,
    ) -> Result<Vec<(usize, &'static str)>> {
        let ids: Vec<usize> = (0..lands.len())
            .filter(|&i| lands[i].role == Role::Id)
            .collect();
        if ids.is_empty() {
            if number.is_some() {
                bail!("the {self:?} bed has no interposer-ID lands");
            }
            return Ok(Vec::new());
        }
        let number =
            number.context("this bed reads an interposer ID: pass --interposer-id 1-1023")?;
        let digits = id_digits(number)?;
        let land = |label: &str| {
            ids.iter()
                .copied()
                .find(|&i| lands[i].label == label)
                .with_context(|| format!("bed has no {label} land"))
        };
        let mut straps = Vec::new();
        for (k, digit) in digits.iter().enumerate() {
            let land = land(&format!("ID{}", k + 1))?;
            let net = match digit {
                0 => "GND",
                1 => "ID7",
                2 => "ID8",
                _ => continue,
            };
            straps.push((land, net));
        }
        for drive in ["ID7", "ID8"] {
            if straps.iter().any(|(_, net)| *net == drive) {
                straps.push((land(drive)?, drive));
            }
        }
        Ok(straps)
    }

    /// The region (x0, y0, x1, y1) the interposer must fit inside, when
    /// the bed constrains it: Switchyard's bench interposer zone, the
    /// 105 × 74 mm landscape A7 at the board's joint edge, which carries
    /// the corner screws.
    pub fn envelope(self) -> Option<[f64; 4]> {
        match self {
            Bed::S13 => None,
            Bed::SwitchyardRev1 => Some([0.0, 0.0, 105.0, 74.0]),
        }
    }

    /// Fit the panel's features to the bed. A bed with an envelope cuts the
    /// interposer to it: the outline becomes the envelope, the panel's
    /// holes and fiducials go (the bed registers on its own alignment, and
    /// the DUT's tooling pins get their own reliefs, [`crate::shroud`]),
    /// and the bed's own top fiducials replace them. The bottom face
    /// carries no assembled parts, so it gets none.
    pub fn fit(self, mut panel: Panel) -> Panel {
        let Some([x0, y0, x1, y1]) = self.envelope() else {
            return panel;
        };
        let corners = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        panel.outline = (0..4)
            .map(|i| Outline::Line {
                start: corners[i],
                end: corners[(i + 1) % 4],
            })
            .collect();
        panel.holes.clear();
        panel.fids_top = self.fiducials();
        panel.fids_bottom.clear();
        panel.envelope = Some([x0, y0, x1, y1]);
        panel
    }

    /// Top fiducials for a bed that cuts the interposer to its envelope:
    /// asymmetric so the board's rotation is unambiguous, and over no land,
    /// so they never block a land's via. Switchyard's sit in the gaps
    /// between cell columns on the outer power row (x 28.5, 76.5) and in
    /// the open middle of the sense row.
    pub fn fiducials(self) -> Vec<[f64; 2]> {
        match self {
            Bed::S13 => Vec::new(),
            Bed::SwitchyardRev1 => vec![[28.5, 7.0], [76.5, 7.0], [64.0, 67.0]],
        }
    }

    /// Mate land diameter. Switchyard's 1.07 mm plunger plus its 0.42 mm
    /// worst-case alignment budget needs Ø1.91.
    pub fn land_dia(self) -> f64 {
        match self {
            Bed::S13 => 1.5,
            Bed::SwitchyardRev1 => 2.0,
        }
    }
}

/// Interposer `number`'s six ID digits, ID1 first: five base-4 digits and
/// the check digit.
pub fn id_digits(number: u16) -> Result<[u8; 6]> {
    if !(1..=1023).contains(&number) {
        bail!(
            "interposer ID {number} is out of range: 0 is reserved, 1-1023 fit five base-4 digits"
        );
    }
    let mut digits = [0u8; 6];
    for (k, digit) in digits[..5].iter_mut().enumerate() {
        *digit = ((number >> (2 * (4 - k))) & 3) as u8;
    }
    let sum: u32 = digits[..5].iter().map(|&d| u32::from(d)).sum();
    digits[5] = ((4 - sum % 4) % 4) as u8;
    Ok(digits)
}

fn switchyard_rev1() -> Result<Vec<Land>> {
    let mut lands = Vec::new();
    for (line, row) in SWITCHYARD_REV1.lines().enumerate().skip(1) {
        let context = || format!("switchyard-rev1.csv line {}", line + 1);
        let fields: Vec<&'static str> = row.split(',').collect();
        let [dut, label, x, y] = fields[..] else {
            bail!("{}: expected 4 fields", context());
        };
        let role = match label {
            "VT" => Role::Vtarget,
            "VL" => Role::Vusb,
            "GND" => Role::Gnd,
            "DP" => Role::UsbDp,
            "DM" => Role::UsbDm,
            "S0" | "S1" | "S2" | "S3" => Role::Ls,
            "S4" | "S5" => Role::Analog,
            "DET0_A" | "DET0_B" | "DET1_A" | "DET1_B" => Role::Detect,
            _ if label.starts_with("ID") => Role::Id,
            _ => bail!("{}: unknown role {label}", context()),
        };
        let block = if !dut.is_empty() {
            dut.parse().with_context(context)?
        } else if label.starts_with("DET0") {
            DET0_BLOCK
        } else if label.starts_with("DET1") {
            DET1_BLOCK
        } else {
            SENSE_BLOCK
        };
        lands.push(Land {
            xy: [
                x.parse().with_context(context)?,
                y.parse().with_context(context)?,
            ],
            role,
            block,
            label,
        });
    }
    Ok(lands)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switchyard_rev1_table() {
        let lands = Bed::SwitchyardRev1.lands(105.0, 74.0).unwrap();
        assert_eq!(lands.len(), 128);
        // Eight cells of 14: VT VT GND VL GND S4 S5 / DM DP GND S0 S2 S3 S1.
        for cell in 0..8 {
            let mut labels: Vec<&str> = lands
                .iter()
                .filter(|land| land.block == cell)
                .map(|land| land.label)
                .collect();
            labels.sort();
            assert_eq!(
                labels,
                [
                    "DM", "DP", "GND", "GND", "GND", "S0", "S1", "S2", "S3", "S4", "S5", "VL",
                    "VT", "VT"
                ],
                "cell {cell}"
            );
        }
        // Two detect pairs, each its own block.
        for block in [DET0_BLOCK, DET1_BLOCK] {
            let pair = lands.iter().filter(|land| land.block == block).count();
            assert_eq!(pair, 2);
        }
        // Fiducials sit over no land.
        for fid in Bed::SwitchyardRev1.fiducials() {
            for land in &lands {
                let d = ((land.xy[0] - fid[0]).powi(2) + (land.xy[1] - fid[1]).powi(2)).sqrt();
                assert!(d > 2.0, "fiducial {fid:?} over {land:?}");
            }
        }
        // Everything inside the conveyor envelope, x 5–100, y 5–69.
        for land in &lands {
            assert!((5.0..=100.0).contains(&land.xy[0]), "{land:?}");
            assert!((5.0..=69.0).contains(&land.xy[1]), "{land:?}");
        }
        assert!(Bed::SwitchyardRev1.lands(74.0, 105.0).is_err());
    }

    #[test]
    fn interposer_id_digits() {
        // 1 = 00001, check (−1) mod 4 = 3.
        assert_eq!(id_digits(1).unwrap(), [0, 0, 0, 0, 1, 3]);
        // 1023 = 33333, check (−15) mod 4 = 1: never all-open.
        assert_eq!(id_digits(1023).unwrap(), [3, 3, 3, 3, 3, 1]);
        // 0b01_10_11_00_01 = 433 → 1 2 3 0 1, check (−7) mod 4 = 1.
        assert_eq!(id_digits(433).unwrap(), [1, 2, 3, 0, 1, 1]);
        assert!(id_digits(0).is_err());
        assert!(id_digits(1024).is_err());
        for number in 1..=1023 {
            let digits = id_digits(number).unwrap();
            let sum: u32 = digits.iter().map(|&d| u32::from(d)).sum();
            assert_eq!(sum % 4, 0, "{number}");
        }

        let lands = Bed::SwitchyardRev1.lands(105.0, 74.0).unwrap();
        let straps = Bed::SwitchyardRev1.id_straps(&lands, Some(433)).unwrap();
        let net_of = |label: &str| {
            straps
                .iter()
                .find(|(land, _)| lands[*land].label == label)
                .map(|(_, net)| *net)
        };
        assert_eq!(net_of("ID1"), Some("ID7"));
        assert_eq!(net_of("ID2"), Some("ID8"));
        assert_eq!(net_of("ID3"), None);
        assert_eq!(net_of("ID4"), Some("GND"));
        assert_eq!(net_of("ID6"), Some("ID7"));
        assert_eq!(net_of("ID7"), Some("ID7"));
        assert_eq!(net_of("ID8"), Some("ID8"));
        assert!(Bed::SwitchyardRev1.id_straps(&lands, None).is_err());
        assert!(Bed::S13.id_straps(&[], Some(1)).is_err());
    }
}
