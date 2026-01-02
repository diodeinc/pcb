//! Image decoding utilities for TUI

use ratatui_image::{picker::Picker, protocol::StatefulProtocol};

/// Whether the terminal supports image display
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageProtocol {
    Supported,
    None,
}

impl ImageProtocol {
    pub fn detect() -> Self {
        // Check for Ghostty or Kitty terminal
        if let Ok(term) = std::env::var("TERM") {
            if term.contains("kitty") || term.contains("ghostty") {
                return ImageProtocol::Supported;
            }
        }
        // Ghostty also sets TERM_PROGRAM
        if let Ok(prog) = std::env::var("TERM_PROGRAM") {
            if prog.to_lowercase().contains("ghostty") {
                return ImageProtocol::Supported;
            }
        }
        ImageProtocol::None
    }

    pub fn is_supported(&self) -> bool {
        matches!(self, ImageProtocol::Supported)
    }
}

/// Decode image bytes into a renderable protocol
pub fn decode_image(bytes: &[u8], picker: &Picker) -> Option<StatefulProtocol> {
    let img = image::load_from_memory(bytes).ok()?;
    Some(picker.new_resize_protocol(img))
}
