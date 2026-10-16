//! A board as it looks from either side, drawn by
//! [`pcb_ir::render::composite`] from its fabrication files: Gerber X2
//! layers, the profile they share and Excellon drill files.
//!
//! A job file, when there is one, names each file's function and the inks
//! and finish to draw in. A file it does not name says its own function in
//! its `.FileFunction` attribute, or, lacking one, by its extension.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow, bail};
use gerberx2::geometry::{GerberObjectMeta, extract_document, layer_role, layer_side};
use pcb_ir::dialects::artwork::{Object, closed_outlines, import_layer_objects};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{Polarity, Resolution};
use pcb_ir::render::composite::{Composite, CompositeStyle, Fabrication, Face};

use super::{mask_color, rgb};
use crate::BoardSide;
use crate::accessors::{ColorInfo, SurfaceFinishCategory, SurfaceFinishInfo};
use crate::xnc::{is_drill_file, read_drill};

/// One fabrication file by name, as read from a directory or an archive.
#[derive(Debug, Clone)]
pub struct FabricationFile {
    pub name: String,
    pub contents: Vec<u8>,
}

/// How far apart two profile draws' ends may lie and still join.
const JOIN_TOLERANCE_MM: f64 = 1e-3;

/// Draw `sides` of the board `files` describe, left to right.
pub fn gerber_composite(
    files: &[FabricationFile],
    sides: &[BoardSide],
    resolution: Resolution,
) -> Result<Composite<GerberObjectMeta>> {
    let is_job = |name: &str| name.to_ascii_lowercase().ends_with(".gbrjob");
    let job = files
        .iter()
        .find(|file| is_job(&file.name))
        .map(|file| {
            Job::parse(&file.contents).with_context(|| format!("Failed to read {}", file.name))
        })
        .transpose()?
        .unwrap_or_default();
    let mut composite = Composite::new();
    let mut layers = Vec::new();
    let mut profile = Vec::new();
    let mut drills = Vec::new();
    for file in files.iter().filter(|file| !is_job(&file.name)) {
        let name = base_name(&file.name);
        let listed = job.files.get(name);
        let Ok(text) = std::str::from_utf8(&file.contents) else {
            if listed.is_some() {
                bail!("{} is not text", file.name);
            }
            continue;
        };
        let is_drill = listed
            .and_then(|listed| listed.format.as_deref())
            .map_or_else(
                || is_drill_file(text),
                |format| format.eq_ignore_ascii_case("XNC"),
            );
        if is_drill {
            let mut image =
                read_drill(text).with_context(|| format!("Failed to read {}", file.name))?;
            if let Some(function) = listed.and_then(|listed| listed.function.clone()) {
                image.file_function = function;
            }
            drills.push(image);
            continue;
        }
        let gerber = match gerberx2::GerberX2::parse(text) {
            Ok(gerber) => gerber,
            // Archives carry readmes and drawings beside the artwork.
            Err(_) if listed.is_none() => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("Failed to parse {}", file.name));
            }
        };
        let document = extract_document(&gerber, resolution.accuracy)
            .with_context(|| format!("Failed to read {}", file.name))?;
        let stated = &document.layers[0].meta;
        let function = listed
            .and_then(|listed| listed.function.clone())
            .or_else(|| (!stated.is_empty()).then(|| stated.clone()))
            .unwrap_or_else(|| function_by_extension(name));
        let role = layer_role(&function);
        if role == LayerRole::Profile {
            profile.extend(closed_outlines(&document, 0, JOIN_TOLERANCE_MM));
        }
        let mut objects = import_layer_objects(&mut composite.artwork, &document, Clone::clone);
        composite.artwork.diagnostics.extend(document.diagnostics);
        layers.push(Layer {
            role,
            side: layer_side(&function),
            objects: objects.swap_remove(0),
        });
    }

    let material = composite
        .material(profile)
        .context("The fabrication files have no profile layer with a closed board outline")?;
    let copper_layers = job.copper_layers.unwrap_or_else(|| {
        let copper = layers
            .iter()
            .filter(|layer| layer.role == LayerRole::Copper);
        copper.count().max(2)
    });
    let images = |role: LayerRole, side: Side| {
        layers
            .iter()
            .filter(move |layer| layer.role == role && layer.side == side)
    };
    let objects = |role: LayerRole, side: Side| {
        images(role, side)
            .flat_map(|layer| layer.objects.iter().cloned())
            .collect::<Vec<_>>()
    };
    let mut faces = Vec::new();
    for &side in sides {
        let ir_side = side.ir_side();
        let cutouts = drills
            .iter()
            .filter(|drill| opens_onto(&drill.file_function, side, copper_layers))
            .flat_map(|drill| &drill.holes)
            .map(|hole| Object::new(Polarity::Dark, hole.to_artwork(&mut composite.artwork)))
            .collect();
        faces.push(Face {
            side: ir_side,
            style: job.style(side),
            copper: objects(LayerRole::Copper, ir_side),
            masked: images(LayerRole::Soldermask, ir_side).next().is_some(),
            openings: objects(LayerRole::Soldermask, ir_side),
            legend: objects(LayerRole::Legend, ir_side),
            cutouts,
        });
    }
    composite
        .draw(&material, &Fabrication::default(), faces)
        .map_err(|error| anyhow!("invalid composite artwork: {error}"))?;
    Ok(composite)
}

/// One Gerber file's artwork in the composite's tables.
struct Layer {
    role: LayerRole,
    side: Side,
    objects: Vec<Object<GerberObjectMeta>>,
}

fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// The function the common extensions give a file without a
/// `.FileFunction` attribute.
fn function_by_extension(name: &str) -> Vec<String> {
    let extension = name.rsplit('.').next().unwrap_or_default();
    let function: &[&str] = match extension.to_ascii_lowercase().as_str() {
        "gtl" => &["Copper", "L1", "Top"],
        "gbl" => &["Copper", "Bot"],
        "gts" => &["Soldermask", "Top"],
        "gbs" => &["Soldermask", "Bot"],
        "gto" => &["Legend", "Top"],
        "gbo" => &["Legend", "Bot"],
        "gtp" => &["Paste", "Top"],
        "gbp" => &["Paste", "Bot"],
        "gm1" | "gko" | "gml" | "gm" => &["Profile", "NP"],
        _ => &[],
    };
    function.iter().map(|field| field.to_string()).collect()
}

/// Whether a drill file of this `.FileFunction` opens onto `side`: its
/// span, `Plated,1,4,PTH` for one, starts at the top copper or ends at the
/// bottom's. A file that names no span goes through.
fn opens_onto(function: &[String], side: BoardSide, copper_layers: usize) -> bool {
    let layer = |index: usize| function.get(index)?.parse::<usize>().ok();
    match (layer(1), layer(2)) {
        (Some(from), Some(to)) => match side {
            BoardSide::Top => from.min(to) == 1,
            BoardSide::Bottom => from.max(to) == copper_layers,
        },
        _ => true,
    }
}

/// What a Gerber job file says of the files beside it.
#[derive(Debug, Default)]
struct Job {
    files: HashMap<String, ListedFile>,
    copper_layers: Option<usize>,
    finish: Option<SurfaceFinishInfo>,
    /// Mask and legend colours, top then bottom.
    masks: [Option<String>; 2],
    legends: [Option<String>; 2],
}

/// A file the job lists, and what it says of it. What it leaves out the
/// file says itself.
#[derive(Debug)]
struct ListedFile {
    format: Option<String>,
    function: Option<Vec<String>>,
}

impl Job {
    fn parse(contents: &[u8]) -> Result<Self> {
        let json: serde_json::Value = serde_json::from_slice(contents)?;
        let text = |value: &serde_json::Value, key: &str| {
            let text = value.get(key)?.as_str()?.trim();
            (!text.is_empty()).then(|| text.to_owned())
        };
        let files = json["FilesAttributes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|file| {
                let listed = ListedFile {
                    format: text(file, "FileFormat"),
                    function: text(file, "FileFunction")
                        .map(|function| function.split(',').map(str::to_owned).collect()),
                };
                Some((base_name(&text(file, "Path")?).to_owned(), listed))
            })
            .collect();
        let specs = &json["GeneralSpecs"];
        let mut job = Self {
            files,
            copper_layers: specs["LayerNumber"].as_u64().map(|count| count as usize),
            finish: text(specs, "Finish").map(|name| SurfaceFinishInfo {
                name,
                category: SurfaceFinishCategory::Other,
            }),
            ..Self::default()
        };
        // The stackup runs top to bottom: what lies above the first copper
        // is the top's, what lies below the last the bottom's.
        let stackup = json["MaterialStackup"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let kinds = stackup
            .iter()
            .map(|layer| text(layer, "Type").unwrap_or_default().to_lowercase())
            .collect::<Vec<_>>();
        let first = kinds.iter().position(|kind| kind == "copper");
        let last = kinds.iter().rposition(|kind| kind == "copper");
        for (index, (layer, kind)) in stackup.iter().zip(&kinds).enumerate() {
            let side = match (first, last) {
                (Some(first), _) if index < first => 0,
                (_, Some(last)) if index > last => 1,
                _ => continue,
            };
            let colors = match kind.as_str() {
                "soldermask" => &mut job.masks,
                "legend" => &mut job.legends,
                _ => continue,
            };
            colors[side] = colors[side].take().or_else(|| text(layer, "Color"));
        }
        Ok(job)
    }

    /// The colours the job gives `side`.
    fn style(&self, side: BoardSide) -> CompositeStyle {
        let ink = |colors: &[Option<String>; 2]| ColorInfo {
            name: colors[usize::from(side == BoardSide::Bottom)].clone(),
            rgb: None,
        };
        CompositeStyle::with_colors(
            mask_color(&ink(&self.masks)),
            ink(&self.legends).rgb_color().map(rgb),
            self.finish.as_ref().map(|finish| rgb(finish.rgb_color())),
        )
    }
}
