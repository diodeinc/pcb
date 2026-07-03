use std::io::{self, IsTerminal, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use terminal_size::{Width, terminal_size};

use crate::dialects::mask;
use crate::render::{RenderOptions, SizeConstraint};

const KITTY_CHUNK_SIZE: usize = 4096;
const MAX_TERMINAL_DIMENSION_PX: u32 = 1200;

pub fn can_render_to_terminal() -> bool {
    io::stdout().is_terminal()
}

/// Render mask layers as an inline image using the kitty graphics protocol.
/// Any size constraint in `options` is replaced by the terminal width.
pub fn to_terminal<LayerMeta>(
    doc: &mask::Document<LayerMeta>,
    options: &RenderOptions,
) -> Result<(), String> {
    let png = crate::render::png(
        doc,
        &RenderOptions {
            layers: options.layers.clone(),
            size: SizeConstraint::MaxDimension(terminal_max_dimension_px()),
        },
    )?;
    if !io::stdout().is_terminal() {
        return Err(
            "stdout is not an interactive terminal; pass an SVG or PNG output path".to_string(),
        );
    }
    let mut stdout = io::stdout().lock();
    write_kitty_png(&mut stdout, &png).map_err(|err| err.to_string())?;
    stdout.write_all(b"\n").map_err(|err| err.to_string())?;
    Ok(())
}

pub fn write_kitty_png<W: Write>(writer: &mut W, png: &[u8]) -> io::Result<()> {
    let encoded = STANDARD.encode(png);
    let mut chunks = encoded.as_bytes().chunks(KITTY_CHUNK_SIZE).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            write!(writer, "\x1b_Ga=T,f=100,m={more};")?;
            first = false;
        } else {
            write!(writer, "\x1b_Gm={more};")?;
        }
        writer.write_all(chunk)?;
        writer.write_all(b"\x1b\\")?;
    }
    Ok(())
}

fn terminal_max_dimension_px() -> u32 {
    let Some((Width(columns), _)) = terminal_size() else {
        return MAX_TERMINAL_DIMENSION_PX;
    };
    u32::from(columns)
        .saturating_mul(12)
        .clamp(1, MAX_TERMINAL_DIMENSION_PX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_png_writer_chunks_payload() {
        let mut out = Vec::new();
        let png = vec![0u8; 4096];

        write_kitty_png(&mut out, &png).unwrap();

        let out = String::from_utf8(out).unwrap();
        assert!(out.starts_with("\x1b_Ga=T,f=100,m=1;"));
        assert!(out.contains("\x1b_Gm=0;"));
        assert!(out.ends_with("\x1b\\"));
    }
}
