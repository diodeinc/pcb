//! A board as it looks from either side, drawn by
//! [`pcb_ir::render::composite`] from its fabrication files: Gerber X2
//! layers, the profile they share and Excellon drill files.
//!
//! A job file, when there is one, names each file's function and the inks
//! and finish to draw in. A file it does not name says its own function in
//! its `.FileFunction` attribute, or, lacking one, by its extension.

use std::collections::HashMap;

use anyhow::{Context, Result, anyhow};
use gerberx2::geometry::{GerberObjectMeta, extract_document, layer_role, layer_side};
use pcb_ir::dialects::LayerRole;
use pcb_ir::dialects::artwork::{Object, closed_outlines, import_layer_objects};
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
    // Each side's images by role, every file of a role drawn in turn.
    let mut images = HashMap::<_, Vec<_>>::new();
    let mut copper_files = 0;
    let mut profile = Vec::new();
    // Every drill and rout file's function and the holes it cuts.
    let mut drills = Vec::new();
    for file in files.iter().filter(|file| !is_job(&file.name)) {
        let listed = job.functions.get(base_name(&file.name));
        // A file states its own function; the job's names it for files that
        // cannot.
        let function = |stated: Vec<String>| {
            if stated.is_empty() {
                listed.cloned().unwrap_or_default()
            } else {
                stated
            }
        };
        // Fabrication files are ASCII; anything else can only be in comments.
        let text = String::from_utf8_lossy(&file.contents);
        if is_drill_file(&text) {
            let image =
                read_drill(&text).with_context(|| format!("Failed to read {}", file.name))?;
            let holes = image
                .holes
                .iter()
                .map(|hole| Object::new(Polarity::Dark, hole.to_artwork(&mut composite.artwork)));
            drills.push((function(image.file_function), holes.collect::<Vec<_>>()));
            continue;
        }
        let gerber = match gerberx2::GerberX2::parse(&text) {
            Ok(gerber) => gerber,
            // Archives carry readmes and drawings beside the artwork.
            Err(_) if listed.is_none() => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("Failed to parse {}", file.name));
            }
        };
        let document = extract_document(&gerber, resolution.accuracy)
            .with_context(|| format!("Failed to read {}", file.name))?;
        let function = function(document.layers[0].meta.clone());
        let role = layer_role(&function);
        if role == LayerRole::Profile {
            profile.extend(closed_outlines(&document, 0, JOIN_TOLERANCE_MM));
            continue;
        }
        let objects = import_layer_objects(&mut composite.artwork, &document, Clone::clone);
        let objects = objects.into_iter().flatten();
        composite.artwork.diagnostics.extend(document.diagnostics);
        if matches!(
            function.first().map(String::as_str),
            Some("Plated" | "NonPlated")
        ) {
            drills.push((function, objects.collect()));
            continue;
        }
        copper_files += usize::from(role == LayerRole::Copper);
        images
            .entry((role, layer_side(&function)))
            .or_default()
            .extend(objects);
    }

    let material = composite
        .material(profile)
        .context("The fabrication files have no profile layer with a closed board outline")?;
    let copper_layers = job.copper_layers.unwrap_or(copper_files.max(2));
    let mut faces = Vec::new();
    for &side in sides {
        let ir_side = side.ir_side();
        let image = |role| images.get(&(role, ir_side)).cloned().unwrap_or_default();
        let cutouts = drills
            .iter()
            .filter(|(function, _)| opens_onto(function, side, copper_layers))
            .flat_map(|(_, holes)| holes.iter().cloned())
            .collect();
        faces.push(Face {
            side: ir_side,
            style: job.style(side),
            copper: image(LayerRole::Copper),
            masked: images.contains_key(&(LayerRole::Soldermask, ir_side)),
            openings: image(LayerRole::Soldermask),
            legend: image(LayerRole::Legend),
            cutouts,
        });
    }
    composite
        .draw(&material, &Fabrication::default(), faces)
        .map_err(|error| anyhow!("invalid composite artwork: {error}"))?;
    Ok(composite)
}

fn base_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
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
    /// The function the job gives each file it lists, by name.
    functions: HashMap<String, Vec<String>>,
    copper_layers: Option<usize>,
    finish: Option<SurfaceFinishInfo>,
    /// Mask and legend colours, top then bottom.
    masks: [Option<String>; 2],
    legends: [Option<String>; 2],
}

impl Job {
    fn parse(contents: &[u8]) -> Result<Self> {
        let json: serde_json::Value = serde_json::from_slice(contents)?;
        let text = |value: &serde_json::Value, key: &str| {
            let text = value.get(key)?.as_str()?.trim();
            (!text.is_empty()).then(|| text.to_owned())
        };
        let functions = json["FilesAttributes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|file| {
                let function = text(file, "FileFunction")?;
                let function = function.split(',').map(str::to_owned).collect();
                Some((base_name(&text(file, "Path")?).to_owned(), function))
            })
            .collect();
        let specs = &json["GeneralSpecs"];
        let mut job = Self {
            functions,
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
        let ink = |colors: &[Option<String>; 2]| {
            colors[usize::from(side == BoardSide::Bottom)]
                .as_deref()
                .map(job_ink)
        };
        CompositeStyle::with_colors(
            ink(&self.masks).as_ref().and_then(mask_color),
            ink(&self.legends).and_then(|ink| Some(rgb(ink.rgb_color()?))),
            self.finish.as_ref().map(|finish| rgb(finish.rgb_color())),
        )
    }
}

/// A job colour, `(Red|…|Black|R<n>G<n>B<n>)` and an optional gloss after a
/// comma, as an ink.
fn job_ink(color: &str) -> ColorInfo {
    let color = color.split(',').next().unwrap_or_default().trim();
    let channels = || {
        let (red, rest) = color.strip_prefix('R')?.split_once('G')?;
        let (green, blue) = rest.split_once('B')?;
        Some((red.parse().ok()?, green.parse().ok()?, blue.parse().ok()?))
    };
    let rgb = channels();
    ColorInfo {
        name: rgb.is_none().then(|| color.to_owned()),
        rgb,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_colours_read_by_name_or_channels_whatever_their_gloss() {
        let job = Job::parse(
            br#"{"MaterialStackup": [
                {"Type": "Legend", "Color": "R10G20B30,Matte"},
                {"Type": "SolderMask", "Color": "Blue,Gloss"},
                {"Type": "Copper"},
                {"Type": "Dielectric"},
                {"Type": "Copper"},
                {"Type": "SolderMask", "Color": "Red"}
            ]}"#,
        )
        .unwrap();
        let (top, bottom) = (job.style(BoardSide::Top), job.style(BoardSide::Bottom));
        assert_eq!(top.legend.color, 0x0a141e);
        assert_eq!(Some(top.mask.color), CompositeStyle::mask_ink("blue"));
        assert_eq!(Some(bottom.mask.color), CompositeStyle::mask_ink("red"));
        assert_eq!(bottom.legend, CompositeStyle::default().legend);
    }
}
