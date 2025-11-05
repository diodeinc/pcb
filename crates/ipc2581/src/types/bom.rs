use crate::Symbol;

/// BOM (Bill of Materials) section
#[derive(Debug, Clone)]
pub struct Bom {
    pub name: Symbol,
    pub items: Vec<BomItem>,
}

/// BomItem represents a part in the bill of materials
#[derive(Debug, Clone)]
pub struct BomItem {
    pub oem_design_number_ref: Symbol,
    pub quantity: Option<u32>,
    pub pin_count: Option<u32>,
    pub category: Option<BomCategory>,
    pub ref_des_list: Vec<BomRefDes>,
    pub characteristics: Option<Characteristics>,
}

/// RefDes reference in BOM
#[derive(Debug, Clone)]
pub struct BomRefDes {
    pub name: Symbol,
    pub package_ref: Symbol,
    pub populate: bool,
    pub layer_ref: Symbol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BomCategory {
    Electrical,
    Mechanical,
    Document,
}

/// Characteristics for a BOM item
#[derive(Debug, Clone)]
pub struct Characteristics {
    pub category: Option<BomCategory>,
    pub textuals: Vec<TextualCharacteristic>,
}

/// Textual characteristic with name/value pairs
#[derive(Debug, Clone)]
pub struct TextualCharacteristic {
    pub definition_source: Option<Symbol>,
    pub name: Option<Symbol>,
    pub value: Option<Symbol>,
}
