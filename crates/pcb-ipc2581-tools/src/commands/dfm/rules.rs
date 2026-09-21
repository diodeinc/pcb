//! Configured PDK capabilities lowered to rules the engine evaluates.
//!
//! A rule is pure data: an identity, a limit, a severity, and a [`RuleKind`]
//! naming which measurement the engine runs. Rule ids are the PDK capability
//! paths, so a report consumer can find every limit's source line in the PDK
//! verbatim; a capability's preferred tier lowers to a warning-level rule
//! under `<capability>.preferred`, with or without a binding minimum.

use anyhow::Result;

use super::design::HoleClass;
use super::pdk::{
    CopperWeight, HoleKind, LayerPosition, Length, LengthCase, LengthLimit, Pdk, PlatedHoleKind,
    Profile, ProfileStatus, Ratio, RatioCase, RatioLimit, RuleConditions, RuleMetadata,
    SelectingRule, SlotPlating, copper_weight_class,
};
use super::report::{Severity, ViewRecipe};

#[derive(Debug, Clone)]
pub(super) struct Rule {
    pub id: String,
    /// The PDK rule this was lowered from. Its cases and tiers share it.
    pub authored_id: String,
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
    pub assumed_board_thickness: Option<Length>,
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
        // A stated thickness only approximates a nominal weight, so both
        // sides are compared as the standard weight they belong to.
        actual_oz
            .is_some_and(|actual| copper_weight_class(actual) == copper_weight_class(required_oz))
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
    Ratio(Ratio),
}

impl LimitValue {
    pub fn length(&self) -> &Length {
        match self {
            Self::Length(length) => length,
            Self::Count(_) | Self::Ratio(_) => {
                unreachable!("a non-length-valued rule has no length limit")
            }
        }
    }

    pub fn count(&self) -> u32 {
        match self {
            Self::Count(count) => *count,
            Self::Length(_) | Self::Ratio(_) => {
                unreachable!("a non-count-valued rule has no count limit")
            }
        }
    }

    pub fn ratio(&self) -> f64 {
        match self {
            Self::Ratio(ratio) => ratio.value(),
            Self::Length(_) | Self::Count(_) => {
                unreachable!("a non-ratio-valued rule has no ratio limit")
            }
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
    /// Ratio: drilled physical span thickness divided by finished hole diameter.
    HoleAspectRatio(HoleClass),
    /// Size: each routed slot's width meets the limit.
    SlotWidth(SlotPlating),
    /// Clearance: holes with overlapping spans keep edge-to-edge distance.
    HolePairClearance(HoleClass, HoleClass),
    /// Clearance: each circular hole stays inside and clear of its board profile.
    HoleToBoardEdgeClearance(HoleClass),
    /// Clearance: each routed slot stays inside and clear of its board profile.
    SlotToBoardEdgeClearance(SlotPlating),
    /// Enclosure: radial copper around each hole on the layers it lands on.
    AnnularRing(HoleClass),
    /// Enclosure: nominal copper around the full materialized plated slot.
    PlatedSlotEnclosure,
    /// Clearance: a circular drill stays clear of unrelated final copper.
    HoleToCopperClearance(HoleClass),
    /// Clearance: a materialized slot stays clear of unrelated final copper.
    SlotToCopperClearance(SlotPlating),
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

/// The entity pools a rule reads, as a set. Extraction builds exactly the
/// union over the configured rules, so a rule set pays only for what it
/// measures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Pools(u16);

impl Pools {
    pub const NONE: Self = Self(0);
    pub const STACKUP: Self = Self(1 << 0);
    pub const HOLES: Self = Self(1 << 1);
    pub const SLOTS: Self = Self(1 << 13);
    pub const COPPER: Self = Self(1 << 2);
    pub const CONDUCTOR_OWNERSHIP: Self = Self(1 << 3);
    pub const COPPER_BOUNDARIES: Self = Self(1 << 4);
    pub const CONDUCTOR_BOUNDARIES: Self = Self(1 << 5);
    pub const HOLE_LANDS: Self = Self(1 << 6);
    pub const SLOT_LANDS: Self = Self(1 << 7);
    pub const RESOLVED_DRILL_SPANS: Self = Self(1 << 8);
    pub const MASKS: Self = Self(1 << 9);
    pub const SCORES: Self = Self(1 << 10);
    pub const BOARD_OUTLINES: Self = Self(1 << 11);
    pub const BOARD_ARRAYS: Self = Self(1 << 12);

    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for Pools {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

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
            Self::HoleAspectRatio(_) => (
                "hole_aspect_ratio",
                "Plated-hole aspect ratio",
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
            Self::HoleToBoardEdgeClearance(_) => (
                "hole_to_board_edge_clearance",
                "Hole-to-board-edge clearance",
                true,
                &["drills", "board_outlines"],
            ),
            Self::SlotToBoardEdgeClearance(_) => (
                "slot_to_board_edge_clearance",
                "Slot-to-board-edge clearance",
                true,
                &["drills", "board_outlines"],
            ),
            Self::PlatedSlotEnclosure => (
                "plated_slot_enclosure",
                "Plated-slot copper enclosure",
                true,
                &["copper", "drills", "board_outlines"],
            ),
            Self::AnnularRing(_) => (
                "annular_ring",
                "Annular ring",
                true,
                &["copper", "drills", "board_outlines"],
            ),
            Self::HoleToCopperClearance(_) => (
                "hole_to_copper_clearance",
                "Hole-to-copper clearance",
                true,
                &["copper", "drills", "board_outlines"],
            ),
            Self::SlotToCopperClearance(_) => (
                "slot_to_copper_clearance",
                "Slot-to-copper clearance",
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
                pools: Pools::STACKUP,
            },
            Self::HoleDiameter(class) => Semantics {
                subject: "hole",
                quantity: "hole_diameter",
                method: "ipc_hole_diameter",
                finding_title: format!("{} hole is below minimum diameter", class.label()),
                quantity_label: format!("{} hole diameter", class.label()),
                witness_roles: Some(["hole_boundary", "hole_boundary"]),
                pools: Pools::HOLES,
            },
            Self::HoleAspectRatio(class) => Semantics {
                subject: "hole",
                quantity: "plated_hole_aspect_ratio",
                method: "physical_drilled_span_thickness_over_finished_hole_diameter",
                finding_title: format!("{} hole exceeds maximum aspect ratio", class.label()),
                quantity_label: format!("{} hole aspect ratio", class.label()),
                witness_roles: None,
                pools: Pools::STACKUP | Pools::HOLES,
            },
            Self::SlotWidth(plating) => Semantics {
                subject: "slot",
                quantity: "slot_width",
                method: "ipc_slot_width_or_outline_medial_axis_width",
                finding_title: format!("{} slot is below minimum width", slot_label(plating)),
                quantity_label: format!("{} routed slot width", slot_label(plating)),
                witness_roles: Some(["first_slot_boundary", "second_slot_boundary"]),
                pools: Pools::SLOTS,
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
                pools: Pools::HOLES,
            },
            Self::HoleToBoardEdgeClearance(class) => Semantics {
                subject: "hole",
                quantity: "hole_edge_to_board_edge_clearance",
                method: "circle_to_enclosing_physical_profile_boundary",
                finding_title: format!("{} hole is too close to the board edge", class.label()),
                quantity_label: format!("{} hole-to-board-edge clearance", class.label()),
                witness_roles: Some(["hole_boundary", "board_outline"]),
                pools: Pools::HOLES | Pools::BOARD_OUTLINES,
            },
            Self::SlotToBoardEdgeClearance(plating) => Semantics {
                subject: "slot",
                quantity: "slot_edge_to_board_edge_clearance",
                method: "materialized_slot_to_enclosing_physical_profile_boundary",
                finding_title: format!(
                    "{} slot is too close to the board edge",
                    slot_label(plating)
                ),
                quantity_label: format!(
                    "{} routed-slot-to-board-edge clearance",
                    slot_label(plating)
                ),
                witness_roles: Some(["slot_boundary", "board_outline"]),
                pools: Pools::SLOTS | Pools::BOARD_OUTLINES,
            },
            Self::PlatedSlotEnclosure => Semantics {
                subject: "slot_layer_pair",
                quantity: "plated_slot_copper_enclosure",
                method: "materialized_slot_to_cavity_filled_copper_boundary",
                finding_title: "Plated-slot copper enclosure is below minimum".to_owned(),
                quantity_label: "plated-slot copper enclosure".to_owned(),
                witness_roles: Some(["slot_boundary", "copper_boundary"]),
                pools: Pools::STACKUP
                    | Pools::SLOTS
                    | Pools::COPPER
                    | Pools::COPPER_BOUNDARIES
                    | Pools::SLOT_LANDS
                    | Pools::RESOLVED_DRILL_SPANS,
            },
            Self::AnnularRing(class) => Semantics {
                subject: "hole_layer_pair",
                quantity: "annular_ring",
                method: "maximal_centered_disk_minus_hole_radius",
                finding_title: format!("{} annular ring is below minimum", class.label()),
                quantity_label: format!("{} annular ring", class.label()),
                witness_roles: Some(["hole_boundary", "copper_boundary"]),
                pools: Pools::HOLES | Pools::COPPER | Pools::COPPER_BOUNDARIES | Pools::HOLE_LANDS,
            },
            Self::HoleToCopperClearance(class) => Semantics {
                subject: "hole_layer_pair",
                quantity: "hole_edge_to_unrelated_copper_clearance",
                method: "analytic_circle_to_attributed_composed_copper_distance",
                finding_title: format!(
                    "{} hole clearance to unrelated copper is below minimum",
                    class.label()
                ),
                quantity_label: format!(
                    "{} hole edge-to-unrelated-copper clearance",
                    class.label()
                ),
                witness_roles: Some(["drilled_hole", "offending_copper"]),
                pools: Pools::HOLES
                    | Pools::COPPER
                    | Pools::CONDUCTOR_BOUNDARIES
                    | Pools::HOLE_LANDS
                    | Pools::RESOLVED_DRILL_SPANS,
            },
            Self::SlotToCopperClearance(plating) => Semantics {
                subject: "slot_layer_pair",
                quantity: "slot_edge_to_unrelated_copper_clearance",
                method: "materialized_slot_to_attributed_composed_copper_distance",
                finding_title: format!(
                    "{} slot clearance to unrelated copper is below minimum",
                    slot_label(plating)
                ),
                quantity_label: format!(
                    "{} slot edge-to-unrelated-copper clearance",
                    slot_label(plating)
                ),
                witness_roles: Some(["routed_slot", "offending_copper"]),
                pools: Pools::STACKUP
                    | Pools::SLOTS
                    | Pools::COPPER
                    | Pools::CONDUCTOR_BOUNDARIES
                    | Pools::SLOT_LANDS
                    | Pools::RESOLVED_DRILL_SPANS,
            },
            Self::LineworkToCopperClearance(Linework::VScore) => Semantics {
                subject: "score_layer_pair",
                quantity: "vscore_centerline_to_copper_clearance",
                method: "segment_to_filled_region_boundary",
                finding_title: "V-score centerline is too close to copper".to_owned(),
                quantity_label: "V-score centerline-to-copper clearance".to_owned(),
                witness_roles: Some(["vscore_centerline", "copper_boundary"]),
                pools: Pools::SCORES | Pools::COPPER | Pools::COPPER_BOUNDARIES,
            },
            Self::LineworkToCopperClearance(Linework::BoardEdge) => Semantics {
                subject: "outline_layer_pair",
                quantity: "board_edge_to_copper_clearance",
                method: "segment_to_filled_region_boundary",
                finding_title: "Board edge is too close to copper".to_owned(),
                quantity_label: "board-edge-to-copper clearance".to_owned(),
                witness_roles: Some(["board_outline", "copper_boundary"]),
                pools: Pools::BOARD_OUTLINES | Pools::COPPER | Pools::COPPER_BOUNDARIES,
            },
            Self::BoardArrayPairClearance => Semantics {
                subject: "array_pair",
                quantity: "board_array_outline_spacing",
                method: "filled_profile_boundary_distance",
                finding_title: "Board arrays are too close together".to_owned(),
                quantity_label: "board-array outline spacing".to_owned(),
                witness_roles: Some(["first_board_array", "second_board_array"]),
                pools: Pools::BOARD_ARRAYS,
            },
            Self::CopperFeatureWidth => Semantics {
                subject: "copper_layer",
                quantity: "copper_feature_width",
                method: "opening_candidate_then_medial_axis_width",
                finding_title: "Copper feature is below minimum width".to_owned(),
                quantity_label: "copper feature width".to_owned(),
                witness_roles: Some(["first_boundary", "second_boundary"]),
                pools: Pools::COPPER,
            },
            Self::CopperClearance => Semantics {
                subject: "conductor_pair",
                quantity: "copper_to_copper_clearance",
                method: "distinct_conductor_boundary_distance",
                finding_title: "Copper spacing is below minimum".to_owned(),
                quantity_label: "copper-to-copper clearance".to_owned(),
                witness_roles: Some(["first_conductor", "second_conductor"]),
                pools: Pools::COPPER | Pools::CONDUCTOR_OWNERSHIP,
            },
            Self::SoldermaskWeb => Semantics {
                subject: "soldermask_layer",
                quantity: "soldermask_web_width",
                method: "closing_candidate_then_medial_axis_width",
                finding_title: "Soldermask web is below minimum".to_owned(),
                quantity_label: "soldermask web width".to_owned(),
                witness_roles: Some(["first_boundary", "second_boundary"]),
                pools: Pools::MASKS,
            },
        }
    }
}

impl Rule {
    /// Every pool this rule depends on. Drill-span checks must use the
    /// declared physical order whenever the file carries a stackup, even
    /// though they measure no thickness; so must stackup-conditioned cases.
    pub fn pools(&self, has_stackup: bool) -> Pools {
        let spans = matches!(
            self.kind,
            RuleKind::HoleToCopperClearance(_)
                | RuleKind::AnnularRing(_)
                | RuleKind::HolePairClearance(_, _)
        );
        let pools = self.kind.semantics().pools;
        if self.conditions.requires_stackup() || (spans && has_stackup) {
            pools | Pools::STACKUP
        } else {
            pools
        }
    }
}

pub(super) fn pools(rules: &[Rule], has_stackup: bool) -> Pools {
    rules
        .iter()
        .map(|rule| rule.pools(has_stackup))
        .fold(Pools::NONE, |union, pools| union | pools)
}

/// Lower the selected profile's support envelope and typed rules. Rules with
/// no `profiles` selector apply to every executable profile in the kit.
/// Required limits are errors; preferred limits become warning rules.
pub(super) fn lower(pdk: &Pdk, selected_profile: Option<&str>) -> Result<Vec<Rule>> {
    pdk.validate_rule_references()?;
    let (profile_name, profile) = pdk.selected_profile(selected_profile)?;
    if profile.status == ProfileStatus::MetadataOnly {
        anyhow::bail!(
            "PDK profile '{profile_name}' is metadata-only; executable numeric rules are required before it can run DFM checks"
        );
    }

    let mut rules = Vec::new();
    if let Some(range) = &profile.support.copper_layers {
        if let Some(limit) = range.minimum() {
            rules.push(Rule {
                id: "profile.support.copper_layers.minimum".to_owned(),
                authored_id: "profile.support.copper_layers".to_owned(),
                title: "Profile minimum copper layer count".to_owned(),
                severity: Severity::Error,
                comparison: Comparison::Minimum,
                limit: LimitValue::Count(limit),
                kind: RuleKind::CopperLayerCount,
                conditions: Conditions::default(),
            });
        }
        if let Some(limit) = range.maximum() {
            rules.push(Rule {
                id: "profile.support.copper_layers.maximum".to_owned(),
                authored_id: "profile.support.copper_layers".to_owned(),
                title: "Profile maximum copper layer count".to_owned(),
                severity: Severity::Error,
                comparison: Comparison::Maximum,
                limit: LimitValue::Count(limit),
                kind: RuleKind::CopperLayerCount,
                conditions: Conditions::default(),
            });
        }
    }

    // Every selecting family lowers alike; what a rule selects names its
    // kind and its title.
    fn selecting<Select>(
        family: &[SelectingRule<Select>],
        (profile_name, profile): (&str, &Profile),
        describe: impl Fn(&Select) -> (String, RuleKind),
    ) -> Vec<Rule> {
        family
            .iter()
            .flat_map(|rule| {
                let (title, kind) = describe(&rule.select);
                lower_length_rule(
                    &rule.metadata,
                    rule.limit.as_ref(),
                    &rule.cases,
                    profile_name,
                    profile,
                    title,
                    kind,
                )
            })
            .collect()
    }
    let (drilling, copper, selected) = (
        &pdk.rules.drilling,
        &pdk.rules.copper,
        (profile_name, profile),
    );
    rules.extend(selecting(&drilling.hole_diameter, selected, |select| {
        let class = hole_class(select.hole);
        (
            format!("Minimum {} hole diameter", class.label()),
            RuleKind::HoleDiameter(class),
        )
    }));
    for rule in &drilling.hole_aspect_ratio {
        let class = plated_hole_class(rule.select.hole);
        rules.extend(lower_ratio_rule(
            &rule.metadata,
            rule.limit.as_ref(),
            &rule.cases,
            profile_name,
            profile,
            format!("Maximum {} hole aspect ratio", class.label()),
            RuleKind::HoleAspectRatio(class),
        ));
    }
    rules.extend(selecting(&drilling.slot_width, selected, |select| {
        (
            format!("Minimum {} routed slot width", slot_label(select.plating)),
            RuleKind::SlotWidth(select.plating),
        )
    }));
    rules.extend(selecting(
        &drilling.hole_to_hole_clearance,
        selected,
        |select| {
            let (first, second) = (
                hole_class(select.first_hole),
                hole_class(select.second_hole),
            );
            (
                format!(
                    "Minimum {}-to-{} hole clearance",
                    first.label(),
                    second.label()
                ),
                RuleKind::HolePairClearance(first, second),
            )
        },
    ));
    rules.extend(selecting(
        &drilling.hole_to_board_edge_clearance,
        selected,
        |select| {
            let class = hole_class(select.hole);
            (
                format!("Minimum {} hole-to-board-edge clearance", class.label()),
                RuleKind::HoleToBoardEdgeClearance(class),
            )
        },
    ));
    rules.extend(selecting(
        &drilling.slot_to_board_edge_clearance,
        selected,
        |select| {
            (
                format!(
                    "Minimum {} routed-slot-to-board-edge clearance",
                    slot_label(select.plating)
                ),
                RuleKind::SlotToBoardEdgeClearance(select.plating),
            )
        },
    ));
    rules.extend(selecting(&copper.annular_ring, selected, |select| {
        let class = plated_hole_class(select.hole);
        (
            format!("Minimum {} annular ring", class.label()),
            RuleKind::AnnularRing(class),
        )
    }));
    rules.extend(selecting(&copper.hole_clearance, selected, |select| {
        let class = hole_class(select.hole);
        (
            format!("Minimum {} hole-to-copper clearance", class.label()),
            RuleKind::HoleToCopperClearance(class),
        )
    }));
    rules.extend(selecting(&copper.slot_clearance, selected, |select| {
        (
            format!(
                "Minimum {} slot-to-copper clearance",
                slot_label(select.plating)
            ),
            RuleKind::SlotToCopperClearance(select.plating),
        )
    }));
    for (ruleset, title, kind) in [
        (
            &pdk.rules.copper.plated_slot_enclosure,
            "Minimum plated-slot copper enclosure",
            RuleKind::PlatedSlotEnclosure,
        ),
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
            rules.extend(lower_length_rule(
                &rule.metadata,
                rule.limit.as_ref(),
                &rule.cases,
                profile_name,
                profile,
                title.to_owned(),
                kind,
            ));
        }
    }
    Ok(rules)
}

#[allow(clippy::too_many_arguments)]
fn lower_length_rule(
    metadata: &RuleMetadata,
    limit: Option<&LengthLimit>,
    cases: &[LengthCase],
    profile_name: &str,
    profile: &Profile,
    title: String,
    kind: RuleKind,
) -> Vec<Rule> {
    if !metadata.applies_to(profile_name) {
        return Vec::new();
    }
    match limit {
        Some(limit) => lower_limit(
            &metadata.id,
            &metadata.id,
            limit,
            &RuleConditions::default(),
            profile,
            title,
            kind,
        ),
        None => cases
            .iter()
            .flat_map(|case| {
                lower_limit(
                    &metadata.id,
                    &format!("{}.{}", metadata.id, case.id),
                    &case.limit,
                    &case.when,
                    profile,
                    title.clone(),
                    kind,
                )
            })
            .collect(),
    }
}

fn lower_limit(
    authored_id: &str,
    id: &str,
    limit: &LengthLimit,
    when: &RuleConditions,
    profile: &Profile,
    title: String,
    kind: RuleKind,
) -> Vec<Rule> {
    let conditions = conditions(when, profile);
    let required = limit.minimum.as_ref().map(|minimum| Rule {
        id: id.to_owned(),
        authored_id: authored_id.to_owned(),
        title: title.clone(),
        severity: Severity::Error,
        comparison: Comparison::Minimum,
        limit: LimitValue::Length(minimum.clone()),
        kind,
        conditions: conditions.clone(),
    });
    required
        .into_iter()
        .chain(limit.preferred.as_ref().map(|preferred| Rule {
            id: format!("{id}.preferred"),
            authored_id: authored_id.to_owned(),
            title: format!("{title} (preferred)"),
            severity: Severity::Warning,
            comparison: Comparison::Minimum,
            limit: LimitValue::Length(preferred.clone()),
            kind,
            conditions,
        }))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn lower_ratio_rule(
    metadata: &RuleMetadata,
    limit: Option<&RatioLimit>,
    cases: &[RatioCase],
    profile_name: &str,
    profile: &Profile,
    title: String,
    kind: RuleKind,
) -> Vec<Rule> {
    if !metadata.applies_to(profile_name) {
        return Vec::new();
    }
    match limit {
        Some(limit) => vec![lower_ratio_limit(
            &metadata.id,
            &metadata.id,
            limit,
            &RuleConditions::default(),
            profile,
            title,
            kind,
        )],
        None => cases
            .iter()
            .map(|case| {
                lower_ratio_limit(
                    &metadata.id,
                    &format!("{}.{}", metadata.id, case.id),
                    &case.limit,
                    &case.when,
                    profile,
                    title.clone(),
                    kind,
                )
            })
            .collect(),
    }
}

fn lower_ratio_limit(
    authored_id: &str,
    id: &str,
    limit: &RatioLimit,
    when: &RuleConditions,
    profile: &Profile,
    title: String,
    kind: RuleKind,
) -> Rule {
    Rule {
        id: id.to_owned(),
        authored_id: authored_id.to_owned(),
        title,
        severity: Severity::Error,
        comparison: Comparison::Maximum,
        limit: LimitValue::Ratio(limit.maximum.clone()),
        kind,
        conditions: conditions(when, profile),
    }
}

fn conditions(rule: &RuleConditions, profile: &Profile) -> Conditions {
    let copper_layers = rule.copper_layers.as_ref();
    let copper = rule.copper.as_ref();
    Conditions {
        minimum_copper_layers: copper_layers.and_then(|range| range.minimum()),
        maximum_copper_layers: copper_layers.and_then(|range| range.maximum()),
        layer: copper.map(|condition| condition.position),
        copper_weight_oz: copper
            .and_then(|condition| condition.weight.as_ref())
            .map(CopperWeight::ounces),
        assumed_board_thickness: profile.defaults.board_thickness.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowers_profile_selected_hole_clearance_tiers_and_conditions() {
        let pdk = Pdk::parse(
            r#"schema_version = 2
default_profile = "a"

[pdk]
id = "test"
name = "Test"
revision = "1"

[profiles.a]
name = "A"

[profiles.b]
name = "B"

[[rules.copper.hole_clearance]]
id = "via-clearance"
profiles = ["a"]
select = { hole = "via" }
cases = [
  { id = "outer-1oz", when = { copper = { position = "outer", weight = "1 oz" } }, limit = { minimum = "0.20 mm", preferred = "0.25 mm" } },
  { id = "inner", when = { copper = { position = "inner" } }, limit = { minimum = "0.18 mm" } },
]

[[rules.copper.hole_clearance]]
id = "npth-clearance"
profiles = ["b"]
select = { hole = "npth" }
limit = { minimum = "0.30 mm" }
"#,
        )
        .unwrap();

        let lowered = lower(&pdk, Some("a")).unwrap();
        assert_eq!(lowered.len(), 3);
        assert_eq!(lowered[0].id, "via-clearance.outer-1oz");
        assert_eq!(lowered[1].id, "via-clearance.outer-1oz.preferred");
        assert_eq!(lowered[2].id, "via-clearance.inner");
        assert_eq!(lowered[0].severity, Severity::Error);
        assert_eq!(lowered[1].severity, Severity::Warning);
        assert_eq!(lowered[0].conditions.layer, Some(LayerPosition::Outer));
        assert_eq!(lowered[0].conditions.copper_weight_oz, Some(1.0));
        assert_eq!(lowered[2].conditions.layer, Some(LayerPosition::Inner));
        assert!(matches!(
            lowered[0].kind,
            RuleKind::HoleToCopperClearance(HoleClass::Via)
        ));
        assert_eq!(lowered[1].limit.length().millimeters(), 0.25);
    }

    #[test]
    fn a_stated_thickness_matches_the_standard_weight_it_approximates() {
        use super::super::design::CopperLayer;
        use super::super::report::LayerRef;
        let layer = |thickness_mm: f64| CopperLayer {
            layer: LayerRef {
                name: "L".to_owned(),
                function: "SIGNAL".to_owned(),
                side: Some("top"),
            },
            position: LayerPosition::Outer,
            copper_weight_oz: Some(thickness_mm / 0.0348),
            image: pcb_ir::geom::ContourSet::empty(pcb_ir::geom::Resolution::default()),
            conductors: Vec::new(),
            piece_pairs: 0,
            lands: Vec::new(),
        };
        let weighs = |ounces: f64, thickness_mm: f64| {
            Conditions {
                copper_weight_oz: Some(ounces),
                ..Conditions::default()
            }
            .applies_to_layer(&layer(thickness_mm))
        };
        // 1.4 mil foil, 0.07 mm, and 0.5 oz as finished: none is within
        // 0.01 oz of the nominal weight its stackup means.
        for (ounces, thickness_mm) in [(1.0, 0.03556), (2.0, 0.07), (0.5, 0.0152)] {
            assert!(
                weighs(ounces, thickness_mm),
                "{thickness_mm} mm is {ounces} oz"
            );
        }
        assert!(weighs(1.0, 0.030), "1 oz as finished");
        assert!(!weighs(1.0, 0.0152) && !weighs(0.5, 0.035) && !weighs(2.0, 0.035));
        assert!(!weighs(1.0, 0.07) && !weighs(3.0, 0.07) && weighs(3.0, 0.105));
    }

    #[test]
    fn lowers_preferred_only_limit_to_warning() {
        let pdk = Pdk::parse(
            r#"schema_version = 2
default_profile = "standard"

[pdk]
id = "test"
name = "Test"
revision = "1"

[profiles.standard]
name = "Standard"

[[rules.soldermask.web]]
id = "soldermask.minimum_web"
limit = { preferred = "4 mil" }
"#,
        )
        .unwrap();

        let lowered = lower(&pdk, None).unwrap();
        assert_eq!(lowered.len(), 1);
        assert_eq!(lowered[0].id, "soldermask.minimum_web.preferred");
        assert_eq!(lowered[0].severity, Severity::Warning);
        assert_eq!(lowered[0].limit.length().millimeters(), 0.1016);
    }

    const EDGE_CLEARANCE_PDK: &str = r#"
schema_version = 2
default_profile = "primary"

[pdk]
id = "edge-clearance"
name = "Edge clearance"
revision = "1"

[profiles.primary]
name = "Primary"
technologies = ["rigid"]

[profiles.secondary]
name = "Secondary"
technologies = ["rigid"]

[[rules.drilling.hole_to_board_edge_clearance]]
id = "via-edge"
profiles = ["primary"]
select = { hole = "via" }
cases = [
  { id = "4-to-12-layer", when = { copper_layers = { minimum = 4, maximum = 12 } }, limit = { minimum = "0.3 mm", preferred = "0.4 mm" } },
]

[[rules.drilling.slot_to_board_edge_clearance]]
id = "plated-slot-edge"
profiles = ["primary"]
select = { plating = "plated" }
limit = { minimum = "0.5 mm" }
"#;

    #[test]
    fn lowers_typed_board_edge_clearance_rules_and_tiers() {
        let pdk = Pdk::parse(EDGE_CLEARANCE_PDK).unwrap();
        let rules = lower(&pdk, Some("primary")).unwrap();
        assert_eq!(rules.len(), 3);

        let required = &rules[0];
        assert_eq!(required.id, "via-edge.4-to-12-layer");
        assert!(matches!(
            required.kind,
            RuleKind::HoleToBoardEdgeClearance(HoleClass::Via)
        ));
        assert_eq!(required.severity, Severity::Error);
        assert_eq!(required.limit.length().millimeters(), 0.3);
        assert_eq!(required.conditions.minimum_copper_layers, Some(4));
        assert_eq!(required.conditions.maximum_copper_layers, Some(12));

        let preferred = &rules[1];
        assert_eq!(preferred.id, "via-edge.4-to-12-layer.preferred");
        assert!(matches!(
            preferred.kind,
            RuleKind::HoleToBoardEdgeClearance(HoleClass::Via)
        ));
        assert_eq!(preferred.severity, Severity::Warning);
        assert_eq!(preferred.limit.length().millimeters(), 0.4);

        assert_eq!(rules[2].id, "plated-slot-edge");
        assert!(matches!(
            rules[2].kind,
            RuleKind::SlotToBoardEdgeClearance(SlotPlating::Plated)
        ));
        assert_eq!(rules[2].limit.length().millimeters(), 0.5);

        assert!(lower(&pdk, Some("secondary")).unwrap().is_empty());
    }
}
