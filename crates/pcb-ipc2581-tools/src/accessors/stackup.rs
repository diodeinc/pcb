use ipc2581::types::{Layer, LayerFunction, Side};
use serde::{Deserialize, Serialize};

use super::IpcAccessor;

/// Detailed stackup information extracted from ECAD section
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackupDetails {
    /// Stackup name
    pub name: String,
    /// Overall board thickness in mm
    pub overall_thickness_mm: Option<f64>,
    /// Number of layers in the stackup
    pub layer_count: usize,
    /// Individual layers in order
    pub layers: Vec<StackupLayerInfo>,
    /// Soldermask color (name and optional RGB)
    pub soldermask_color: Option<ColorInfo>,
    /// Silkscreen color (name and optional RGB)
    pub silkscreen_color: Option<ColorInfo>,
    /// Surface finish specification
    pub surface_finish: Option<SurfaceFinishInfo>,
    /// Copper weight shared by the top and bottom conductors, in oz/ft².
    pub outer_copper_oz: Option<f64>,
    /// Copper weight shared by the internal conductors, in oz/ft².
    pub inner_copper_oz: Option<f64>,
}

impl StackupDetails {
    pub fn outer_copper_weight(&self) -> Option<String> {
        self.outer_copper_oz.map(format_copper_weight)
    }

    pub fn inner_copper_weight(&self) -> Option<String> {
        self.inner_copper_oz.map(format_copper_weight)
    }
}

/// The copper weight every conductor on the given sides shares, if they
/// agree. A layer's side is the source's own statement of where it sits;
/// its name says nothing outside one exporter's conventions.
fn uniform_copper_oz(conductors: &[(Option<Side>, Option<f64>)], sides: &[Side]) -> Option<f64> {
    let mut thicknesses = conductors
        .iter()
        .filter(|(side, _)| side.is_some_and(|side| sides.contains(&side)))
        .map(|&(_, thickness_mm)| thickness_mm);
    let first = thicknesses.next()??;
    thicknesses
        .all(|thickness| thickness.is_some_and(|thickness| (thickness - first).abs() < 0.001))
        .then_some(first / COPPER_MM_PER_OZ)
}

/// Finished thickness of one ounce per square foot of copper.
const COPPER_MM_PER_OZ: f64 = 0.0348;

fn format_copper_weight(oz: f64) -> String {
    let standard_oz = if oz < 0.75 {
        0.5
    } else if oz < 1.25 {
        1.0
    } else if oz < 1.75 {
        1.5
    } else if oz < 2.5 {
        2.0
    } else if oz < 3.5 {
        3.0
    } else {
        4.0
    };
    format!("{:.2} oz (~{} oz)", oz, standard_oz)
}

/// Surface finish information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceFinishInfo {
    /// Finish type name (e.g., "ENIG", "OSP", "HASL")
    pub name: String,
    /// Canonical category used for downstream quoting logic.
    pub category: SurfaceFinishCategory,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SurfaceFinishCategory {
    Enig,
    LeadFreeHasl,
    Hasl,
    Other,
}

impl SurfaceFinishInfo {
    /// Get realistic RGB color for this surface finish
    pub fn rgb_color(&self) -> (u8, u8, u8) {
        let name_upper = self.name.to_uppercase();

        // Use prefix matching to handle finish type variants
        if name_upper.starts_with("ENEPIG") {
            (218, 186, 85) // Slightly lighter gold
        } else if name_upper.starts_with("ENIG")
            || name_upper.starts_with("IMMERSION GOLD")
            || name_upper.starts_with("GOLD")
        {
            (212, 175, 55) // Metallic gold
        } else if name_upper.starts_with("OSP") {
            (205, 127, 50) // Dull copper/bronze
        } else if name_upper.starts_with("HASL") || name_upper.starts_with("TIN-LEAD") {
            (220, 220, 220) // Light gray/tin
        } else if name_upper.starts_with("IMMERSION SILVER") {
            (230, 232, 230) // Bright silver
        } else if name_upper.starts_with("IMMERSION TIN") {
            (200, 200, 200) // Medium gray
        } else if name_upper.starts_with("BARE COPPER") {
            (184, 115, 51) // Copper brown
        } else if name_upper.starts_with("CARBON") {
            (32, 32, 32) // Dark gray/black
        } else {
            (128, 128, 128) // Gray for unknown
        }
    }

    /// Get hex color string for HTML
    pub fn hex_color(&self) -> String {
        let (r, g, b) = self.rgb_color();
        format!("#{:02X}{:02X}{:02X}", r, g, b)
    }
}

/// Color information from specs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorInfo {
    /// Color name (e.g., "GREEN", "WHITE", "BLACK")
    pub name: Option<String>,
    /// RGB values (0-255)
    pub rgb: Option<(u8, u8, u8)>,
}

impl ColorInfo {
    /// Get RGB values, either from stored RGB or by mapping the color name
    pub fn rgb_color(&self) -> Option<(u8, u8, u8)> {
        self.rgb.or_else(|| {
            self.name
                .as_ref()
                .and_then(|name| match name.to_lowercase().as_str() {
                    "black" => Some((0x00, 0x00, 0x00)),
                    "white" => Some((0xFF, 0xFF, 0xFF)),
                    "green" => Some((0x00, 0x64, 0x00)),
                    "red" => Some((0x8B, 0x00, 0x00)),
                    "blue" => Some((0x00, 0x00, 0x8B)),
                    "yellow" => Some((0xFF, 0xD7, 0x00)),
                    "brown" => Some((0x8B, 0x45, 0x13)),
                    "orange" => Some((0xFF, 0x8C, 0x00)),
                    "pink" => Some((0xFF, 0xC0, 0xCB)),
                    "purple" | "magenta" => Some((0x80, 0x00, 0x80)),
                    "gray" | "grey" => Some((0x80, 0x80, 0x80)),
                    _ => None,
                })
        })
    }

    /// Get hex color string (e.g., "#FF0000")
    pub fn hex_color(&self) -> Option<String> {
        self.rgb_color()
            .map(|(r, g, b)| format!("#{:02X}{:02X}{:02X}", r, g, b))
    }
}

/// Simplified layer type for stackup display
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StackupLayerType {
    Conductor,
    DielectricCore,
    DielectricPrepreg,
    DielectricOther,
    Soldermask,
    Other,
}

impl StackupLayerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Conductor => "Conductor",
            Self::DielectricCore => "Dielectric (Core)",
            Self::DielectricPrepreg => "Dielectric (Prepreg)",
            Self::DielectricOther => "Dielectric",
            Self::Soldermask => "Soldermask",
            Self::Other => "Other",
        }
    }

    pub fn is_dielectric(&self) -> bool {
        matches!(
            self,
            Self::DielectricCore | Self::DielectricPrepreg | Self::DielectricOther
        )
    }
}

/// Individual layer information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackupLayerInfo {
    /// Layer name
    pub name: String,
    /// Layer type (Conductor, Dielectric, Soldermask, etc.)
    pub layer_type: StackupLayerType,
    /// Thickness in mm
    pub thickness_mm: Option<f64>,
    /// Material name
    pub material: Option<String>,
    /// Dielectric constant (Dk)
    pub dielectric_constant: Option<f64>,
    /// Loss tangent
    pub loss_tangent: Option<f64>,
    /// Layer number in stackup
    pub layer_number: Option<u32>,
}

impl<'a> IpcAccessor<'a> {
    /// Extract detailed stackup information
    pub fn stackup_details(&self) -> Option<StackupDetails> {
        let ecad = self.ecad()?;
        let stackup = ecad.cad_data.stackups.first()?;

        let layer_map: std::collections::HashMap<_, _> = ecad
            .cad_data
            .layers
            .iter()
            .map(|layer| (layer.name, layer))
            .collect();

        // Build a map of spec names to specs for material properties
        let spec_map: std::collections::HashMap<_, _> = ecad
            .cad_header
            .specs
            .iter()
            .map(|(name, spec)| (self.ipc.resolve(*name).to_string(), spec))
            .collect();

        // The board's inks are its first mask's and its first legend's.
        let inks = self.stackup_inks();
        let ink = |is: fn(LayerFunction) -> bool| {
            let first = inks.iter().find(|(layer, _)| is(layer.layer_function));
            first.map(|(_, ink)| ink.clone())
        };
        let soldermask_color = ink(|function| function == LayerFunction::Soldermask);
        let silkscreen_color =
            ink(|function| matches!(function, LayerFunction::Silkscreen | LayerFunction::Legend));

        let mut layers = Vec::new();
        let mut conductors = Vec::new();

        let mut stackup_layers = stackup.layers.iter().collect::<Vec<_>>();
        let unique_layer_numbers = stackup_layers
            .iter()
            .filter_map(|layer| layer.layer_number)
            .collect::<std::collections::HashSet<_>>();
        // Sequence defines physical order only when every layer has a unique
        // number. Otherwise retain declaration order rather than invent one.
        if unique_layer_numbers.len() == stackup_layers.len() {
            stackup_layers.sort_by_key(|layer| layer.layer_number);
        }

        for (idx, stackup_layer) in stackup_layers.into_iter().enumerate() {
            let layer_name = self.ipc.resolve(stackup_layer.layer_ref).to_string();
            let layer = layer_map.get(&stackup_layer.layer_ref).copied();
            let layer_function = layer.map(|layer| layer.layer_function);

            // Determine layer type from layer function
            let layer_type = match layer_function {
                Some(function) if crate::layers::is_copper(function) => StackupLayerType::Conductor,
                Some(LayerFunction::Soldermask) => StackupLayerType::Soldermask,
                Some(LayerFunction::DielCore) => StackupLayerType::DielectricCore,
                Some(LayerFunction::DielPreg) => StackupLayerType::DielectricPrepreg,
                Some(LayerFunction::DielBase)
                | Some(LayerFunction::DielAdhv)
                | Some(LayerFunction::DielBondPly)
                | Some(LayerFunction::DielCoverlay) => StackupLayerType::DielectricOther,
                _ => StackupLayerType::Other,
            };
            if layer_type == StackupLayerType::Conductor {
                conductors.push((layer.and_then(|layer| layer.side), stackup_layer.thickness));
            }

            // Get material properties from spec if available
            let (material, spec_dk, spec_loss_tan) = if let Some(spec_ref) = &stackup_layer.spec_ref
            {
                let spec_name = self.ipc.resolve(*spec_ref).to_string();
                if let Some(spec) = spec_map.get(&spec_name) {
                    let material = spec.material.map(|m| self.ipc.resolve(m).to_string());
                    (material, spec.dielectric_constant, spec.loss_tangent)
                } else {
                    (None, None, None)
                }
            } else {
                (None, None, None)
            };

            // Prefer stackup_layer material over spec material
            let final_material = stackup_layer
                .material
                .map(|m| self.ipc.resolve(m).to_string())
                .or(material);

            // Prefer stackup_layer properties over spec properties
            let final_dk = stackup_layer.dielectric_constant.or(spec_dk);
            let final_loss_tan = stackup_layer.loss_tangent.or(spec_loss_tan);

            layers.push(StackupLayerInfo {
                name: layer_name,
                layer_type,
                thickness_mm: stackup_layer.thickness,
                material: final_material,
                dielectric_constant: final_dk,
                loss_tangent: final_loss_tan,
                layer_number: stackup_layer.layer_number.or(Some((idx + 1) as u32)),
            });
        }

        // Extract surface finish from copper layer specs
        let surface_finish = self.extract_surface_finish(&stackup.layers, &spec_map, &layer_map);

        Some(StackupDetails {
            name: self.ipc.resolve(stackup.name).to_string(),
            overall_thickness_mm: stackup.overall_thickness,
            layer_count: layers.len(),
            layers,
            soldermask_color,
            silkscreen_color,
            surface_finish,
            outer_copper_oz: uniform_copper_oz(&conductors, &[Side::Top, Side::Bottom]),
            inner_copper_oz: uniform_copper_oz(&conductors, &[Side::Internal]),
        })
    }

    /// The ink of every stackup layer that has a spec, in stackup order: the
    /// layer and the colour its spec gives it, by term, by RGB, or in a
    /// `Color : <name>` property. Each side's mask and legend is its own
    /// layer, so each keeps its own ink.
    pub fn stackup_inks(&self) -> Vec<(&'a Layer, ColorInfo)> {
        let Some(ecad) = self.ecad() else {
            return Vec::new();
        };
        let layer = |name| ecad.cad_data.layers.iter().find(|layer| layer.name == name);
        let spec = |name: ipc2581::Symbol| {
            let specs = &ecad.cad_header.specs;
            Some(specs.iter().find(|(spec, _)| **spec == name)?.1)
        };
        ecad.cad_data
            .stackups
            .first()
            .into_iter()
            .flat_map(|stackup| &stackup.layers)
            .filter_map(|stackup_layer| {
                let spec = spec(stackup_layer.spec_ref?)?;
                let named = spec.color_term.map(|term| self.ipc.resolve(term));
                let property = || {
                    let texts = spec.properties.iter().map(|text| self.ipc.resolve(*text));
                    texts
                        .filter_map(|text| text.strip_prefix("Color : "))
                        .map(str::trim)
                        .next()
                };
                let ink = ColorInfo {
                    name: named.or_else(property).map(str::to_string),
                    rgb: spec.color_rgb,
                };
                Some((layer(stackup_layer.layer_ref)?, ink))
            })
            .collect()
    }

    /// Extract surface finish from coating layer specs (COATINGCOND/COATINGNONCOND)
    fn extract_surface_finish(
        &self,
        stackup_layers: &[ipc2581::types::StackupLayer],
        spec_map: &std::collections::HashMap<String, &ipc2581::types::Spec>,
        layer_map: &std::collections::HashMap<ipc2581::Symbol, &Layer>,
    ) -> Option<SurfaceFinishInfo> {
        // Per IPC-2581C spec section 8.1.1.16: SurfaceFinish is referenced by
        // StackupLayer elements that reference a Layer with layerFunction
        // COATINGCOND or COATINGNONCOND
        for stackup_layer in stackup_layers {
            let layer_function = layer_map
                .get(&stackup_layer.layer_ref)
                .map(|layer| layer.layer_function);

            // Only check coating layers
            if !matches!(
                layer_function,
                Some(LayerFunction::CoatingCond) | Some(LayerFunction::CoatingNonCond)
            ) {
                continue;
            }

            // Check if spec has surface finish
            if let Some(spec_ref) = &stackup_layer.spec_ref {
                let spec_name = self.ipc.resolve(*spec_ref).to_string();
                if let Some(spec) = spec_map.get(&spec_name)
                    && let Some(surface_finish) = &spec.surface_finish
                {
                    let category = classify_finish_type(surface_finish.finish_type);
                    return Some(SurfaceFinishInfo {
                        name: format_finish_type(surface_finish.finish_type),
                        category,
                    });
                }
            }
        }

        None
    }
}

fn classify_finish_type(finish_type: ipc2581::types::FinishType) -> SurfaceFinishCategory {
    use ipc2581::types::FinishType;

    match finish_type {
        FinishType::EnigN
        | FinishType::EnigG
        | FinishType::EnepigN
        | FinishType::EnepigG
        | FinishType::EnepigP
        | FinishType::Dig
        | FinishType::G
        | FinishType::GS
        | FinishType::GwbOneG
        | FinishType::GwbOneN
        | FinishType::GwbTwoG
        | FinishType::GwbTwoN => SurfaceFinishCategory::Enig,
        FinishType::S => SurfaceFinishCategory::LeadFreeHasl,
        FinishType::T | FinishType::X | FinishType::TLU => SurfaceFinishCategory::Hasl,
        FinishType::IAg
        | FinishType::ISn
        | FinishType::Osp
        | FinishType::HtOsp
        | FinishType::N
        | FinishType::NB
        | FinishType::C
        | FinishType::Other => SurfaceFinishCategory::Other,
    }
}

/// Dielectric material information extracted from stackup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialInfo {
    /// Distinct dielectric material names (e.g., ["FR4", "Rogers 4350B"])
    pub dielectric: Vec<String>,
}

/// Impedance control information extracted from stackup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpedanceControlInfo {
    /// Whether any dielectric layer has Dk specified
    pub has_dielectric_constant: bool,
    /// Whether any dielectric layer has loss tangent specified
    pub has_loss_tangent: bool,
    /// Distinct Dk values found across dielectric layers
    pub dielectric_constants: Vec<f64>,
    /// Distinct loss tangent values found across dielectric layers
    pub loss_tangents: Vec<f64>,
}

impl ImpedanceControlInfo {
    /// A design is likely impedance-controlled if Dk values are specified
    pub fn is_impedance_controlled(&self) -> bool {
        self.has_dielectric_constant
    }
}

impl<'a> IpcAccessor<'a> {
    /// Extract dielectric material names from the stackup
    pub fn material_info(&self) -> Option<MaterialInfo> {
        let stackup = self.stackup_details()?;

        let dielectric: Vec<String> = stackup
            .layers
            .iter()
            .filter(|l| l.layer_type.is_dielectric())
            .filter_map(|l| l.material.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();

        if dielectric.is_empty() {
            return None;
        }

        Some(MaterialInfo { dielectric })
    }

    /// Extract impedance control information from the stackup
    pub fn impedance_control_info(&self) -> Option<ImpedanceControlInfo> {
        let stackup = self.stackup_details()?;

        let mut dk_set = std::collections::BTreeSet::new();
        let mut df_set = std::collections::BTreeSet::new();

        for layer in &stackup.layers {
            if !layer.layer_type.is_dielectric() {
                continue;
            }
            if let Some(dk) = layer.dielectric_constant {
                // Use fixed-precision key to dedup
                dk_set.insert((dk * 1000.0) as i64);
            }
            if let Some(df) = layer.loss_tangent {
                df_set.insert((df * 100000.0) as i64);
            }
        }

        let dielectric_constants: Vec<f64> = dk_set.iter().map(|&v| v as f64 / 1000.0).collect();
        let loss_tangents: Vec<f64> = df_set.iter().map(|&v| v as f64 / 100000.0).collect();

        Some(ImpedanceControlInfo {
            has_dielectric_constant: !dielectric_constants.is_empty(),
            has_loss_tangent: !loss_tangents.is_empty(),
            dielectric_constants,
            loss_tangents,
        })
    }

    /// Extract distinct nonstandard TEXT attribute values from layer feature sets.
    pub fn nonstandard_text_attributes(&self) -> Vec<String> {
        let Some(step) = self.board_step() else {
            return Vec::new();
        };

        let mut values = Vec::new();
        let mut seen = std::collections::BTreeSet::new();

        for layer_feature in &step.layer_features {
            for set in &layer_feature.sets {
                for attr in set
                    .nonstandard_attributes
                    .slice(&layer_feature.nonstandard_attributes)
                {
                    if self.ipc.resolve(attr.name) != "TEXT" {
                        continue;
                    }

                    let Some(value) = attr.value else {
                        continue;
                    };

                    let text = self.ipc.resolve(value).trim();
                    if text.is_empty() {
                        continue;
                    }

                    if seen.insert(text.to_string()) {
                        values.push(text.to_string());
                    }
                }
            }
        }

        values
    }
}

/// Format FinishType enum to display string
/// Values per IPC-6012 Table 3-3 "Final Finish and Coating Requirements"
/// -N suffix = suitable for soldering (Nickel barrier critical)
/// -G suffix = suitable for wire bonding (Gold surface critical)
fn format_finish_type(finish_type: ipc2581::types::FinishType) -> String {
    use ipc2581::types::FinishType;
    match finish_type {
        FinishType::S => "HASL".to_string(),
        FinishType::T => "Tin-Lead".to_string(),
        FinishType::X | FinishType::TLU => "Tin-Lead Unfused".to_string(),
        FinishType::EnigN => "ENIG".to_string(),
        FinishType::EnigG => "ENIG (Wire Bond)".to_string(),
        FinishType::EnepigN => "ENEPIG".to_string(),
        FinishType::EnepigG => "ENEPIG (Wire Bond)".to_string(),
        FinishType::EnepigP => "ENEPIG (Probe)".to_string(),
        FinishType::Dig => "Immersion Gold".to_string(),
        FinishType::IAg => "Immersion Silver".to_string(),
        FinishType::ISn => "Immersion Tin".to_string(),
        FinishType::Osp => "OSP".to_string(),
        FinishType::HtOsp => "OSP (High Temp)".to_string(),
        FinishType::N => "Bare Copper".to_string(),
        FinishType::NB => "Bare Copper (No Bond)".to_string(),
        FinishType::C => "Carbon".to_string(),
        FinishType::G => "Gold".to_string(),
        FinishType::GS => "Gold/Nickel".to_string(),
        FinishType::GwbOneG | FinishType::GwbOneN => "Gold Wire Bond Type 1".to_string(),
        FinishType::GwbTwoG | FinishType::GwbTwoN => "Gold Wire Bond Type 2".to_string(),
        FinishType::Other => "Other".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stackup_details_follow_sequence_and_copper_sides() {
        let ipc = ipc2581::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="PRIMARY" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="CORE" layerFunction="DIELCORE" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="GND" layerFunction="PLANE" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="PWR" layerFunction="PLANE" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="In_SECONDARY" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
      <Stackup name="Primary" overallThickness="1.6">
        <StackupGroup name="Group">
          <StackupLayer layerOrGroupRef="In_SECONDARY" thickness="0.0696" sequence="5"/>
          <StackupLayer layerOrGroupRef="GND" thickness="0.0174" sequence="2"/>
          <StackupLayer layerOrGroupRef="CORE" thickness="1.4" sequence="3"/>
          <StackupLayer layerOrGroupRef="PWR" thickness="0.0174" sequence="4"/>
          <StackupLayer layerOrGroupRef="PRIMARY" thickness="0.0696" sequence="1"/>
        </StackupGroup>
      </Stackup>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();

        let stackup = IpcAccessor::new(&ipc).stackup_details().unwrap();

        assert_eq!(
            stackup
                .layers
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>(),
            ["PRIMARY", "GND", "CORE", "PWR", "In_SECONDARY"]
        );
        assert!((stackup.outer_copper_oz.unwrap() - 2.0).abs() < 1e-9);
        assert!((stackup.inner_copper_oz.unwrap() - 0.5).abs() < 1e-9);
        assert_eq!(stackup.outer_copper_weight().unwrap(), "2.00 oz (~2 oz)");
    }

    #[test]
    fn disagreeing_conductors_report_no_common_weight() {
        let conductors = [
            (Some(Side::Top), Some(0.035)),
            (Some(Side::Bottom), Some(0.070)),
        ];
        assert_eq!(
            uniform_copper_oz(&conductors, &[Side::Top, Side::Bottom]),
            None
        );
        assert_eq!(uniform_copper_oz(&conductors, &[Side::Internal]), None);
    }
}
