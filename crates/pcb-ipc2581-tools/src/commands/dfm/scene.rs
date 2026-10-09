//! The checked material, for viewers to draw findings over.
//!
//! Each Step's own layers, drills and lines are report shapes in the Step's
//! frame, drawn wherever the layout places the Step, exactly as its findings
//! are. The material is dark only: negative polarity is resolved into it, and
//! holes are the drill layers'. Pads keep their exact standard shapes.

use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use pcb_ir::dialects::artwork::{self, Aperture, ApertureShape, Geometry, Object};
use pcb_ir::dialects::ipc::process;
use pcb_ir::dialects::ipc::{
    ArtworkScope, ArtworkTarget, FeatureBucket, ProfileSet, lower_layer_to_artwork_objects_with,
    profile_occurrences_for,
};
use pcb_ir::geom::path::{ContourBuf, stroke_to_fill};
use pcb_ir::geom::region::ContourSet;
use pcb_ir::geom::{Affine2, BBox, FillRule, LineCap, Paint, Point, Polarity, Resolution};
#[cfg(not(target_family = "wasm"))]
use rayon::prelude::*;

use super::design::Design;
use super::report::{
    Finding, Frame, LayerRef, LayoutContext, ReportPoint, RuleResult, Scene, ScenePass, Shape,
};
use crate::gerber::{catalogue_aperture, standard_primitives};

pub(super) fn export(
    designs: &[Design<'_>],
    layout: &LayoutContext,
    rules: &[RuleResult],
    frames: &[Frame],
    findings: &[Finding],
) -> Result<Scene> {
    let wanted = rules
        .iter()
        .flat_map(|rule| rule.view.features.iter().copied())
        .collect();
    let mut passes = scene_passes(&wanted, designs)?;
    passes
        .iter_mut()
        .flat_map(|pass| &mut pass.shapes)
        .for_each(|(_, shape)| shape.simplify());
    let placed = |frame: u32, bounds: BBox| {
        frames[frame as usize]
            .placements
            .iter()
            .map(move |placement| bounds.transformed(affine(placement.transform)))
    };
    let mut bounds = passes
        .iter()
        .flat_map(|pass| &pass.shapes)
        .flat_map(|(frame, shape)| placed(*frame, extent(shape)))
        .fold(
            layout
                .bounding_box
                .map(|bbox| bbox.as_bbox())
                .unwrap_or_default(),
            BBox::union,
        );
    for finding in findings {
        let rule = rules
            .iter()
            .find(|rule| rule.id == finding.rule_id)
            .context("DFM finding references an absent rule")?;
        ensure!(
            !rule.view.spatial || !finding.sites.is_empty(),
            "spatial DFM finding {} has no check-owned sites",
            finding.id
        );
        for site in &finding.sites {
            let site_bounds = site.bounding_box.as_bbox();
            ensure!(
                site_bounds.is_valid() && !site_bounds.is_empty(),
                "DFM site {} has invalid bounds",
                site.id
            );
            bounds = placed(finding.frame, site_bounds).fold(bounds, BBox::union);
            for feature in rule
                .view
                .features
                .iter()
                .filter(|&&feature| feature != "stackup")
            {
                ensure!(
                    passes
                        .iter()
                        .any(|pass| pass.feature == *feature && pass_applies(pass, &site.layers)),
                    "DFM site {} has no matching {} context in its declared layers",
                    site.id,
                    feature
                );
            }
        }
    }
    if bounds.is_empty() {
        bounds = BBox::new(Point::ZERO, Point::new(100.0, 100.0));
    }
    ensure!(bounds.is_valid(), "DFM scene has invalid bounds");
    Ok(Scene {
        bounds: bounds.into(),
        passes,
    })
}

fn affine([m00, m10, m01, m11, m02, m12]: [f64; 6]) -> Affine2 {
    Affine2 {
        m00,
        m01,
        m02,
        m10,
        m11,
        m12,
    }
}

fn pass_applies(pass: &ScenePass, layers: &[LayerRef]) -> bool {
    pass.layer
        .as_ref()
        .is_none_or(|layer| layers.iter().any(|candidate| candidate.name == *layer))
}

fn pass(
    label: String,
    feature: &'static str,
    color: &'static str,
    layer: Option<String>,
    shapes: Vec<(u32, Shape)>,
) -> ScenePass {
    ScenePass {
        label,
        feature,
        layer,
        color,
        shapes,
    }
}

fn scene_passes(wanted: &BTreeSet<&str>, designs: &[Design<'_>]) -> Result<Vec<ScenePass>> {
    let design = &designs[0];
    let layout = &design.imported.geometry;
    let frames = || designs.iter().zip(0_u32..);
    let copper = design.copper_layers.iter().map(|layer| &layer.layer.name);
    let masks = design.mask_layers.iter().map(|layer| &layer.layer.name);
    let layers = (copper.filter(|_| wanted.contains("copper")))
        .map(|name| (name.clone(), "copper", "#d87822", name))
        .chain(
            masks
                .filter(|_| wanted.contains("mask_openings"))
                .map(|name| (format!("{name} openings"), "mask_openings", "#159447", name)),
        )
        .collect::<Vec<_>>();
    #[cfg(not(target_family = "wasm"))]
    let layers = layers.into_par_iter();
    #[cfg(target_family = "wasm")]
    let layers = layers.into_iter();
    let mut passes = layers
        .map(|(label, feature, color, layer)| {
            let shapes = material(designs, layer)?;
            Ok(pass(label, feature, color, Some(layer.clone()), shapes))
        })
        .collect::<Result<Vec<_>>>()?;
    if wanted.contains("drills") {
        let layers = designs
            .iter()
            .flat_map(|design| {
                let holes = design.holes.iter().map(|hole| &hole.layer.name);
                holes.chain(design.slots.iter().map(|slot| &slot.layer.name))
            })
            .collect::<BTreeSet<_>>();
        for layer in layers {
            // Each Step's own holes and slots; placed ones are their Step's.
            let shapes = frames()
                .flat_map(|(design, frame)| {
                    let holes = design
                        .holes
                        .iter()
                        .filter(|hole| hole.branch.is_none() && hole.layer.name == *layer)
                        .map(|hole| Shape::circle(hole.center, hole.diameter_mm));
                    let slots = design
                        .slots
                        .iter()
                        .filter(|slot| slot.branch.is_none() && slot.layer.name == *layer)
                        .map(|slot| Shape::region(&slot.outline));
                    holes.chain(slots).map(move |shape| (frame, shape))
                })
                .collect();
            let label = format!("{layer} drills / routes");
            passes.push(pass(
                label,
                "drills",
                "#5c7cfa",
                Some(layer.clone()),
                shapes,
            ));
        }
    }
    if wanted.contains("scores") {
        let layers = designs
            .iter()
            .flat_map(|design| design.scores.iter().map(|score| &score.layer.name))
            .collect::<BTreeSet<_>>();
        for layer in layers {
            let shapes = frames()
                .flat_map(|(design, frame)| {
                    design
                        .scores
                        .iter()
                        .filter(|score| score.layer.name == *layer)
                        .map(move |score| (frame, Shape::segment(score.start, score.end)))
                })
                .collect();
            let label = format!("{layer} centerlines");
            passes.push(pass(
                label,
                "scores",
                "#333333",
                Some(layer.clone()),
                shapes,
            ));
        }
    }
    // Even a clean report retains its physical frame for navigation. Board
    // scope must only show the canonical definition, never the root panel.
    let board = design.scope == ArtworkScope::Board;
    let profiles = if board {
        ProfileSet::BoardOutlines
    } else {
        ProfileSet::FabricationOutlines
    };
    let outlines = profile_occurrences_for(layout, profiles)
        .into_iter()
        .map(|occurrence| {
            let cutouts = occurrence.profile.cutouts.slice(&layout.profile_cutouts);
            let contours = [occurrence.profile.outer_path]
                .into_iter()
                .chain(cutouts.iter().map(|cutout| cutout.path))
                .flat_map(|path| layout.transformed_path_contours(path, occurrence.transform))
                .collect::<Vec<_>>();
            Ok((0, Shape::path(closed_lines(&contours, design.resolution)?)))
        })
        .collect::<Result<_>>()?;
    passes.push(pass(
        "Physical outlines".into(),
        "board_outlines",
        "#333333",
        None,
        outlines,
    ));
    if wanted.contains("array_outlines") && !board {
        let arrays = design
            .board_arrays
            .iter()
            .map(|array| array.instance_index)
            .collect::<BTreeSet<_>>();
        let outlines = profile_occurrences_for(layout, ProfileSet::LayoutBoundaries)
            .into_iter()
            .filter(|occurrence| {
                occurrence
                    .instance
                    .is_none_or(|index| arrays.contains(&index))
            })
            .map(|occurrence| {
                let contours = layout
                    .transformed_path_contours(occurrence.profile.outer_path, occurrence.transform);
                Ok((0, Shape::path(closed_lines(&contours, design.resolution)?)))
            })
            .collect::<Result<_>>()?;
        passes.push(pass(
            "Array / panel outlines".into(),
            "array_outlines",
            "#333333",
            None,
            outlines,
        ));
    }
    Ok(passes)
}

/// One layer's material, each Step's own in its own frame.
fn material(designs: &[Design<'_>], layer: &str) -> Result<Vec<(u32, Shape)>> {
    let primitives = standard_primitives(designs[0].imported);
    let target = ArtworkTarget {
        catalogue: &|primitive| catalogue_aperture(&primitives, primitive),
        ..ArtworkTarget::default()
    };
    let mut shapes = Vec::new();
    for (design, frame) in designs.iter().zip(0_u32..) {
        let imported = design.imported;
        let id = imported
            .layer_id(layer)
            .with_context(|| format!("IPC-2581 layer '{layer}' was not found"))?;
        let root = design.placements[0];
        let mut doc =
            imported.materialize_occurrence_layer(id, design.scope, root, &|held| held == root)?;
        process::normalize_for_artwork(&mut doc, design.resolution)?;
        process::retain_features(&mut doc, |feature| feature.bucket != FeatureBucket::Cutout);
        process::resolve_negative_polarity(&mut doc, design.resolution)?;
        let mut artwork = artwork::Document::<(), ()>::new();
        let objects =
            lower_layer_to_artwork_objects_with(&doc, 0, &mut artwork, &target, &|_, _| ());
        let mut own = Vec::new();
        draw(
            &artwork,
            &objects,
            Affine2::IDENTITY,
            design.resolution,
            &mut own,
        )
        .with_context(|| format!("failed to draw DFM scene layer {layer}"))?;
        // A fill too thin to regularize paints nothing.
        let painted = own
            .into_iter()
            .filter(|shape| shape.kind != "region" || !shape.paths.is_empty());
        shapes.extend(painted.map(|shape| (frame, shape)));
    }
    Ok(shapes)
}

fn draw(
    artwork: &artwork::Document,
    objects: &[Object],
    at: Affine2,
    resolution: Resolution,
    shapes: &mut Vec<Shape>,
) -> Result<()> {
    for object in objects {
        ensure!(
            object.polarity == Polarity::Dark,
            "scene material is dark only"
        );
        match object.geometry {
            Geometry::Flash {
                aperture,
                transform,
            } => shapes.push(flash(
                &artwork.apertures[aperture as usize],
                at.concat(transform),
                resolution,
            )?),
            Geometry::Stroke { path } | Geometry::Region { path } => {
                let path = &artwork.arena.paths[path as usize];
                let contours = artwork
                    .arena
                    .path_contours(path)
                    .into_iter()
                    .map(|contour| contour.transformed(at))
                    .collect::<Vec<_>>();
                shapes.push(painted(&contours, path.paint, at.max_scale(), resolution)?);
            }
            Geometry::Instance { block, transform } => {
                let block = &artwork.blocks[block as usize].objects;
                draw(artwork, block, at.concat(transform), resolution, shapes)?;
            }
            Geometry::GridInstance {
                block,
                transform,
                repeat,
            } => {
                let block = &artwork.blocks[block as usize].objects;
                for offset in repeat.offsets() {
                    let placed = at.concat(Affine2::translation(offset).concat(transform));
                    draw(artwork, block, placed, resolution, shapes)?;
                }
            }
        }
    }
    Ok(())
}

/// A standard aperture keeps its exact form: a circle, a round-capped
/// stroke for an obround, and a rectangle's corners drawn round to its corner
/// radius. Anything else is its regularized outline.
fn flash(aperture: &Aperture, at: Affine2, resolution: Resolution) -> Result<Shape> {
    let point = |x: f64, y: f64| ReportPoint::from(at.transform_point(Point::new(x, y)));
    let scale = at.max_scale();
    Ok(match aperture.shape {
        _ if aperture.hole_diameter > 0.0 => region(
            &transformed(aperture.contours(), at),
            FillRule::EvenOdd,
            resolution,
        )?,
        ApertureShape::Circle { diameter } => {
            Shape::circle(at.transform_point(Point::ZERO), diameter * scale)
        }
        ApertureShape::Obround { width, height } => {
            let half = (width - height).abs() / 2.0;
            let ends = if width >= height {
                [point(-half, 0.0), point(half, 0.0)]
            } else {
                [point(0.0, -half), point(0.0, half)]
            };
            Shape::stroke(vec![ends.to_vec()], width.min(height) * scale)
        }
        ApertureShape::Rectangle { width, height } => rounded(width, height, 0.0, point, scale),
        ApertureShape::RoundRect {
            width,
            height,
            radius,
        } => rounded(width, height, radius, point, scale),
        ApertureShape::Contour {
            ref outline,
            fill_rule,
        } => region(
            &transformed(vec![outline.clone()], at),
            fill_rule,
            resolution,
        )?,
        ApertureShape::Polygon { .. } => region(
            &transformed(aperture.contours(), at),
            FillRule::EvenOdd,
            resolution,
        )?,
    })
}

fn rounded(
    width: f64,
    height: f64,
    radius: f64,
    point: impl Fn(f64, f64) -> ReportPoint,
    scale: f64,
) -> Shape {
    let radius = radius.min(width.min(height) / 2.0);
    let (x, y) = (width / 2.0 - radius, height / 2.0 - radius);
    let ring = vec![point(-x, -y), point(x, -y), point(x, y), point(-x, y)];
    if radius > 0.0 {
        Shape::rounded(ring, 2.0 * radius * scale)
    } else {
        Shape::polygon(ring)
    }
}

fn transformed(contours: Vec<ContourBuf>, at: Affine2) -> Vec<ContourBuf> {
    contours
        .into_iter()
        .map(|contour| contour.transformed(at))
        .collect()
}

/// A painted path: a fill is its regularized region and a round-capped stroke
/// its centerlines; any other stroke is the region it outlines.
fn painted(
    contours: &[ContourBuf],
    paint: Paint,
    scale: f64,
    resolution: Resolution,
) -> Result<Shape> {
    match paint {
        Paint::Fill { rule } => region(contours, rule, resolution),
        Paint::Stroke(style) if style.cap == LineCap::Round => Ok(Shape::stroke(
            lines(contours, resolution)?,
            style.width * scale,
        )),
        Paint::Stroke(mut style) => {
            style.width *= scale;
            let outline = stroke_to_fill(contours, style, resolution.accuracy)?.unwrap_or_default();
            region(&outline, FillRule::NonZero, resolution)
        }
        Paint::None => anyhow::bail!("scene material paints every path"),
    }
}

fn region(contours: &[ContourBuf], rule: FillRule, resolution: Resolution) -> Result<Shape> {
    Ok(Shape::region(&ContourSet::from_contours(
        contours, rule, resolution,
    )?))
}

/// Contours as polylines, their curves in chords within the resolution.
fn polylines(contours: &[ContourBuf], resolution: Resolution) -> Result<Vec<Vec<Point>>> {
    let tolerance = resolution.accuracy.max_error_mm();
    contours
        .iter()
        .map(|contour| {
            let mut line = contour
                .segments()
                .next()
                .map(|first| first.start())
                .into_iter()
                .collect::<Vec<_>>();
            for segment in contour.segments() {
                line.extend(segment.chords(tolerance)?.0);
            }
            Ok(line)
        })
        .collect()
}

fn lines(contours: &[ContourBuf], resolution: Resolution) -> Result<Vec<Vec<ReportPoint>>> {
    Ok(points(polylines(contours, resolution)?))
}

/// Closed contours as lines that return to where they start.
fn closed_lines(contours: &[ContourBuf], resolution: Resolution) -> Result<Vec<Vec<ReportPoint>>> {
    let mut lines = polylines(contours, resolution)?;
    for line in &mut lines {
        if line.first() != line.last() {
            line.extend(line.first().copied());
        }
    }
    Ok(points(lines))
}

fn points(lines: Vec<Vec<Point>>) -> Vec<Vec<ReportPoint>> {
    lines
        .into_iter()
        .map(|line| line.into_iter().map(ReportPoint::from).collect())
        .collect()
}

/// The bounds a shape paints.
fn extent(shape: &Shape) -> BBox {
    let point = |point: ReportPoint| Point::new(point.x, point.y);
    let half = shape.width_mm.or(shape.diameter).unwrap_or(0.0) / 2.0;
    let points = (shape.paths.iter().flatten().copied())
        .chain(shape.center)
        .chain(shape.start)
        .chain(shape.end)
        .map(point)
        .chain(
            shape
                .bounding_box
                .into_iter()
                .flat_map(|bbox| [point(bbox.min), point(bbox.max)]),
        );
    points
        .fold(BBox::empty(), |bounds, point| {
            bounds.union(BBox::new(point, point))
        })
        .expand(half)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;

    const MASK_BOARD: &str = r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
      <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/><LayerRef name="F.Mask"/></Content>
      <Ecad><CadHeader units="MILLIMETER"/><CadData>
        <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
        <Step name="board" type="BOARD"><Datum x="0" y="0"/>
          <Profile><Polygon><PolyBegin x="-20" y="-20"/><PolyStepSegment x="20" y="-20"/><PolyStepSegment x="20" y="20"/><PolyStepSegment x="-20" y="20"/><PolyStepSegment x="-20" y="-20"/></Polygon></Profile>
          <LayerFeature layerRef="F.Mask">
            <Set polarity="POSITIVE"><Features><Contour><Polygon><PolyBegin x="-10" y="-10"/><PolyStepSegment x="10" y="-10"/><PolyStepSegment x="10" y="10"/><PolyStepSegment x="-10" y="10"/><PolyStepSegment x="-10" y="-10"/></Polygon></Contour></Features></Set>
            <Set polarity="NEGATIVE"><Features><Contour><Polygon><PolyBegin x="-2" y="-2"/><PolyStepSegment x="2" y="-2"/><PolyStepSegment x="2" y="2"/><PolyStepSegment x="-2" y="2"/><PolyStepSegment x="-2" y="-2"/></Polygon></Contour></Features></Set>
          </LayerFeature>
        </Step>
      </CadData></Ecad>
    </IPC-2581>"#;

    #[test]
    fn negative_polarity_is_resolved_into_dark_material() {
        let rules = fixtures::rules(&fixtures::pdk(
            "[[rules.soldermask.web]]\nid = \"mask-web\"\nlimit = { minimum = \"0.1 mm\" }",
        ));
        let imported = fixtures::import(MASK_BOARD);
        let design = Design::board(&imported, &rules, Resolution::default());
        let shapes = material(std::slice::from_ref(&design), "F.Mask").unwrap();
        let [(0, opening)] = shapes.as_slice() else {
            panic!("one opening: {shapes:?}");
        };
        assert_eq!(opening.kind, "region");
        assert_eq!(opening.paths.len(), 2, "the clear square is a hole");
    }

    #[test]
    fn drills_are_drawn_once_in_their_step_frame() {
        let resolution = Resolution::default();
        let imported = fixtures::import(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
          <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="panel"/><LayerRef name="DRILL"/></Content>
          <Ecad><CadHeader units="MILLIMETER"/><CadData>
            <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
            <Step name="board" type="BOARD">
              <LayerFeature layerRef="DRILL"><Set><Hole name="via" diameter="1" platingStatus="VIA" x="40" y="-30"/></Set></LayerFeature>
            </Step>
            <Step name="panel" type="PALLET">
              <StepRepeat stepRef="board" x="100" y="0" nx="3" ny="1" dx="50" dy="0"/>
              <LayerFeature layerRef="DRILL"><Set><Hole name="tooling" diameter="3" platingStatus="NONPLATED" x="5" y="5"/></Set></LayerFeature>
            </Step>
          </CadData></Ecad>
        </IPC-2581>"#,
        );
        let rules = fixtures::rules(&fixtures::pdk(
            "[[rules.drilling.hole_diameter]]\nid = \"via\"\nselect = { hole = \"via\" }\nlimit = { minimum = \"0.1 mm\" }",
        ));
        let designs =
            Design::frames(&imported, ArtworkScope::ArrayFlattened, &rules, resolution).unwrap();
        let passes = scene_passes(&BTreeSet::from(["drills"]), &designs).unwrap();
        let drills = passes.iter().find(|pass| pass.feature == "drills").unwrap();
        let mut holes = drills
            .shapes
            .iter()
            .map(|(frame, shape)| (*frame, shape.center.unwrap().x, shape.diameter.unwrap()))
            .collect::<Vec<_>>();
        holes.sort_by_key(|hole| hole.0);
        assert_eq!(
            holes,
            [(0, 5.0, 3.0), (1, 40.0, 1.0)],
            "the via once, in the board's frame"
        );
        assert_eq!(designs[1].placements.len(), 3);
    }
}
