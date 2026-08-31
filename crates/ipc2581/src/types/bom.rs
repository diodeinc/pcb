use crate::Symbol;

/// BOM (Bill of Materials) section
#[derive(Debug, Clone)]
pub struct Bom {
    pub name: Symbol,
    pub header: Option<BomHeader>,
    pub items: Vec<BomItem>,
}

/// Metadata and Step scope declared by a BOM.
#[derive(Debug, Clone)]
pub struct BomHeader {
    pub assembly: Symbol,
    pub revision: Symbol,
    pub affecting: Option<bool>,
    pub step_refs: Vec<Symbol>,
}

/// BomItem represents a part in the bill of materials
#[derive(Debug, Clone)]
pub struct BomItem {
    pub oem_design_number_ref: Symbol,
    /// Numeric interpretation of `quantity`, when the source value is an integer.
    pub quantity: Option<u32>,
    /// Required source `quantity` text, retained even when it is not an integer.
    pub quantity_raw: Symbol,
    /// Numeric interpretation of the optional IPC-2581C `pinCount`.
    pub pin_count: Option<u32>,
    /// Optional source `pinCount` text, retained as source provenance.
    pub pin_count_raw: Option<Symbol>,
    pub category: Option<BomCategory>,
    pub internal_part_number: Option<Symbol>,
    pub description: Option<Symbol>,
    /// BOM designators in source document order.
    pub designators: Vec<BomDesignator>,
    pub characteristics: Option<Characteristics>,
    pub spec_refs: Vec<Symbol>,
}

impl BomItem {
    pub fn reference_designators(&self) -> impl Iterator<Item = &BomRefDes> {
        self.designators
            .iter()
            .filter_map(|designator| match designator {
                BomDesignator::Reference(reference) => Some(reference),
                _ => None,
            })
    }
}

#[derive(Debug, Clone)]
pub enum BomDesignator {
    Reference(BomRefDes),
    Material(BomNamedDesignator),
    Document(BomNamedDesignator),
    Tool(BomNamedDesignator),
    Find(BomFindDesignator),
}

/// RefDes reference in BOM
#[derive(Debug, Clone)]
pub struct BomRefDes {
    pub name: Symbol,
    pub package_ref: Option<Symbol>,
    /// Explicit population state. An absent IPC attribute remains `None`.
    pub populate: Option<bool>,
    pub layer_ref: Option<Symbol>,
    pub model_ref: Option<Symbol>,
    pub tunings: Vec<BomTuning>,
    pub firmwares: Vec<BomFirmware>,
}

/// Named material, document, or tool designator in a BOM item.
#[derive(Debug, Clone)]
pub struct BomNamedDesignator {
    pub name: Symbol,
    pub layer_ref: Option<Symbol>,
}

/// Numeric find designator in a BOM item.
#[derive(Debug, Clone)]
pub struct BomFindDesignator {
    pub number: u32,
    /// Required source `number` text, retained as source provenance.
    pub number_raw: Symbol,
    pub layer_ref: Option<Symbol>,
    pub model_ref: Option<Symbol>,
}

#[derive(Debug, Clone)]
pub struct BomTuning {
    pub value: Symbol,
    pub comments: Option<Symbol>,
}

#[derive(Debug, Clone)]
pub struct BomFirmware {
    pub program_name: Symbol,
    pub program_version: Symbol,
    pub file: BomFirmwareFile,
    pub payload: BomFirmwarePayload,
}

#[derive(Debug, Clone)]
pub struct BomFirmwareFile {
    pub name: Symbol,
    pub crc: Symbol,
}

#[derive(Debug, Clone)]
pub enum BomFirmwarePayload {
    Reference(Symbol),
    Cached(Symbol),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BomCategory {
    Electrical,
    Programmable,
    Mechanical,
    Material,
    Document,
}

/// Characteristics for a BOM item
#[derive(Debug, Clone)]
pub struct Characteristics {
    pub category: Option<BomCategory>,
    pub measured: Vec<MeasuredCharacteristic>,
    pub ranged: Vec<RangedCharacteristic>,
    pub enumerated: Vec<EnumeratedCharacteristic>,
    pub textuals: Vec<TextualCharacteristic>,
}

#[derive(Debug, Clone)]
pub struct MeasuredCharacteristic {
    pub definition_source: Option<Symbol>,
    pub name: Option<Symbol>,
    pub value: Option<f64>,
    pub engineering_unit: Option<Symbol>,
    pub negative_tolerance: Option<f64>,
    pub positive_tolerance: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct RangedCharacteristic {
    pub definition_source: Option<Symbol>,
    pub name: Option<Symbol>,
    pub lower_value: Option<f64>,
    pub upper_value: Option<f64>,
    pub engineering_unit: Option<Symbol>,
    pub negative_tolerance: Option<f64>,
    pub positive_tolerance: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct EnumeratedCharacteristic {
    pub definition_source: Option<Symbol>,
    pub name: Option<Symbol>,
    pub value: Option<Symbol>,
}

/// Textual characteristic with name/value pairs
#[derive(Debug, Clone)]
pub struct TextualCharacteristic {
    pub definition_source: Option<Symbol>,
    pub name: Option<Symbol>,
    pub value: Option<Symbol>,
}
