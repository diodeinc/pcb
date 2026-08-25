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
    /// Width: final composed copper material narrower than the limit.
    CopperFeatureWidth,
    /// Clearance: final copper owned by distinct electrical conductors.
    CopperClearance,
    /// Residue: final soldermask material between openings narrower than the limit.
    SoldermaskWeb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Linework {
    VScore,
    BoardEdge,
}

/// Everything the engine and report know about a rule kind, in one table:
/// what one `checked` unit is, the measured quantity and method, how a
/// finding is titled and phrased, the roles of its two witness points, and
/// the pools it reads.
pub(super) struct Semantics {
    pub subject: &'static str,
    pub quantity: &'static str,
    pub method: &'static str,
    pub finding_title: String,
    pub quantity_label: String,
    pub witness_roles: [&'static str; 2],
    pub pools: Pools,
}

/// The entity pools a rule reads. Extraction builds exactly the union over
/// the configured rules, so a rule set pays only for what it measures.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Pools {
    pub drilled: bool,
    pub copper: bool,
    pub conductor_ownership: bool,
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
            conductor_ownership: self.conductor_ownership || other.conductor_ownership,
            copper_boundaries: self.copper_boundaries || other.copper_boundaries,
            hole_lands: self.hole_lands || other.hole_lands,
            masks: self.masks || other.masks,
            scores: self.scores || other.scores,
            board_outlines: self.board_outlines || other.board_outlines,
            board_arrays: self.board_arrays || other.board_arrays,
        }
    }
}

const DRILLED: Pools = Pools {
    drilled: true,
    copper: false,
    conductor_ownership: false,
    copper_boundaries: false,
    hole_lands: false,
    masks: false,
    scores: false,
    board_outlines: false,
    board_arrays: false,
};
const COPPER: Pools = Pools {
    copper: true,
    ..DRILLED
};
const COPPER_BOUNDARIES: Pools = Pools {
    copper_boundaries: true,
    ..COPPER
};
const NONE: Pools = Pools {
    drilled: false,
    ..DRILLED
};

impl RuleKind {
    pub fn semantics(self) -> Semantics {
        match self {
            Self::HoleDiameter(class) => Semantics {
                subject: "hole",
                quantity: "hole_diameter",
                method: "ipc_hole_diameter",
                finding_title: format!("{} hole is below minimum diameter", class.label()),
                quantity_label: format!("{} hole diameter", class.label()),
                witness_roles: ["hole_boundary", "hole_boundary"],
                pools: DRILLED,
            },
            Self::SlotWidth => Semantics {
                subject: "slot",
                quantity: "slot_width",
                method: "ipc_slot_width_or_outline_medial_axis_width",
                finding_title: "Slot is below minimum width".to_owned(),
                quantity_label: "routed slot width".to_owned(),
                witness_roles: ["first_slot_boundary", "second_slot_boundary"],
                pools: DRILLED,
            },
            Self::HolePairClearance => Semantics {
                subject: "hole",
                quantity: "hole_edge_to_hole_edge_clearance",
                method: "circle_edge_distance",
                finding_title: "Hole-to-hole clearance is below minimum".to_owned(),
                quantity_label: "hole edge-to-edge clearance".to_owned(),
                witness_roles: ["first_hole_boundary", "second_hole_boundary"],
                pools: DRILLED,
            },
            Self::AnnularRing(class) => Semantics {
                subject: "hole_layer_pair",
                quantity: "annular_ring",
                method: "maximal_centered_disk_minus_hole_radius",
                finding_title: format!("{} annular ring is below minimum", class.label()),
                quantity_label: format!("{} annular ring", class.label()),
                witness_roles: ["hole_boundary", "copper_boundary"],
                pools: Pools {
                    hole_lands: true,
                    ..COPPER_BOUNDARIES
                },
            },
            Self::LineworkToCopperClearance(Linework::VScore) => Semantics {
                subject: "score_layer_pair",
                quantity: "vscore_centerline_to_copper_clearance",
                method: "segment_to_filled_region_boundary",
                finding_title: "V-score centerline is too close to copper".to_owned(),
                quantity_label: "V-score centerline-to-copper clearance".to_owned(),
                witness_roles: ["vscore_centerline", "copper_boundary"],
                pools: Pools {
                    scores: true,
                    drilled: false,
                    ..COPPER_BOUNDARIES
                },
            },
            Self::LineworkToCopperClearance(Linework::BoardEdge) => Semantics {
                subject: "outline_layer_pair",
                quantity: "board_edge_to_copper_clearance",
                method: "segment_to_filled_region_boundary",
                finding_title: "Board edge is too close to copper".to_owned(),
                quantity_label: "board-edge-to-copper clearance".to_owned(),
                witness_roles: ["board_outline", "copper_boundary"],
                pools: Pools {
                    board_outlines: true,
                    drilled: false,
                    ..COPPER_BOUNDARIES
                },
            },
            Self::BoardArrayPairClearance => Semantics {
                subject: "array_pair",
                quantity: "board_array_outline_spacing",
                method: "filled_profile_boundary_distance",
                finding_title: "Board arrays are too close together".to_owned(),
                quantity_label: "board-array outline spacing".to_owned(),
                witness_roles: ["first_board_array", "second_board_array"],
                pools: Pools {
                    board_arrays: true,
                    ..NONE
                },
            },
            Self::CopperFeatureWidth => Semantics {
                subject: "copper_layer",
                quantity: "copper_feature_width",
                method: "opening_candidate_then_medial_axis_width",
                finding_title: "Copper feature is below minimum width".to_owned(),
                quantity_label: "copper feature width".to_owned(),
                witness_roles: ["first_boundary", "second_boundary"],
                pools: Pools {
                    drilled: false,
                    ..COPPER
                },
            },
            Self::CopperClearance => Semantics {
                subject: "conductor_pair",
                quantity: "copper_to_copper_clearance",
                method: "distinct_conductor_boundary_distance",
                finding_title: "Copper spacing is below minimum".to_owned(),
                quantity_label: "copper-to-copper clearance".to_owned(),
                witness_roles: ["first_conductor", "second_conductor"],
                pools: Pools {
                    drilled: false,
                    conductor_ownership: true,
                    ..COPPER
                },
            },
            Self::SoldermaskWeb => Semantics {
                subject: "soldermask_layer",
                quantity: "soldermask_web_width",
                method: "closing_candidate_then_medial_axis_width",
                finding_title: "Soldermask web is below minimum".to_owned(),
                quantity_label: "soldermask web width".to_owned(),
                witness_roles: ["first_boundary", "second_boundary"],
                pools: Pools {
                    masks: true,
                    ..NONE
                },
            },
        }
    }
}

pub(super) fn pools(rules: &[Rule]) -> Pools {
    rules
        .iter()
        .map(|rule| rule.kind.semantics().pools)
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
            RuleKind::CopperFeatureWidth,
        ),
        (
            &copper.minimum_copper_clearance,
            "copper.minimum_copper_clearance",
            "Minimum copper-to-copper clearance",
            RuleKind::CopperClearance,
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
            RuleKind::SoldermaskWeb,
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
