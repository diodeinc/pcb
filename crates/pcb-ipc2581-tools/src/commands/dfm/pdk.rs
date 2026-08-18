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
    pub panelization: PanelizationCapabilities,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DrillingCapabilities {
    #[serde(default)]
    pub minimum_via_hole_diameter: Option<Length>,
    #[serde(default)]
    pub minimum_pth_hole_diameter: Option<Length>,
    #[serde(default)]
    pub minimum_npth_hole_diameter: Option<Length>,
    #[serde(default)]
    pub minimum_hole_to_hole_clearance: Option<Length>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CopperCapabilities {
    #[serde(default)]
    pub minimum_via_annular_ring: Option<Length>,
    #[serde(default)]
    pub minimum_pth_annular_ring: Option<Length>,
    #[serde(default)]
    pub minimum_board_edge_clearance: Option<Length>,
    #[serde(default)]
    pub minimum_vscore_to_copper_clearance: Option<Length>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelizationCapabilities {
    #[serde(default)]
    pub minimum_board_array_spacing: Option<Length>,
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
            "mil" | "mils" => number * 0.0254,
            _ => return Err(Self::expected(value)),
        };
        Ok(Self {
            original: value.trim().to_owned(),
            millimeters,
        })
    }

    fn expected(value: &str) -> String {
        format!("length '{value}' must be a positive '<number> mm' or '<number> mil' value")
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
minimum_via_annular_ring = "0.125 mm"
minimum_vscore_to_copper_clearance = "15 mil"

[capabilities.panelization]
minimum_board_array_spacing = "300 mil"
"#;

    #[test]
    fn parses_mixed_units_into_canonical_millimeters() {
        let pdk = Pdk::parse(MIXED_UNIT_PDK).unwrap();
        let drilling = &pdk.capabilities.drilling;
        assert_eq!(
            drilling
                .minimum_via_hole_diameter
                .as_ref()
                .unwrap()
                .millimeters(),
            0.2
        );
        assert!(
            (drilling
                .minimum_hole_to_hole_clearance
                .as_ref()
                .unwrap()
                .millimeters()
                - 0.254)
                .abs()
                < 1e-12
        );
        assert!(
            (pdk.capabilities
                .panelization
                .minimum_board_array_spacing
                .as_ref()
                .unwrap()
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
    }
}
