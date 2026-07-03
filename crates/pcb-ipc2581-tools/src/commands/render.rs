use std::io::Write;
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
    pub flat: bool,
}

/// Render processed geometry for one IPC-2581 layer.
pub fn execute(input_file: &Path, options: &RenderOptions) -> Result<()> {
    let target = resolve_target(options)?;
    let content = file_utils::load_ipc_file(input_file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let view = options.layout_target.geometry_view();
    let mut geometry = geometry::extract_layer_for_view(&ipc, &options.layer, view)?;
    pcb_ir::dialects::ipc::process::compose_for_rendering(&mut geometry);
    if options.flat {
        pcb_ir::dialects::ipc::process::flatten_layers_to_masks(&mut geometry);
    }

    match target {
        RenderTarget::Svg => render_svg(&geometry, options, view)?,
        RenderTarget::Png => render_png(&geometry, options, view)?,
        RenderTarget::Terminal => {
            let mask = geometry::render::layer_mask(&geometry, true, view.profile_set());
            pcb_ir::render::to_terminal(&mask, &pcb_ir::render::RenderOptions::default())
                .map_err(anyhow::Error::msg)?;
        }
    }

    for diagnostic in &geometry.diagnostics {
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
                    "Could not render IPC-2581 layer to stdout; run from an interactive terminal or pass --output <path>.svg or <path>.png"
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

fn render_svg(
    geometry: &pcb_ir::dialects::ipc::Document<ipc2581::Symbol, ipc2581::types::LayerFunction>,
    options: &RenderOptions,
    view: pcb_ir::dialects::ipc::View,
) -> Result<()> {
    let svg = geometry::render::render_layer_svg(geometry, true, view.profile_set());

    if let Some(output) = &options.output {
        std::fs::write(output, svg)
            .with_context(|| format!("Failed to write SVG to {}", output.display()))?;
        println!(
            "✓ IPC-2581 layer '{}' rendered to {}",
            options.layer,
            output.display()
        );
    } else {
        print!("{svg}");
    }

    Ok(())
}

fn render_png(
    geometry: &pcb_ir::dialects::ipc::Document<ipc2581::Symbol, ipc2581::types::LayerFunction>,
    options: &RenderOptions,
    view: pcb_ir::dialects::ipc::View,
) -> Result<()> {
    let mask = geometry::render::layer_mask(geometry, true, view.profile_set());
    let png = pcb_ir::render::png(&mask, &pcb_ir::render::RenderOptions::default())
        .map_err(anyhow::Error::msg)?;

    if let Some(output) = &options.output {
        std::fs::write(output, png)
            .with_context(|| format!("Failed to write PNG to {}", output.display()))?;
        println!(
            "✓ IPC-2581 layer '{}' rendered to {}",
            options.layer,
            output.display()
        );
    } else {
        std::io::stdout()
            .lock()
            .write_all(&png)
            .context("Failed to write PNG to stdout")?;
    }

    Ok(())
}
