use ipc2581::types::LayerFunction;
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
}

impl StackupDetails {
    /// Calculate outer copper weight if consistent across all outer layers
    pub fn outer_copper_weight(&self) -> Option<String> {
        let outer_layers: Vec<_> = self
            .layers
            .iter()
            .filter(|l| {
                l.layer_type == StackupLayerType::Conductor
                    && (l.name.contains("F.Cu") || l.name.contains("B.Cu"))
            })
            .collect();

        outer_layers.first().and_then(|first| {
            first.thickness_mm.and_then(|thickness| {
                let all_same = outer_layers.iter().all(|l| {
                    l.thickness_mm
                        .map(|t| (t - thickness).abs() < 0.001)
                        .unwrap_or(false)
                });
                if all_same {
                    Some(Self::format_copper_weight(thickness))
                } else {
                    None
                }
            })
        })
    }

    /// Calculate inner copper weight if consistent across all inner layers
    pub fn inner_copper_weight(&self) -> Option<String> {
        let inner_layers: Vec<_> = self
            .layers
            .iter()
            .filter(|l| l.layer_type == StackupLayerType::Conductor && l.name.contains("In"))
            .collect();

        inner_layers.first().and_then(|first| {
            first.thickness_mm.and_then(|thickness| {
                let all_same = inner_layers.iter().all(|l| {
                    l.thickness_mm
                        .map(|t| (t - thickness).abs() < 0.001)
                        .unwrap_or(false)
                });
                if all_same {
                    Some(Self::format_copper_weight(thickness))
                } else {
                    None
                }
            })
        })
    }

    /// Format copper weight from thickness in mm (1 oz/ft² = 0.0348 mm)
    fn format_copper_weight(thickness_mm: f64) -> String {
        let oz = thickness_mm / 0.0348;
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
}

/// Surface finish information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceFinishInfo {
    /// Finish type name (e.g., "ENIG", "OSP", "HASL")
    pub name: String,
    /// Whether this was parsed from standard IPC-2581 location (true) or fallback (false)
    pub is_standard: bool,
}

impl SurfaceFinishInfo {
    /// Get realistic RGB color for this surface finish
    pub fn rgb_color(&self) -> (u8, u8, u8) {
        let name_upper = self.name.to_uppercase();

        // Use prefix matching to handle finish type variants
        if name_upper.starts_with("ENEPIG") {
            (218, 186, 85) // Slightly lighter gold
        } else if name_upper.starts_with("ENIG")
            || name_upper.starts_with("DIRECT IMMERSION GOLD")
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

        // Build a map of layer names to layer functions
        let layer_map: std::collections::HashMap<_, _> = ecad
            .cad_data
            .layers
            .iter()
            .map(|layer| {
                (
                    self.ipc.resolve(layer.name).to_string(),
                    layer.layer_function,
                )
            })
            .collect();

        // Build a map of spec names to specs for material properties
        let spec_map: std::collections::HashMap<_, _> = ecad
            .cad_header
            .specs
            .iter()
            .map(|(name, spec)| (self.ipc.resolve(*name).to_string(), spec))
            .collect();

        // Extract soldermask and silkscreen colors
        let mut soldermask_color = None;
        let mut silkscreen_color = None;

        for stackup_layer in &stackup.layers {
            let layer_name = self.ipc.resolve(stackup_layer.layer_ref).to_string();
            let layer_function = layer_map.get(&layer_name).copied();

            // Check if this is a soldermask or silkscreen layer
            if let Some(spec_ref) = &stackup_layer.spec_ref {
                let spec_name = self.ipc.resolve(*spec_ref).to_string();
                if let Some(spec) = spec_map.get(&spec_name) {
                    // Extract color from Spec (try multiple sources)
                    let mut color_name = spec.color_term.map(|c| self.ipc.resolve(c).to_string());
                    let color_rgb = spec.color_rgb;

                    // Also check properties for "Color : XXX" format
                    if color_name.is_none() {
                        for prop in &spec.properties {
                            let prop_text = self.ipc.resolve(*prop);
                            if let Some(stripped) = prop_text.strip_prefix("Color : ") {
                                color_name = Some(stripped.trim().to_string());
                                break;
                            }
                        }
                    }

                    let color_info = ColorInfo {
                        name: color_name,
                        rgb: color_rgb,
                    };

                    match layer_function {
                        Some(LayerFunction::Soldermask) if soldermask_color.is_none() => {
                            soldermask_color = Some(color_info);
                        }
                        Some(LayerFunction::Silkscreen) | Some(LayerFunction::Legend)
                            if silkscreen_color.is_none() =>
                        {
                            silkscreen_color = Some(color_info);
                        }
                        _ => {}
                    }
                }
            }
        }

        let mut layers = Vec::new();

        for (idx, stackup_layer) in stackup.layers.iter().enumerate() {
            let layer_name = self.ipc.resolve(stackup_layer.layer_ref).to_string();
            let layer_function = layer_map.get(&layer_name).copied();

            // Determine layer type from layer function
            let layer_type = match layer_function {
                Some(LayerFunction::Conductor)
                | Some(LayerFunction::Signal)
                | Some(LayerFunction::Plane)
                | Some(LayerFunction::Mixed)
                | Some(LayerFunction::CondFilm)
                | Some(LayerFunction::CondFoil) => StackupLayerType::Conductor,
                Some(LayerFunction::Soldermask) => StackupLayerType::Soldermask,
                Some(LayerFunction::DielCore) => StackupLayerType::DielectricCore,
                Some(LayerFunction::DielPreg) => StackupLayerType::DielectricPrepreg,
                Some(LayerFunction::DielBase)
                | Some(LayerFunction::DielAdhv)
                | Some(LayerFunction::DielBondPly)
                | Some(LayerFunction::DielCoverlay) => StackupLayerType::DielectricOther,
                _ => StackupLayerType::Other,
            };

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
        })
    }

    /// Extract surface finish from copper layer specs, with fallback to text elements
    fn extract_surface_finish(
        &self,
        stackup_layers: &[ipc2581::types::StackupLayer],
        spec_map: &std::collections::HashMap<String, &ipc2581::types::Spec>,
        layer_map: &std::collections::HashMap<String, LayerFunction>,
    ) -> Option<SurfaceFinishInfo> {
        // First, try to extract from standard IPC-2581 Spec elements
        for stackup_layer in stackup_layers {
            let layer_name = self.ipc.resolve(stackup_layer.layer_ref).to_string();
            let layer_function = layer_map.get(&layer_name).copied();

            // Only check conductor layers
            if !matches!(
                layer_function,
                Some(LayerFunction::Conductor)
                    | Some(LayerFunction::Signal)
                    | Some(LayerFunction::Plane)
                    | Some(LayerFunction::Mixed)
                    | Some(LayerFunction::CondFilm)
                    | Some(LayerFunction::CondFoil)
            ) {
                continue;
            }

            // Check if spec has surface finish
            if let Some(spec_ref) = &stackup_layer.spec_ref {
                let spec_name = self.ipc.resolve(*spec_ref).to_string();
                if let Some(spec) = spec_map.get(&spec_name) {
                    if let Some(surface_finish) = &spec.surface_finish {
                        return Some(SurfaceFinishInfo {
                            name: format_finish_type(surface_finish.finish_type),
                            is_standard: true,
                        });
                    }
                }
            }
        }

        // Fallback: Look for finish in NonstandardAttribute elements (KiCad non-standard export)
        // KiCad puts surface finish as NonstandardAttribute name="TEXT" value="ENIG"
        let ecad = self.ecad()?;
        for step in &ecad.cad_data.steps {
            for layer_feature in &step.layer_features {
                let layer_name = self.ipc.resolve(layer_feature.layer_ref);
                // Only check fab/documentation layers where KiCad puts text
                if !layer_name.contains("Fab")
                    && !layer_name.contains("Dwgs")
                    && !layer_name.contains("User")
                {
                    continue;
                }

                for feature_set in &layer_feature.sets {
                    for attr in &feature_set.nonstandard_attributes {
                        // Check if this is a TEXT attribute with a finish value
                        if self.ipc.resolve(attr.name) == "TEXT" {
                            if let Some(value_sym) = attr.value {
                                let text = self.ipc.resolve(value_sym);
                                if let Some(finish_name) = match_surface_finish(text) {
                                    return Some(SurfaceFinishInfo {
                                        name: finish_name,
                                        is_standard: false,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        None
    }
}

/// Format FinishType enum to display string
fn format_finish_type(finish_type: ipc2581::types::FinishType) -> String {
    use ipc2581::types::FinishType;
    match finish_type {
        FinishType::S => "HASL".to_string(),
        FinishType::T => "Tin-Lead".to_string(),
        FinishType::X | FinishType::TLU => "Tin-Lead Unfused".to_string(),
        FinishType::EnigN => "ENIG".to_string(),
        FinishType::EnigG => "ENIG (High Current)".to_string(),
        FinishType::EnepigN => "ENEPIG".to_string(),
        FinishType::EnepigG => "ENEPIG (High Current)".to_string(),
        FinishType::EnepigP => "ENEPIG (Probe)".to_string(),
        FinishType::Dig => "Direct Immersion Gold".to_string(),
        FinishType::IAg => "Immersion Silver".to_string(),
        FinishType::ISn => "Immersion Tin".to_string(),
        FinishType::Osp => "OSP".to_string(),
        FinishType::HtOsp => "OSP (High Temp)".to_string(),
        FinishType::N => "Bare Copper".to_string(),
        FinishType::NB => "Bare Copper (No Bondability)".to_string(),
        FinishType::C => "Carbon Contact".to_string(),
        FinishType::G => "Gold (Wire Bond)".to_string(),
        FinishType::GS => "Gold over Electroless Nickel".to_string(),
        FinishType::GwbOneG => "Gold Wire Bond Type 1, Grade G".to_string(),
        FinishType::GwbOneN => "Gold Wire Bond Type 1, Grade N".to_string(),
        FinishType::GwbTwoG => "Gold Wire Bond Type 2, Grade G".to_string(),
        FinishType::GwbTwoN => "Gold Wire Bond Type 2, Grade N".to_string(),
        FinishType::Other => "Other".to_string(),
    }
}

/// Match text value to known surface finish type (case-insensitive)
fn match_surface_finish(text: &str) -> Option<String> {
    let text_upper = text.trim().to_uppercase();
    match text_upper.as_str() {
        "ENIG" => Some("ENIG".to_string()),
        "ENEPIG" => Some("ENEPIG".to_string()),
        "OSP" => Some("OSP".to_string()),
        "HASL" => Some("HASL".to_string()),
        "LEAD FREE HASL" | "LF HASL" | "LFHASL" => Some("HASL (Lead-Free)".to_string()),
        "IMMERSION SILVER" | "IAG" => Some("Immersion Silver".to_string()),
        "IMMERSION TIN" | "ISN" => Some("Immersion Tin".to_string()),
        "IMMERSION GOLD" | "DIG" => Some("Direct Immersion Gold".to_string()),
        "BARE COPPER" => Some("Bare Copper".to_string()),
        _ => None,
    }
}
