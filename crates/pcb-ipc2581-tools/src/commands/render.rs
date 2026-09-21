use pcb_ir::geom::Resolution;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::geometry;
use crate::utils::file as file_utils;
use crate::{LayoutTarget, RenderFormat, ipc2581};

/// Options for rendering processed geometry from a single IPC-2581 layer.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub layer: String,
    pub output: Option<PathBuf>,
    pub format: RenderFormat,
    pub layout_target: LayoutTarget,
}

/// Render processed geometry for one IPC-2581 layer.
///
/// The layer runs through the same normalization Gerber export uses, so a
/// render and a fabrication file describe the same image.
pub fn execute(input_file: &Path, options: &RenderOptions, resolution: Resolution) -> Result<()> {
    let target = resolve_target(options)?;
    let content = file_utils::load_ipc_file(input_file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let view = options.layout_target.artwork_scope();
    let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution)?;
    let artwork =
        geometry::render::layer_artwork(&imported, &options.layer, view, true, resolution)?.artwork;
    let render = pcb_ir::render::RenderOptions::default().with_accuracy(resolution.accuracy);

    match target {
        RenderTarget::Svg => write_output(
            options,
            "SVG",
            pcb_ir::render::artwork_svg(&artwork, &render)?.as_bytes(),
        )?,
        RenderTarget::Png => write_output(
            options,
            "PNG",
            &pcb_ir::render::artwork_png(&artwork, &render).map_err(anyhow::Error::msg)?,
        )?,
        RenderTarget::Terminal => {
            pcb_ir::render::artwork_to_terminal(&artwork, &render).map_err(anyhow::Error::msg)?
        }
    }

    for diagnostic in &artwork.diagnostics {
        eprintln!("warning: {}", diagnostic.message);
    }

    Ok(())
}

enum RenderTarget {
    Svg,
    Png,
    Terminal,
}

fn resolve_target(options: &RenderOptions) -> Result<RenderTarget> {
    match options.format {
        RenderFormat::Auto => {
            if let Some(output) = &options.output {
                infer_format_from_output(output)
            } else if pcb_ir::render::can_render_to_terminal() {
                Ok(RenderTarget::Terminal)
            } else {
                bail!(
                    "Could not render IPC-2581 layer to stdout; run from a terminal with kitty graphics (kitty, Ghostty, WezTerm) or pass --output <path>.svg or <path>.png"
                )
            }
        }
        RenderFormat::Svg => Ok(RenderTarget::Svg),
        RenderFormat::Png => Ok(RenderTarget::Png),
    }
}

fn infer_format_from_output(output: &Path) -> Result<RenderTarget> {
    match output
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("svg") => Ok(RenderTarget::Svg),
        Some("png") => Ok(RenderTarget::Png),
        _ => bail!(
            "Could not infer IPC-2581 render format from {}; pass --format svg or --format png",
            output.display()
        ),
    }
}

fn write_output(options: &RenderOptions, format: &str, contents: &[u8]) -> Result<()> {
    if let Some(output) = &options.output {
        std::fs::write(output, contents)
            .with_context(|| format!("Failed to write {format} to {}", output.display()))?;
        println!(
            "✓ IPC-2581 layer '{}' rendered to {}",
            options.layer,
            output.display()
        );
    } else {
        pcb_ui::write_stdout(|stdout| stdout.write_all(contents))
            .with_context(|| format!("Failed to write {format} to stdout"))?;
    }
    Ok(())
}
