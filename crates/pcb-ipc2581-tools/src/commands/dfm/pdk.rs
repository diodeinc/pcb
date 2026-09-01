use std::collections::{BTreeMap, HashSet};

use anyhow::{Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pdk {
    pub schema_version: u32,
    pub pdk: PdkIdentity,
    pub default_profile: String,
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub sources: BTreeMap<String, SourceReference>,
    #[serde(default)]
    pub rules: Rules,
}

impl Pdk {
    pub fn parse(source: &str) -> Result<Self> {
        let pdk: Self = toml::from_str(source)?;
        if pdk.schema_version != 2 {
            bail!(
                "unsupported PDK schema_version {}; expected 2",
                pdk.schema_version
            );
        }
        if !pdk.profiles.contains_key(&pdk.default_profile) {
            bail!(
                "default_profile '{}' does not name a profile in [profiles]",
                pdk.default_profile
            );
        }
        for (name, profile) in &pdk.profiles {
            if profile.name.trim().is_empty() {
                bail!("profile '{name}' must have a non-empty name");
            }
            if let Some(class) = profile.performance_class
                && !(1..=3).contains(&class)
            {
                bail!("profile '{name}' performance_class must be 1, 2, or 3");
            }
            if let Some(source) = profile.source.as_deref()
                && !pdk.sources.contains_key(source)
            {
                bail!("profile '{name}' cites unknown source '{source}'");
            }
            profile.support.validate(name)?;
        }
        for (id, source) in &pdk.sources {
            if source.title.trim().is_empty() || source.url.trim().is_empty() {
                bail!("source '{id}' requires a non-empty title and URL");
            }
        }
        Ok(pdk)
    }

    pub fn selected_profile(&self, profile: Option<&str>) -> Result<(&str, &Profile)> {
        let name = profile.unwrap_or(&self.default_profile);
        let (name, selected) = self
            .profiles
            .get_key_value(name)
            .ok_or_else(|| anyhow::anyhow!("PDK has no profile '{name}'"))?;
        Ok((name.as_str(), selected))
    }

    pub fn validate_rule_references(&self) -> Result<()> {
        let mut authored_ids = HashSet::new();
        let mut configured_ids = HashSet::from([
            "profile.support.copper_layers.minimum".to_owned(),
            "profile.support.copper_layers.maximum".to_owned(),
        ]);
        for rule in self.rules.all() {
            let metadata = rule.metadata();
            if metadata.id.trim().is_empty() {
                bail!("PDK rule ids must not be empty");
            }
            if !authored_ids.insert(metadata.id.clone()) {
                bail!("duplicate PDK rule id '{}'", metadata.id);
            }
            for profile in &metadata.profiles {
                if !self.profiles.contains_key(profile) {
                    bail!("rule '{}' selects unknown profile '{profile}'", metadata.id);
                }
            }
            if let Some(source) = metadata.source.as_deref()
                && !self.sources.contains_key(source)
            {
                bail!("rule '{}' cites unknown source '{source}'", metadata.id);
            }
            rule.validate()?;
            for id in rule.configured_ids() {
                if !configured_ids.insert(id.clone()) {
                    bail!("duplicate lowered PDK rule id '{id}'");
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdkIdentity {
    pub id: String,
    pub name: String,
    pub revision: String,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub process: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: ProfileStatus,
    #[serde(default)]
    pub performance_class: Option<u8>,
    #[serde(default)]
    pub producibility_level: Option<ProducibilityLevel>,
    #[serde(default)]
    pub technologies: Vec<Technology>,
    #[serde(default)]
    pub coverage: Vec<String>,
    #[serde(default)]
    pub support: ProfileSupport,
    #[serde(default)]
    pub defaults: ProfileDefaults,
    #[serde(default)]
    pub source: Option<String>,
}

/// Hard, measurable bounds a design must satisfy to use a profile.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSupport {
    #[serde(default)]
    pub copper_layers: Option<CountRange>,
}

impl ProfileSupport {
    fn validate(&self, profile: &str) -> Result<()> {
        if let Some(range) = &self.copper_layers {
            range.validate(&format!("profile '{profile}' support.copper_layers"))?;
        }
        Ok(())
    }
}

/// An inclusive positive-integer range. `exact` is shorthand for equal bounds.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CountRange {
    #[serde(default)]
    pub exact: Option<u32>,
    #[serde(default)]
    pub minimum: Option<u32>,
    #[serde(default)]
    pub maximum: Option<u32>,
}

impl CountRange {
    fn validate(&self, context: &str) -> Result<()> {
        if self.exact.is_none() && self.minimum.is_none() && self.maximum.is_none() {
            bail!("{context} requires exact, minimum, or maximum");
        }
        if self.exact.is_some() && (self.minimum.is_some() || self.maximum.is_some()) {
            bail!("{context} exact cannot be combined with minimum or maximum");
        }
        for (name, count) in [
            ("exact", self.exact),
            ("minimum", self.minimum),
            ("maximum", self.maximum),
        ] {
            if count == Some(0) {
                bail!("{context} {name} must be a positive integer");
            }
        }
        if let (Some(minimum), Some(maximum)) = (self.minimum, self.maximum)
            && minimum > maximum
        {
            bail!("{context} minimum ({minimum}) must not exceed maximum ({maximum})");
        }
        Ok(())
    }

    pub fn minimum(&self) -> Option<u32> {
        self.exact.or(self.minimum)
    }

    pub fn maximum(&self) -> Option<u32> {
        self.exact.or(self.maximum)
    }

    fn overlaps(&self, other: &Self) -> bool {
        self.minimum().unwrap_or(1) <= other.maximum().unwrap_or(u32::MAX)
            && other.minimum().unwrap_or(1) <= self.maximum().unwrap_or(u32::MAX)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileStatus {
    #[default]
    Executable,
    MetadataOnly,
}

impl ProfileStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Executable => "executable",
            Self::MetadataOnly => "metadata_only",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum ProducibilityLevel {
    A,
    B,
    C,
}

impl ProducibilityLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Technology {
    Rigid,
    Flex,
    RigidFlex,
    Hdi,
}

impl Technology {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rigid => "rigid",
            Self::Flex => "flex",
            Self::RigidFlex => "rigid_flex",
            Self::Hdi => "hdi",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDefaults {
    #[serde(default)]
    pub material: Option<String>,
    #[serde(default)]
    pub board_thickness: Option<Length>,
    #[serde(default)]
    pub outer_copper_weight: Option<CopperWeight>,
    #[serde(default)]
    pub inner_copper_weight: Option<CopperWeight>,
    #[serde(default)]
    pub soldermask_color: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReference {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub accessed: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    #[serde(default)]
    pub drilling: DrillingRules,
    #[serde(default)]
    pub copper: CopperRules,
    #[serde(default)]
    pub soldermask: SoldermaskRules,
    #[serde(default)]
    pub panelization: PanelizationRules,
}

impl Rules {
    fn all(&self) -> impl Iterator<Item = RuleDefinition<'_>> {
        self.drilling
            .hole_diameter
            .iter()
            .map(RuleDefinition::HoleDiameter)
            .chain(
                self.drilling
                    .hole_aspect_ratio
                    .iter()
                    .map(RuleDefinition::HoleAspectRatio),
            )
            .chain(
                self.drilling
                    .slot_width
                    .iter()
                    .map(RuleDefinition::SlotWidth),
            )
            .chain(
                self.drilling
                    .hole_to_hole_clearance
                    .iter()
                    .map(RuleDefinition::HolePair),
            )
            .chain(
                self.drilling
                    .hole_to_board_edge_clearance
                    .iter()
                    .map(RuleDefinition::HoleToBoardEdge),
            )
            .chain(
                self.drilling
                    .slot_to_board_edge_clearance
                    .iter()
                    .map(RuleDefinition::SlotToBoardEdge),
            )
            .chain(
                self.copper
                    .annular_ring
                    .iter()
                    .map(RuleDefinition::AnnularRing),
            )
            .chain(
                self.copper
                    .hole_clearance
                    .iter()
                    .map(RuleDefinition::HoleClearance),
            )
            .chain(
                self.copper
                    .feature_width
                    .iter()
                    .map(RuleDefinition::CopperLength),
            )
            .chain(
                self.copper
                    .clearance
                    .iter()
                    .map(RuleDefinition::CopperLength),
            )
            .chain(
                self.copper
                    .board_edge_clearance
                    .iter()
                    .map(RuleDefinition::CopperLength),
            )
            .chain(
                self.copper
                    .vscore_clearance
                    .iter()
                    .map(RuleDefinition::CopperLength),
            )
            .chain(self.soldermask.web.iter().map(RuleDefinition::OtherLength))
            .chain(
                self.panelization
                    .board_spacing
                    .iter()
                    .map(RuleDefinition::OtherLength),
            )
    }
}

enum RuleDefinition<'a> {
    HoleDiameter(&'a HoleDiameterRule),
    HoleAspectRatio(&'a HoleAspectRatioRule),
    SlotWidth(&'a SlotWidthRule),
    HolePair(&'a HolePairRule),
    HoleToBoardEdge(&'a HoleToBoardEdgeClearanceRule),
    SlotToBoardEdge(&'a SlotToBoardEdgeClearanceRule),
    AnnularRing(&'a AnnularRingRule),
    HoleClearance(&'a HoleClearanceRule),
    CopperLength(&'a LengthRule),
    OtherLength(&'a LengthRule),
}

impl RuleDefinition<'_> {
    fn metadata(&self) -> &RuleMetadata {
        match self {
            Self::HoleDiameter(rule) => &rule.metadata,
            Self::HoleAspectRatio(rule) => &rule.metadata,
            Self::SlotWidth(rule) => &rule.metadata,
            Self::HolePair(rule) => &rule.metadata,
            Self::HoleToBoardEdge(rule) => &rule.metadata,
            Self::SlotToBoardEdge(rule) => &rule.metadata,
            Self::AnnularRing(rule) => &rule.metadata,
            Self::HoleClearance(rule) => &rule.metadata,
            Self::CopperLength(rule) | Self::OtherLength(rule) => &rule.metadata,
        }
    }

    fn limits(&self) -> (Option<&LengthLimit>, &[LengthCase]) {
        match self {
            Self::HoleDiameter(rule) => (rule.limit.as_ref(), &rule.cases),
            Self::HoleAspectRatio(_) => {
                unreachable!("an aspect-ratio rule has no length limits")
            }
            Self::SlotWidth(rule) => (rule.limit.as_ref(), &rule.cases),
            Self::HolePair(rule) => (rule.limit.as_ref(), &rule.cases),
            Self::HoleToBoardEdge(rule) => (rule.limit.as_ref(), &rule.cases),
            Self::SlotToBoardEdge(rule) => (rule.limit.as_ref(), &rule.cases),
            Self::AnnularRing(rule) => (rule.limit.as_ref(), &rule.cases),
            Self::HoleClearance(rule) => (rule.limit.as_ref(), &rule.cases),
            Self::CopperLength(rule) | Self::OtherLength(rule) => {
                (rule.limit.as_ref(), &rule.cases)
            }
        }
    }

    fn validate(&self) -> Result<()> {
        let metadata = self.metadata();
        if let Self::HoleAspectRatio(rule) = self {
            return validate_ratio_limits(metadata, rule.limit.as_ref(), &rule.cases);
        }
        let (limit, cases) = self.limits();
        validate_limits(
            metadata,
            limit,
            cases,
            matches!(
                self,
                Self::AnnularRing(_) | Self::HoleClearance(_) | Self::CopperLength(_)
            ),
        )
    }

    fn configured_ids(&self) -> Vec<String> {
        let metadata = self.metadata();
        if let Self::HoleAspectRatio(rule) = self {
            return if rule.limit.is_some() {
                vec![metadata.id.clone()]
            } else {
                rule.cases
                    .iter()
                    .map(|case| format!("{}.{}", metadata.id, case.id))
                    .collect()
            };
        }
        let (limit, cases) = self.limits();
        match limit {
            Some(limit) => limit.ids(&metadata.id),
            None => cases
                .iter()
                .flat_map(|case| case.limit.ids(&format!("{}.{}", metadata.id, case.id)))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrillingRules {
    #[serde(default)]
    pub hole_diameter: Vec<HoleDiameterRule>,
    #[serde(default)]
    pub hole_aspect_ratio: Vec<HoleAspectRatioRule>,
    #[serde(default)]
    pub slot_width: Vec<SlotWidthRule>,
    #[serde(default)]
    pub hole_to_hole_clearance: Vec<HolePairRule>,
    #[serde(default)]
    pub hole_to_board_edge_clearance: Vec<HoleToBoardEdgeClearanceRule>,
    #[serde(default)]
    pub slot_to_board_edge_clearance: Vec<SlotToBoardEdgeClearanceRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopperRules {
    #[serde(default)]
    pub annular_ring: Vec<AnnularRingRule>,
    #[serde(default)]
    pub hole_clearance: Vec<HoleClearanceRule>,
    #[serde(default)]
    pub feature_width: Vec<LengthRule>,
    #[serde(default)]
    pub clearance: Vec<LengthRule>,
    #[serde(default)]
    pub board_edge_clearance: Vec<LengthRule>,
    #[serde(default)]
    pub vscore_clearance: Vec<LengthRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoldermaskRules {
    #[serde(default)]
    pub web: Vec<LengthRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelizationRules {
    #[serde(default)]
    pub board_spacing: Vec<LengthRule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleMetadata {
    pub id: String,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(default)]
    pub source: Option<String>,
}

impl RuleMetadata {
    pub fn applies_to(&self, profile: &str) -> bool {
        self.profiles.is_empty() || self.profiles.iter().any(|candidate| candidate == profile)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleConditions {
    #[serde(default)]
    pub copper_layers: Option<CountRange>,
    #[serde(default)]
    pub copper: Option<CopperCondition>,
}

impl RuleConditions {
    fn validate(&self, id: &str) -> Result<()> {
        if let Some(range) = &self.copper_layers {
            range.validate(&format!("rule '{id}' when.copper_layers"))?;
        }
        Ok(())
    }

    fn overlaps(&self, other: &Self) -> bool {
        let layers_overlap = match (&self.copper_layers, &other.copper_layers) {
            (Some(left), Some(right)) => left.overlaps(right),
            _ => true,
        };
        let copper_overlap = match (&self.copper, &other.copper) {
            (Some(left), Some(right)) => left.overlaps(right),
            _ => true,
        };
        layers_overlap && copper_overlap
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopperCondition {
    pub position: LayerPosition,
    #[serde(default)]
    pub weight: Option<CopperWeight>,
}

impl CopperCondition {
    fn overlaps(&self, other: &Self) -> bool {
        self.position == other.position
            && match (&self.weight, &other.weight) {
                (Some(left), Some(right)) => (left.ounces() - right.ounces()).abs() <= 0.02,
                _ => true,
            }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerPosition {
    Outer,
    Inner,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LengthLimit {
    pub minimum: Length,
    #[serde(default)]
    pub preferred: Option<Length>,
}

impl LengthLimit {
    fn validate(&self, id: &str) -> Result<()> {
        if let Some(preferred) = &self.preferred
            && preferred.millimeters() <= self.minimum.millimeters()
        {
            bail!(
                "rule '{id}': preferred limit {} must exceed the minimum {}",
                preferred.original(),
                self.minimum.original()
            );
        }
        Ok(())
    }

    fn ids(&self, id: &str) -> Vec<String> {
        std::iter::once(id.to_owned())
            .chain(self.preferred.as_ref().map(|_| format!("{id}.preferred")))
            .collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LengthCase {
    pub id: String,
    #[serde(default)]
    pub when: RuleConditions,
    pub limit: LengthLimit,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RatioLimit {
    pub maximum: Ratio,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RatioCase {
    pub id: String,
    #[serde(default)]
    pub when: RuleConditions,
    pub limit: RatioLimit,
}

fn validate_limits(
    metadata: &RuleMetadata,
    limit: Option<&LengthLimit>,
    cases: &[LengthCase],
    allow_copper_conditions: bool,
) -> Result<()> {
    match (limit, cases.is_empty()) {
        (Some(limit), true) => return limit.validate(&metadata.id),
        (None, false) => {}
        (Some(_), false) => {
            bail!(
                "rule '{}': limit and cases are mutually exclusive",
                metadata.id
            )
        }
        (None, true) => bail!("rule '{}': requires limit or cases", metadata.id),
    }

    for case in cases {
        case.limit
            .validate(&format!("{}.{}", metadata.id, case.id))?;
    }
    validate_cases(
        metadata,
        cases.iter().map(|case| (case.id.as_str(), &case.when)),
        allow_copper_conditions,
    )
}

fn validate_ratio_limits(
    metadata: &RuleMetadata,
    limit: Option<&RatioLimit>,
    cases: &[RatioCase],
) -> Result<()> {
    match (limit, cases.is_empty()) {
        (Some(_), true) => return Ok(()),
        (None, false) => {}
        (Some(_), false) => {
            bail!(
                "rule '{}': limit and cases are mutually exclusive",
                metadata.id
            )
        }
        (None, true) => bail!("rule '{}': requires limit or cases", metadata.id),
    }
    validate_cases(
        metadata,
        cases.iter().map(|case| (case.id.as_str(), &case.when)),
        false,
    )
}

fn validate_cases<'a>(
    metadata: &RuleMetadata,
    cases: impl Iterator<Item = (&'a str, &'a RuleConditions)>,
    allow_copper_conditions: bool,
) -> Result<()> {
    let cases = cases.collect::<Vec<_>>();
    let mut ids = HashSet::new();
    for (id, when) in &cases {
        if id.trim().is_empty() {
            bail!("rule '{}': case ids must not be empty", metadata.id);
        }
        if !ids.insert(*id) {
            bail!("rule '{}': duplicate case id '{}'", metadata.id, id);
        }
        when.validate(&format!("{}.{}", metadata.id, id))?;
        if when.copper.is_some() && !allow_copper_conditions {
            bail!(
                "rule '{}.{}': when.copper is supported only for copper rules",
                metadata.id,
                id
            );
        }
    }
    for (index, left) in cases.iter().enumerate() {
        for right in &cases[index + 1..] {
            if left.1.overlaps(right.1) {
                bail!(
                    "rule '{}': cases '{}' and '{}' overlap",
                    metadata.id,
                    left.0,
                    right.0
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoleDiameterRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub select: HoleSelector,
    #[serde(default)]
    pub limit: Option<LengthLimit>,
    #[serde(default)]
    pub cases: Vec<LengthCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoleAspectRatioRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub select: PlatedHoleSelector,
    #[serde(default)]
    pub limit: Option<RatioLimit>,
    #[serde(default)]
    pub cases: Vec<RatioCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotWidthRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub select: SlotSelector,
    #[serde(default)]
    pub limit: Option<LengthLimit>,
    #[serde(default)]
    pub cases: Vec<LengthCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HolePairRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub select: HolePairSelector,
    #[serde(default)]
    pub limit: Option<LengthLimit>,
    #[serde(default)]
    pub cases: Vec<LengthCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoleToBoardEdgeClearanceRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub select: HoleSelector,
    #[serde(default)]
    pub limit: Option<LengthLimit>,
    #[serde(default)]
    pub cases: Vec<LengthCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotToBoardEdgeClearanceRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub select: SlotSelector,
    #[serde(default)]
    pub limit: Option<LengthLimit>,
    #[serde(default)]
    pub cases: Vec<LengthCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnularRingRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub select: PlatedHoleSelector,
    #[serde(default)]
    pub limit: Option<LengthLimit>,
    #[serde(default)]
    pub cases: Vec<LengthCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoleClearanceRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub select: HoleSelector,
    #[serde(default)]
    pub limit: Option<LengthLimit>,
    #[serde(default)]
    pub cases: Vec<LengthCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LengthRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    #[serde(default)]
    pub limit: Option<LengthLimit>,
    #[serde(default)]
    pub cases: Vec<LengthCase>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoleSelector {
    pub hole: HoleKind,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotSelector {
    pub plating: SlotPlating,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HolePairSelector {
    pub first_hole: HoleKind,
    pub second_hole: HoleKind,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatedHoleSelector {
    pub hole: PlatedHoleKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoleKind {
    Via,
    Pth,
    Npth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatedHoleKind {
    Via,
    Pth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotPlating {
    Plated,
    Nonplated,
}

/// A positive finite unitless PDK ratio.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "f64")]
pub struct Ratio(f64);

impl Ratio {
    pub fn value(&self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Ratio {
    type Error = String;

    fn try_from(value: f64) -> std::result::Result<Self, Self::Error> {
        if value.is_finite() && value > 0.0 {
            Ok(Self(value))
        } else {
            Err("ratio must be a positive finite unitless number".to_owned())
        }
    }
}

/// A dimensional PDK value with its source spelling retained for auditability.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "String")]
pub struct Length {
    original: String,
    millimeters: f64,
}

impl Length {
    pub fn original(&self) -> &str {
        &self.original
    }

    pub fn millimeters(&self) -> f64 {
        self.millimeters
    }

    fn parse(value: &str) -> std::result::Result<Self, String> {
        let mut pieces = value.split_whitespace();
        let number = pieces
            .next()
            .ok_or_else(|| Self::expected(value))?
            .parse::<f64>()
            .map_err(|_| Self::expected(value))?;
        let unit = pieces.next().ok_or_else(|| Self::expected(value))?;
        if pieces.next().is_some() || !(number.is_finite() && number > 0.0) {
            return Err(Self::expected(value));
        }
        let millimeters = match unit.to_ascii_lowercase().as_str() {
            "mm" => number,
            "mil" | "mils" => number * pcb_ir::geom::Unit::MM_PER_INCH / 1000.0,
            "um" => number * 0.001,
            _ => return Err(Self::expected(value)),
        };
        Ok(Self {
            original: value.trim().to_owned(),
            millimeters,
        })
    }

    fn expected(value: &str) -> String {
        format!(
            "length '{value}' must be a positive '<number> mm', '<number> mil', or '<number> um' value"
        )
    }
}

impl TryFrom<String> for Length {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

/// Finished copper weight in ounces per square foot.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "String")]
pub struct CopperWeight {
    original: String,
    ounces: f64,
}

impl CopperWeight {
    pub fn original(&self) -> &str {
        &self.original
    }

    pub fn ounces(&self) -> f64 {
        self.ounces
    }
}

impl TryFrom<String> for CopperWeight {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        let mut pieces = value.split_whitespace();
        let number = pieces
            .next()
            .ok_or_else(|| copper_weight_error(&value))?
            .parse::<f64>()
            .map_err(|_| copper_weight_error(&value))?;
        let unit = pieces.next().ok_or_else(|| copper_weight_error(&value))?;
        if pieces.next().is_some()
            || !(number.is_finite() && number > 0.0)
            || !matches!(
                unit.to_ascii_lowercase().as_str(),
                "oz" | "oz/ft2" | "oz/ft²"
            )
        {
            return Err(copper_weight_error(&value));
        }
        Ok(Self {
            original: value.trim().to_owned(),
            ounces: number,
        })
    }
}

fn copper_weight_error(value: &str) -> String {
    format!("copper weight '{value}' must be a positive '<number> oz' value")
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED_UNIT_PDK: &str = r#"
schema_version = 2
default_profile = "standard"

[pdk]
id = "example"
name = "Example process"
revision = "1"

[profiles.standard]
name = "Standard"
technologies = ["rigid"]

[profiles.standard.support]
copper_layers = { minimum = 2, maximum = 10 }

[profiles.standard.defaults]
outer_copper_weight = "1 oz"

[sources.example]
title = "Example source"
url = "https://example.com/pdk"

[[rules.drilling.hole_diameter]]
id = "via-hole"
select = { hole = "via" }
limit = { minimum = "0.2 mm" }

[[rules.drilling.hole_aspect_ratio]]
id = "via-aspect-ratio"
select = { hole = "via" }
limit = { maximum = 8.0 }

[[rules.drilling.hole_to_hole_clearance]]
id = "via-spacing"
select = { first_hole = "via", second_hole = "via" }
limit = { minimum = "10 mil" }

[[rules.drilling.hole_to_board_edge_clearance]]
id = "npth-edge"
profiles = ["standard"]
source = "example"
select = { hole = "npth" }
cases = [
  { id = "2-to-8-layer", when = { copper_layers = { minimum = 2, maximum = 8 } }, limit = { minimum = "0.3 mm", preferred = "0.4 mm" } },
]

[[rules.drilling.slot_to_board_edge_clearance]]
id = "nonplated-slot-edge"
profiles = ["standard"]
source = "example"
select = { plating = "nonplated" }
limit = { minimum = "15 mil" }

[[rules.copper.annular_ring]]
id = "via-ring"
select = { hole = "via" }
limit = { minimum = "100 um", preferred = "0.125 mm" }

[[rules.copper.feature_width]]
id = "copper-width"
cases = [
  { id = "2-layer-outer", when = { copper_layers = { exact = 2 }, copper = { position = "outer", weight = "1 oz" } }, limit = { minimum = "0.1 mm" } },
  { id = "multilayer", when = { copper_layers = { minimum = 3, maximum = 10 } }, limit = { minimum = "0.09 mm" } },
]

[[rules.copper.hole_clearance]]
id = "via-copper-clearance"
select = { hole = "via" }
limit = { minimum = "0.2 mm", preferred = "0.25 mm" }

[[rules.panelization.board_spacing]]
id = "array-spacing"
limit = { minimum = "300 mil" }
"#;

    #[test]
    fn parses_profiles_typed_rules_units_and_tiers() {
        let pdk = Pdk::parse(MIXED_UNIT_PDK).unwrap();
        pdk.validate_rule_references().unwrap();
        let support = pdk.profiles["standard"]
            .support
            .copper_layers
            .as_ref()
            .unwrap();
        assert_eq!(support.minimum(), Some(2));
        assert_eq!(support.maximum(), Some(10));
        assert_eq!(
            pdk.rules.drilling.hole_diameter[0].select.hole,
            HoleKind::Via
        );
        assert_eq!(
            pdk.rules.drilling.hole_diameter[0]
                .limit
                .as_ref()
                .unwrap()
                .minimum
                .millimeters(),
            0.2
        );
        assert_eq!(
            pdk.rules.drilling.hole_aspect_ratio[0]
                .limit
                .as_ref()
                .unwrap()
                .maximum
                .value(),
            8.0
        );
        assert!(
            (pdk.rules.drilling.hole_to_hole_clearance[0]
                .limit
                .as_ref()
                .unwrap()
                .minimum
                .millimeters()
                - 0.254)
                .abs()
                < 1e-12
        );
        let hole_edge = &pdk.rules.drilling.hole_to_board_edge_clearance[0];
        assert_eq!(hole_edge.select.hole, HoleKind::Npth);
        assert_eq!(hole_edge.metadata.profiles, ["standard"]);
        assert_eq!(hole_edge.metadata.source.as_deref(), Some("example"));
        let hole_case = &hole_edge.cases[0];
        assert_eq!(hole_case.id, "2-to-8-layer");
        assert_eq!(
            hole_case.when.copper_layers.as_ref().unwrap().minimum(),
            Some(2)
        );
        assert_eq!(
            hole_case.when.copper_layers.as_ref().unwrap().maximum(),
            Some(8)
        );
        assert_eq!(hole_case.limit.minimum.millimeters(), 0.3);
        assert_eq!(
            hole_case.limit.preferred.as_ref().unwrap().millimeters(),
            0.4
        );
        let slot_edge = &pdk.rules.drilling.slot_to_board_edge_clearance[0];
        assert_eq!(slot_edge.select.plating, SlotPlating::Nonplated);
        assert!((slot_edge.limit.as_ref().unwrap().minimum.millimeters() - 0.381).abs() < 1e-12);
        assert_eq!(
            pdk.rules.copper.feature_width[0].cases[0]
                .when
                .copper_layers
                .as_ref()
                .unwrap()
                .exact,
            Some(2)
        );
        assert_eq!(
            pdk.profiles["standard"]
                .defaults
                .outer_copper_weight
                .as_ref()
                .unwrap()
                .ounces(),
            1.0
        );
        assert_eq!(
            pdk.rules.copper.hole_clearance[0].select.hole,
            HoleKind::Via
        );
        assert_eq!(
            pdk.rules.copper.hole_clearance[0]
                .limit
                .as_ref()
                .unwrap()
                .preferred
                .as_ref()
                .unwrap()
                .millimeters(),
            0.25
        );
    }

    #[test]
    fn rejects_unknown_profiles_sources_and_duplicate_rule_ids() {
        assert!(
            Pdk::parse(&MIXED_UNIT_PDK.replace(
                "default_profile = \"standard\"",
                "default_profile = \"missing\""
            ))
            .is_err()
        );
        let unknown_profile = MIXED_UNIT_PDK.replace(
            "id = \"via-hole\"",
            "id = \"via-hole\"\nprofiles = [\"missing\"]",
        );
        assert!(
            Pdk::parse(&unknown_profile)
                .unwrap()
                .validate_rule_references()
                .is_err()
        );
        let duplicate = MIXED_UNIT_PDK.replace("id = \"via-spacing\"", "id = \"via-hole\"");
        assert!(
            Pdk::parse(&duplicate)
                .unwrap()
                .validate_rule_references()
                .is_err()
        );
        let reserved = MIXED_UNIT_PDK.replace(
            "id = \"via-hole\"",
            "id = \"profile.support.copper_layers.minimum\"",
        );
        assert!(
            Pdk::parse(&reserved)
                .unwrap()
                .validate_rule_references()
                .unwrap_err()
                .to_string()
                .contains("duplicate lowered PDK rule id")
        );
    }

    #[test]
    fn rejects_old_shapes_bad_ranges_and_overlapping_cases() {
        assert!(
            Pdk::parse(&MIXED_UNIT_PDK.replace("schema_version = 2", "schema_version = 1"))
                .is_err()
        );
        assert!(Pdk::parse(&MIXED_UNIT_PDK.replace("\"0.2 mm\"", "0.2")).is_err());
        let invalid = MIXED_UNIT_PDK.replace(
            "limit = { minimum = \"0.2 mm\" }",
            "cases = [{ id = \"bad\", when = { copper_layers = { minimum = 4, maximum = 2 } }, limit = { minimum = \"0.2 mm\" } }]",
        );
        assert!(
            Pdk::parse(&invalid)
                .unwrap()
                .validate_rule_references()
                .is_err()
        );
        let unsupported = MIXED_UNIT_PDK.replace(
            "limit = { minimum = \"0.2 mm\" }",
            "cases = [{ id = \"bad\", when = { copper = { position = \"outer\" } }, limit = { minimum = \"0.2 mm\" } }]",
        );
        assert!(
            Pdk::parse(&unsupported)
                .unwrap()
                .validate_rule_references()
                .unwrap_err()
                .to_string()
                .contains("supported only for copper rules")
        );
        let overlapping = MIXED_UNIT_PDK.replace(
            "{ id = \"multilayer\", when = { copper_layers = { minimum = 3, maximum = 10 } }, limit = { minimum = \"0.09 mm\" } }",
            "{ id = \"overlap\", when = { copper_layers = { minimum = 2, maximum = 10 } }, limit = { minimum = \"0.09 mm\" } }",
        );
        assert!(
            Pdk::parse(&overlapping)
                .unwrap()
                .validate_rule_references()
                .unwrap_err()
                .to_string()
                .contains("overlap")
        );

        let malformed = MIXED_UNIT_PDK.replace(
            "select = { hole = \"via\" }\nlimit = { minimum = \"0.2 mm\", preferred = \"0.25 mm\" }",
            "select = { hole = \"slot\" }\nlimit = { minimum = \"0.2 mm\", preferred = \"0.25 mm\" }",
        );
        assert!(Pdk::parse(&malformed).is_err());

        let invalid_tier = MIXED_UNIT_PDK.replace(
            "limit = { minimum = \"0.2 mm\", preferred = \"0.25 mm\" }",
            "limit = { minimum = \"0.2 mm\", preferred = \"0.15 mm\" }",
        );
        assert!(
            Pdk::parse(&invalid_tier)
                .unwrap()
                .validate_rule_references()
                .is_err()
        );

        let old_hole_shape = MIXED_UNIT_PDK.replace(
            "select = { hole = \"via\" }\nlimit = { minimum = \"0.2 mm\", preferred = \"0.25 mm\" }",
            "hole = \"via\"\nminimum = \"0.2 mm\"\npreferred = \"0.25 mm\"",
        );
        assert!(Pdk::parse(&old_hole_shape).is_err());
    }

    #[test]
    fn rejects_npth_and_invalid_hole_aspect_ratios() {
        assert!(
            Pdk::parse(&MIXED_UNIT_PDK.replace(
                "select = { hole = \"via\" }\nlimit = { maximum = 8.0 }",
                "select = { hole = \"npth\" }\nlimit = { maximum = 8.0 }"
            ))
            .is_err()
        );
        for invalid in ["0.0", "-1.0", "inf", "nan", "\"8.0\""] {
            assert!(
                Pdk::parse(
                    &MIXED_UNIT_PDK.replace("maximum = 8.0", &format!("maximum = {invalid}"))
                )
                .is_err(),
                "accepted invalid ratio {invalid}"
            );
        }
        assert!(
            Pdk::parse(&MIXED_UNIT_PDK.replace(
                "select = { hole = \"via\" }\nlimit = { maximum = 8.0 }",
                "hole = \"via\"\nmaximum = 8.0"
            ))
            .is_err()
        );
    }

    #[test]
    fn validates_named_hole_aspect_ratio_cases() {
        let pdk = MIXED_UNIT_PDK.replace(
            "limit = { maximum = 8.0 }",
            "cases = [\n  { id = \"2-layer\", when = { copper_layers = { exact = 2 } }, limit = { maximum = 8.0 } },\n  { id = \"multilayer\", when = { copper_layers = { minimum = 3 } }, limit = { maximum = 10.0 } },\n]",
        );
        Pdk::parse(&pdk)
            .unwrap()
            .validate_rule_references()
            .unwrap();
    }

    #[test]
    fn rejects_malformed_board_edge_clearance_rules() {
        assert!(
            Pdk::parse(&MIXED_UNIT_PDK.replace(
                "select = { hole = \"npth\" }",
                "select = { hole = \"pad\" }"
            ))
            .is_err()
        );
        assert!(
            Pdk::parse(&MIXED_UNIT_PDK.replace(
                "select = { plating = \"nonplated\" }",
                "select = { plating = \"unplated\" }"
            ))
            .is_err()
        );
        assert!(
            Pdk::parse(&MIXED_UNIT_PDK.replace(
                "select = { hole = \"npth\" }",
                "select = { plating = \"nonplated\" }"
            ))
            .is_err()
        );

        let invalid_tier = MIXED_UNIT_PDK.replace(
            "minimum = \"0.3 mm\", preferred = \"0.4 mm\"",
            "minimum = \"0.3 mm\", preferred = \"0.2 mm\"",
        );
        assert!(
            Pdk::parse(&invalid_tier)
                .unwrap()
                .validate_rule_references()
                .unwrap_err()
                .to_string()
                .contains("preferred limit")
        );
        let unsupported_condition = MIXED_UNIT_PDK.replace(
            "when = { copper_layers = { minimum = 2, maximum = 8 } }",
            "when = { copper = { position = \"outer\" } }",
        );
        assert!(
            Pdk::parse(&unsupported_condition)
                .unwrap()
                .validate_rule_references()
                .unwrap_err()
                .to_string()
                .contains("supported only for copper rules")
        );
        let limit_and_cases = MIXED_UNIT_PDK.replace(
            "cases = [\n  { id = \"2-to-8-layer\"",
            "limit = { minimum = \"0.3 mm\" }\ncases = [\n  { id = \"2-to-8-layer\"",
        );
        assert!(
            Pdk::parse(&limit_and_cases)
                .unwrap()
                .validate_rule_references()
                .unwrap_err()
                .to_string()
                .contains("mutually exclusive")
        );
    }
}
