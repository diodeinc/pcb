//! Canonical component assembly data.
//!
//! This dialect joins component, BOM, AVL, package, and layout facts without
//! carrying source-format types or quote policy. Geometry and dimensions are
//! canonically in millimeters.

use crate::dialects::ipc::LayoutStepKind;
use crate::geom::{Affine2, Point};

#[derive(Debug, Clone, Default)]
pub struct Document {
    pub scope: Scope,
    pub root_step: Option<u32>,
    pub steps: Vec<Step>,
    pub primary_bom: Option<u32>,
    pub boms: Vec<Bom>,
    pub avl: Option<Avl>,
    pub packages: Vec<PackageDefinition>,
    pub components: Vec<ComponentDefinition>,
    pub occurrences: Vec<ComponentOccurrence>,
}

impl Document {
    pub fn bom_item(&self, reference: BomReferenceId) -> &BomItem {
        &self.boms[reference.bom as usize].items[reference.item as usize]
    }

    pub fn bom_designator(&self, reference: BomReferenceId) -> &ReferenceDesignator {
        match &self.bom_item(reference).designators[reference.designator as usize] {
            BomDesignator::Reference(designator) => designator,
            _ => unreachable!("component BOM reference points to a non-reference designator"),
        }
    }

    pub fn preferred_bom_reference(
        &self,
        component: &ComponentDefinition,
    ) -> Option<BomReferenceId> {
        self.primary_bom
            .and_then(|primary| {
                component
                    .bom_references
                    .iter()
                    .copied()
                    .find(|reference| reference.bom == primary)
            })
            .or_else(|| component.bom_references.first().copied())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Scope {
    Board,
    #[default]
    BoardArray,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub name: String,
    pub kind: LayoutStepKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentDefinitionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackageDefinitionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BomReferenceId {
    pub bom: u32,
    pub item: u32,
    pub designator: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentOccurrenceId {
    pub component: ComponentDefinitionId,
    pub layout: LayoutOccurrenceId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LayoutOccurrenceId {
    Root,
    Instance(u32),
}

impl LayoutOccurrenceId {
    pub(crate) fn source_instance(self) -> Option<u32> {
        match self {
            Self::Root => None,
            Self::Instance(instance) => Some(instance),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ComponentDefinition {
    pub id: ComponentDefinitionId,
    pub step: u32,
    pub source_index: u32,
    pub designator: Option<String>,
    pub package_ref: Option<String>,
    pub package: Option<PackageDefinitionId>,
    pub material_designator: Option<String>,
    pub layer_ref: String,
    pub topside_layer_ref: Option<String>,
    pub side: Side,
    pub mount: Mount,
    pub part: String,
    pub model_ref: Option<String>,
    pub weight: Option<f64>,
    pub height: Option<f64>,
    pub standoff: Option<f64>,
    pub source_transform: Option<Transform>,
    pub local_from_component: Affine2,
    pub nonstandard_attributes: Vec<Attribute>,
    pub slot_cavity_ref: Option<String>,
    pub spec_refs: Vec<String>,
    pub bom_references: Vec<BomReferenceId>,
    pub population: Population,
}

#[derive(Debug, Clone, Copy)]
pub struct ComponentOccurrence {
    pub id: ComponentOccurrenceId,
    pub root_from_component: Affine2,
    pub board: Option<LayoutOccurrenceId>,
    pub root_from_board: Affine2,
    pub board_from_component: Option<Affine2>,
    pub population: Population,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Population {
    #[default]
    Unspecified,
    Populate,
    DoNotPopulate,
    Conflicting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Top,
    Bottom,
    Both,
    Internal,
    All,
    None,
    Unspecified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mount {
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub x_offset: f64,
    pub y_offset: f64,
    pub rotation_degrees: f64,
    pub mirror: bool,
    pub face_up: bool,
    pub scale: f64,
}

#[derive(Debug, Clone)]
pub struct Attribute {
    pub name: String,
    pub value: Option<String>,
    pub attribute_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PackageDefinition {
    pub id: PackageDefinitionId,
    pub step: u32,
    pub source_index: u32,
    pub name: String,
    pub package_type: String,
    pub pin_one: Option<String>,
    pub pin_one_orientation: Option<String>,
    pub height: Option<f64>,
    pub negative_body_extension: Option<f64>,
    pub comment: Option<String>,
    pub pickup_point: Option<Point>,
    pub pins: Vec<PackagePin>,
}

#[derive(Debug, Clone)]
pub struct PackagePin {
    pub view: PackagePinView,
    pub number: String,
    pub name: Option<String>,
    pub pin_type: PackagePinType,
    pub electrical_type: Option<PackagePinElectricalType>,
    pub mount_type: Option<PackagePinMountType>,
    pub polarity: Option<PackagePinPolarity>,
    pub location: Option<Point>,
    pub transform: Option<Transform>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagePinView {
    Primary,
    Topside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagePinType {
    Through,
    Blind,
    Surface,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagePinElectricalType {
    Electrical,
    Mechanical,
    Undefined,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackagePinPolarity {
    Plus,
    Minus,
    Anode,
    Cathode,
}

#[derive(Debug, Clone)]
pub struct Bom {
    pub name: String,
    pub header: Option<BomHeader>,
    pub items: Vec<BomItem>,
}

#[derive(Debug, Clone)]
pub struct BomHeader {
    pub assembly: String,
    pub revision: String,
    pub affecting: Option<bool>,
    pub step_refs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BomItem {
    pub oem_design_number: String,
    pub quantity: Option<u32>,
    pub quantity_raw: String,
    pub pin_count: Option<u32>,
    pub pin_count_raw: Option<String>,
    pub category: Option<BomCategory>,
    pub internal_part_number: Option<String>,
    pub description: Option<String>,
    pub designators: Vec<BomDesignator>,
    pub characteristics: Option<Characteristics>,
    pub spec_refs: Vec<String>,
}

impl BomItem {
    pub fn textual_characteristic(&self, name: &str) -> Option<&str> {
        self.characteristics
            .iter()
            .flat_map(|characteristics| &characteristics.values)
            .find_map(|characteristic| match characteristic {
                Characteristic::Textual {
                    name: Some(candidate),
                    value: Some(value),
                    ..
                } if candidate.eq_ignore_ascii_case(name) => Some(value.as_str()),
                _ => None,
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BomCategory {
    Electrical,
    Programmable,
    Mechanical,
    Material,
    Document,
}

#[derive(Debug, Clone)]
pub enum BomDesignator {
    Reference(ReferenceDesignator),
    Material(NamedDesignator),
    Document(NamedDesignator),
    Tool(NamedDesignator),
    Find(FindDesignator),
}

#[derive(Debug, Clone)]
pub struct ReferenceDesignator {
    pub name: String,
    pub package_ref: Option<String>,
    pub populate: Option<bool>,
    pub layer_ref: Option<String>,
    pub model_ref: Option<String>,
    pub tunings: Vec<Tuning>,
    pub firmwares: Vec<Firmware>,
}

#[derive(Debug, Clone)]
pub struct NamedDesignator {
    pub name: String,
    pub layer_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FindDesignator {
    pub number: u32,
    pub number_raw: String,
    pub layer_ref: Option<String>,
    pub model_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Tuning {
    pub value: String,
    pub comments: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Firmware {
    pub program_name: String,
    pub program_version: String,
    pub file_name: String,
    pub file_crc: String,
    pub payload: FirmwarePayload,
}

#[derive(Debug, Clone)]
pub enum FirmwarePayload {
    Reference(String),
    Cached(String),
}

#[derive(Debug, Clone)]
pub struct Characteristics {
    pub category: Option<BomCategory>,
    pub values: Vec<Characteristic>,
}

#[derive(Debug, Clone)]
pub enum Characteristic {
    Measured {
        definition_source: Option<String>,
        name: Option<String>,
        value: Option<f64>,
        engineering_unit: Option<String>,
        negative_tolerance: Option<f64>,
        positive_tolerance: Option<f64>,
    },
    Ranged {
        definition_source: Option<String>,
        name: Option<String>,
        lower_value: Option<f64>,
        upper_value: Option<f64>,
        engineering_unit: Option<String>,
        negative_tolerance: Option<f64>,
        positive_tolerance: Option<f64>,
    },
    Enumerated {
        definition_source: Option<String>,
        name: Option<String>,
        value: Option<String>,
    },
    Textual {
        definition_source: Option<String>,
        name: Option<String>,
        value: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Avl {
    pub name: String,
    pub header: Option<AvlHeader>,
    pub items: Vec<AvlItem>,
}

#[derive(Debug, Clone)]
pub struct AvlHeader {
    pub title: String,
    pub source: String,
    pub author: String,
    pub datetime: String,
    pub version: u32,
    pub comment: Option<String>,
    pub modification_ref: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AvlItem {
    pub oem_design_number: String,
    pub alternatives: Vec<ApprovedPart>,
    pub spec_refs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ApprovedPart {
    pub external_vendor: Option<String>,
    pub external_mpn: Option<String>,
    pub qualified: Option<bool>,
    pub chosen: Option<bool>,
    pub manufacturer_parts: Vec<ManufacturerPart>,
    pub vendor_refs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ManufacturerPart {
    pub name: String,
    pub rank: Option<u32>,
    pub cost: Option<f64>,
    pub moisture_sensitivity: Option<MoistureSensitivity>,
    pub available: Option<bool>,
    pub other: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoistureSensitivity {
    Unlimited,
    OneYear,
    FourWeeks,
    Hours168,
    Hours72,
    Hours48,
    Hours24,
    Bake,
}
