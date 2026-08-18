use anyhow::{Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pdk {
    pub schema_version: u32,
    pub pdk: PdkIdentity,
    #[serde(default)]
    pub capabilities: Capabilities,
}

impl Pdk {
    pub fn parse(source: &str) -> Result<Self> {
        let pdk: Self = toml::from_str(source)?;
        if pdk.schema_version != 1 {
            bail!(
                "unsupported PDK schema_version {}; expected 1",
                pdk.schema_version
            );
        }
        Ok(pdk)
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    #[serde(default)]
    pub drilling: DrillingCapabilities,
    #[serde(default)]
    pub copper: CopperCapabilities,
    #[serde(default)]
    pub soldermask: SoldermaskCapabilities,
    #[serde(default)]
    pub panelization: PanelizationCapabilities,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrillingCapabilities {
    #[serde(default)]
    pub minimum_via_hole_diameter: Option<Limit>,
    #[serde(default)]
    pub minimum_pth_hole_diameter: Option<Limit>,
    #[serde(default)]
    pub minimum_npth_hole_diameter: Option<Limit>,
    #[serde(default)]
    pub minimum_slot_width: Option<Limit>,
    #[serde(default)]
    pub minimum_hole_to_hole_clearance: Option<Limit>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopperCapabilities {
    #[serde(default)]
    pub minimum_via_annular_ring: Option<Limit>,
    #[serde(default)]
    pub minimum_pth_annular_ring: Option<Limit>,
    #[serde(default)]
    pub minimum_feature_width: Option<Limit>,
    #[serde(default)]
    pub minimum_copper_clearance: Option<Limit>,
    #[serde(default)]
    pub minimum_board_edge_clearance: Option<Limit>,
    #[serde(default)]
    pub minimum_vscore_to_copper_clearance: Option<Limit>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoldermaskCapabilities {
    #[serde(default)]
    pub minimum_web: Option<Limit>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelizationCapabilities {
    #[serde(default)]
    pub minimum_board_array_spacing: Option<Limit>,
}

/// A capability's limit: a bare length is the binding minimum, and the table
/// form adds a preferred tier the fab would rather see met.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Limit {
    Minimum(Length),
    Tiered(TieredLimit),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TieredLimit {
    pub minimum: Length,
    #[serde(default)]
    pub preferred: Option<Length>,
}

impl Limit {
    pub fn minimum(&self) -> &Length {
        match self {
            Self::Minimum(length) => length,
            Self::Tiered(tiered) => &tiered.minimum,
        }
    }

    pub fn preferred(&self) -> Option<&Length> {
        match self {
            Self::Minimum(_) => None,
            Self::Tiered(tiered) => tiered.preferred.as_ref(),
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

#[cfg(test)]
mod tests {
    use super::*;

    const MIXED_UNIT_PDK: &str = r#"
schema_version = 1

[pdk]
id = "example"
name = "Example process"
revision = "1"

[capabilities.drilling]
minimum_via_hole_diameter = "0.2 mm"
minimum_hole_to_hole_clearance = "10 mil"

[capabilities.copper]
minimum_via_annular_ring = { minimum = "100 um", preferred = "0.125 mm" }
minimum_vscore_to_copper_clearance = "15 mil"

[capabilities.panelization]
minimum_board_array_spacing = "300 mil"
"#;

    #[test]
    fn parses_mixed_units_and_tiers_into_canonical_millimeters() {
        let pdk = Pdk::parse(MIXED_UNIT_PDK).unwrap();
        let drilling = &pdk.capabilities.drilling;
        assert_eq!(
            drilling
                .minimum_via_hole_diameter
                .as_ref()
                .unwrap()
                .minimum()
                .millimeters(),
            0.2
        );
        assert!(
            (drilling
                .minimum_hole_to_hole_clearance
                .as_ref()
                .unwrap()
                .minimum()
                .millimeters()
                - 0.254)
                .abs()
                < 1e-12
        );
        let annular = pdk
            .capabilities
            .copper
            .minimum_via_annular_ring
            .as_ref()
            .unwrap();
        assert!((annular.minimum().millimeters() - 0.1).abs() < 1e-12);
        assert_eq!(annular.preferred().unwrap().millimeters(), 0.125);
        assert!(
            (pdk.capabilities
                .panelization
                .minimum_board_array_spacing
                .as_ref()
                .unwrap()
                .minimum()
                .millimeters()
                - 7.62)
                .abs()
                < 1e-12
        );
    }

    #[test]
    fn rejects_bare_numbers_and_unknown_fields() {
        assert!(Pdk::parse(&MIXED_UNIT_PDK.replace("\"0.2 mm\"", "0.2")).is_err());
        assert!(
            Pdk::parse(&MIXED_UNIT_PDK.replace("revision = \"1\"", "revision = \"1\"\ntyop = 1"))
                .is_err()
        );
        assert!(
            Pdk::parse(
                &MIXED_UNIT_PDK.replace("preferred = \"0.125 mm\"", "prefered = \"0.125 mm\"")
            )
            .is_err()
        );
    }
}
