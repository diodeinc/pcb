//! Configured PDK capabilities lowered to rules the engine evaluates.
//!
//! A rule is pure data: an identity, a limit, a severity, and a [`RuleKind`]
//! naming which measurement the engine runs. Rule ids are the PDK capability
//! paths, so a report consumer can find every limit's source line in the PDK
//! verbatim; a capability's preferred tier lowers to a second, warning-level
//! rule under `<capability>.preferred`.

use anyhow::{Result, bail};

use super::design::HoleClass;
use super::pdk::{Length, Limit, Pdk};
use super::report::Severity;

#[derive(Debug, Clone)]
pub(super) struct Rule {
    pub id: String,
    pub title: String,
    pub severity: Severity,
    pub limit: Length,
    pub kind: RuleKind,
}

/// The measurement semantics of a rule: a size, a clearance, an enclosure,
/// or a morphological residue over one of the design's entity pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuleKind {
    /// Size: each hole's drilled diameter meets the limit.
    HoleDiameter(HoleClass),
    /// Size: each routed slot's width meets the limit.
    SlotWidth,
    /// Clearance: holes with overlapping spans keep edge-to-edge distance.
    HolePairClearance,
    /// Enclosure: radial copper around each hole on the layers it lands on.
    AnnularRing(HoleClass),
    /// Clearance: reference linework stays clear of each copper image.
    LineworkToCopperClearance(Linework),
    /// Clearance: sibling board-array outlines keep their spacing.
    BoardArrayPairClearance,
    /// Residue: image material narrower than the limit.
    ThinFeature(ImageSel),
    /// Residue: gaps in the image narrower than the limit.
    ThinGap(ImageSel),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Linework {
    VScore,
    BoardEdge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImageSel {
    Copper,
    Soldermask,
}

impl RuleKind {
    /// The title every finding of this rule carries.
    pub fn finding_title(self) -> String {
        match self {
            Self::HoleDiameter(class) => {
                format!("{} hole is below minimum diameter", class.label())
            }
            Self::SlotWidth => "Slot is below minimum width".to_owned(),
            Self::HolePairClearance => "Hole-to-hole clearance is below minimum".to_owned(),
            Self::AnnularRing(class) => format!("{} annular ring is below minimum", class.label()),
            Self::LineworkToCopperClearance(Linework::VScore) => {
                "V-score centerline is too close to copper".to_owned()
            }
            Self::LineworkToCopperClearance(Linework::BoardEdge) => {
                "Board edge is too close to copper".to_owned()
            }
            Self::BoardArrayPairClearance => "Board arrays are too close together".to_owned(),
            Self::ThinFeature(ImageSel::Copper) => {
                "Copper feature is below minimum width".to_owned()
            }
            Self::ThinGap(ImageSel::Copper) => "Copper spacing is below minimum".to_owned(),
            Self::ThinFeature(ImageSel::Soldermask) => {
                "Soldermask feature is below minimum width".to_owned()
            }
            Self::ThinGap(ImageSel::Soldermask) => "Soldermask web is below minimum".to_owned(),
        }
    }

    /// The measured quantity as prose, for finding messages.
    pub fn quantity_label(self) -> String {
        match self {
            Self::HoleDiameter(class) => format!("{} hole diameter", class.label()),
            Self::SlotWidth => "routed slot width".to_owned(),
            Self::HolePairClearance => "hole edge-to-edge clearance".to_owned(),
            Self::AnnularRing(class) => format!("{} annular ring", class.label()),
            Self::LineworkToCopperClearance(Linework::VScore) => {
                "V-score centerline-to-copper clearance".to_owned()
            }
            Self::LineworkToCopperClearance(Linework::BoardEdge) => {
                "board-edge-to-copper clearance".to_owned()
            }
            Self::BoardArrayPairClearance => "board-array outline spacing".to_owned(),
            Self::ThinFeature(ImageSel::Copper) => "copper feature width".to_owned(),
            Self::ThinGap(ImageSel::Copper) => "copper-to-copper clearance".to_owned(),
            Self::ThinFeature(ImageSel::Soldermask) => "soldermask feature width".to_owned(),
            Self::ThinGap(ImageSel::Soldermask) => "soldermask web width".to_owned(),
        }
    }

    /// The roles of a finding's two witness points: the ends of the measured
    /// distance.
    pub fn witness_roles(self) -> [&'static str; 2] {
        match self {
            Self::HoleDiameter(_) => ["hole_boundary", "hole_boundary"],
            Self::SlotWidth => ["first_slot_boundary", "second_slot_boundary"],
            Self::HolePairClearance => ["first_hole_boundary", "second_hole_boundary"],
            Self::AnnularRing(_) => ["hole_boundary", "copper_boundary"],
            Self::LineworkToCopperClearance(Linework::VScore) => {
                ["vscore_centerline", "copper_boundary"]
            }
            Self::LineworkToCopperClearance(Linework::BoardEdge) => {
                ["board_outline", "copper_boundary"]
            }
            Self::BoardArrayPairClearance => ["first_board_array", "second_board_array"],
            Self::ThinFeature(_) | Self::ThinGap(_) => ["first_boundary", "second_boundary"],
        }
    }

    /// What one `checked` unit is for this rule.
    pub fn subject(self) -> &'static str {
        match self {
            Self::HoleDiameter(_) | Self::HolePairClearance => "hole",
            Self::SlotWidth => "slot",
            Self::AnnularRing(_) => "hole_layer_pair",
            Self::LineworkToCopperClearance(Linework::VScore) => "score_layer_pair",
            Self::LineworkToCopperClearance(Linework::BoardEdge) => "outline_layer_pair",
            Self::BoardArrayPairClearance => "array_pair",
            Self::ThinFeature(ImageSel::Copper) | Self::ThinGap(ImageSel::Copper) => "copper_layer",
            Self::ThinFeature(ImageSel::Soldermask) | Self::ThinGap(ImageSel::Soldermask) => {
                "soldermask_layer"
            }
        }
    }

    /// The measured quantity every finding of this rule reports.
    pub fn quantity(self) -> &'static str {
        match self {
            Self::HoleDiameter(_) => "hole_diameter",
            Self::SlotWidth => "slot_width",
            Self::HolePairClearance => "hole_edge_to_hole_edge_clearance",
            Self::AnnularRing(_) => "annular_ring",
            Self::LineworkToCopperClearance(Linework::VScore) => {
                "vscore_centerline_to_copper_clearance"
            }
            Self::LineworkToCopperClearance(Linework::BoardEdge) => {
                "board_edge_to_copper_clearance"
            }
            Self::BoardArrayPairClearance => "board_array_outline_spacing",
            Self::ThinFeature(ImageSel::Copper) => "copper_feature_width",
            Self::ThinGap(ImageSel::Copper) => "copper_to_copper_clearance",
            Self::ThinFeature(ImageSel::Soldermask) => "soldermask_feature_width",
            Self::ThinGap(ImageSel::Soldermask) => "soldermask_web_width",
        }
    }

    /// How the quantity is measured.
    pub fn method(self) -> &'static str {
        match self {
            Self::HoleDiameter(_) => "ipc_hole_diameter",
            Self::SlotWidth => "ipc_slot_width_or_outline_medial_axis_width",
            Self::HolePairClearance => "circle_edge_distance",
            Self::AnnularRing(_) => "maximal_centered_disk_minus_hole_radius",
            Self::LineworkToCopperClearance(_) => "segment_to_filled_region_boundary",
            Self::BoardArrayPairClearance => "filled_profile_boundary_distance",
            Self::ThinFeature(_) => "opening_candidate_then_medial_axis_width",
            Self::ThinGap(_) => "closing_candidate_then_medial_axis_width",
        }
    }
}

/// The entity pools a rule reads. Extraction builds exactly the union over
/// the configured rules, so a rule set pays only for what it measures.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Pools {
    pub drilled: bool,
    pub copper: bool,
    pub copper_boundaries: bool,
    pub hole_lands: bool,
    pub masks: bool,
    pub scores: bool,
    pub board_outlines: bool,
    pub board_arrays: bool,
}

impl std::ops::BitOr for Pools {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self {
            drilled: self.drilled || other.drilled,
            copper: self.copper || other.copper,
            copper_boundaries: self.copper_boundaries || other.copper_boundaries,
            hole_lands: self.hole_lands || other.hole_lands,
            masks: self.masks || other.masks,
            scores: self.scores || other.scores,
            board_outlines: self.board_outlines || other.board_outlines,
            board_arrays: self.board_arrays || other.board_arrays,
        }
    }
}

impl RuleKind {
    pub fn pools(self) -> Pools {
        let none = Pools::default();
        match self {
            Self::HoleDiameter(_) | Self::HolePairClearance | Self::SlotWidth => Pools {
                drilled: true,
                ..none
            },
            Self::AnnularRing(_) => Pools {
                drilled: true,
                copper: true,
                copper_boundaries: true,
                hole_lands: true,
                ..none
            },
            Self::LineworkToCopperClearance(Linework::VScore) => Pools {
                scores: true,
                copper: true,
                copper_boundaries: true,
                ..none
            },
            Self::LineworkToCopperClearance(Linework::BoardEdge) => Pools {
                board_outlines: true,
                copper: true,
                copper_boundaries: true,
                ..none
            },
            Self::BoardArrayPairClearance => Pools {
                board_arrays: true,
                ..none
            },
            Self::ThinFeature(ImageSel::Copper) | Self::ThinGap(ImageSel::Copper) => Pools {
                copper: true,
                ..none
            },
            Self::ThinFeature(ImageSel::Soldermask) | Self::ThinGap(ImageSel::Soldermask) => {
                Pools {
                    masks: true,
                    ..none
                }
            }
        }
    }
}

pub(super) fn pools(rules: &[Rule]) -> Pools {
    rules
        .iter()
        .map(|rule| rule.kind.pools())
        .fold(Pools::default(), |union, pools| union | pools)
}

/// Lower every configured capability to its rules. Absent capabilities lower
/// to nothing; new capabilities are new rows here. The minimum tier is an
/// error-severity rule under the capability path; a preferred tier adds a
/// warning-severity rule under `<path>.preferred` and must exceed the
/// minimum.
pub(super) fn lower(pdk: &Pdk) -> Result<Vec<Rule>> {
    let drilling = &pdk.capabilities.drilling;
    let copper = &pdk.capabilities.copper;
    let soldermask = &pdk.capabilities.soldermask;
    let panelization = &pdk.capabilities.panelization;
    let table: [(&Option<Limit>, &str, &str, RuleKind); 13] = [
        (
            &drilling.minimum_via_hole_diameter,
            "drilling.minimum_via_hole_diameter",
            "Minimum via hole diameter",
            RuleKind::HoleDiameter(HoleClass::Via),
        ),
        (
            &drilling.minimum_pth_hole_diameter,
            "drilling.minimum_pth_hole_diameter",
            "Minimum plated through-hole diameter",
            RuleKind::HoleDiameter(HoleClass::Pth),
        ),
        (
            &drilling.minimum_npth_hole_diameter,
            "drilling.minimum_npth_hole_diameter",
            "Minimum non-plated hole diameter",
            RuleKind::HoleDiameter(HoleClass::Npth),
        ),
        (
            &drilling.minimum_slot_width,
            "drilling.minimum_slot_width",
            "Minimum routed slot width",
            RuleKind::SlotWidth,
        ),
        (
            &drilling.minimum_hole_to_hole_clearance,
            "drilling.minimum_hole_to_hole_clearance",
            "Minimum hole edge-to-edge clearance",
            RuleKind::HolePairClearance,
        ),
        (
            &copper.minimum_via_annular_ring,
            "copper.minimum_via_annular_ring",
            "Minimum via annular ring",
            RuleKind::AnnularRing(HoleClass::Via),
        ),
        (
            &copper.minimum_pth_annular_ring,
            "copper.minimum_pth_annular_ring",
            "Minimum plated through-hole annular ring",
            RuleKind::AnnularRing(HoleClass::Pth),
        ),
        (
            &copper.minimum_feature_width,
            "copper.minimum_feature_width",
            "Minimum copper feature width",
            RuleKind::ThinFeature(ImageSel::Copper),
        ),
        (
            &copper.minimum_copper_clearance,
            "copper.minimum_copper_clearance",
            "Minimum copper-to-copper clearance",
            RuleKind::ThinGap(ImageSel::Copper),
        ),
        (
            &copper.minimum_vscore_to_copper_clearance,
            "copper.minimum_vscore_to_copper_clearance",
            "Minimum V-score centerline-to-copper clearance",
            RuleKind::LineworkToCopperClearance(Linework::VScore),
        ),
        (
            &copper.minimum_board_edge_clearance,
            "copper.minimum_board_edge_clearance",
            "Minimum board-edge-to-copper clearance",
            RuleKind::LineworkToCopperClearance(Linework::BoardEdge),
        ),
        (
            &soldermask.minimum_web,
            "soldermask.minimum_web",
            "Minimum soldermask web",
            RuleKind::ThinGap(ImageSel::Soldermask),
        ),
        (
            &panelization.minimum_board_array_spacing,
            "panelization.minimum_board_array_spacing",
            "Minimum spacing between board-array outlines",
            RuleKind::BoardArrayPairClearance,
        ),
    ];

    let mut rules = Vec::new();
    for (limit, id, title, kind) in table {
        let Some(limit) = limit else {
            continue;
        };
        rules.push(Rule {
            id: id.to_owned(),
            title: title.to_owned(),
            severity: Severity::Error,
            limit: limit.minimum().clone(),
            kind,
        });
        if let Some(preferred) = limit.preferred() {
            if preferred.millimeters() <= limit.minimum().millimeters() {
                bail!(
                    "{id}: preferred limit {} must exceed the minimum {}",
                    preferred.original(),
                    limit.minimum().original()
                );
            }
            rules.push(Rule {
                id: format!("{id}.preferred"),
                title: format!("{title} (preferred)"),
                severity: Severity::Warning,
                limit: preferred.clone(),
                kind,
            });
        }
    }
    Ok(rules)
}
