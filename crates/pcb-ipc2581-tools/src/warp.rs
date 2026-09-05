//! Panel warp read off an IPC-2581 fabrication panel.
//!
//! The physics lives in [`pcb_ir::geom::warp`]; this is the adapter that
//! supplies it with a stackup and the per-layer copper it needs.

use anyhow::{Context, Result, bail};
use pcb_ir::geom::GeometryAccuracy;
use pcb_ir::geom::warp::{
    LAMINATE_RELAXATION_DROP_K, Material, PanelField, StackLayer, ThermalStack, WarpEstimate,
    estimate_warp,
};
use pcb_ir::geom::{BBox, ContourSet, Point};

use crate::ipc2581::Ipc2581;
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::import::ipc2581::import_design;

/// Cells across the panel's longer side.
///
/// The shapes the estimate resolves are defined on the panel rather than in
/// millimetres, so what the sampling has to be fine enough for is a fraction of
/// the panel, not a fixed distance. Fixing the count instead of the pitch also
/// makes the fields legible at every panel size, where one pitch either wastes
/// work on a large panel or renders a small one in a handful of cells.
const SAMPLES_ACROSS: f64 = 96.0;

/// Floor on the sampling pitch, in millimetres.
///
/// Below the pitch of the thieving lattice a cell stops reporting a density and
/// starts reporting whether it happened to land on a void. The model wants the
/// density.
const MIN_SAMPLE_PITCH_MM: f64 = 2.0;

/// One copper layer's coverage across the panel.
pub struct LayerCoverage {
    pub layer_name: String,
    /// Fraction of each sample cell covered by copper.
    pub coverage: Vec<f64>,
    /// Mean coverage over the panel.
    pub mean: f64,
}

/// Everything the report needs about one panel.
pub struct WarpAnalysis {
    pub stack: ThermalStack,
    /// Sample positions shared by every field below.
    pub samples: Vec<Point>,
    pub bounds: BBox,
    pub layers: Vec<LayerCoverage>,
    /// `sum_l t_l z_l rho_l(x)`, the geometric copper moment.
    pub moment: PanelField,
    pub warp: WarpEstimate,
    pub temperature_drop_k: f64,
    /// Side of the square cell each coverage value was measured over.
    pub sample_pitch_mm: f64,
}

/// Analyze the panel described by `ipc`.
///
/// Requires a stackup carrying a thickness for every layer: without it there is
/// no neutral axis, no lever arms, and nothing to estimate.
pub fn analyze(ipc: &Ipc2581, accuracy: GeometryAccuracy) -> Result<WarpAnalysis> {
    let imported = import_design(ipc, accuracy)?;
    let (stack, copper_names) = physical_stack(ipc)?;
    let conductors = stack.conductor_weights();
    let bounds = panel_bounds(ipc, accuracy)?;
    let sample_pitch_mm =
        (bounds.width().max(bounds.height()) / SAMPLES_ACROSS).max(MIN_SAMPLE_PITCH_MM);
    let columns = (bounds.width() / sample_pitch_mm).ceil().max(1.0) as usize;
    let rows = (bounds.height() / sample_pitch_mm).ceil().max(1.0) as usize;
    let samples = grid(bounds, columns, rows);

    let layers = copper_names
        .iter()
        .map(|layer_name| {
            let image: ContourSet = imported
                .composed_layer_image(
                    imported
                        .layer_id(layer_name)
                        .context("missing copper layer")?,
                    ArtworkScope::ArrayFlattened,
                    accuracy,
                )
                .with_context(|| format!("failed to extract copper on layer '{layer_name}'"))?;
            let coverage = image.grid_coverage(bounds, columns, rows);
            let mean = coverage.iter().sum::<f64>() / coverage.len() as f64;
            Ok(LayerCoverage {
                layer_name: layer_name.clone(),
                coverage,
                mean,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let values = (0..samples.len())
        .map(|sample| {
            layers
                .iter()
                .zip(&conductors)
                .map(|(layer, conductor)| conductor.moment_arm_mm2 * layer.coverage[sample])
                .sum()
        })
        .collect::<Vec<f64>>();
    let moment =
        PanelField::new(samples.clone(), values, bounds).context("panel has no area to sample")?;

    let warp = estimate_warp(
        &stack,
        Material::LAMINATE,
        &moment,
        LAMINATE_RELAXATION_DROP_K,
    );
    Ok(WarpAnalysis {
        stack,
        samples,
        bounds,
        layers,
        moment,
        warp,
        temperature_drop_k: LAMINATE_RELAXATION_DROP_K,
        sample_pitch_mm,
    })
}

/// The physical stackup, in order, with conductors marked, and the names of its
/// conductor layers in that same order.
///
/// Both come from the stackup so a lever arm cannot land on the wrong layer:
/// the ECAD layer list carries the same layers, but nothing obliges it to carry
/// them in the build order the stackup describes. This is the one place the
/// stackup becomes physics -- the balance solver's stack weights come through
/// here too, so what balancing optimizes is exactly what warp measures.
pub(crate) fn physical_stack(ipc: &Ipc2581) -> Result<(ThermalStack, Vec<String>)> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let stackup = ecad
        .cad_data
        .stackups
        .first()
        .context("IPC-2581 file carries no physical stackup")?;
    let mut ordered = stackup.layers.iter().collect::<Vec<_>>();
    if ordered.iter().all(|layer| layer.layer_number.is_some()) {
        ordered.sort_by_key(|layer| layer.layer_number);
    }

    // Silkscreen and coating are carried in the stackup at zero thickness. They
    // have no lever arm and no stiffness, so they are not part of the laminate
    // and are dropped. A thickness that is absent rather than zero is a
    // different matter: the build is then unknown and nothing can be estimated.
    let build = ordered
        .iter()
        .map(|layer| {
            let thickness_mm = layer
                .thickness
                .context("physical stackup is missing a thickness for at least one layer")?;
            if !thickness_mm.is_finite() || thickness_mm < 0.0 {
                bail!("physical stackup carries a negative layer thickness");
            }
            let name = ipc.resolve(layer.layer_ref);
            let is_conductor = ecad.cad_data.layers.iter().any(|ecad_layer| {
                ipc.resolve(ecad_layer.name) == name
                    && crate::layers::is_copper(ecad_layer.layer_function)
            });
            Ok((thickness_mm > 0.0).then(|| {
                (
                    StackLayer {
                        thickness_mm,
                        material: if is_conductor {
                            Material::COPPER
                        } else {
                            Material::LAMINATE
                        },
                        is_conductor,
                    },
                    name.to_string(),
                )
            }))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    let copper_names = build
        .iter()
        .filter(|(layer, _)| layer.is_conductor)
        .map(|(_, name)| name.clone())
        .collect();
    let layers = build.into_iter().map(|(layer, _)| layer).collect();
    let stack = ThermalStack::new(layers).context("physical stackup describes no usable layers")?;
    Ok((stack, copper_names))
}

fn panel_bounds(ipc: &Ipc2581, accuracy: GeometryAccuracy) -> Result<BBox> {
    let layout =
        crate::geometry::extract_layout(ipc).context("failed to extract the panel outline")?;
    let profile = crate::geometry::board_array_fabrication_profile(ipc, &layout, &[], accuracy)
        .context("failed to derive the panel profile")?;
    let outline = ContourSet::from_filled_contours(
        &profile
            .array_outlines
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>(),
        pcb_ir::geom::tol::REGION_MM,
        accuracy,
    )?;
    if outline.is_empty() {
        bail!("panel has no outline to measure");
    }
    Ok(outline.bbox)
}

/// Centres of the cells [`ContourSet::grid_coverage`] measures.
fn grid(bounds: BBox, columns: usize, rows: usize) -> Vec<Point> {
    (0..rows)
        .flat_map(|row| {
            (0..columns).map(move |column| {
                Point::new(
                    bounds.min.x + bounds.width() * (column as f64 + 0.5) / columns as f64,
                    bounds.min.y + bounds.height() * (row as f64 + 0.5) / rows as f64,
                )
            })
        })
        .collect()
}
