use crate::Symbol;

/// Ecad section containing CadData
#[derive(Debug, Clone)]
pub struct Ecad {
    pub cad_data: CadData,
}

/// CadData contains Steps and Layers with design data
#[derive(Debug, Clone)]
pub struct CadData {
    pub steps: Vec<Step>,
    pub layers: Vec<Layer>,
}

/// Step represents a design (board, panel, etc.)
#[derive(Debug, Clone)]
pub struct Step {
    pub name: Symbol,
    pub datum: Option<Datum>,
    pub profile: Option<Profile>,
    pub packages: Vec<Package>,
    pub components: Vec<Component>,
    pub logical_nets: Vec<LogicalNet>,
    pub phy_net_groups: Vec<PhyNetGroup>,
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
