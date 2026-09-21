use super::{Span, Tokens, Units, UserPrimitive, from_token, token};
use crate::Symbol;
use std::collections::HashMap;
use std::fmt;

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
    /// Typed child elements exactly as carried by the IPC Spec payload.
    pub items: Vec<SpecItem>,
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

/// A child item inside a CadHeader Spec.
#[derive(Debug, Clone)]
pub struct SpecItem {
    pub element: Symbol,
    pub kind: SpecItemKind,
    pub item_type: Option<Symbol>,
    pub comment: Option<Symbol>,
    pub properties: Vec<SpecProperty>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecItemKind {
    General,
    Dielectric,
    Conductor,
    SurfaceFinish,
    VCut,
    Other,
}

#[derive(Debug, Clone)]
pub struct SpecProperty {
    pub value: Option<f64>,
    pub text: Option<Symbol>,
    pub unit: Option<Symbol>,
    pub plus_tol: Option<f64>,
    pub minus_tol: Option<f64>,
    pub tol_percent: Option<bool>,
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
    /// Millimeters, or percent of the thickness when `tol_percent`.
    pub tol_plus: Option<f64>,
    pub tol_minus: Option<f64>,
    pub tol_percent: bool,
    pub layers: Vec<StackupLayer>,
}

/// StackupLayer defines a single layer in the stackup
#[derive(Debug, Clone)]
pub struct StackupLayer {
    pub layer_ref: Symbol,
    pub thickness: Option<f64>,
    /// Millimeters, or percent of the thickness when `tol_percent`.
    pub tol_plus: Option<f64>,
    pub tol_minus: Option<f64>,
    pub tol_percent: bool,
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
    pub step_type: Option<StepType>,
    pub datum: Option<Datum>,
    pub profile: Option<Profile>,
    pub step_repeats: Vec<StepRepeat>,
    pub padstack_defs: Vec<PadStackDef>,
    pub packages: Vec<Package>,
    pub components: Vec<Component>,
    pub logical_nets: Vec<LogicalNet>,
    pub phy_net_groups: Vec<PhyNetGroup>,
    pub layer_features: Vec<LayerFeature>,
}

/// IPC-2581 Step type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepType {
    Board,
    Pallet,
    Ic,
}

impl StepType {
    const TOKENS: Tokens<Self> = &[
        ("BOARD", Self::Board),
        ("PALLET", Self::Pallet),
        ("IC", Self::Ic),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "Step type", token)
    }
}

/// StepRepeat places one Step within another Step, usually a board within a panel.
#[derive(Debug, Clone)]
pub struct StepRepeat {
    pub step_ref: Symbol,
    pub x: f64,
    pub y: f64,
    pub nx: u32,
    pub ny: u32,
    pub dx: f64,
    pub dy: f64,
    pub angle: f64,
    pub mirror: bool,
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
    /// Transform of the layer shape about the padstack origin, offsets in
    /// millimeters. Allegro writes the shape offset here and leaves
    /// `Location` at the origin; KiCad does the opposite.
    pub xform: Option<super::Xform>,
    /// Shape offset from the padstack origin, in millimeters. The pad's
    /// `Xform` rotates and mirrors this offset together with the shape.
    pub x: f64,
    pub y: f64,
    /// The layer shape: a dictionary reference or any inline `Feature`.
    pub feature: Option<FeatureShape>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatingStatus {
    Plated,
    NonPlated,
    Via,
    ViaCapped,
}

impl PlatingStatus {
    const TOKENS: Tokens<Self> = &[
        ("PLATED", Self::Plated),
        ("NONPLATED", Self::NonPlated),
        ("VIA", Self::Via),
        ("VIA_CAPPED", Self::ViaCapped),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "platingStatus", token)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadUse {
    Regular,
    Antipad,
    Thermal,
    Other,
}

impl PadUse {
    const TOKENS: Tokens<Self> = &[
        ("REGULAR", Self::Regular),
        ("ANTIPAD", Self::Antipad),
        ("THERMAL", Self::Thermal),
        ("OTHER", Self::Other),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "padUse", token)
    }
}

/// Package describes a component package (land pattern + outline)
#[derive(Debug, Clone)]
pub struct Package {
    pub name: Symbol,
    pub package_type: Symbol,
    pub pin_one: Option<Symbol>,
    pub pin_one_orientation: Option<Symbol>,
    pub height: Option<f64>,
    pub negative_body_extension: Option<f64>,
    pub comment: Option<Symbol>,
    pub outline: Option<PackageOutline>,
    pub pickup_point: Option<super::Location>,
    pub land_pattern: Option<PackageLandPattern>,
    pub silkscreen: Option<PackageSilkscreen>,
    pub assembly_drawing: Option<PackageAssemblyDrawing>,
    pub pins: Vec<PackagePin>,
    pub topside: Option<PackageSideView>,
    pub other_side_view: Option<PackageOtherSideView>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PackageOutline {
    pub polygon: super::Polygon,
    pub polygon_xform: Option<super::Xform>,
    pub polygon_line_desc: Option<super::LineDesc>,
    pub polygon_line_desc_ref: Option<Symbol>,
    pub polygon_fill_desc: Option<super::FillDesc>,
    pub polygon_fill_desc_ref: Option<Symbol>,
    pub line_desc: super::LineDescGroup,
}

#[derive(Debug, Clone)]
pub struct PackageLandPattern {
    pub pads: Vec<Pad>,
    pub targets: Vec<PackageTarget>,
}

#[derive(Debug, Clone)]
pub struct PackageTarget {
    pub xform: Option<super::Xform>,
    pub location: super::Location,
    pub shape: StandardShape,
}

#[derive(Debug, Clone)]
pub struct PackageSilkscreen {
    pub outlines: Vec<PackageOutline>,
    pub markings: Vec<PackageMarking>,
}

#[derive(Debug, Clone)]
pub struct PackageAssemblyDrawing {
    /// The descriptive IPC-2581C document permits this to be absent, while
    /// the revision C XML Schema requires it. Preserve the source distinction.
    pub outline: Option<PackageOutline>,
    pub markings: Vec<PackageMarking>,
}

#[derive(Debug, Clone)]
pub struct PackageMarking {
    pub usage: Option<Symbol>,
    pub xform: Option<super::Xform>,
    pub location: Option<super::Location>,
    pub feature: FeatureShape,
}

#[derive(Debug, Clone)]
pub struct PackageSideView {
    pub outline: Option<PackageOutline>,
    pub land_pattern: Option<PackageLandPattern>,
    pub silkscreen: Option<PackageSilkscreen>,
    pub assembly_drawing: Option<PackageAssemblyDrawing>,
    pub pins: Vec<PackagePin>,
}

#[derive(Debug, Clone)]
pub struct PackageOtherSideView {
    pub outline: Option<PackageOutline>,
    pub silkscreen: Option<PackageSilkscreen>,
    pub assembly_drawing: Option<PackageAssemblyDrawing>,
}

#[derive(Debug, Clone)]
pub struct PackagePin {
    pub number: Symbol,
    pub name: Option<Symbol>,
    pub pin_type: PackagePinType,
    pub electrical_type: Option<PackagePinElectricalType>,
    pub mount_type: Option<PackagePinMountType>,
    pub polarity: Option<PackagePinPolarity>,
    pub xform: Option<super::Xform>,
    pub location: Option<super::Location>,
    pub shape: StandardShape,
}

/// Shape content allowed by the IPC-2581C `StandardShape` substitution group.
#[derive(Debug, Clone)]
pub enum StandardShape {
    Primitive(Box<super::StandardPrimitive>),
    PrimitiveRef(Symbol),
}

/// Shape content allowed by the broader IPC-2581C `Feature` substitution group.
///
/// Nearly every shape is a dictionary reference, so the inline definitions
/// are boxed to keep each pad that holds one of these small.
#[derive(Debug, Clone)]
pub enum FeatureShape {
    StandardPrimitive(Box<super::StandardPrimitive>),
    StandardPrimitiveRef(Symbol),
    UserPrimitive(Box<super::UserPrimitive>),
    UserPrimitiveRef(Symbol),
    UserShape(Box<super::UserShape>),
    Text(Box<super::Text>),
    Outline(Box<PackageOutline>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagePinType {
    Through,
    Blind,
    Surface,
}

impl PackagePinType {
    const TOKENS: Tokens<Self> = &[
        ("THRU", Self::Through),
        ("BLIND", Self::Blind),
        ("SURFACE", Self::Surface),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "Pin type", token)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagePinElectricalType {
    Electrical,
    Mechanical,
    Undefined,
}

impl PackagePinElectricalType {
    const TOKENS: Tokens<Self> = &[
        ("ELECTRICAL", Self::Electrical),
        ("MECHANICAL", Self::Mechanical),
        ("UNDEFINED", Self::Undefined),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "Pin electricalType", token)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagePinMountType {
    SurfaceMountPin,
    SurfaceMountPad,
    ThroughHolePin,
    ThroughHoleHole,
    PressFit,
    NonBoard,
    Hole,
    WireBond,
    Undefined,
}

impl PackagePinMountType {
    const TOKENS: Tokens<Self> = &[
        ("SURFACE_MOUNT_PIN", Self::SurfaceMountPin),
        ("SURFACE_MOUNT_PAD", Self::SurfaceMountPad),
        ("THROUGH_HOLE_PIN", Self::ThroughHolePin),
        ("THROUGH_HOLE_HOLE", Self::ThroughHoleHole),
        ("PRESSFIT", Self::PressFit),
        ("NONBOARD", Self::NonBoard),
        ("HOLE", Self::Hole),
        ("WIRE_BOND", Self::WireBond),
        ("UNDEFINED", Self::Undefined),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "Pin mountType", token)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagePinPolarity {
    Plus,
    Minus,
    Anode,
    Cathode,
}

impl PackagePinPolarity {
    const TOKENS: Tokens<Self> = &[
        ("PLUS", Self::Plus),
        ("MINUS", Self::Minus),
        ("ANODE", Self::Anode),
        ("CATHODE", Self::Cathode),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "pinPolarity", token)
    }
}

/// Component instance on the board
#[derive(Debug, Clone)]
pub struct Component {
    pub ref_des: Option<Symbol>,
    pub package_ref: Option<Symbol>,
    pub mat_des: Option<Symbol>,
    pub layer_ref: Symbol,
    pub layer_ref_topside: Option<Symbol>,
    pub mount_type: MountType,
    pub part: Symbol,
    pub model_ref: Option<Symbol>,
    pub weight: Option<f64>,
    pub height: Option<f64>,
    pub standoff: Option<f64>,
    pub location: super::Location,
    pub xform: Option<super::Xform>,
    pub nonstandard_attributes: Vec<NonstandardAttribute>,
    pub slot_cavity_ref: Option<Symbol>,
    pub spec_refs: Vec<Symbol>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountType {
    Smt,
    Thmt,
    Embedded,
    PressFit,
    WireBonded,
    Glued,
    Clamped,
    Socketed,
    Formed,
    Other,
}

impl MountType {
    const TOKENS: Tokens<Self> = &[
        ("SMT", Self::Smt),
        ("THMT", Self::Thmt),
        ("EMBEDDED", Self::Embedded),
        ("PRESSFIT", Self::PressFit),
        ("WIRE_BONDED", Self::WireBonded),
        ("GLUED", Self::Glued),
        ("CLAMPED", Self::Clamped),
        ("SOCKETED", Self::Socketed),
        ("FORMED", Self::Formed),
        ("OTHER", Self::Other),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "Component mountType", token)
    }
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
    pub component_ref: Option<Symbol>,
    pub pin: Symbol,
    pub title: Option<Symbol>,
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
    pub span: Option<LayerSpan>,
    pub spec_refs: Vec<Symbol>,
    /// Layer-specific outlines; a rigid-flex layer can have several.
    pub profiles: Vec<Profile>,
}

#[derive(Debug, Clone, Copy)]
pub struct LayerSpan {
    pub from_layer: Option<Symbol>,
    pub to_layer: Option<Symbol>,
}

/// LayerFeature contains features on a layer
///
/// Allegro writes one `Set` per pad, so a layer holds its sets' features,
/// spec refs and attributes in three tables that each set spans.
#[derive(Clone)]
pub struct LayerFeature {
    pub layer_ref: Symbol,
    pub sets: Vec<FeatureSet>,
    pub features: Vec<SetFeature>,
    pub spec_refs: Vec<Symbol>,
    pub nonstandard_attributes: Vec<NonstandardAttribute>,
}

/// FeatureSet groups features with common properties
#[derive(Debug, Clone)]
pub struct FeatureSet {
    pub net: Option<Symbol>,      // Net name from Set element
    pub geometry: Option<Symbol>, // Reference to PadStackDef or other geometry definition
    pub component_ref: Option<Symbol>,
    pub geometry_usage: Option<GeometryUsage>,
    pub polarity: Option<Polarity>,
    /// Into [`LayerFeature::spec_refs`].
    pub spec_refs: Span,
    /// Into [`LayerFeature::features`], in source document order.
    pub features: Span,
    /// Into [`LayerFeature::nonstandard_attributes`].
    pub nonstandard_attributes: Span,
}

/// Intended use of geometry in a feature set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryUsage {
    Thieving,
    ThermalRelief,
    Text,
    Teardrop,
    Graphic,
    None,
}

impl GeometryUsage {
    const TOKENS: Tokens<Self> = &[
        ("THIEVING", Self::Thieving),
        ("THERMAL_RELIEF", Self::ThermalRelief),
        ("TEXT", Self::Text),
        ("TEARDROP", Self::Teardrop),
        ("GRAPHIC", Self::Graphic),
        ("NONE", Self::None),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "geometryUsage", token)
    }
}

impl LayerFeature {
    /// Every feature of the layer, descending into placement groups.
    ///
    /// Placement-group members are yielded once, in group-local coordinates:
    /// the group's `locations` and `xform` are NOT applied. Consumers that
    /// need placed occurrences must match [`SetFeature::PlacementGroup`]
    /// directly and apply its placements themselves.
    fn flat_features(&self) -> impl Iterator<Item = &SetFeature> {
        self.features.iter().flat_map(|feature| match feature {
            SetFeature::PlacementGroup(group) => group.features.iter(),
            _ => std::slice::from_ref(feature).iter(),
        })
    }

    pub fn holes(&self) -> impl Iterator<Item = &Hole> {
        self.flat_features().filter_map(|feature| match feature {
            SetFeature::Hole(hole) => Some(hole),
            _ => None,
        })
    }

    pub fn slots(&self) -> impl Iterator<Item = &Slot> {
        self.flat_features().filter_map(|feature| match feature {
            SetFeature::Slot(slot) => Some(&**slot),
            _ => None,
        })
    }

    pub fn fiducials(&self) -> impl Iterator<Item = &Fiducial> {
        self.flat_features().filter_map(|feature| match feature {
            SetFeature::Fiducial(fiducial) => Some(&**fiducial),
            _ => None,
        })
    }
}

/// Prints each set with the features, spec refs and attributes it spans.
impl fmt::Debug for LayerFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        struct Sets<'a>(&'a LayerFeature);
        struct Set<'a>(&'a LayerFeature, &'a FeatureSet);

        impl fmt::Debug for Sets<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let sets = self.0.sets.iter().map(|set| Set(self.0, set));
                f.debug_list().entries(sets).finish()
            }
        }

        impl fmt::Debug for Set<'_> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let Self(layer, set) = self;
                let attributes = set
                    .nonstandard_attributes
                    .slice(&layer.nonstandard_attributes);
                f.debug_struct("FeatureSet")
                    .field("net", &set.net)
                    .field("geometry", &set.geometry)
                    .field("component_ref", &set.component_ref)
                    .field("geometry_usage", &set.geometry_usage)
                    .field("polarity", &set.polarity)
                    .field("spec_refs", &set.spec_refs.slice(&layer.spec_refs))
                    .field("features", &set.features.slice(&layer.features))
                    .field("nonstandard_attributes", &attributes)
                    .finish()
            }
        }

        f.debug_struct("LayerFeature")
            .field("layer_ref", &self.layer_ref)
            .field("sets", &Sets(self))
            .finish()
    }
}

/// Geometry-bearing children of a Set in source document order.
///
/// A pad, hole or line is what a layer holds by the hundred thousand, so the
/// rare fat kinds are boxed and do not size the rest.
#[derive(Debug, Clone)]
pub enum SetFeature {
    Hole(Hole),
    Slot(Box<Slot>),
    Pad(Pad),
    Fiducial(Box<Fiducial>),
    Stroke(Stroke),
    UserPrimitive(FeatureUserPrimitive),
    Polygon(super::Polygon),
    StandardPrimitiveRef(FeaturePrimitiveRef),
    UserPrimitiveRef(FeaturePrimitiveRef),
    /// One or more local feature definitions placed at shared IPC
    /// `Features/Location` transforms. Keeping the definitions separate from
    /// their placements avoids cloning arbitrary contours for every location.
    PlacementGroup(Box<FeaturePlacementGroup>),
}

#[derive(Debug, Clone)]
pub struct FeaturePlacementGroup {
    pub xform: Option<super::Xform>,
    pub locations: Vec<super::Point>,
    pub features: Vec<SetFeature>,
}

/// Inline user primitive feature carried directly by a Features block.
#[derive(Debug, Clone)]
pub struct FeatureUserPrimitive {
    pub primitive: UserPrimitive,
    pub x: f64,
    pub y: f64,
}

/// IPC fiducial and panel mark feature carried by a Set.
#[derive(Debug, Clone)]
pub struct Fiducial {
    pub kind: FiducialKind,
    pub location: super::Location,
    pub xform: Option<super::Xform>,
    pub shape: FiducialShape,
    pub pin_ref: Option<PinRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiducialKind {
    BadBoardMark,
    Global,
    GoodPanelMark,
    Local,
}

impl FiducialKind {
    const TOKENS: Tokens<Self> = &[
        ("BadBoardMark", Self::BadBoardMark),
        ("GlobalFiducial", Self::Global),
        ("GoodPanelMark", Self::GoodPanelMark),
        ("LocalFiducial", Self::Local),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "fiducial element", token)
    }
}

#[derive(Debug, Clone)]
pub enum FiducialShape {
    Primitive(super::StandardPrimitive),
    StandardPrimitiveRef(Symbol),
}

/// NonstandardAttribute from Set elements
#[derive(Debug, Clone)]
pub struct NonstandardAttribute {
    pub name: Symbol,
    pub value: Option<Symbol>,
    pub attr_type: Option<Symbol>,
}

/// A stroked `Line`, `Arc` or `Polyline` of a Set, in step coordinates.
#[derive(Debug, Clone)]
pub struct Stroke {
    pub path: StrokePath,
    /// The `LineDescRef` if the feature has one, else its inline `LineDesc`.
    /// Without either there is no width to draw.
    pub line_desc: Option<super::LineDescGroup>,
}

#[derive(Debug, Clone)]
pub enum StrokePath {
    Line(super::Line),
    Arc(super::Arc),
    Polyline(super::Polyline),
}

/// Primitive reference used directly as feature geometry.
#[derive(Debug, Clone)]
pub struct FeaturePrimitiveRef {
    pub id: Symbol,
    pub x: f64,
    pub y: f64,
}

/// Hole represents a drilled hole instance
#[derive(Debug, Clone)]
pub struct Hole {
    pub name: Option<Symbol>,
    pub shape: HoleShape,
    pub diameter: f64,
    pub plating_status: PlatingStatus,
    pub xform: Option<super::Xform>,
    pub spec_refs: Vec<Symbol>,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoleShape {
    Circle,
    Square,
}

impl HoleShape {
    const TOKENS: Tokens<Self> = &[("CIRCLE", Self::Circle), ("SQUARE", Self::Square)];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "Hole type", token)
    }
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
    pub z_axis_dim: bool,
    pub xform: Option<super::Xform>,
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
    /// The pad's own shape, which takes precedence over its padstack's.
    pub feature: Option<FeatureShape>,
    pub pin_ref: Option<PinRef>,
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
    Component,
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
    Pin,
    ThievingKeepInout, // Copper thieving constraints

    // Composite
    StackupComposite,

    Other,
}

impl LayerFunction {
    /// `ROUTE`, `SCORE` and `BOARD_FAB` are not schema tokens but are read.
    const TOKENS: Tokens<Self> = &[
        ("CONDUCTOR", Self::Conductor),
        ("CONDFILM", Self::CondFilm),
        ("CONDFOIL", Self::CondFoil),
        ("PLANE", Self::Plane),
        ("SIGNAL", Self::Signal),
        ("MIXED", Self::Mixed),
        ("COATINGCOND", Self::CoatingCond),
        ("COATINGNONCOND", Self::CoatingNonCond),
        ("SOLDERMASK", Self::Soldermask),
        ("SOLDERPASTE", Self::Solderpaste),
        ("PASTEMASK", Self::Pastemask),
        ("SILKSCREEN", Self::Silkscreen),
        ("LEGEND", Self::Legend),
        ("DRILL", Self::Drill),
        ("ROUT", Self::Rout),
        ("ROUTE", Self::Rout),
        ("V_CUT", Self::VCut),
        ("SCORE", Self::Score),
        ("EDGE_CHAMFER", Self::EdgeChamfer),
        ("EDGE_PLATING", Self::EdgePlating),
        ("DIELBASE", Self::DielBase),
        ("DIELCORE", Self::DielCore),
        ("DIELPREG", Self::DielPreg),
        ("DIELADHV", Self::DielAdhv),
        ("DIELBONDPLY", Self::DielBondPly),
        ("DIELCOVERLAY", Self::DielCoverlay),
        ("COMPONENT", Self::Component),
        ("COMPONENT_TOP", Self::ComponentTop),
        ("COMPONENT_BOTTOM", Self::ComponentBottom),
        ("COMPONENT_EMBEDDED", Self::ComponentEmbedded),
        ("COMPONENT_FORMED", Self::ComponentFormed),
        ("ASSEMBLY", Self::Assembly),
        ("CONDUCTIVE_ADHESIVE", Self::ConductiveAdhesive),
        ("GLUE", Self::Glue),
        ("HOLEFILL", Self::HoleFill),
        ("SOLDERBUMP", Self::SolderBump),
        ("STIFFENER", Self::Stiffener),
        ("CAPACITIVE", Self::Capacitive),
        ("RESISTIVE", Self::Resistive),
        ("DOCUMENT", Self::Document),
        ("GRAPHIC", Self::Graphic),
        ("BOARD_OUTLINE", Self::BoardOutline),
        ("BOARDFAB", Self::BoardFab),
        ("BOARD_FAB", Self::BoardFab),
        ("REWORK", Self::Rework),
        ("FIXTURE", Self::Fixture),
        ("PROBE", Self::Probe),
        ("COURTYARD", Self::Courtyard),
        ("LANDPATTERN", Self::LandPattern),
        ("PIN", Self::Pin),
        ("THIEVING_KEEP_INOUT", Self::ThievingKeepInout),
        ("STACKUP_COMPOSITE", Self::StackupComposite),
        ("OTHER", Self::Other),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "layerFunction", token)
    }

    pub fn is_dielectric(self) -> bool {
        matches!(
            self,
            Self::DielBase
                | Self::DielCore
                | Self::DielPreg
                | Self::DielAdhv
                | Self::DielBondPly
                | Self::DielCoverlay
        )
    }

    pub fn is_coating(self) -> bool {
        matches!(self, Self::CoatingCond | Self::CoatingNonCond)
    }

    pub fn is_fabrication(self) -> bool {
        matches!(
            self,
            Self::Drill
                | Self::Rout
                | Self::VCut
                | Self::Score
                | Self::EdgeChamfer
                | Self::EdgePlating
                | Self::BoardOutline
        )
    }
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

impl Side {
    const TOKENS: Tokens<Self> = &[
        ("TOP", Self::Top),
        ("BOTTOM", Self::Bottom),
        ("BOTH", Self::Both),
        ("INTERNAL", Self::Internal),
        ("ALL", Self::All),
        ("NONE", Self::None),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "side", token)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    Positive,
    Negative,
}

impl Polarity {
    const TOKENS: Tokens<Self> = &[("POSITIVE", Self::Positive), ("NEGATIVE", Self::Negative)];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "polarity", token)
    }
}

/// WhereMeasured indicates where overall thickness is measured
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhereMeasured {
    Metal,
    Mask,
    Laminate,
    Other,
}

impl WhereMeasured {
    const TOKENS: Tokens<Self> = &[
        ("METAL", Self::Metal),
        ("MASK", Self::Mask),
        ("LAMINATE", Self::Laminate),
        ("OTHER", Self::Other),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "whereMeasured", token)
    }
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

impl FinishType {
    const TOKENS: Tokens<Self> = &[
        ("S", Self::S),
        ("T", Self::T),
        ("X", Self::X),
        ("TLU", Self::TLU),
        ("ENIG-N", Self::EnigN),
        ("ENIG-G", Self::EnigG),
        ("ENEPIG-N", Self::EnepigN),
        ("ENEPIG-G", Self::EnepigG),
        ("ENEPIG-P", Self::EnepigP),
        ("DIG", Self::Dig),
        ("IAg", Self::IAg),
        ("ISn", Self::ISn),
        ("OSP", Self::Osp),
        ("HT_OSP", Self::HtOsp),
        ("N", Self::N),
        ("NB", Self::NB),
        ("C", Self::C),
        ("G", Self::G),
        ("GS", Self::GS),
        ("GWB-1-G", Self::GwbOneG),
        ("GWB-1-N", Self::GwbOneN),
        ("GWB-2-G", Self::GwbTwoG),
        ("GWB-2-N", Self::GwbTwoN),
        ("OTHER", Self::Other),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "SurfaceFinish type", token)
    }
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

impl ProductCriteria {
    const TOKENS: Tokens<Self> = &[
        ("ALLOWED", Self::Allowed),
        ("SUGGESTED", Self::Suggested),
        ("PREFERRED", Self::Preferred),
        ("REQUIRED", Self::Required),
        ("CHOSEN", Self::Chosen),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "criteria", token)
    }
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

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;

    /// Allegro writes a `Set` and a `Pad` per pad, a hundred thousand on a
    /// board, so what each costs is the size of the model.
    #[test]
    fn per_pad_records_stay_small() {
        assert!(size_of::<FeatureSet>() <= 56);
        assert!(size_of::<SetFeature>() <= 128);
        assert!(size_of::<FeatureShape>() <= 16);
    }
}
