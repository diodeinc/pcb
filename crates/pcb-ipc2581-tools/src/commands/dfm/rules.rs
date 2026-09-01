//! Configured PDK capabilities lowered to rules the engine evaluates.
//!
//! A rule is pure data: an identity, a limit, a severity, and a [`RuleKind`]
//! naming which measurement the engine runs. Rule ids are the PDK capability
//! paths, so a report consumer can find every limit's source line in the PDK
//! verbatim; a capability's preferred tier lowers to a second, warning-level
//! rule under `<capability>.preferred`.

use anyhow::Result;

use super::design::HoleClass;
use super::pdk::{
    CopperWeight, HoleKind, LayerPosition, Length, Pdk, PlatedHoleKind, Profile, ProfileStatus,
    RuleConditions, RuleMetadata, SlotPlating,
};
use super::report::{Severity, ViewRecipe};

#[derive(Debug, Clone)]
pub(super) struct Rule {
    pub id: String,
    pub title: String,
    pub severity: Severity,
    pub comparison: Comparison,
    pub limit: LimitValue,
    pub kind: RuleKind,
    pub conditions: Conditions,
}

#[derive(Debug, Clone, Default)]
pub(super) struct Conditions {
    pub minimum_copper_layers: Option<u32>,
    pub maximum_copper_layers: Option<u32>,
    pub layer: Option<LayerPosition>,
    pub copper_weight_oz: Option<f64>,
    pub assumed_outer_copper_weight_oz: Option<f64>,
    pub assumed_inner_copper_weight_oz: Option<f64>,
}

impl Conditions {
    pub fn requires_stackup(&self) -> bool {
        self.minimum_copper_layers.is_some()
            || self.maximum_copper_layers.is_some()
            || self.copper_weight_oz.is_some()
    }

    pub fn applies_to_design(&self, design: &super::design::Design) -> bool {
        if self.minimum_copper_layers.is_none() && self.maximum_copper_layers.is_none() {
            return true;
        }
        let Some(stackup) = design.stackup.as_ref() else {
            return false;
        };
        let count = stackup.copper_layers.len() as u32;
        self.minimum_copper_layers
            .is_none_or(|minimum| count >= minimum)
            && self
                .maximum_copper_layers
                .is_none_or(|maximum| count <= maximum)
    }

    pub fn applies_to_layer(&self, layer: &super::design::CopperLayer) -> bool {
        if self
            .layer
            .is_some_and(|position| position != layer.position)
        {
            return false;
        }
        let Some(required_oz) = self.copper_weight_oz else {
            return true;
        };
        let actual_oz = layer.copper_weight_oz.or(match layer.position {
            LayerPosition::Outer => self.assumed_outer_copper_weight_oz,
            LayerPosition::Inner => self.assumed_inner_copper_weight_oz,
        });
        actual_oz.is_some_and(|actual| (actual - required_oz).abs() <= 0.01)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Comparison {
    Minimum,
    Maximum,
}

impl Comparison {
    pub fn label(self) -> &'static str {
        match self {
            Self::Minimum => "minimum",
            Self::Maximum => "maximum",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum LimitValue {
    Length(Length),
    Count(u32),
}

impl LimitValue {
    pub fn length(&self) -> &Length {
        match self {
            Self::Length(length) => length,
            Self::Count(_) => unreachable!("a count-valued rule has no length limit"),
        }
    }

    pub fn count(&self) -> u32 {
        match self {
            Self::Count(count) => *count,
            Self::Length(_) => unreachable!("a length-valued rule has no count limit"),
        }
    }
}

/// The measurement semantics of a rule: a size, a clearance, an enclosure,
/// or a morphological residue over one of the design's entity pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuleKind {
    /// Count: conductive layers in the one physical stackup.
    CopperLayerCount,
    /// Size: each hole's drilled diameter meets the limit.
    HoleDiameter(HoleClass),
    /// Size: each routed slot's width meets the limit.
    SlotWidth(SlotPlating),
    /// Clearance: holes with overlapping spans keep edge-to-edge distance.
    HolePairClearance(HoleClass, HoleClass),
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
    pub witness_roles: Option<[&'static str; 2]>,
    pub pools: Pools,
}

/// The entity pools a rule reads. Extraction builds exactly the union over
/// the configured rules, so a rule set pays only for what it measures.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Pools {
    pub stackup: bool,
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
            stackup: self.stackup || other.stackup,
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
    stackup: false,
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
    stackup: false,
    drilled: false,
    ..DRILLED
};

impl RuleKind {
    pub fn view_recipe(self) -> ViewRecipe {
        let (kind, title, spatial, features): (_, _, _, &[_]) = match self {
            Self::CopperLayerCount => (
                "copper_layer_count",
                "Copper layer count",
                false,
                &["stackup"],
            ),
            Self::HoleDiameter(_) => (
                "hole_diameter",
                "Hole diameter",
                true,
                &["drills", "board_outlines"],
            ),
            Self::SlotWidth(_) => (
                "slot_width",
                "Slot width",
                true,
                &["drills", "board_outlines"],
            ),
            Self::HolePairClearance(_, _) => (
                "hole_clearance",
                "Hole-to-hole clearance",
                true,
                &["drills", "board_outlines"],
            ),
            Self::AnnularRing(_) => (
                "annular_ring",
                "Annular ring",
                true,
                &["copper", "drills", "board_outlines"],
            ),
            Self::LineworkToCopperClearance(Linework::BoardEdge) => (
                "board_edge_clearance",
                "Board-edge clearance",
                true,
                &["copper", "board_outlines"],
            ),
            Self::LineworkToCopperClearance(Linework::VScore) => (
                "vscore_clearance",
                "V-score clearance",
                true,
                &["copper", "scores", "board_outlines"],
            ),
            Self::BoardArrayPairClearance => {
                ("array_spacing", "Array spacing", true, &["array_outlines"])
            }
            Self::CopperFeatureWidth => (
                "copper_width",
                "Copper width",
                true,
                &["copper", "board_outlines"],
            ),
            Self::CopperClearance => (
                "copper_clearance",
                "Copper clearance",
                true,
                &["copper", "board_outlines"],
            ),
            Self::SoldermaskWeb => (
                "soldermask_web",
                "Soldermask web",
                true,
                &["mask_openings", "board_outlines"],
            ),
        };
        ViewRecipe {
            kind,
            title,
            spatial,
            features: features.to_vec(),
        }
    }

    pub fn semantics(self) -> Semantics {
        match self {
            Self::CopperLayerCount => Semantics {
                subject: "stackup",
                quantity: "copper_layer_count",
                method: "physical_stackup_conductive_layer_count",
                finding_title: "Copper layer count is outside the supported range".to_owned(),
                quantity_label: "copper layer count".to_owned(),
                witness_roles: None,
                pools: Pools {
                    stackup: true,
                    ..NONE
                },
            },
            Self::HoleDiameter(class) => Semantics {
                subject: "hole",
                quantity: "hole_diameter",
                method: "ipc_hole_diameter",
                finding_title: format!("{} hole is below minimum diameter", class.label()),
                quantity_label: format!("{} hole diameter", class.label()),
                witness_roles: Some(["hole_boundary", "hole_boundary"]),
                pools: DRILLED,
            },
            Self::SlotWidth(plating) => Semantics {
                subject: "slot",
                quantity: "slot_width",
                method: "ipc_slot_width_or_outline_medial_axis_width",
                finding_title: format!("{} slot is below minimum width", slot_label(plating)),
                quantity_label: format!("{} routed slot width", slot_label(plating)),
                witness_roles: Some(["first_slot_boundary", "second_slot_boundary"]),
                pools: DRILLED,
            },
            Self::HolePairClearance(first, second) => Semantics {
                subject: "hole",
                quantity: "hole_edge_to_hole_edge_clearance",
                method: "circle_edge_distance",
                finding_title: format!(
                    "{}-to-{} hole clearance is below minimum",
                    first.label(),
                    second.label()
                ),
                quantity_label: format!(
                    "{}-to-{} hole edge-to-edge clearance",
                    first.label(),
                    second.label()
                ),
                witness_roles: Some(["first_hole_boundary", "second_hole_boundary"]),
                pools: DRILLED,
            },
            Self::AnnularRing(class) => Semantics {
                subject: "hole_layer_pair",
                quantity: "annular_ring",
                method: "maximal_centered_disk_minus_hole_radius",
                finding_title: format!("{} annular ring is below minimum", class.label()),
                quantity_label: format!("{} annular ring", class.label()),
                witness_roles: Some(["hole_boundary", "copper_boundary"]),
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
                witness_roles: Some(["vscore_centerline", "copper_boundary"]),
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
                witness_roles: Some(["board_outline", "copper_boundary"]),
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
                witness_roles: Some(["first_board_array", "second_board_array"]),
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
                witness_roles: Some(["first_boundary", "second_boundary"]),
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
                witness_roles: Some(["first_conductor", "second_conductor"]),
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
                witness_roles: Some(["first_boundary", "second_boundary"]),
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
        .map(|rule| {
            let mut pools = rule.kind.semantics().pools;
            pools.stackup |= rule.conditions.requires_stackup();
            pools
        })
        .fold(Pools::default(), |union, pools| union | pools)
}

/// Lower the selected profile's typed rules. Rules with no `profiles` selector
/// apply to every executable profile in the kit. Required limits are errors;
/// optional preferred limits become warning rules under `<id>.preferred`.
pub(super) fn lower(pdk: &Pdk, selected_profile: Option<&str>) -> Result<Vec<Rule>> {
    pdk.validate_rule_references()?;
    let (profile_name, profile) = pdk.selected_profile(selected_profile)?;
    if profile.status == ProfileStatus::MetadataOnly {
        anyhow::bail!(
            "PDK profile '{profile_name}' is metadata-only; licensed numeric IPC profile rules are required before it can run DFM checks"
        );
    }

    let mut rules = Vec::new();
    for rule in &pdk.rules.stackup.copper_layer_count {
        if !rule.metadata.applies_to(profile_name) {
            continue;
        }
        let conditions = conditions(&rule.metadata.when, profile);
        if let Some(limit) = rule.minimum {
            rules.push(Rule {
                id: if rule.maximum.is_some() {
                    format!("{}.minimum", rule.metadata.id)
                } else {
                    rule.metadata.id.clone()
                },
                title: "Minimum copper layer count".to_owned(),
                severity: Severity::Error,
                comparison: Comparison::Minimum,
                limit: LimitValue::Count(limit),
                kind: RuleKind::CopperLayerCount,
                conditions: conditions.clone(),
            });
        }
        if let Some(limit) = rule.maximum {
            rules.push(Rule {
                id: if rule.minimum.is_some() {
                    format!("{}.maximum", rule.metadata.id)
                } else {
                    rule.metadata.id.clone()
                },
                title: "Maximum copper layer count".to_owned(),
                severity: Severity::Error,
                comparison: Comparison::Maximum,
                limit: LimitValue::Count(limit),
                kind: RuleKind::CopperLayerCount,
                conditions,
            });
        }
    }

    for rule in &pdk.rules.drilling.hole_diameter {
        lower_length_rule(
            &mut rules,
            &rule.metadata,
            &rule.minimum,
            rule.preferred.as_ref(),
            profile_name,
            profile,
            format!("Minimum {} hole diameter", hole_class(rule.hole).label()),
            RuleKind::HoleDiameter(hole_class(rule.hole)),
        );
    }
    for rule in &pdk.rules.drilling.slot_width {
        lower_length_rule(
            &mut rules,
            &rule.metadata,
            &rule.minimum,
            rule.preferred.as_ref(),
            profile_name,
            profile,
            format!("Minimum {} routed slot width", slot_label(rule.plating)),
            RuleKind::SlotWidth(rule.plating),
        );
    }
    for rule in &pdk.rules.drilling.hole_to_hole_clearance {
        let first = hole_class(rule.first_hole);
        let second = hole_class(rule.second_hole);
        lower_length_rule(
            &mut rules,
            &rule.metadata,
            &rule.minimum,
            rule.preferred.as_ref(),
            profile_name,
            profile,
            format!(
                "Minimum {}-to-{} hole clearance",
                first.label(),
                second.label()
            ),
            RuleKind::HolePairClearance(first, second),
        );
    }
    for rule in &pdk.rules.copper.annular_ring {
        let class = plated_hole_class(rule.hole);
        lower_length_rule(
            &mut rules,
            &rule.metadata,
            &rule.minimum,
            rule.preferred.as_ref(),
            profile_name,
            profile,
            format!("Minimum {} annular ring", class.label()),
            RuleKind::AnnularRing(class),
        );
    }
    for (ruleset, title, kind) in [
        (
            &pdk.rules.copper.feature_width,
            "Minimum copper feature width",
            RuleKind::CopperFeatureWidth,
        ),
        (
            &pdk.rules.copper.clearance,
            "Minimum copper-to-copper clearance",
            RuleKind::CopperClearance,
        ),
        (
            &pdk.rules.copper.board_edge_clearance,
            "Minimum board-edge-to-copper clearance",
            RuleKind::LineworkToCopperClearance(Linework::BoardEdge),
        ),
        (
            &pdk.rules.copper.vscore_clearance,
            "Minimum V-score centerline-to-copper clearance",
            RuleKind::LineworkToCopperClearance(Linework::VScore),
        ),
        (
            &pdk.rules.soldermask.web,
            "Minimum soldermask web",
            RuleKind::SoldermaskWeb,
        ),
        (
            &pdk.rules.panelization.board_spacing,
            "Minimum spacing between board-array outlines",
            RuleKind::BoardArrayPairClearance,
        ),
    ] {
        for rule in ruleset {
            lower_length_rule(
                &mut rules,
                &rule.metadata,
                &rule.minimum,
                rule.preferred.as_ref(),
                profile_name,
                profile,
                title.to_owned(),
                kind,
            );
        }
    }
    Ok(rules)
}

#[allow(clippy::too_many_arguments)]
fn lower_length_rule(
    rules: &mut Vec<Rule>,
    metadata: &RuleMetadata,
    minimum: &Length,
    preferred: Option<&Length>,
    profile_name: &str,
    profile: &Profile,
    title: String,
    kind: RuleKind,
) {
    if !metadata.applies_to(profile_name) {
        return;
    }
    let conditions = conditions(&metadata.when, profile);
    rules.push(Rule {
        id: metadata.id.clone(),
        title: title.clone(),
        severity: Severity::Error,
        comparison: Comparison::Minimum,
        limit: LimitValue::Length(minimum.clone()),
        kind,
        conditions: conditions.clone(),
    });
    if let Some(preferred) = preferred {
        rules.push(Rule {
            id: format!("{}.preferred", metadata.id),
            title: format!("{title} (preferred)"),
            severity: Severity::Warning,
            comparison: Comparison::Minimum,
            limit: LimitValue::Length(preferred.clone()),
            kind,
            conditions,
        });
    }
}

fn conditions(rule: &RuleConditions, profile: &Profile) -> Conditions {
    Conditions {
        minimum_copper_layers: rule.minimum_copper_layers,
        maximum_copper_layers: rule.maximum_copper_layers,
        layer: rule.layer,
        copper_weight_oz: rule.copper_weight.as_ref().map(CopperWeight::ounces),
        assumed_outer_copper_weight_oz: profile
            .defaults
            .outer_copper_weight
            .as_ref()
            .map(CopperWeight::ounces),
        assumed_inner_copper_weight_oz: profile
            .defaults
            .inner_copper_weight
            .as_ref()
            .map(CopperWeight::ounces),
    }
}

fn hole_class(kind: HoleKind) -> HoleClass {
    match kind {
        HoleKind::Via => HoleClass::Via,
        HoleKind::Pth => HoleClass::Pth,
        HoleKind::Npth => HoleClass::Npth,
    }
}

fn plated_hole_class(kind: PlatedHoleKind) -> HoleClass {
    match kind {
        PlatedHoleKind::Via => HoleClass::Via,
        PlatedHoleKind::Pth => HoleClass::Pth,
    }
}

fn slot_label(plating: SlotPlating) -> &'static str {
    match plating {
        SlotPlating::Plated => "plated",
        SlotPlating::Nonplated => "non-plated",
    }
}
