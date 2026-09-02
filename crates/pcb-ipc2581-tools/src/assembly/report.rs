//! Stable JSON-facing PCBA assembly report contract.

use serde::Serialize;

pub const REPORT_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AssemblyReport {
    pub schema_version: u32,
    pub units: Units,
    pub source: Source,
    pub scope: Scope,
    pub readiness: Readiness,
    pub summary: Summary,
    pub profiles: Vec<PhysicalProfile>,
    pub boards: Vec<BoardOccurrence>,
    pub packages: Vec<Package>,
    pub components: Vec<Component>,
    pub terminations: Vec<Termination>,
    pub holes: Vec<Hole>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Units {
    pub length: &'static str,
    pub angle: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Source {
    pub format: &'static str,
    pub revision: String,
    pub creation_software: Option<String>,
    pub software_package: Option<SoftwarePackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SoftwarePackage {
    pub name: String,
    pub revision: Option<String>,
    pub vendor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scope {
    pub kind: ScopeKind,
    pub root_step: Option<String>,
    pub coordinate_frame: &'static str,
    pub profile_ids: Vec<String>,
    pub bounds_mm: Option<Bounds>,
    pub area_mm2: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Board,
    BoardArray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    ReviewRequired,
    Incomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub board_occurrences: u64,
    pub packages: u64,
    pub components: ComponentSummary,
    pub terminations: TerminationSummary,
    pub paste: PasteSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentSummary {
    pub total: u64,
    pub included: u64,
    pub excluded: u64,
    pub included_populated: u64,
    pub included_do_not_populate: u64,
    pub included_population_unresolved: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminationSummary {
    pub total: u64,
    pub on_included_populated_components: u64,
    pub surface_on_included_populated_components: u64,
    pub through_on_included_populated_components: u64,
    pub blind_on_included_populated_components: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PasteSummary {
    pub islands: u64,
    pub exactly_linked_to_termination: u64,
    pub on_included_populated_components: u64,
    pub exactly_linked_on_included_populated_components: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BoardOccurrence {
    pub id: String,
    pub step: String,
    pub path: Vec<LayoutPathSegment>,
    pub profile_ids: Vec<String>,
    pub bounds_mm: Option<Bounds>,
    pub area_mm2: Option<f64>,
    /// Board-local to selected-scope affine matrix `[a, b, c, d, tx, ty]`.
    pub transform: [f64; 6],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhysicalProfile {
    pub id: String,
    pub source_step: String,
    pub bounds_mm: Bounds,
    pub area_mm2: f64,
    pub outer: Contour,
    pub cutouts: Vec<Contour>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Bounds {
    pub min: Point,
    pub max: Point,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LayoutPathSegment {
    pub step: String,
    pub repeat: Option<RepeatPosition>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepeatPosition {
    pub index_x: u32,
    pub index_y: u32,
    pub first_x_mm: f64,
    pub first_y_mm: f64,
    pub pitch_x_mm: f64,
    pub pitch_y_mm: f64,
    pub rotation_degrees: f64,
    pub mirror: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Package {
    pub id: String,
    pub source_step: String,
    pub name: String,
    pub package_type: String,
    pub pin_one: Option<String>,
    pub pin_one_orientation: Option<String>,
    pub height_mm: Option<f64>,
    pub negative_body_extension_mm: Option<f64>,
    pub comment: Option<String>,
    pub pickup_point_mm: Option<Point>,
    pub views: Vec<PackageView>,
    pub pins: Vec<PackagePin>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackageView {
    pub kind: PackageViewKind,
    pub outline: Option<PackageOutline>,
    pub land_pattern: Option<PackageLandPattern>,
    pub silkscreen: Option<PackageSilkscreen>,
    pub assembly_drawing: Option<PackageAssemblyDrawing>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageViewKind {
    Primary,
    Topside,
    OtherSide,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackageOutline {
    pub transform: Option<SourceTransform>,
    pub shape: PackageShape,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackageLandPattern {
    pub pads: Vec<PackagePad>,
    pub targets: Vec<PackageTarget>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackagePad {
    pub padstack_ref: Option<String>,
    pub x_mm: Option<f64>,
    pub y_mm: Option<f64>,
    pub transform: Option<SourceTransform>,
    pub graphic: Option<PackageGraphic>,
    pub pin_ref: Option<PackagePinReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackagePinReference {
    pub component_ref: Option<String>,
    pub pin: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackageTarget {
    pub location_mm: Point,
    pub transform: Option<SourceTransform>,
    pub shape: PackageShape,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackageSilkscreen {
    pub outlines: Vec<PackageOutline>,
    pub markings: Vec<PackageMarking>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackageAssemblyDrawing {
    pub outline: Option<PackageOutline>,
    pub markings: Vec<PackageMarking>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackageMarking {
    pub usage: Option<String>,
    pub location_mm: Option<Point>,
    pub transform: Option<SourceTransform>,
    pub graphic: PackageGraphic,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum PackageGraphic {
    Shape(PackageShape),
    Text(PackageText),
    Outline(PackageOutline),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackageShape {
    pub status: PackageGeometryStatus,
    pub references: Vec<PackageGeometryReference>,
    pub polarity: GeometryPolarity,
    pub paths: Vec<GeometryPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageGeometryStatus {
    Complete,
    Partial,
    Unresolved,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageGeometryReference {
    pub kind: PackageGeometryReferenceKind,
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageGeometryReferenceKind {
    StandardPrimitive,
    UserPrimitive,
    LineDescription,
    FillDescription,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GeometryPath {
    pub paint: PathPaint,
    pub contours: Vec<Contour>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PathPaint {
    None,
    Fill {
        rule: FillRule,
    },
    Stroke {
        width_mm: f64,
        cap: LineCap,
        join: LineJoin,
        pattern: LinePattern,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineCap {
    Round,
    Square,
    Butt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineJoin {
    Round,
    Miter,
    Bevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinePattern {
    Solid,
    Dotted,
    Dashed,
    Center,
    Phantom,
    Erase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GeometryPolarity {
    Dark,
    Clear,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackageText {
    pub text: String,
    pub font_size: u32,
    pub font_size_source: String,
    pub transform: Option<SourceTransform>,
    pub bounds_mm: Bounds,
    pub font_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackagePin {
    pub view: PackagePinView,
    pub number: String,
    pub name: Option<String>,
    pub pin_type: PinType,
    pub electrical_type: Option<PinElectricalType>,
    pub mount_type: Option<PinMountType>,
    pub polarity: Option<PinPolarity>,
    pub location_mm: Option<Point>,
    pub transform: Option<SourceTransform>,
    pub shape: PackageShape,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackagePinView {
    Primary,
    Topside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PinType {
    Through,
    Blind,
    Surface,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PinElectricalType {
    Electrical,
    Mechanical,
    Undefined,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PinMountType {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PinPolarity {
    Plus,
    Minus,
    Anode,
    Cathode,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Component {
    pub id: String,
    pub board_id: Option<String>,
    pub source_step: String,
    pub layout_path: Vec<LayoutPathSegment>,
    pub reference_designator: Option<String>,
    pub part: String,
    pub package_id: Option<String>,
    pub package_ref: Option<String>,
    pub bom: Option<BomEvidence>,
    pub population: Population,
    pub side: Side,
    pub mount: ComponentMount,
    pub assembly_status: AssemblyStatus,
    pub exclusion_reason: Option<ExclusionReason>,
    /// Component-local to selected-scope affine matrix `[a, b, c, d, tx, ty]`.
    pub transform: [f64; 6],
    pub termination_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BomEvidence {
    pub bom: String,
    pub oem_design_number: String,
    pub category: Option<BomCategory>,
    pub quantity: Option<u32>,
    pub quantity_source: String,
    pub pin_count: Option<u32>,
    pub pin_count_source: Option<String>,
    pub internal_part_number: Option<String>,
    pub approved_parts: Vec<ApprovedPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovedPart {
    pub external_vendor: Option<String>,
    pub external_mpn: Option<String>,
    pub qualified: Option<bool>,
    pub chosen: Option<bool>,
    pub manufacturer_part_numbers: Vec<String>,
    pub vendor_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Population {
    Unspecified,
    Populate,
    DoNotPopulate,
    Conflicting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Top,
    Bottom,
    Both,
    Internal,
    All,
    None,
    Unspecified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentMount {
    Smt,
    ThroughHole,
    Embedded,
    PressFit,
    WireBonded,
    Glued,
    Clamped,
    Socketed,
    Formed,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BomCategory {
    Electrical,
    Programmable,
    Mechanical,
    Material,
    Document,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssemblyStatus {
    Included,
    Excluded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    DocumentBomCategory,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Termination {
    pub id: String,
    pub component_id: String,
    pub pin: String,
    pub pin_type: PinType,
    pub mount_type: Option<PinMountType>,
    pub padstack: String,
    pub location_mm: Point,
    pub side: Side,
    pub population: Population,
    pub lands: Vec<LandEvidence>,
    pub paste_islands: Vec<PasteEvidence>,
    pub mask_openings: Vec<MaskEvidence>,
    pub hole_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LandEvidence {
    pub layer: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PasteEvidence {
    pub layer: String,
    pub side: Side,
    pub location_mm: Point,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaskEvidence {
    pub layer: String,
    pub side: Side,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hole {
    pub id: String,
    pub board_id: Option<String>,
    pub source_layer: String,
    pub source_name: Option<String>,
    pub kind: HoleKind,
    pub location_mm: Point,
    pub finished_diameter_mm: Option<f64>,
    pub plating: HolePlating,
    pub padstack: Option<String>,
    pub net: Option<String>,
    pub span: HoleSpan,
    pub termination: TerminationAssociation,
    pub protection: ProtectionIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HoleKind {
    Round,
    Slot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HolePlating {
    Unknown,
    None,
    Plated,
    NonPlated,
    Via,
    ViaCapped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HoleSpan {
    pub kind: HoleSpanKind,
    pub from_layer: Option<String>,
    pub to_layer: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HoleSpanKind {
    Unknown,
    Layer,
    ThroughBoard,
    FromTo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TerminationAssociation {
    pub status: AssociationStatus,
    pub basis: Option<AssociationBasis>,
    pub termination_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssociationBasis {
    SourceIdentity,
    ExactGeometry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssociationStatus {
    Explicit,
    ExactGeometric,
    Ambiguous,
    Conflicting,
    Unresolved,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtectionIntent {
    pub status: ProtectionStatus,
    pub methods: Vec<ProtectionMethod>,
    pub fill_material: Option<FillMaterial>,
    pub evidence: Vec<ProtectionEvidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionStatus {
    Explicit,
    Conflicting,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionMethod {
    Open,
    Tented,
    Plugged,
    Filled,
    Capped,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FillMaterial {
    NonConductive,
    Conductive,
    Copper,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtectionEvidence {
    pub id: String,
    pub kind: ProtectionEvidenceKind,
    pub layer: String,
    pub side: Side,
    pub span: HoleSpan,
    pub specs: Vec<String>,
    pub terms: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectionEvidenceKind {
    SourceTerms,
    ViaCappedPlatingStatus,
    HoleFillLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Contour {
    pub commands: Vec<PathCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PathCommand {
    MoveTo {
        x: f64,
        y: f64,
    },
    LineTo {
        x: f64,
        y: f64,
    },
    ArcTo {
        x: f64,
        y: f64,
        center_x: f64,
        center_y: f64,
        clockwise: bool,
    },
    CubicTo {
        control_1_x: f64,
        control_1_y: f64,
        control_2_x: f64,
        control_2_y: f64,
        x: f64,
        y: f64,
    },
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SourceTransform {
    pub x_offset_mm: f64,
    pub y_offset_mm: f64,
    pub rotation_degrees: f64,
    pub mirror: bool,
    pub face_up: bool,
    pub scale: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub id: String,
    pub severity: DiagnosticSeverity,
    pub code: DiagnosticCode,
    pub subject: DiagnosticSubject,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    MissingPopulation,
    ConflictingPopulation,
    MissingReferenceDesignator,
    MissingPackage,
    MissingPhysicalTerminations,
    AmbiguousHoleTermination,
    ConflictingHoleTermination,
    ConflictingViaProtection,
    UnknownViaProtection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticSubject {
    pub kind: DiagnosticSubjectKind,
    pub id: String,
    pub reference_designator: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSubjectKind {
    Component,
    Hole,
}
