use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::geometry;
use crate::utils::file as file_utils;
use crate::{RenderFormat, ipc2581};

/// Options for rendering processed geometry from a single IPC-2581 layer.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub layer: String,
    pub output: Option<PathBuf>,
    pub format: RenderFormat,
    pub flat: bool,
}

/// Render processed geometry for one IPC-2581 layer.
pub fn execute(input_file: &Path, options: &RenderOptions) -> Result<()> {
    let target = resolve_target(options)?;
    let content = file_utils::load_ipc_file(input_file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let mut geometry = geometry::extract_layer(&ipc, &options.layer)?;
    pcb_ir::dialects::ipc::process::process_document(&mut geometry);
    if options.flat {
        pcb_ir::dialects::ipc::process::flatten_layers_to_masks(&mut geometry);
    }

    match target {
        RenderTarget::Svg => render_svg(&geometry, options)?,
        RenderTarget::Png => render_png(&geometry, options)?,
        RenderTarget::Terminal => {
            let mask = common_mask(&geometry);
            pcb_ir::dialects::mask::render_layers_to_terminal(&mask, &visible_layers(&mask))
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
            } else if pcb_ir::dialects::mask::can_render_to_terminal() {
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
    geometry: &pcb_ir::dialects::ipc::GeometryDocument<
        ipc2581::Symbol,
        ipc2581::types::LayerFunction,
    >,
    options: &RenderOptions,
) -> Result<()> {
    let mask = common_mask(geometry);
    let svg = pcb_ir::dialects::mask::render_svg_layers(&mask, &visible_layers(&mask));

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
    geometry: &pcb_ir::dialects::ipc::GeometryDocument<
        ipc2581::Symbol,
        ipc2581::types::LayerFunction,
    >,
    options: &RenderOptions,
) -> Result<()> {
    let mask = common_mask(geometry);
    let png = pcb_ir::dialects::mask::render_png_layers(&mask, &visible_layers(&mask))
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

fn common_mask(
    geometry: &pcb_ir::dialects::ipc::GeometryDocument<
        ipc2581::Symbol,
        ipc2581::types::LayerFunction,
    >,
) -> pcb_ir::dialects::mask::MaskDocument<ipc2581::types::LayerFunction> {
    let layer = &geometry.layers[0];
    let geom = pcb_ir::dialects::ipc::lower_layer_with_board_outlines_to_geom(
        geometry,
        0,
        common_layer_role(layer.layer_function),
        pcb_ir::common::Side::None,
    );
    pcb_ir::dialects::geom::lower_filled_to_mask(&pcb_ir::dialects::geom::outline_strokes(geom))
}

fn visible_layers<LayerMeta>(mask: &pcb_ir::dialects::mask::MaskDocument<LayerMeta>) -> Vec<usize> {
    (0..mask.layers.len()).collect()
}

fn common_layer_role(function: ipc2581::types::LayerFunction) -> pcb_ir::common::LayerRole {
    use ipc2581::types::LayerFunction;
    match function {
        LayerFunction::Conductor
        | LayerFunction::CondFilm
        | LayerFunction::CondFoil
        | LayerFunction::Plane
        | LayerFunction::Signal
        | LayerFunction::Mixed => pcb_ir::common::LayerRole::Copper,
        LayerFunction::Solderpaste | LayerFunction::Pastemask => pcb_ir::common::LayerRole::Paste,
        LayerFunction::Soldermask => pcb_ir::common::LayerRole::Soldermask,
        LayerFunction::Silkscreen | LayerFunction::Legend => pcb_ir::common::LayerRole::Legend,
        LayerFunction::Rout | LayerFunction::BoardOutline => pcb_ir::common::LayerRole::Profile,
        _ => pcb_ir::common::LayerRole::Other,
    }
}
