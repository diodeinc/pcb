pub type ArtworkLayer =
    pcb_ir::dialects::artwork::ArtworkDocument<LayerAttributes, ObjectAttributes>;

#[derive(Debug, Clone, Default)]
pub struct LayerAttributes {
    pub file_function: Vec<String>,
    pub part: Option<Vec<String>>,
    pub file_polarity: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ObjectAttributes {
    pub aperture_function: Option<Vec<String>>,
    pub net: Option<String>,
    pub component: Option<String>,
    pub pin: Option<String>,
}
