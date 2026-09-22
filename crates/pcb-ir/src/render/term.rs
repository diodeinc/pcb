use std::io::{self, IsTerminal, Write};

use crate::render::{RenderOptions, SizeConstraint};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

const KITTY_CHUNK_SIZE: usize = 4096;
/// The cells of a window that does not say its size, and the pixels of a
/// cell of one that does not say those.
const DEFAULT_WINDOW_CELLS: (u16, u16) = (120, 40);
const DEFAULT_CELL_PX: (u32, u32) = (10, 20);

/// Whether stdout is a terminal that displays kitty graphics. Any other
/// terminal prints the image payload as text, so only ones known to speak
/// the protocol qualify.
pub fn can_render_to_terminal() -> bool {
    io::stdout().is_terminal() && speaks_kitty_graphics(|name| std::env::var(name).ok())
}

/// kitty, Ghostty and WezTerm announce themselves through the environment.
/// A multiplexer inherits those variables but swallows the graphics.
fn speaks_kitty_graphics(env: impl Fn(&str) -> Option<String>) -> bool {
    let is = |name: &str, values: &[&str]| {
        env(name).is_some_and(|value| values.contains(&value.as_str()))
    };
    env("TMUX").is_none()
        && (env("KITTY_WINDOW_ID").is_some()
            || is("TERM", &["xterm-kitty", "xterm-ghostty"])
            || is("TERM_PROGRAM", &["ghostty", "WezTerm"]))
}

/// Render artwork layers as an inline image using the kitty graphics
/// protocol. Any size constraint in `options` is replaced by the terminal
/// window's.
pub fn artwork_to_terminal<LayerMeta, ObjectMeta>(
    doc: &crate::dialects::artwork::Document<LayerMeta, ObjectMeta>,
    options: &RenderOptions,
) -> Result<(), String> {
    write_terminal_png(crate::render::artwork_png(doc, &terminal_options(options))?)
}

fn terminal_options(options: &RenderOptions) -> RenderOptions {
    let (width_px, height_px) = terminal_image_box_px();
    RenderOptions {
        size: SizeConstraint::Within {
            width_px,
            height_px,
        },
        ..options.clone()
    }
}

fn write_terminal_png(png: Vec<u8>) -> Result<(), String> {
    if !can_render_to_terminal() {
        return Err(
            "stdout is not a terminal with kitty graphics; pass an SVG or PNG output path"
                .to_string(),
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

/// The box an inline image fits: the window's width and two thirds of its
/// height, so the picture fills the terminal it is asked for in and stays on
/// screen with the command that drew it.
///
/// An image displays pixel for pixel, so the box is in the window's own
/// pixels, which a terminal that speaks kitty graphics reports.
fn terminal_image_box_px() -> (u32, u32) {
    let cells = crossterm::terminal::size().unwrap_or(DEFAULT_WINDOW_CELLS);
    let reported = crossterm::terminal::window_size()
        .ok()
        .map(|window| (u32::from(window.width), u32::from(window.height)))
        .filter(|&(width, height)| width > 0 && height > 0);
    let (width, height) = reported.unwrap_or((
        u32::from(cells.0) * DEFAULT_CELL_PX.0,
        u32::from(cells.1) * DEFAULT_CELL_PX.1,
    ));
    (width, height * 2 / 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_kitty_graphics_terminals_qualify() {
        let speaks = |vars: &[(&str, &str)]| {
            speaks_kitty_graphics(|name| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            })
        };

        assert!(speaks(&[("KITTY_WINDOW_ID", "1")]));
        assert!(speaks(&[("TERM", "xterm-kitty")]));
        assert!(speaks(&[("TERM", "xterm-ghostty")]));
        assert!(speaks(&[("TERM_PROGRAM", "WezTerm")]));
        assert!(!speaks(&[]));
        assert!(!speaks(&[
            ("TERM", "xterm-256color"),
            ("TERM_PROGRAM", "Apple_Terminal")
        ]));
        assert!(!speaks(&[("TERM_PROGRAM", "vscode")]));
        assert!(!speaks(&[
            ("KITTY_WINDOW_ID", "1"),
            ("TMUX", "/tmp/tmux-501/default,1,0")
        ]));
    }

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
