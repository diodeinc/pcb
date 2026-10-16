//! A board composite from its fabrication files: Gerber X2 layers, the
//! profile they share and Excellon drill files, drawn as a design's
//! composite is.
//!
//! A job file, when there is one, names each file's function and the inks
//! and finish to draw in. A file it does not name says its own function in
//! its `.FileFunction` attribute, or, lacking one, by its extension.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use gerberx2::geometry::{GerberObjectMeta, extract_document, layer_role, layer_side};
use pcb_ir::dialects::artwork::{self, Aperture, Geometry, import_layer_objects};
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{
    BBox, ContourBuf, LineCap, Paint, PathCmd, PathOp, Polarity, Resolution, StrokeStyle,
};

use super::{Composite, CompositeObject, CompositeStyle, Surface, material};
use crate::BoardSide;
use crate::accessors::{ColorInfo, SurfaceFinishCategory, SurfaceFinishInfo};
use crate::xnc::{DrillHole, read_drill};

type Object = CompositeObject<GerberObjectMeta>;

/// One fabrication file by name, as read from a directory or an archive.
#[derive(Debug, Clone)]
pub struct FabricationFile {
    pub name: String,
    pub contents: Vec<u8>,
}

/// How far apart two profile ends may lie and still join.
const JOIN_TOLERANCE_MM: f64 = 1e-3;

/// Draw `sides` of the board `files` describe, left to right, as
/// [`super::composite_artwork`] draws a design.
pub fn gerber_composite(
    files: &[FabricationFile],
    sides: &[BoardSide],
    resolution: Resolution,
) -> Result<Composite<GerberObjectMeta>> {
    let job = files
        .iter()
        .find(|file| file.name.to_ascii_lowercase().ends_with(".gbrjob"))
        .map(|file| {
            Job::parse(&file.contents).with_context(|| format!("Failed to read {}", file.name))
        })
        .transpose()?
        .unwrap_or_default();
    let mut composite = Composite::new();
    let mut layers = Vec::new();
    let mut drills = Vec::new();
    for file in files {
        let name = base_name(&file.name);
        if name.to_ascii_lowercase().ends_with(".gbrjob") {
            continue;
        }
        let listed = job.files.get(name);
        let Ok(text) = std::str::from_utf8(&file.contents) else {
            if listed.is_some() {
                bail!("{} is not text", file.name);
            }
            continue;
        };
        let is_drill = listed.map_or_else(
            || text.trim_start().starts_with("M48"),
            |listed| listed.format.eq_ignore_ascii_case("XNC"),
        );
        if is_drill {
            let mut image =
                read_drill(text).with_context(|| format!("Failed to read {}", file.name))?;
            if let Some(listed) = listed {
                image.file_function = listed.function.clone();
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
        let function = match listed {
            Some(listed) => listed.function.clone(),
            None if !stated.is_empty() => stated.clone(),
            None => function_by_extension(name),
        };
        let profile = (layer_role(&function) == LayerRole::Profile)
            .then(|| profile_contours(&artwork::expand_instances(&document)));
        let objects = import_layer_objects(&mut composite.artwork, &document, Clone::clone)
            .into_iter()
            .next()
            .unwrap_or_default();
        composite.artwork.diagnostics.extend(document.diagnostics);
        layers.push(Layer {
            role: layer_role(&function),
            side: layer_side(&function),
            objects,
            profile,
        });
    }

    let profile = layers
        .iter_mut()
        .filter_map(|layer| layer.profile.take())
        .flatten()
        .collect::<Vec<_>>();
    let bounds = profile
        .iter()
        .map(|contour| contour.bbox)
        .fold(BBox::empty(), BBox::union);
    if bounds.is_empty() {
        bail!("The fabrication files have no profile layer with a closed board outline");
    }
    let material = material(&mut composite.artwork, bounds, profile);

    let copper_layers = job.copper_layers.unwrap_or_else(|| {
        let copper = layers
            .iter()
            .filter(|layer| layer.role == LayerRole::Copper);
        copper.count().max(2)
    });
    let objects = |role: LayerRole, side: Side| {
        layers
            .iter()
            .filter(move |layer| layer.role == role && layer.side == side)
            .flat_map(|layer| layer.objects.iter().cloned())
    };
    composite.draw_sides(sides, &material, &[], &[], |artwork, side| {
        let ir_side = side.ir_side();
        let cutouts = drills
            .iter()
            .filter(|drill| opens_onto(&drill.file_function, side, copper_layers))
            .flat_map(|drill| &drill.holes)
            .map(|hole| hole_object(artwork, hole))
            .collect();
        let masked = layers
            .iter()
            .any(|layer| layer.role == LayerRole::Soldermask && layer.side == ir_side);
        let surface = Surface {
            copper: objects(LayerRole::Copper, ir_side).collect(),
            masked,
            openings: objects(LayerRole::Soldermask, ir_side).collect(),
            legend: objects(LayerRole::Legend, ir_side).collect(),
            cutouts,
        };
        let style = CompositeStyle::of_inks(
            job.mask(side).as_ref(),
            job.legend(side).as_ref(),
            job.finish.as_ref(),
        );
        Ok((surface, style))
    })?;
    Ok(composite)
}

/// One Gerber file's artwork in the composite's tables.
struct Layer {
    role: LayerRole,
    side: Side,
    objects: Vec<Object>,
    /// A profile layer's closed outlines.
    profile: Option<Vec<ContourBuf>>,
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

/// A hole as the cut it makes: a round tool's flash, or its stroke from one
/// end of a slot to the other.
fn hole_object(
    artwork: &mut super::CompositeDocument<GerberObjectMeta>,
    hole: &DrillHole,
) -> Object {
    let geometry = if hole.start.distance_to(hole.end) <= 1e-9 {
        let aperture = artwork.push_aperture(Aperture::circle(hole.diameter));
        Geometry::Flash {
            aperture,
            transform: pcb_ir::geom::Affine2::translation(hole.start),
        }
    } else {
        let tool = Paint::Stroke(StrokeStyle::new(hole.diameter, LineCap::Round));
        let centerline = ContourBuf::new(vec![
            PathCmd::move_to(hole.start),
            PathCmd::line_to(hole.end),
        ]);
        Geometry::Stroke {
            path: artwork.push_path(tool, [centerline]),
        }
    };
    Object::new(Polarity::Dark, geometry)
}

/// The closed outlines a profile layer draws. Its draws join end to end
/// into loops, as an outline drawn one segment at a time must; a run that
/// never closes bounds nothing and is left out.
fn profile_contours<LayerMeta>(
    document: &artwork::Document<LayerMeta, GerberObjectMeta>,
) -> Vec<ContourBuf> {
    let mut open = Vec::new();
    for object in &document.objects {
        if object.polarity != Polarity::Dark {
            continue;
        }
        let (Geometry::Stroke { path } | Geometry::Region { path }) = object.geometry else {
            continue;
        };
        let contours = document.arena.path(path).contours;
        open.extend(
            document
                .arena
                .contour_bufs(contours)
                .into_iter()
                .map(|contour| {
                    let cmds = contour.cmds;
                    cmds.into_iter()
                        .filter(|cmd| cmd.op != PathOp::Close)
                        .collect::<Vec<_>>()
                }),
        );
    }
    open.retain(|cmds| cmds.len() > 1);
    let mut closed = Vec::new();
    while let Some(mut run) = open.pop() {
        loop {
            let (start, end) = (run[0].p0, run[run.len() - 1].p0);
            if start.distance_to(end) <= JOIN_TOLERANCE_MM {
                closed.push(ContourBuf::new(run));
                break;
            }
            let next = open.iter().position(|cmds| {
                let ends = [cmds[0].p0, cmds[cmds.len() - 1].p0];
                ends.iter()
                    .any(|at| at.distance_to(end) <= JOIN_TOLERANCE_MM)
            });
            let Some(next) = next else { break };
            let next = open.swap_remove(next);
            let next = if next[0].p0.distance_to(end) <= JOIN_TOLERANCE_MM {
                next
            } else {
                reversed(&next)
            };
            run.extend_from_slice(&next[1..]);
        }
    }
    closed
}

/// An open run of commands drawn from its end back to its start.
fn reversed(cmds: &[PathCmd]) -> Vec<PathCmd> {
    let mut out = vec![PathCmd::move_to(cmds[cmds.len() - 1].p0)];
    for pair in cmds.windows(2).rev() {
        out.push(PathCmd {
            p0: pair[0].p0,
            clockwise: !pair[1].clockwise,
            ..pair[1]
        });
    }
    out
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

#[derive(Debug)]
struct ListedFile {
    format: String,
    function: Vec<String>,
}

impl Job {
    fn parse(contents: &[u8]) -> Result<Self> {
        let json: serde_json::Value = serde_json::from_slice(contents)?;
        let text = |value: &serde_json::Value, key: &str| {
            value
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        };
        let files = json["FilesAttributes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|file| {
                let path = text(file, "Path")?;
                let function = text(file, "FileFunction").unwrap_or_default();
                let listed = ListedFile {
                    format: text(file, "FileFormat").unwrap_or_default(),
                    function: function.split(',').map(str::to_owned).collect(),
                };
                Some((base_name(&path).to_owned(), listed))
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
        let kind =
            |layer: &serde_json::Value| text(layer, "Type").unwrap_or_default().to_lowercase();
        let copper = stackup.iter().map(kind).collect::<Vec<_>>();
        let first = copper.iter().position(|kind| kind == "copper");
        let last = copper.iter().rposition(|kind| kind == "copper");
        for (index, layer) in stackup.iter().enumerate() {
            let side = match (first, last) {
                (Some(first), _) if index < first => 0,
                (_, Some(last)) if index > last => 1,
                _ => continue,
            };
            let colors = match kind(layer).as_str() {
                "soldermask" => &mut job.masks,
                "legend" => &mut job.legends,
                _ => continue,
            };
            colors[side] = colors[side].take().or_else(|| text(layer, "Color"));
        }
        Ok(job)
    }

    fn mask(&self, side: BoardSide) -> Option<ColorInfo> {
        Self::ink(&self.masks, side)
    }

    fn legend(&self, side: BoardSide) -> Option<ColorInfo> {
        Self::ink(&self.legends, side)
    }

    fn ink(colors: &[Option<String>; 2], side: BoardSide) -> Option<ColorInfo> {
        let name = colors[usize::from(side == BoardSide::Bottom)].clone()?;
        Some(ColorInfo {
            name: Some(name),
            rgb: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::geom::Point;

    #[test]
    fn an_outline_drawn_a_segment_at_a_time_closes() {
        // A 10 x 4 board's edges drawn as four separate strokes, one of
        // them backwards, and a stray mark that closes nothing.
        let gerber = "%FSLAX46Y46*%\n%MOMM*%\n%TF.FileFunction,Profile,NP*%\n%ADD10C,0.05*%\nD10*\nG01*\n\
            X0Y0D02*\nX10000000Y0D01*\n\
            X10000000Y4000000D02*\nX10000000Y0D01*\n\
            X10000000Y4000000D02*\nX0Y4000000D01*\n\
            X0Y4000000D02*\nX0Y0D01*\n\
            X20000000Y0D02*\nX21000000Y0D01*\nM02*\n";
        let gerber = gerberx2::GerberX2::parse(gerber).unwrap();
        let document = extract_document(&gerber, Resolution::default().accuracy).unwrap();
        let closed = profile_contours(&document);
        assert_eq!(closed.len(), 1);
        assert_eq!(
            closed[0].bbox,
            BBox::new(Point::new(0.0, 0.0), Point::new(10.0, 4.0))
        );
    }
}
