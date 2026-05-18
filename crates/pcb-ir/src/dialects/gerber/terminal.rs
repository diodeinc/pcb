use std::io::{self, Write};

use crate::dialects::gerber::{GeometryDocument, lower_to_geom};
use crate::dialects::{geom, mask};

pub fn can_render_to_terminal() -> bool {
    mask::can_render_to_terminal()
}

pub fn render_to_terminal<A: Clone>(doc: &GeometryDocument<A>) -> Result<(), String> {
    let geom = lower_to_geom(doc);
    let mask = geom::lower_filled_to_mask(&geom);
    mask::render_to_terminal(&mask, 0)
}

pub fn write_kitty_png<W: Write>(writer: &mut W, png: &[u8]) -> io::Result<()> {
    mask::write_kitty_png(writer, png)
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
