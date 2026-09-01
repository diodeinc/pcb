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
        let mut ids = HashSet::new();
        for rule in self.rules.all() {
            let metadata = rule.metadata();
            if metadata.id.trim().is_empty() {
                bail!("PDK rule ids must not be empty");
            }
            if !ids.insert(metadata.id.clone()) {
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
    pub defaults: ProfileDefaults,
    #[serde(default)]
    pub source: Option<String>,
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
    pub stackup: StackupRules,
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
        self.stackup
            .copper_layer_count
            .iter()
            .map(RuleDefinition::Count)
            .chain(
                self.drilling
                    .hole_diameter
                    .iter()
                    .map(RuleDefinition::HoleDiameter),
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
                self.copper
                    .annular_ring
                    .iter()
                    .map(RuleDefinition::AnnularRing),
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
    Count(&'a CountRule),
    HoleDiameter(&'a HoleDiameterRule),
    SlotWidth(&'a SlotWidthRule),
    HolePair(&'a HolePairRule),
    AnnularRing(&'a AnnularRingRule),
    CopperLength(&'a LengthRule),
    OtherLength(&'a LengthRule),
}

impl RuleDefinition<'_> {
    fn metadata(&self) -> &RuleMetadata {
        match self {
            Self::Count(rule) => &rule.metadata,
            Self::HoleDiameter(rule) => &rule.metadata,
            Self::SlotWidth(rule) => &rule.metadata,
            Self::HolePair(rule) => &rule.metadata,
            Self::AnnularRing(rule) => &rule.metadata,
            Self::CopperLength(rule) | Self::OtherLength(rule) => &rule.metadata,
        }
    }

    fn validate(&self) -> Result<()> {
        let metadata = self.metadata();
        metadata.when.validate(&metadata.id)?;
        if metadata.when.layer.is_some()
            && !matches!(self, Self::AnnularRing(_) | Self::CopperLength(_))
        {
            bail!(
                "rule '{}': layer and copper_weight conditions are supported only for copper rules",
                metadata.id
            );
        }
        match self {
            Self::Count(rule) => rule.validate(),
            Self::HoleDiameter(rule) => {
                validate_length(&rule.metadata, &rule.minimum, rule.preferred.as_ref())
            }
            Self::SlotWidth(rule) => {
                validate_length(&rule.metadata, &rule.minimum, rule.preferred.as_ref())
            }
            Self::HolePair(rule) => {
                validate_length(&rule.metadata, &rule.minimum, rule.preferred.as_ref())
            }
            Self::AnnularRing(rule) => {
                validate_length(&rule.metadata, &rule.minimum, rule.preferred.as_ref())
            }
            Self::CopperLength(rule) | Self::OtherLength(rule) => rule.validate(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StackupRules {
    #[serde(default)]
    pub copper_layer_count: Vec<CountRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrillingRules {
    #[serde(default)]
    pub hole_diameter: Vec<HoleDiameterRule>,
    #[serde(default)]
    pub slot_width: Vec<SlotWidthRule>,
    #[serde(default)]
    pub hole_to_hole_clearance: Vec<HolePairRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopperRules {
    #[serde(default)]
    pub annular_ring: Vec<AnnularRingRule>,
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
    pub when: RuleConditions,
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
    pub minimum_copper_layers: Option<u32>,
    #[serde(default)]
    pub maximum_copper_layers: Option<u32>,
    #[serde(default)]
    pub layer: Option<LayerPosition>,
    #[serde(default)]
    pub copper_weight: Option<CopperWeight>,
}

impl RuleConditions {
    fn validate(&self, id: &str) -> Result<()> {
        for (name, count) in [
            ("minimum_copper_layers", self.minimum_copper_layers),
            ("maximum_copper_layers", self.maximum_copper_layers),
        ] {
            if count == Some(0) {
                bail!("rule '{id}' {name} must be a positive integer");
            }
        }
        if let (Some(minimum), Some(maximum)) =
            (self.minimum_copper_layers, self.maximum_copper_layers)
            && minimum > maximum
        {
            bail!(
                "rule '{id}' minimum_copper_layers ({minimum}) must not exceed maximum_copper_layers ({maximum})"
            );
        }
        if self.copper_weight.is_some() && self.layer.is_none() {
            bail!("rule '{id}' copper_weight requires a layer condition");
        }
        Ok(())
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
pub struct CountRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    #[serde(default)]
    pub minimum: Option<u32>,
    #[serde(default)]
    pub maximum: Option<u32>,
}

impl CountRule {
    fn validate(&self) -> Result<()> {
        if self.minimum.is_none() && self.maximum.is_none() {
            bail!(
                "rule '{}' requires minimum, maximum, or both",
                self.metadata.id
            );
        }
        for (name, count) in [("minimum", self.minimum), ("maximum", self.maximum)] {
            if count == Some(0) {
                bail!(
                    "rule '{}' {name} must be a positive integer",
                    self.metadata.id
                );
            }
        }
        if let (Some(minimum), Some(maximum)) = (self.minimum, self.maximum)
            && minimum > maximum
        {
            bail!(
                "rule '{}' minimum ({minimum}) must not exceed maximum ({maximum})",
                self.metadata.id
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LengthRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub minimum: Length,
    #[serde(default)]
    pub preferred: Option<Length>,
}

impl LengthRule {
    fn validate(&self) -> Result<()> {
        validate_length(&self.metadata, &self.minimum, self.preferred.as_ref())
    }
}

fn validate_length(
    metadata: &RuleMetadata,
    minimum: &Length,
    preferred: Option<&Length>,
) -> Result<()> {
    if let Some(preferred) = preferred
        && preferred.millimeters() <= minimum.millimeters()
    {
        bail!(
            "rule '{}': preferred limit {} must exceed the minimum {}",
            metadata.id,
            preferred.original(),
            minimum.original()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoleDiameterRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub hole: HoleKind,
    pub minimum: Length,
    #[serde(default)]
    pub preferred: Option<Length>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotWidthRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub plating: SlotPlating,
    pub minimum: Length,
    #[serde(default)]
    pub preferred: Option<Length>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HolePairRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub first_hole: HoleKind,
    pub second_hole: HoleKind,
    pub minimum: Length,
    #[serde(default)]
    pub preferred: Option<Length>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnularRingRule {
    #[serde(flatten)]
    pub metadata: RuleMetadata,
    pub hole: PlatedHoleKind,
    pub minimum: Length,
    #[serde(default)]
    pub preferred: Option<Length>,
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

[profiles.standard.defaults]
outer_copper_weight = "1 oz"

[[rules.stackup.copper_layer_count]]
id = "layers"
minimum = 2
maximum = 10

[[rules.drilling.hole_diameter]]
id = "via-hole"
hole = "via"
minimum = "0.2 mm"

[[rules.drilling.hole_to_hole_clearance]]
id = "via-spacing"
first_hole = "via"
second_hole = "via"
minimum = "10 mil"

[[rules.copper.annular_ring]]
id = "via-ring"
hole = "via"
minimum = "100 um"
preferred = "0.125 mm"

[[rules.panelization.board_spacing]]
id = "array-spacing"
minimum = "300 mil"
"#;

    #[test]
    fn parses_profiles_typed_rules_units_and_tiers() {
        let pdk = Pdk::parse(MIXED_UNIT_PDK).unwrap();
        pdk.validate_rule_references().unwrap();
        assert_eq!(pdk.rules.stackup.copper_layer_count[0].minimum, Some(2));
        assert_eq!(pdk.rules.stackup.copper_layer_count[0].maximum, Some(10));
        assert_eq!(
            pdk.rules.drilling.hole_diameter[0].minimum.millimeters(),
            0.2
        );
        assert!(
            (pdk.rules.drilling.hole_to_hole_clearance[0]
                .minimum
                .millimeters()
                - 0.254)
                .abs()
                < 1e-12
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
    }

    #[test]
    fn rejects_schema_v1_bare_lengths_and_invalid_conditions() {
        assert!(
            Pdk::parse(&MIXED_UNIT_PDK.replace("schema_version = 2", "schema_version = 1"))
                .is_err()
        );
        assert!(Pdk::parse(&MIXED_UNIT_PDK.replace("\"0.2 mm\"", "0.2")).is_err());
        let invalid = MIXED_UNIT_PDK.replace(
            "id = \"via-hole\"",
            "id = \"via-hole\"\nwhen = { minimum_copper_layers = 4, maximum_copper_layers = 2 }",
        );
        assert!(
            Pdk::parse(&invalid)
                .unwrap()
                .validate_rule_references()
                .is_err()
        );
        let unsupported = MIXED_UNIT_PDK.replace(
            "id = \"via-hole\"",
            "id = \"via-hole\"\nwhen = { layer = \"outer\" }",
        );
        assert!(
            Pdk::parse(&unsupported)
                .unwrap()
                .validate_rule_references()
                .unwrap_err()
                .to_string()
                .contains("supported only for copper rules")
        );
    }
}
