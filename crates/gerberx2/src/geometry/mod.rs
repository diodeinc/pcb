mod extract;

pub use extract::{
    GerberArtworkDocument, GerberObjectMeta, ObjectClass, SourceKind, extract_document,
};

use pcb_ir::dialects::{LayerRole, Side};

/// Map a Gerber `.FileFunction` attribute to a layer role.
pub fn layer_role(file_function: &[String]) -> LayerRole {
    match file_function.first().map(String::as_str) {
        Some("Copper") => LayerRole::Copper,
        Some("Soldermask") => LayerRole::Soldermask,
        Some("Paste") => LayerRole::Paste,
        Some("Legend") => LayerRole::Legend,
        Some("Profile") => LayerRole::Profile,
        _ => LayerRole::Other,
    }
}

/// Map a Gerber `.FileFunction` attribute to a board side.
pub fn layer_side(file_function: &[String]) -> Side {
    if file_function.iter().any(|field| field == "Top") {
        Side::Top
    } else if file_function.iter().any(|field| field == "Bot") {
        Side::Bottom
    } else if file_function.iter().any(|field| field == "Inr") {
        Side::Inner
    } else {
        Side::None
    }
}
