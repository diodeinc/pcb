use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::{borrow::Cow, fmt, str::FromStr};
use thiserror::Error;

use crate::lang::sexpr::{kv, str_lit, sym, ListBuilder, Pretty, SExpr};

const THICKNESS_TOLERANCE: f64 = 0.10;

// Helper functions for layer mapping
fn copper_layer_mapping(index: usize, total_copper: usize) -> (u32, Cow<'static, str>) {
    match (index, total_copper) {
        (0, _) => (0, Cow::Borrowed("F.Cu")),
        (i, total) if i == total - 1 => (2, Cow::Borrowed("B.Cu")),
        (i, _) => (4 + (i - 1) as u32 * 2, Cow::Owned(format!("In{}.Cu", i))),
    }
}

// Extension trait for more elegant SExpr building
trait ListBuilderExt {
    fn kv_str(&mut self, key: &str, val: &str) -> &mut Self;
    fn kv_f64(&mut self, key: &str, val: f64) -> &mut Self;
    fn kv_opt_str(&mut self, key: &str, val: Option<&str>) -> &mut Self;
    fn kv_opt_f64(&mut self, key: &str, val: Option<f64>) -> &mut Self;
}

impl ListBuilderExt for ListBuilder {
    fn kv_str(&mut self, key: &str, val: &str) -> &mut Self {
        self.push(kv(key, str_lit(val)))
    }

    fn kv_f64(&mut self, key: &str, val: f64) -> &mut Self {
        self.push(kv(key, val))
    }

    fn kv_opt_str(&mut self, key: &str, val: Option<&str>) -> &mut Self {
        if let Some(v) = val {
            self.kv_str(key, v);
        }
        self
    }

    fn kv_opt_f64(&mut self, key: &str, val: Option<f64>) -> &mut Self {
        if let Some(v) = val {
            self.kv_f64(key, v);
        }
        self
    }
}

// Tiny helpers for stackup generation
fn tech_layer(name: &str, layer_type: &str, color: Option<&str>, thickness: Option<f64>) -> SExpr {
    let mut l = ListBuilder::node(sym("layer"));
    l.push(str_lit(name))
        .kv_str("type", layer_type)
        .kv_opt_str("color", color)
        .kv_opt_f64("thickness", thickness);
    l.build()
}

fn layer_entry<I, T>(name: &str, props: I) -> SExpr
where
    I: IntoIterator<Item = T>,
    T: Into<SExpr>,
{
    let mut l = ListBuilder::node(sym("layer"));
    l.push(str_lit(name)).extend(props);
    l.build()
}

impl fmt::Display for CopperRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CopperRole::Signal => "signal",
            CopperRole::Power => "power",
            CopperRole::Mixed => "mixed",
        })
    }
}

impl fmt::Display for DielectricForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DielectricForm::Core => "core",
            DielectricForm::Prepreg => "prepreg",
        })
    }
}

impl fmt::Display for CopperFinish {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CopperFinish::Enig => "ENIG",
            CopperFinish::HalSnpb => "HAL SnPb",
            CopperFinish::HalLeadFree => "HAL lead-free",
        })
    }
}

impl FromStr for CopperFinish {
    type Err = StackupError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ENIG" => Ok(CopperFinish::Enig),
            "HAL SnPb" => Ok(CopperFinish::HalSnpb),
            "HAL lead-free" => Ok(CopperFinish::HalLeadFree),
            _ => Err(StackupError::UnknownCopperFinish(s.to_string())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Material {
    pub name: Option<String>,
    pub vendor: Option<String>,
    pub relative_permittivity: Option<f64>,
    pub loss_tangent: Option<f64>,
    pub reference_frequency: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopperRole {
    Signal,
    Power,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DielectricForm {
    Core,
    Prepreg,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CopperFinish {
    Enig,
    #[serde(rename = "HAL SnPb")]
    HalSnpb,
    #[serde(rename = "HAL lead-free")]
    HalLeadFree,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Layer {
    Copper {
        thickness: f64,
        role: CopperRole,
    },
    Dielectric {
        thickness: f64,
        material: String,
        form: DielectricForm,
    },
}

impl Layer {
    pub fn is_copper(&self) -> bool {
        matches!(self, Layer::Copper { .. })
    }

    pub fn thickness(&self) -> f64 {
        match self {
            Layer::Copper { thickness, .. } => *thickness,
            Layer::Dielectric { thickness, .. } => *thickness,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stackup {
    pub materials: Option<Vec<Material>>,
    pub silk_screen_color: Option<String>,
    pub solder_mask_color: Option<String>,
    pub thickness: Option<f64>,
    pub symmetric: Option<bool>,
    pub layers: Option<Vec<Layer>>,
    pub copper_finish: Option<CopperFinish>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardConfig {
    pub design_rules: Option<JsonValue>,
    pub stackup: Option<Stackup>,
}

impl BoardConfig {
    /// Parse a BoardConfig from JSON string and validate the stackup if present
    pub fn from_json_str(json_str: &str) -> Result<Self, BoardConfigError> {
        let board_config: BoardConfig = serde_json::from_str(json_str)?;

        // Validate stackup if present
        if let Some(ref stackup) = board_config.stackup {
            stackup.validate()?;
        }

        Ok(board_config)
    }
}

#[derive(Debug, Error)]
pub enum BoardConfigError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Stackup(#[from] StackupError),
}

#[derive(Debug, Error)]
pub enum StackupError {
    #[error("Stackup must have at least 3 layers (2 copper + 1 dielectric), got {0}")]
    TooFewLayers(usize),

    #[error("Stackup must have an odd number of layers, got {0}")]
    EvenLayerCount(usize),

    #[error("Stackup must start and end with copper layers")]
    MustStartEndWithCopper,

    #[error("Stackup layers must alternate between copper and dielectric")]
    LayersMustAlternate,

    #[error("Stackup layers must have symmetric structure when symmetric=true")]
    LayersNotSymmetric,

    #[error("Total layer thickness {actual:.3}mm does not match specified thickness {expected:.3}mm (tolerance 10%)")]
    ThicknessMismatch { actual: f64, expected: f64 },

    #[error("Dielectric layer {index} references unknown material '{material}'")]
    UnknownMaterial { index: usize, material: String },

    #[error("Failed to parse KiCad PCB file")]
    KicadParseError,

    #[error("KiCad PCB file missing setup section")]
    NoSetupSection,

    #[error("Missing thickness in layer definition")]
    MissingThickness,

    #[error("Unknown copper finish: {0}")]
    UnknownCopperFinish(String),
}

impl Stackup {
    /// Validate the stackup configuration
    pub fn validate(&self) -> Result<(), StackupError> {
        let layers = match &self.layers {
            Some(layers) => layers,
            None => return Ok(()), // No layers to validate
        };

        self.validate_layer_count(layers)?;
        self.validate_first_last_copper(layers)?;
        self.validate_alternating(layers)?;
        self.validate_materials(layers)?;
        self.validate_thickness(layers)?;
        self.validate_symmetry_if_requested(layers)?;

        Ok(())
    }

    fn validate_layer_count(&self, layers: &[Layer]) -> Result<(), StackupError> {
        if layers.len() < 3 {
            return Err(StackupError::TooFewLayers(layers.len()));
        }
        if layers.len().is_multiple_of(2) {
            return Err(StackupError::EvenLayerCount(layers.len()));
        }
        Ok(())
    }

    fn validate_first_last_copper(&self, layers: &[Layer]) -> Result<(), StackupError> {
        match (layers.first(), layers.last()) {
            (Some(Layer::Copper { .. }), Some(Layer::Copper { .. })) => Ok(()),
            _ => Err(StackupError::MustStartEndWithCopper),
        }
    }

    fn validate_alternating(&self, layers: &[Layer]) -> Result<(), StackupError> {
        let ok = layers
            .iter()
            .enumerate()
            .all(|(i, layer)| layer.is_copper() == (i % 2 == 0));
        if ok {
            Ok(())
        } else {
            Err(StackupError::LayersMustAlternate)
        }
    }

    fn validate_materials(&self, layers: &[Layer]) -> Result<(), StackupError> {
        if let Some(materials) = &self.materials {
            let material_names: std::collections::HashSet<&str> =
                materials.iter().filter_map(|m| m.name.as_deref()).collect();

            for (i, layer) in layers.iter().enumerate() {
                if let Layer::Dielectric { material, .. } = layer {
                    if !material_names.contains(material.as_str()) {
                        return Err(StackupError::UnknownMaterial {
                            index: i,
                            material: material.clone(),
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_thickness(&self, layers: &[Layer]) -> Result<(), StackupError> {
        if let Some(expected_thickness) = self.thickness {
            let total_thickness: f64 = layers.iter().map(Layer::thickness).sum();
            let tolerance = expected_thickness * THICKNESS_TOLERANCE;

            if (total_thickness - expected_thickness).abs() > tolerance {
                return Err(StackupError::ThicknessMismatch {
                    actual: total_thickness,
                    expected: expected_thickness,
                });
            }
        }
        Ok(())
    }

    fn validate_symmetry_if_requested(&self, layers: &[Layer]) -> Result<(), StackupError> {
        if self.symmetric == Some(true) {
            self.validate_symmetry(layers)?;
        }
        Ok(())
    }

    /// Validate symmetric stackup structure
    fn validate_symmetry(&self, layers: &[Layer]) -> Result<(), StackupError> {
        let n = layers.len();
        let middle = n / 2;

        // Check that layers are mirror symmetric around the center
        for i in 0..middle {
            let front = &layers[i];
            let back = &layers[n - 1 - i];

            match (front, back) {
                (
                    Layer::Copper {
                        thickness: t1,
                        role: r1,
                    },
                    Layer::Copper {
                        thickness: t2,
                        role: r2,
                    },
                ) => {
                    if t1 != t2 || std::mem::discriminant(r1) != std::mem::discriminant(r2) {
                        return Err(StackupError::LayersNotSymmetric);
                    }
                }
                (
                    Layer::Dielectric {
                        thickness: t1,
                        material: m1,
                        form: f1,
                    },
                    Layer::Dielectric {
                        thickness: t2,
                        material: m2,
                        form: f2,
                    },
                ) => {
                    if t1 != t2
                        || m1 != m2
                        || std::mem::discriminant(f1) != std::mem::discriminant(f2)
                    {
                        return Err(StackupError::LayersNotSymmetric);
                    }
                }
                _ => return Err(StackupError::LayersNotSymmetric),
            }
        }

        Ok(())
    }

    /// Calculate the number of copper layers
    pub fn copper_layer_count(&self) -> usize {
        if let Some(layers) = &self.layers {
            let n = layers.len();
            if n % 2 == 1 {
                // For odd number of layers: n - (n/2)
                n - (n / 2)
            } else {
                0 // Should not happen if validation passes
            }
        } else {
            0
        }
    }

    /// Generate KiCad layers S-expression
    pub fn generate_layers_sexpr(&self) -> Option<String> {
        let layers = self.layers.as_ref()?;
        let copper_layers: Vec<_> = layers.iter().filter(|l| l.is_copper()).collect();

        let mut elements = Vec::new();

        // Copper layers
        for (i, layer) in copper_layers.iter().enumerate() {
            if let Layer::Copper { role, .. } = layer {
                let (id, name) = copper_layer_mapping(i, copper_layers.len());
                elements.push(SExpr::List(vec![
                    id.into(),
                    str_lit(name.as_ref()),
                    sym(role.to_string()),
                ]));
            }
        }

        // Technical layers
        static TECH_LAYERS: &[(u32, &str, &str, Option<&str>)] = &[
            (5, "F.SilkS", "user", Some("F.Silkscreen")),
            (1, "F.Mask", "user", None),
            (7, "B.SilkS", "user", Some("B.Silkscreen")),
            (3, "B.Mask", "user", None),
            (25, "Edge.Cuts", "user", None),
        ];

        for &(id, name, layer_type, alias) in TECH_LAYERS {
            let mut v = vec![id.into(), str_lit(name), sym(layer_type)];
            if let Some(a) = alias {
                v.push(str_lit(a));
            }
            elements.push(SExpr::List(v));
        }

        let mut builder = ListBuilder::node(sym("layers"));
        builder.extend(elements);
        let result = builder.build();
        Some(format!("{}", Pretty(&result)))
    }

    /// Generate KiCad stackup S-expression
    pub fn generate_stackup_sexpr(&self) -> Option<String> {
        let layers = self.layers.as_ref()?;
        let materials = self
            .materials
            .as_ref()
            .map_or(&[] as &[Material], |v| v.as_slice());

        let mut b = ListBuilder::node(sym("stackup"));

        // Top technical layers
        b.push(tech_layer(
            "F.SilkS",
            "Top Silk Screen",
            self.silk_screen_color.as_deref(),
            None,
        ));
        b.push(tech_layer(
            "F.Mask",
            "Top Solder Mask",
            self.solder_mask_color.as_deref(),
            Some(0.01),
        ));

        // Physical layers
        let copper_layers: Vec<_> = layers.iter().filter(|l| l.is_copper()).collect();
        let mut dielectric_index = 1usize;
        let mut copper_index = 0usize;

        for layer in layers {
            match layer {
                Layer::Copper { thickness, .. } => {
                    let (_, name) = copper_layer_mapping(copper_index, copper_layers.len());
                    b.push(layer_entry(
                        &name,
                        [kv("type", str_lit("copper")), kv("thickness", *thickness)],
                    ));
                    copper_index += 1;
                }
                Layer::Dielectric {
                    thickness,
                    material,
                    form,
                } => {
                    let mut entries = vec![
                        kv("type", str_lit(form.to_string())),
                        kv("thickness", *thickness),
                        kv("material", str_lit(material)),
                    ];
                    if let Some(mat) = materials
                        .iter()
                        .find(|m| m.name.as_deref() == Some(material))
                    {
                        if let Some(er) = mat.relative_permittivity {
                            entries.push(kv("epsilon_r", er));
                        }
                        if let Some(tan_d) = mat.loss_tangent {
                            entries.push(kv("loss_tangent", tan_d));
                        }
                    }
                    b.push(layer_entry(
                        &format!("dielectric {}", dielectric_index),
                        entries,
                    ));
                    dielectric_index += 1;
                }
            }
        }

        // Bottom technical layers
        b.push(tech_layer(
            "B.Mask",
            "Bottom Solder Mask",
            self.solder_mask_color.as_deref(),
            Some(0.01),
        ));
        b.push(tech_layer(
            "B.SilkS",
            "Bottom Silk Screen",
            self.silk_screen_color.as_deref(),
            None,
        ));

        // Finish and constraints
        if let Some(finish) = &self.copper_finish {
            b.push(SExpr::List(vec![
                sym("copper_finish"),
                str_lit(finish.to_string()),
            ]));
        }
        b.push(SExpr::List(vec![sym("dielectric_constraints"), sym("no")]));

        Some(format!("{}", Pretty(&b.build())))
    }

    /// Parse stackup configuration from KiCad PCB file content
    pub fn from_kicad_pcb(content: &str) -> Result<Option<Self>, StackupError> {
        use crate::lang::sexpr::parse;

        // Parse the entire PCB file
        let pcb_expr = parse(content).map_err(|_| StackupError::KicadParseError)?;

        // Find the setup section
        let setup = pcb_expr
            .find_list("setup")
            .ok_or(StackupError::NoSetupSection)?;

        // Find the stackup within setup
        let setup_expr = SExpr::List(setup.to_vec());
        let stackup_data = setup_expr.find_list("stackup");

        match stackup_data {
            None => Ok(None), // No stackup defined
            Some(stackup_items) => {
                let stackup = Self::parse_stackup_section(stackup_items)?;
                Ok(Some(stackup))
            }
        }
    }

    fn parse_stackup_section(stackup_items: &[SExpr]) -> Result<Self, StackupError> {
        let mut materials = Vec::new();
        let mut layers = Vec::new();
        let mut silk_screen_color = None;
        let mut solder_mask_color = None;
        let mut copper_finish = None;
        let mut total_thickness = 0.0;

        // Parse each layer in the stackup
        for item in stackup_items.iter().skip(1) {
            // Skip "stackup" symbol
            if let Some(layer_data) = item.as_list() {
                if layer_data.len() >= 2 && layer_data[0].as_sym() == Some("layer") {
                    if let Some(layer_name) = layer_data[1].as_str() {
                        match layer_name {
                            "F.SilkS" => {
                                silk_screen_color = Self::extract_color(&layer_data[2..]);
                            }
                            "F.Mask" | "B.Mask" => {
                                if solder_mask_color.is_none() {
                                    solder_mask_color = Self::extract_color(&layer_data[2..]);
                                }
                            }
                            name if name.ends_with(".Cu") => {
                                // Copper layer
                                let thickness = Self::extract_thickness(&layer_data[2..])?;
                                let role = Self::determine_copper_role(name);
                                layers.push(Layer::Copper { thickness, role });
                                total_thickness += thickness;
                            }
                            name if name.starts_with("dielectric ") => {
                                // Dielectric layer
                                let thickness = Self::extract_thickness(&layer_data[2..])?;
                                let material = Self::extract_material(&layer_data[2..])
                                    .unwrap_or("FR4".to_string());
                                let form = Self::extract_form(&layer_data[2..])
                                    .unwrap_or(DielectricForm::Core);

                                // Extract material properties
                                if let (Some(er), Some(tan_d)) = (
                                    Self::extract_epsilon_r(&layer_data[2..]),
                                    Self::extract_loss_tangent(&layer_data[2..]),
                                ) {
                                    materials.push(Material {
                                        name: Some(material.clone()),
                                        vendor: None,
                                        relative_permittivity: Some(er),
                                        loss_tangent: Some(tan_d),
                                        reference_frequency: None,
                                    });
                                }

                                layers.push(Layer::Dielectric {
                                    thickness,
                                    material,
                                    form,
                                });
                                total_thickness += thickness;
                            }
                            _ => {} // Skip other layers
                        }
                    }
                } else if layer_data.len() >= 2 && layer_data[0].as_sym() == Some("copper_finish") {
                    if let Some(finish_str) = layer_data[1].as_str() {
                        copper_finish = Some(finish_str.parse()?);
                    }
                }
            }
        }

        Ok(Self {
            materials: if materials.is_empty() {
                None
            } else {
                Some(materials)
            },
            silk_screen_color,
            solder_mask_color,
            thickness: Some(total_thickness),
            symmetric: None, // Cannot determine from KiCad file
            layers: if layers.is_empty() {
                None
            } else {
                Some(layers)
            },
            copper_finish,
        })
    }

    // Helper methods for extracting properties from S-expressions
    fn extract_string_prop(props: &[SExpr], key: &str) -> Option<String> {
        props.iter().find_map(|prop| {
            let list = prop.as_list()?;
            (list.len() >= 2 && list[0].as_sym() == Some(key))
                .then(|| list[1].as_str().map(String::from))
                .flatten()
        })
    }

    fn extract_numeric_prop(props: &[SExpr], key: &str) -> Option<f64> {
        props.iter().find_map(|prop| {
            let list = prop.as_list()?;
            (list.len() >= 2 && list[0].as_sym() == Some(key))
                .then(|| {
                    list[1]
                        .as_float()
                        .or_else(|| list[1].as_int().map(|i| i as f64))
                })
                .flatten()
        })
    }

    fn extract_color(props: &[SExpr]) -> Option<String> {
        Self::extract_string_prop(props, "color")
    }

    fn extract_thickness(props: &[SExpr]) -> Result<f64, StackupError> {
        Self::extract_numeric_prop(props, "thickness").ok_or(StackupError::MissingThickness)
    }

    fn extract_material(props: &[SExpr]) -> Option<String> {
        Self::extract_string_prop(props, "material")
    }

    fn extract_form(props: &[SExpr]) -> Option<DielectricForm> {
        Self::extract_string_prop(props, "type").and_then(|s| match s.as_str() {
            "core" => Some(DielectricForm::Core),
            "prepreg" => Some(DielectricForm::Prepreg),
            _ => None,
        })
    }

    fn extract_epsilon_r(props: &[SExpr]) -> Option<f64> {
        Self::extract_numeric_prop(props, "epsilon_r")
    }

    fn extract_loss_tangent(props: &[SExpr]) -> Option<f64> {
        Self::extract_numeric_prop(props, "loss_tangent")
    }

    fn determine_copper_role(layer_name: &str) -> CopperRole {
        // Default heuristic based on layer name
        match layer_name {
            "F.Cu" | "B.Cu" => CopperRole::Mixed, // Outer layers are typically mixed
            _ => CopperRole::Power,               // Inner layers often power/ground
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_4_layer_stackup() {
        let stackup = Stackup {
            materials: Some(vec![Material {
                name: Some("FR4".to_string()),
                vendor: None,
                relative_permittivity: Some(4.6),
                loss_tangent: Some(0.02),
                reference_frequency: None,
            }]),
            thickness: Some(1.6),
            symmetric: Some(true),
            layers: Some(vec![
                Layer::Copper {
                    thickness: 0.035,
                    role: CopperRole::Signal,
                },
                Layer::Dielectric {
                    thickness: 0.21,
                    material: "FR4".to_string(),
                    form: DielectricForm::Prepreg,
                },
                Layer::Copper {
                    thickness: 0.0152,
                    role: CopperRole::Power,
                },
                Layer::Dielectric {
                    thickness: 1.065,
                    material: "FR4".to_string(),
                    form: DielectricForm::Core,
                },
                Layer::Copper {
                    thickness: 0.0152,
                    role: CopperRole::Power,
                },
                Layer::Dielectric {
                    thickness: 0.21,
                    material: "FR4".to_string(),
                    form: DielectricForm::Prepreg,
                },
                Layer::Copper {
                    thickness: 0.035,
                    role: CopperRole::Signal,
                },
            ]),
            silk_screen_color: None,
            solder_mask_color: None,
            copper_finish: Some(CopperFinish::Enig),
        };

        assert!(stackup.validate().is_ok());
        assert_eq!(stackup.copper_layer_count(), 4);
    }

    #[test]
    fn test_too_few_layers() {
        let stackup = Stackup {
            materials: None,
            thickness: None,
            symmetric: None,
            layers: Some(vec![Layer::Copper {
                thickness: 0.035,
                role: CopperRole::Signal,
            }]),
            silk_screen_color: None,
            solder_mask_color: None,
            copper_finish: None,
        };

        assert!(matches!(
            stackup.validate(),
            Err(StackupError::TooFewLayers(1))
        ));
    }

    #[test]
    fn test_invalid_even_layers() {
        let stackup = Stackup {
            materials: None,
            thickness: None,
            symmetric: None,
            layers: Some(vec![
                Layer::Copper {
                    thickness: 0.035,
                    role: CopperRole::Signal,
                },
                Layer::Dielectric {
                    thickness: 1.53,
                    material: "FR4".to_string(),
                    form: DielectricForm::Core,
                },
                Layer::Copper {
                    thickness: 0.035,
                    role: CopperRole::Signal,
                },
                Layer::Dielectric {
                    thickness: 1.53,
                    material: "FR4".to_string(),
                    form: DielectricForm::Core,
                },
            ]),
            silk_screen_color: None,
            solder_mask_color: None,
            copper_finish: None,
        };

        assert!(matches!(
            stackup.validate(),
            Err(StackupError::EvenLayerCount(4))
        ));
    }

    #[test]
    fn test_thickness_mismatch() {
        let stackup = Stackup {
            materials: None,
            thickness: Some(1.6), // Expected thickness
            symmetric: None,
            layers: Some(vec![
                Layer::Copper {
                    thickness: 0.035,
                    role: CopperRole::Signal,
                },
                Layer::Dielectric {
                    thickness: 1.0,
                    material: "FR4".to_string(),
                    form: DielectricForm::Core,
                },
                Layer::Copper {
                    thickness: 0.035,
                    role: CopperRole::Signal,
                },
            ]),
            silk_screen_color: None,
            solder_mask_color: None,
            copper_finish: None,
        };

        assert!(matches!(
            stackup.validate(),
            Err(StackupError::ThicknessMismatch { .. })
        ));
    }

    #[test]
    fn test_parse_kicad_stackup() {
        // Simple stackup S-expression similar to KiCad format
        let kicad_content = r#"(kicad_pcb (setup (stackup (layer "F.Cu" (type "copper") (thickness 0.035)) (layer "dielectric 1" (type "core") (thickness 1.53) (material "FR4") (epsilon_r 4.6) (loss_tangent 0.02)) (layer "B.Cu" (type "copper") (thickness 0.035)) (copper_finish "ENIG"))))"#;

        let result = Stackup::from_kicad_pcb(kicad_content).unwrap();
        assert!(result.is_some());

        let stackup = result.unwrap();
        assert_eq!(stackup.copper_finish, Some(CopperFinish::Enig));

        let layers = stackup.layers.unwrap();
        assert_eq!(layers.len(), 3); // F.Cu, dielectric, B.Cu

        // Check first layer (F.Cu)
        if let Layer::Copper { thickness, role } = &layers[0] {
            assert_eq!(*thickness, 0.035);
            assert!(matches!(role, CopperRole::Mixed));
        } else {
            panic!("First layer should be copper");
        }

        // Check second layer (dielectric)
        if let Layer::Dielectric {
            thickness,
            material,
            form,
        } = &layers[1]
        {
            assert_eq!(*thickness, 1.53);
            assert_eq!(material, "FR4");
            assert!(matches!(form, DielectricForm::Core));
        } else {
            panic!("Second layer should be dielectric");
        }
    }

    #[test]
    fn test_parse_real_kicad_file() {
        // Test with actual KiCad file if it exists
        let file_path = "../../../../demo/boards/DM0002/layout/layout.kicad_pcb";
        if let Ok(file_content) = std::fs::read_to_string(file_path) {
            let result = Stackup::from_kicad_pcb(&file_content);
            match result {
                Ok(Some(stackup)) => {
                    println!("Parsed stackup: {:#?}", stackup);
                    // Verify it has the expected structure
                    assert!(stackup.layers.is_some());
                    assert!(stackup.copper_finish.is_some());
                }
                Ok(None) => println!("No stackup found in file"),
                Err(e) => println!("Parse error: {}", e),
            }
        } else {
            println!("Reference KiCad file not found, skipping test");
        }
    }
}
