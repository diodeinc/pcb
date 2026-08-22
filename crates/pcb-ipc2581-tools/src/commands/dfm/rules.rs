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
    pub fn needs(self) -> Needs {
        let mut needs = Needs::default();
        match self {
            Self::HoleDiameter(_) => {
                needs.holes = true;
                needs.classified_holes = true;
            }
            Self::HolePairClearance => needs.holes = true,
            Self::SlotWidth => needs.slots = true,
            Self::AnnularRing(_) => {
                needs.holes = true;
                needs.classified_holes = true;
                needs.copper = true;
            }
            Self::LineworkToCopperClearance(Linework::VScore) => {
                needs.scores = true;
                needs.copper = true;
            }
            Self::LineworkToCopperClearance(Linework::BoardEdge) => {
                needs.board_outlines = true;
                needs.copper = true;
            }
            Self::BoardArrayPairClearance => needs.board_arrays = true,
            Self::ThinFeature(ImageSel::Copper) | Self::ThinGap(ImageSel::Copper) => {
                needs.copper = true;
            }
            Self::ThinFeature(ImageSel::Soldermask) | Self::ThinGap(ImageSel::Soldermask) => {
                needs.masks = true;
            }
        }
        needs
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
            Self::SlotWidth => "ipc_nominal_or_outline_boundary_distance",
            Self::HolePairClearance => "circle_edge_distance",
            Self::AnnularRing(_) => "maximal_centered_disk_minus_hole_radius",
            Self::LineworkToCopperClearance(_) => "segment_to_filled_region_boundary",
            Self::BoardArrayPairClearance => "filled_profile_boundary_distance",
            Self::ThinFeature(_) => "opening_candidate_then_boundary_distance",
            Self::ThinGap(_) => "closing_candidate_then_boundary_distance",
        }
    }
}

/// Which entity pools the configured rules read; extraction builds exactly
/// this union.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Needs {
    pub holes: bool,
    /// Class-specific rules cannot certify a hole whose plating intent is
    /// absent or unknown; extraction fails closed when this is set.
    pub classified_holes: bool,
    pub slots: bool,
    pub copper: bool,
    pub masks: bool,
    pub scores: bool,
    pub board_outlines: bool,
    pub board_arrays: bool,
}

impl Needs {
    fn union(self, other: Self) -> Self {
        Self {
            holes: self.holes || other.holes,
            classified_holes: self.classified_holes || other.classified_holes,
            slots: self.slots || other.slots,
            copper: self.copper || other.copper,
            masks: self.masks || other.masks,
            scores: self.scores || other.scores,
            board_outlines: self.board_outlines || other.board_outlines,
            board_arrays: self.board_arrays || other.board_arrays,
        }
    }
}

pub(super) fn needs(rules: &[Rule]) -> Needs {
    rules
        .iter()
        .map(|rule| rule.kind.needs())
        .fold(Needs::default(), Needs::union)
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
