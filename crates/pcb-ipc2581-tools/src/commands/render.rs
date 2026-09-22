use pcb_ir::geom::Resolution;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::geometry;
use crate::utils::file as file_utils;
use crate::{LayoutTarget, RenderFormat, ipc2581};

/// Options for rendering processed geometry from a single IPC-2581 layer.
#[derive(Debug, Clone)]
pub struct LayerRenderOptions {
    pub layer: String,
    pub output: Option<PathBuf>,
    pub format: RenderFormat,
    pub layout_target: LayoutTarget,
}

/// Render processed geometry for one IPC-2581 layer.
///
/// The layer runs through the same normalization Gerber export uses, so a
/// render and a fabrication file describe the same image.
pub fn execute(
    input_file: &Path,
    options: &LayerRenderOptions,
    resolution: Resolution,
) -> Result<()> {
    let subject = format!("IPC-2581 layer '{}'", options.layer);
    let target = RenderTarget::resolve(options.output.as_deref(), options.format, &subject)?;
    let content = file_utils::load_ipc_file(input_file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let view = options.layout_target.artwork_scope();
    let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution)?;
    let artwork =
        geometry::render::layer_artwork(&imported, &options.layer, view, true, resolution)?.artwork;
    render_artwork(
        &artwork,
        target,
        options.output.as_deref(),
        &subject,
        &pcb_ir::render::RenderOptions::default().with_accuracy(resolution.accuracy),
    )
}

/// Where a render goes, settled before any geometry is loaded.
#[derive(Debug, Clone, Copy)]
pub enum RenderTarget {
    Svg,
    Png,
    Terminal,
}

impl RenderTarget {
    /// The target `format` names, inferring it from the output's extension
    /// or the terminal's abilities when left to `Auto`.
    pub fn resolve(output: Option<&Path>, format: RenderFormat, subject: &str) -> Result<Self> {
        match (format, output) {
            (RenderFormat::Svg, _) => Ok(Self::Svg),
            (RenderFormat::Png, _) => Ok(Self::Png),
            (RenderFormat::Auto, Some(output)) => match output
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("svg") => Ok(Self::Svg),
                Some("png") => Ok(Self::Png),
                _ => bail!(
                    "Could not infer the render format from {}; pass --format svg or --format png",
                    output.display()
                ),
            },
            (RenderFormat::Auto, None) if pcb_ir::render::can_render_to_terminal() => {
                Ok(Self::Terminal)
            }
            (RenderFormat::Auto, None) => bail!(
                "Could not render {subject} to stdout; run from a terminal with kitty graphics (kitty, Ghostty, WezTerm) or pass --output <path>.svg or <path>.png"
            ),
        }
    }
}

/// Draw `artwork` to `target`, warning of what the artwork could not carry.
pub fn render_artwork<LayerMeta: Clone, ObjectMeta: Clone>(
    artwork: &pcb_ir::dialects::artwork::Document<LayerMeta, ObjectMeta>,
    target: RenderTarget,
    output: Option<&Path>,
    subject: &str,
    options: &pcb_ir::render::RenderOptions,
) -> Result<()> {
    for diagnostic in &artwork.diagnostics {
        eprintln!("warning: {}", diagnostic.message);
    }
    let (format, image) = match target {
        RenderTarget::Svg => (
            "SVG",
            pcb_ir::render::artwork_svg(artwork, options)?.into_bytes(),
        ),
        RenderTarget::Png => (
            "PNG",
            pcb_ir::render::artwork_png(artwork, options).map_err(anyhow::Error::msg)?,
        ),
        RenderTarget::Terminal => {
            return pcb_ir::render::artwork_to_terminal(artwork, options)
                .map_err(anyhow::Error::msg);
        }
    };
    match output {
        Some(output) => {
            std::fs::write(output, image)
                .with_context(|| format!("Failed to write {format} to {}", output.display()))?;
            println!("✓ {subject} rendered to {}", output.display());
        }
        None => pcb_ui::write_stdout(|stdout| stdout.write_all(&image))
            .with_context(|| format!("Failed to write {format} to stdout"))?,
    }
    Ok(())
}
