use super::Units;
use crate::Symbol;
use std::collections::HashMap;

/// CadHeader defines units and specifications for the ECAD section
///
/// All dimensional values in the ECAD section (coordinates, sizes, etc.)
/// are defined in the units specified here. After parsing, all values
/// are converted to millimeters for internal consistency.
#[derive(Debug, Clone)]
pub struct CadHeader {
    pub units: Units,
    pub specs: HashMap<Symbol, Spec>,
}

/// Spec defines material, dielectric, and other properties
///
/// Specs are referenced by StackupLayers, Components, and other elements
/// via SpecRef to provide detailed material and electrical characteristics.
#[derive(Debug, Clone)]
pub struct Spec {
    pub name: Symbol,
    pub material: Option<Symbol>,
    pub dielectric_constant: Option<f64>,
    pub loss_tangent: Option<f64>,
    /// All Property text values from General type="MATERIAL" elements
    pub properties: Vec<Symbol>,
    /// Surface finish specification (ENIG, OSP, etc.)
    pub surface_finish: Option<SurfaceFinish>,
    /// Copper weight in oz/ft² from Conductor type="WEIGHT"
    pub copper_weight_oz: Option<f64>,
    /// Color specified via ColorTerm element (e.g., "GREEN", "WHITE", "BLACK")
    pub color_term: Option<Symbol>,
    /// RGB color specified via Color element (r, g, b values 0-255)
    pub color_rgb: Option<(u8, u8, u8)>,
}

/// Ecad section containing CadHeader and CadData
#[derive(Debug, Clone)]
pub struct Ecad {
    pub cad_header: CadHeader,
    pub cad_data: CadData,
}

/// CadData contains Steps, Layers, and Stackups with design data
#[derive(Debug, Clone)]
pub struct CadData {
    pub steps: Vec<Step>,
    pub layers: Vec<Layer>,
    pub stackups: Vec<Stackup>,
}

/// Stackup defines the layer stack with overall thickness
#[derive(Debug, Clone)]
pub struct Stackup {
    pub name: Symbol,
    pub overall_thickness: Option<f64>,
    pub where_measured: Option<WhereMeasured>,
    pub tol_plus: Option<f64>,
    pub tol_minus: Option<f64>,
    pub layers: Vec<StackupLayer>,
}

/// StackupLayer defines a single layer in the stackup
#[derive(Debug, Clone)]
pub struct StackupLayer {
    pub layer_ref: Symbol,
    pub thickness: Option<f64>,
    pub tol_plus: Option<f64>,
    pub tol_minus: Option<f64>,
    pub material: Option<Symbol>,
    pub spec_ref: Option<Symbol>, // Reference to Spec for looking up properties
    pub dielectric_constant: Option<f64>,
    pub loss_tangent: Option<f64>,
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
    pub cutouts: Vec<super::Polygon>,
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
    pub standard_primitive_ref: Option<Symbol>,
    pub user_primitive_ref: Option<Symbol>,
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
    pub profile: Option<Profile>, // Layer-specific outline (for rigid-flex)
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
    pub net: Option<Symbol>,      // Net name from Set element
    pub geometry: Option<Symbol>, // Reference to PadStackDef or other geometry definition
    pub polarity: Option<Polarity>,
    pub holes: Vec<Hole>,
    pub slots: Vec<Slot>,
    pub pads: Vec<Pad>,
    pub traces: Vec<Trace>,
    pub polygons: Vec<super::Polygon>, // Copper pours from Features
    pub lines: Vec<Line>,              // Trace lines from Features > UserSpecial > Line
}

/// Line represents a straight trace segment
#[derive(Debug, Clone)]
pub struct Line {
    pub start_x: f64,
    pub start_y: f64,
    pub end_x: f64,
    pub end_y: f64,
    pub line_width: f64,
    pub line_end: Option<super::LineEnd>,
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

/// Shape definition for a SlotCavity
///
/// Per IPC-2581 spec section 8.2.3.10.6:
/// "The shape is defined by the substitution group Feature, which can be
/// either a user defined shape or a standard primitive shape."
#[derive(Debug, Clone)]
pub enum SlotShape {
    /// Outline defined as a polygon
    Outline(super::Polygon),
    /// Standard primitive shape (Oval, Circle, RectCenter, etc.)
    Primitive(super::StandardPrimitive),
}

/// Slot represents a slotted hole or cavity
#[derive(Debug, Clone)]
pub struct Slot {
    pub name: Option<Symbol>,
    pub shape: SlotShape,
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
    pub xform: Option<super::Xform>,
    /// Inline primitive override (takes precedence over padstack definition)
    pub standard_primitive_ref: Option<Symbol>,
    /// Inline user primitive override (takes precedence over padstack definition)
    pub user_primitive_ref: Option<Symbol>,
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
    // Conductive layers
    Conductor,
    CondFilm,
    CondFoil,
    Plane,
    Signal,
    Mixed,

    // Coating layers (surface finishes)
    CoatingCond,    // Conductive coating (ENIG, immersion silver, etc.)
    CoatingNonCond, // Non-conductive coating (OSP)

    // Soldermask and paste
    Soldermask,
    Solderpaste,
    Pastemask, // Paste mask (can be different from solderpaste)

    // Silkscreen/Legend
    Silkscreen,
    Legend,

    // Drilling and routing
    Drill,
    Rout,
    VCut,
    Score,
    EdgeChamfer,
    EdgePlating,

    // Dielectric layers
    DielBase,
    DielCore,
    DielPreg,
    DielAdhv,     // Dielectric adhesive high voltage
    DielBondPly,  // Dielectric bond ply
    DielCoverlay, // Dielectric coverlay (flex circuits)

    // Component layers
    ComponentTop,
    ComponentBottom,
    ComponentEmbedded,
    ComponentFormed, // Formed components (thin-film, resistors, etc.)
    Assembly,

    // Specialized material layers
    ConductiveAdhesive,
    Glue,
    HoleFill,
    SolderBump,
    Stiffener,
    Capacitive, // Capacitive material layer
    Resistive,  // Resistive material layer

    // Documentation and tooling
    Document,
    Graphic,
    BoardOutline,
    BoardFab,
    Rework,
    Fixture,
    Probe,
    Courtyard,
    LandPattern,
    ThievingKeepInout, // Copper thieving constraints

    // Composite
    StackupComposite,

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

/// WhereMeasured indicates where overall thickness is measured
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhereMeasured {
    Metal,
    Mask,
    Laminate,
    Other,
}

/// Surface finish material type according to IPC-6012
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishType {
    // Solder leveling
    S, // Solder (Hot Air Solder Leveling - HASL)

    // Tin-lead
    T,   // Tin-lead
    X,   // Tin-lead unfused
    TLU, // Tin-lead unfused

    // Immersion/electroless finishes
    EnigN,   // Electroless Nickel Immersion Gold (normal)
    EnigG,   // Electroless Nickel Immersion Gold (high current)
    EnepigN, // Electroless Nickel Electroless Palladium Immersion Gold (normal)
    EnepigG, // Electroless Nickel Electroless Palladium Immersion Gold (high current)
    EnepigP, // Electroless Nickel Electroless Palladium Immersion Gold (probe)
    Dig,     // Direct Immersion Gold
    IAg,     // Immersion Silver
    ISn,     // Immersion Tin

    // Organic finishes
    Osp,   // Organic Solderability Preservative
    HtOsp, // High Temperature OSP

    // Bare copper
    N,  // Bare copper (none)
    NB, // Bare copper no bondability requirement

    // Carbon contact
    C, // Carbon contact

    // Gold wire bond finishes
    G,       // Gold (wire bond)
    GS,      // Gold over electroless nickel (soft)
    GwbOneG, // Gold wire bond Type 1, Grade G (IPC-4556)
    GwbOneN, // Gold wire bond Type 1, Grade N (IPC-4556)
    GwbTwoG, // Gold wire bond Type 2, Grade G (IPC-4556)
    GwbTwoN, // Gold wire bond Type 2, Grade N (IPC-4556)

    Other,
}

/// Product criteria for surface finish product selection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductCriteria {
    Allowed,
    Suggested,
    Preferred,
    Required,
    Chosen,
}

/// Product specification for a surface finish
#[derive(Debug, Clone)]
pub struct FinishProduct {
    pub name: Symbol,
    pub criteria: Option<ProductCriteria>,
}

/// Surface finish specification
#[derive(Debug, Clone)]
pub struct SurfaceFinish {
    pub finish_type: FinishType,
    pub comment: Option<Symbol>,
    pub products: Vec<FinishProduct>,
}
