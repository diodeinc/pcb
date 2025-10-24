use crate::Symbol;

/// Ecad section containing CadData
#[derive(Debug, Clone)]
pub struct Ecad {
    pub cad_data: CadData,
}

/// CadData contains Steps, Layers, and Stackup with design data
#[derive(Debug, Clone)]
pub struct CadData {
    pub steps: Vec<Step>,
    pub layers: Vec<Layer>,
    pub stackup: Option<Stackup>,
}

/// Stackup defines the layer stack with overall thickness
#[derive(Debug, Clone)]
pub struct Stackup {
    pub name: Symbol,
    pub overall_thickness: Option<f64>,
    pub layers: Vec<StackupLayer>,
}

/// StackupLayer defines a single layer in the stackup
#[derive(Debug, Clone)]
pub struct StackupLayer {
    pub layer_ref: Symbol,
    pub thickness: Option<f64>,
    pub material: Option<Symbol>,
    pub dielectric_constant: Option<f64>,
    pub layer_number: Option<u32>,
}

/// Step represents a design (board, panel, etc.)
#[derive(Debug, Clone)]
pub struct Step {
    pub name: Symbol,
    pub datum: Option<Datum>,
    pub profile: Option<Profile>,
    pub padstack_defs: Vec<PadStackDef>,
    pub packages: Vec<Package>,
    pub components: Vec<Component>,
    pub logical_nets: Vec<LogicalNet>,
    pub phy_net_groups: Vec<PhyNetGroup>,
    pub layer_features: Vec<LayerFeature>,
}

/// Datum defines the origin point for a Step
#[derive(Debug, Clone, Copy)]
pub struct Datum {
    pub x: f64,
    pub y: f64,
}

/// Profile defines the board outline
#[derive(Debug, Clone)]
pub struct Profile {
    pub polygon: super::Polygon,
}

/// PadStackDef defines a padstack (pad/hole combination)
#[derive(Debug, Clone)]
pub struct PadStackDef {
    pub name: Symbol,
    pub hole_def: Option<PadstackHoleDef>,
    pub pad_defs: Vec<PadstackPadDef>,
}

/// PadstackHoleDef defines the drill hole
#[derive(Debug, Clone)]
pub struct PadstackHoleDef {
    pub name: Symbol,
    pub diameter: f64,
    pub plating_status: PlatingStatus,
    pub plus_tol: f64,
    pub minus_tol: f64,
    pub x: f64,
    pub y: f64,
}

/// PadstackPadDef defines pad on a specific layer
#[derive(Debug, Clone)]
pub struct PadstackPadDef {
    pub layer_ref: Symbol,
    pub pad_use: PadUse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatingStatus {
    Plated,
    NonPlated,
    Via,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadUse {
    Regular,
    Antipad,
    Thermal,
}

/// Package describes a component package (land pattern + outline)
#[derive(Debug, Clone)]
pub struct Package {
    pub name: Symbol,
    pub package_type: Symbol,
    pub pin_one: Option<Symbol>,
    pub height: Option<f64>,
}

/// Component instance on the board
#[derive(Debug, Clone)]
pub struct Component {
    pub ref_des: Symbol,
    pub package_ref: Symbol,
    pub layer_ref: Symbol,
    pub mount_type: Option<MountType>,
    pub part: Option<Symbol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountType {
    Smt,
    Tht,
    Other,
}

/// LogicalNet represents electrical connectivity
#[derive(Debug, Clone)]
pub struct LogicalNet {
    pub name: Symbol,
    pub pin_refs: Vec<PinRef>,
}

/// PinRef references a component pin
#[derive(Debug, Clone)]
pub struct PinRef {
    pub component_ref: Symbol,
    pub pin: Symbol,
}

/// PhyNetGroup contains physical net routing data
#[derive(Debug, Clone)]
pub struct PhyNetGroup {
    pub name: Symbol,
}

/// Layer represents a physical layer in the PCB
#[derive(Debug, Clone)]
pub struct Layer {
    pub name: Symbol,
    pub layer_function: LayerFunction,
    pub side: Option<Side>,
    pub polarity: Option<Polarity>,
}

/// LayerFeature contains features on a layer
#[derive(Debug, Clone)]
pub struct LayerFeature {
    pub layer_ref: Symbol,
    pub sets: Vec<FeatureSet>,
}

/// FeatureSet groups features with common properties
#[derive(Debug, Clone)]
pub struct FeatureSet {
    pub holes: Vec<Hole>,
    pub pads: Vec<Pad>,
    pub traces: Vec<Trace>,
}

/// Hole represents a drilled hole instance
#[derive(Debug, Clone)]
pub struct Hole {
    pub name: Option<Symbol>,
    pub diameter: f64,
    pub plating_status: PlatingStatus,
    pub x: f64,
    pub y: f64,
}

/// Pad represents a pad instance on a layer
#[derive(Debug, Clone)]
pub struct Pad {
    pub padstack_def_ref: Option<Symbol>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub rotation: Option<f64>,
}

/// Trace represents a copper trace or line on a layer
#[derive(Debug, Clone)]
pub struct Trace {
    pub line_desc_ref: Option<Symbol>,
    pub points: Vec<TracePoint>,
}

/// Point in a trace
#[derive(Debug, Clone)]
pub struct TracePoint {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerFunction {
    Conductor,
    CondFilm,
    CondFoil,
    Plane,
    Signal,
    Mixed,
    Soldermask,
    Solderpaste,
    Silkscreen,
    Legend,
    Drill,
    Rout,
    VCut,
    DielBase,
    DielCore,
    DielPreg,
    Document,
    Graphic,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Top,
    Bottom,
    Both,
    Internal,
    All,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    Positive,
    Negative,
}
