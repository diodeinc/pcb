use ipc2581::Symbol;
use ipc2581::types::{Layer, LayerFunction, Side, Spec};
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
        self.outer_copper_oz
            .map(|oz| format_copper_weight(oz, nominal_copper_weight(oz, true)))
    }

    pub fn inner_copper_weight(&self) -> Option<String> {
        self.inner_copper_oz
            .map(|oz| format_copper_weight(oz, nominal_copper_weight(oz, false)))
    }
}

/// The copper weight every conductor on the given sides shares, if they
/// agree. A layer's side is the source's own statement of where it sits;
/// its name says nothing outside one exporter's conventions.
fn uniform_copper_oz(conductors: &[(Option<Side>, Option<f64>)], sides: &[Side]) -> Option<f64> {
    let mut weights = conductors
        .iter()
        .filter(|(side, _)| side.is_some_and(|side| sides.contains(&side)))
        .map(|&(_, ounces)| ounces);
    let first = weights.next()??;
    weights
        .all(|ounces| ounces.is_some_and(|ounces| (ounces - first).abs() < 0.03))
        .then_some(first)
}

/// The copper weight a stackup layer states, in oz/ft²: its spec's
/// `Conductor` weight, otherwise its thickness.
pub(crate) fn stated_copper_oz(spec: Option<&Spec>, thickness_mm: Option<f64>) -> Option<f64> {
    /// Finished thickness of one ounce per square foot of copper.
    const COPPER_MM_PER_OZ: f64 = 0.0348;
    spec.and_then(|spec| spec.copper_weight_oz)
        .or_else(|| thickness_mm.map(|millimeters| millimeters / COPPER_MM_PER_OZ))
}

/// The standard foil weight a stated copper weight names, in ounces, or
/// `None` when it names none.
///
/// A stackup states a thickness that only approximates the weight ordered:
/// IPC-4562A allows foil 10 % under nominal and fabrication typically
/// leaves about 88 %, so a weight reaches down to 85 % of its nominal. Above
/// it allows 0.1 oz of export rounding, and outer layers 0.45 oz more of
/// plating. Where an outer layer could be plated thinner foil, the nearest
/// nominal wins. ¾ oz is not a standard weight: fabs do not offer it.
pub(crate) fn nominal_copper_weight(ounces: f64, outer: bool) -> Option<f64> {
    const BELOW_ONE_OUNCE: [f64; 4] = [0.125, 0.25, 1.0 / 3.0, 0.5];
    const EPSILON: f64 = 1e-9;
    let plating = if outer { 0.45 } else { 0.0 };
    let whole = ounces.floor().max(1.0);
    BELOW_ONE_OUNCE
        .into_iter()
        .chain([whole, whole + 1.0])
        .filter(|nominal| {
            ounces >= nominal * 0.85 - EPSILON && ounces <= nominal + 0.1 + plating + EPSILON
        })
        .min_by(|left, right| (ounces - left).abs().total_cmp(&(ounces - right).abs()))
}

fn format_copper_weight(oz: f64, nominal: Option<f64>) -> String {
    match nominal {
        Some(nominal) => format!("{oz:.2} oz (~{} oz)", format_ounces(nominal)),
        None => format!("{oz:.2} oz"),
    }
}

/// A standard weight as written: `1`, `0.5`, `0.333`.
pub(crate) fn format_ounces(ounces: f64) -> String {
    let rounded = (ounces * 1000.0).round() / 1000.0;
    format!("{rounded}")
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

/// Colour names and the RGB values they stand for, preferred names first.
const NAMED_COLORS: [(&str, (u8, u8, u8)); 13] = [
    ("black", (0x00, 0x00, 0x00)),
    ("white", (0xFF, 0xFF, 0xFF)),
    ("green", (0x00, 0x64, 0x00)),
    ("red", (0x8B, 0x00, 0x00)),
    ("blue", (0x00, 0x00, 0x8B)),
    ("yellow", (0xFF, 0xD7, 0x00)),
    ("brown", (0x8B, 0x45, 0x13)),
    ("orange", (0xFF, 0x8C, 0x00)),
    ("pink", (0xFF, 0xC0, 0xCB)),
    ("purple", (0x80, 0x00, 0x80)),
    ("magenta", (0x80, 0x00, 0x80)),
    ("gray", (0x80, 0x80, 0x80)),
    ("grey", (0x80, 0x80, 0x80)),
];

impl ColorInfo {
    /// The colour a stackup spec states: its colour term or `Color : <name>`
    /// property, and its RGB.
    pub(crate) fn of_spec<'a>(spec: &Spec, resolve: impl Fn(Symbol) -> &'a str) -> Self {
        let named = spec.color_term.map(&resolve);
        let property = || {
            spec.properties
                .iter()
                .filter_map(|text| resolve(*text).strip_prefix("Color : "))
                .map(str::trim)
                .next()
        };
        Self {
            name: named.or_else(property).map(str::to_string),
            rgb: spec.color_rgb,
        }
    }

    /// Get RGB values: the stored RGB, a `#rrggbb[aa]` name as KiCad writes
    /// a custom colour, or the RGB a colour name stands for.
    pub fn rgb_color(&self) -> Option<(u8, u8, u8)> {
        self.rgb.or_else(|| {
            let name = self.name.as_deref()?.trim();
            if let Some(hex) = name
                .strip_prefix('#')
                .filter(|hex| matches!(hex.len(), 6 | 8))
            {
                // Every pair, alpha included, must be hex.
                if !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return None;
                }
                let channel = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
                return Some((channel(0)?, channel(2)?, channel(4)?));
            }
            NAMED_COLORS
                .iter()
                .find(|(named, _)| named.eq_ignore_ascii_case(name))
                .map(|(_, rgb)| *rgb)
        })
    }

    /// The stated colour text, without KiCad's `Not specified` placeholder.
    fn stated_name(&self) -> Option<&str> {
        self.name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case("not specified"))
    }

    /// Whether the spec states a colour at all, recognized or not.
    pub fn is_stated(&self) -> bool {
        self.stated_name().is_some() || self.rgb.is_some()
    }

    /// The colour's name, lowercase: a stated name as written, otherwise the
    /// ink colour its RGB or `#rrggbb` value falls within. None when no colour
    /// is stated or its RGB falls outside every ink colour's bounds.
    pub fn color_name(&self) -> Option<String> {
        match self.stated_name() {
            Some(name) if !name.starts_with('#') => Some(name.to_lowercase()),
            _ => ink_color(self.rgb_color()?).map(str::to_owned),
        }
    }

    /// Get hex color string (e.g., "#FF0000")
    pub fn hex_color(&self) -> Option<String> {
        self.rgb_color()
            .map(|(r, g, b)| format!("#{:02X}{:02X}{:02X}", r, g, b))
    }
}

/// The ink colour an RGB value falls within, by brightness, chroma and hue.
/// The bounds are strict: greys, oranges, pinks and cyans match no ink rather
/// than the nearest one.
fn ink_color((r, g, b): (u8, u8, u8)) -> Option<&'static str> {
    let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max - min;
    if max <= 64.0 && chroma <= 24.0 {
        return Some("black");
    }
    if min >= 200.0 && chroma <= 40.0 {
        return Some("white");
    }
    if chroma < 24.0 {
        return None;
    }
    let hue = if max == r {
        60.0 * (g - b) / chroma
    } else if max == g {
        60.0 * (b - r) / chroma + 120.0
    } else {
        60.0 * (r - g) / chroma + 240.0
    }
    .rem_euclid(360.0);
    match hue {
        h if !(15.0..345.0).contains(&h) && min <= 120.0 => Some("red"),
        h if (40.0..70.0).contains(&h) && max >= 150.0 => Some("yellow"),
        h if (75.0..165.0).contains(&h) => Some("green"),
        h if (190.0..250.0).contains(&h) => Some("blue"),
        h if (260.0..320.0).contains(&h) => Some("purple"),
        _ => None,
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
                let spec = stackup_layer
                    .spec_ref
                    .and_then(|spec_ref| spec_map.get(self.ipc.resolve(spec_ref)).copied());
                conductors.push((
                    layer.and_then(|layer| layer.side),
                    stated_copper_oz(spec, stackup_layer.thickness),
                ));
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
                let ink = ColorInfo::of_spec(spec, |symbol| self.ipc.resolve(symbol));
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
    #[test]
    fn ink_colors_have_strict_bounds() {
        let named = |hex: &str| {
            super::ColorInfo {
                name: Some(hex.to_owned()),
                rgb: None,
            }
            .color_name()
        };
        // KiCad's mask presets, and the DO-AH42D dictionary green.
        for (hex, expected) in [
            ("#3C9650", "green"),
            ("#006400", "green"),
            ("#191919E6", "black"),
            ("#000000", "black"),
            ("#FFFFFF", "white"),
            ("#D4DBDE", "white"),
            ("#800000", "red"),
            ("#1974AB", "blue"),
            ("#4D1B6B", "purple"),
            ("#FFCC00", "yellow"),
        ] {
            assert_eq!(named(hex).as_deref(), Some(expected), "{hex}");
        }
        // Greys, oranges, pinks and cyans are no ink, not the nearest one.
        for hex in ["#808080", "#FF8C00", "#FFC0CB", "#00CED1", "#8B4513"] {
            assert_eq!(named(hex), None, "{hex}");
        }
        assert_eq!(named("#006400ZZ"), None);
        assert_eq!(named("Not specified"), None);
        assert_eq!(named("Green").as_deref(), Some("green"));
    }
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
    <CadHeader units="MILLIMETER">
      <Spec name="ONE_OZ"><Conductor type="WEIGHT"><Property value="1" unit="OZ"/></Conductor></Spec>
    </CadHeader>
    <CadData>
      <Layer name="PRIMARY" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="CORE" layerFunction="DIELCORE" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="GND" layerFunction="PLANE" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="PWR" layerFunction="PLANE" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="In_SECONDARY" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
      <Stackup name="Primary" overallThickness="1.6">
        <StackupGroup name="Group">
          <StackupLayer layerOrGroupRef="In_SECONDARY" thickness="0.0696" sequence="5"/>
          <StackupLayer layerOrGroupRef="GND" thickness="0.0174" sequence="2"><SpecRef id="ONE_OZ"/></StackupLayer>
          <StackupLayer layerOrGroupRef="CORE" thickness="1.4" sequence="3"/>
          <StackupLayer layerOrGroupRef="PWR" thickness="0.0174" sequence="4"><SpecRef id="ONE_OZ"/></StackupLayer>
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
        // A stated weight wins over the thickness, as in DFM.
        assert!((stackup.inner_copper_oz.unwrap() - 1.0).abs() < 1e-9);
        assert_eq!(stackup.outer_copper_weight().unwrap(), "2.00 oz (~2 oz)");
    }

    #[test]
    fn stated_copper_names_the_standard_weight_within_its_tolerance() {
        for (ounces, outer, nominal) in [
            (0.437, false, Some(0.5)),
            (0.862, false, Some(1.0)),
            (1.753, false, Some(2.0)),
            (1.3, false, None),
            (0.35, false, Some(1.0 / 3.0)),
            // Plating thickens outer copper, never inner.
            (1.46, true, Some(1.0)),
            (1.46, false, None),
            (1.6, true, None),
            (0.74, true, Some(0.5)),
            (0.9, true, Some(1.0)),
            (2.011, true, Some(2.0)),
        ] {
            assert_eq!(nominal_copper_weight(ounces, outer), nominal, "{ounces} oz");
        }
    }

    #[test]
    fn disagreeing_conductors_report_no_common_weight() {
        let conductors = [
            (Some(Side::Top), Some(1.0)),
            (Some(Side::Bottom), Some(2.0)),
        ];
        assert_eq!(
            uniform_copper_oz(&conductors, &[Side::Top, Side::Bottom]),
            None
        );
        assert_eq!(uniform_copper_oz(&conductors, &[Side::Internal]), None);
    }
}
