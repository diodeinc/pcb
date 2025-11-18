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
}

/// Color information from specs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorInfo {
    /// Color name (e.g., "GREEN", "WHITE", "BLACK")
    pub name: Option<String>,
    /// RGB values (0-255)
    pub rgb: Option<(u8, u8, u8)>,
}

/// Individual layer information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackupLayerInfo {
    /// Layer name
    pub name: String,
    /// Layer type (Conductor, Dielectric, Soldermask, etc.)
    pub layer_type: String,
    /// Specific dielectric type (Core, Prepreg) for dielectric layers
    pub dielectric_type: Option<String>,
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
            let (layer_type, dielectric_type) = match layer_function {
                Some(LayerFunction::Conductor)
                | Some(LayerFunction::Signal)
                | Some(LayerFunction::Plane)
                | Some(LayerFunction::Mixed)
                | Some(LayerFunction::CondFilm)
                | Some(LayerFunction::CondFoil) => ("Conductor".to_string(), None),
                Some(LayerFunction::Soldermask) => ("Soldermask".to_string(), None),
                Some(LayerFunction::DielCore) => {
                    ("Dielectric".to_string(), Some("Core".to_string()))
                }
                Some(LayerFunction::DielPreg) => {
                    ("Dielectric".to_string(), Some("Prepreg".to_string()))
                }
                Some(LayerFunction::DielBase)
                | Some(LayerFunction::DielAdhv)
                | Some(LayerFunction::DielBondPly)
                | Some(LayerFunction::DielCoverlay) => ("Dielectric".to_string(), None),
                _ => ("Other".to_string(), None),
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
                dielectric_type,
                thickness_mm: stackup_layer.thickness,
                material: final_material,
                dielectric_constant: final_dk,
                loss_tangent: final_loss_tan,
                layer_number: stackup_layer.layer_number.or(Some((idx + 1) as u32)),
            });
        }

        Some(StackupDetails {
            name: self.ipc.resolve(stackup.name).to_string(),
            overall_thickness_mm: stackup.overall_thickness,
            layer_count: layers.len(),
            layers,
            soldermask_color,
            silkscreen_color,
        })
    }
}
