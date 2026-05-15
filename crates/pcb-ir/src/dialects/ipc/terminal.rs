use std::io::{self, IsTerminal, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use terminal_size::{Width, terminal_size};

use crate::dialects::ipc::GeometryDocument;

const KITTY_CHUNK_SIZE: usize = 4096;
const MAX_TERMINAL_DIMENSION_PX: u32 = 1200;

pub fn can_render_to_terminal() -> bool {
    io::stdout().is_terminal()
}

pub fn render_layer_to_terminal<S, L, F>(
    doc: &GeometryDocument<S, L>,
    layer_index: usize,
    flat_style: F,
) -> Result<(), String>
where
    F: Copy + Fn(&L) -> (&'static str, f64),
{
    if !io::stdout().is_terminal() {
        return Err(
            "stdout is not an interactive terminal; pass --output <path>.svg or <path>.png"
                .to_string(),
        );
    }

    let png = super::raster::render_layer_png_with_max_dimension(
        doc,
        layer_index,
        terminal_max_dimension_px(),
        flat_style,
    )?;
    let mut stdout = io::stdout().lock();
    write_kitty_png(&mut stdout, &png).map_err(|err| err.to_string())?;
    stdout.write_all(b"\n").map_err(|err| err.to_string())?;
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
