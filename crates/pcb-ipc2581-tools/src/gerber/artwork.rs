pub type ArtworkLayer = pcb_ir::dialects::artwork::ArtworkDocument<Vec<String>, ObjectAttributes>;

#[derive(Debug, Clone, Default)]
pub struct ObjectAttributes {
    pub aperture_function: Option<String>,
    pub net: Option<String>,
}
