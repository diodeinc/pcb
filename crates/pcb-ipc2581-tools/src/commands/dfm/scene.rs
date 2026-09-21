//! Full-scene vector data for external diagnostic viewers.
//!
//! The native PCB IR renderer retains arcs, polarity, apertures, and cutouts.
//! Every semantic layer is exported once in world millimeters. Check-owned
//! evidence keeps its measured geometry and uncertainty separately; a camera
//! never clips, reconstructs, or replaces the checked finding.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use pcb_ir::dialects::ipc::{ArtworkScope, ProfileSet, profile_occurrences_for};
use pcb_ir::dialects::{LayerRole, Side, artwork, mask};
use pcb_ir::geom::path::{ContourBuf, PathCmd};
use pcb_ir::geom::{Affine2, BBox, FillRule, Paint, Point, Polarity};
use pcb_ir::render::RenderOptions;

use super::design::Design;
use super::report::{
    Finding, Frame, LayerRef, LayoutContext, ReportBBox, RuleResult, Scene, ScenePass,
};
use crate::geometry;

struct GeometryPass {
    label: String,
    feature: &'static str,
    layer: Option<String>,
    role: LayerRole,
    color: &'static str,
    source: GeometrySource,
    bounds: BBox,
}

enum GeometrySource {
    /// Native artwork uses the same scope and manufacturing composition as
    /// the checks, while retaining analytic curves for close inspection.
    Layer,
    /// Analytic drills, drawn once for each Step that owns some and placed
    /// wherever the layout places that Step.
    Placed(artwork::Document<(), ()>),
    /// Un-clipped physical profile paths and score lines.
    Shapes {
        shapes: Vec<Vec<ContourBuf>>,
        fill_rule: FillRule,
    },
}

impl GeometryPass {
    fn layer(
        label: String,
        feature: &'static str,
        role: LayerRole,
        color: &'static str,
        layer: Option<String>,
        bounds: BBox,
    ) -> Self {
        Self {
            label,
            feature,
            layer,
            role,
            color,
            source: GeometrySource::Layer,
            bounds,
        }
    }

    fn placed(
        feature: &'static str,
        color: &'static str,
        layer: String,
        artwork: artwork::Document<(), ()>,
    ) -> Self {
        let drawn = &artwork.layers[0];
        Self {
            label: drawn.name.clone(),
            feature,
            layer: Some(layer),
            role: drawn.role,
            color,
            bounds: drawn.bbox,
            source: GeometrySource::Placed(artwork),
        }
    }

    fn shapes(
        label: String,
        feature: &'static str,
        role: LayerRole,
        color: &'static str,
        layer: Option<String>,
        fill_rule: FillRule,
        shapes: Vec<Vec<ContourBuf>>,
    ) -> Self {
        let bounds = shapes
            .iter()
            .flatten()
            .map(|contour| contour.bbox)
            .fold(BBox::empty(), BBox::union);
        Self {
            label,
            feature,
            layer,
            role,
            color,
            source: GeometrySource::Shapes { shapes, fill_rule },
            bounds,
        }
    }

    /// `pass` numbers this render within its scene: the passes are inlined
    /// into one page, where element ids are global.
    fn svg(&self, design: &Design<'_>, bounds: BBox, pass: usize) -> Result<String> {
        let options = RenderOptions::default()
            .with_viewport(bounds)
            .with_accuracy(design.resolution.accuracy)
            .with_id_prefix(format!("p{pass}-"));
        match &self.source {
            GeometrySource::Layer => {
                let layer = self.layer.as_deref().context("artwork pass has no layer")?;
                let artwork = native_artwork(design, layer)
                    .with_context(|| format!("failed to prepare DFM scene layer {layer}"))?;
                Ok(pcb_ir::render::artwork_svg(&artwork, &options)?)
            }
            GeometrySource::Placed(artwork) => Ok(pcb_ir::render::artwork_svg(artwork, &options)?),
            GeometrySource::Shapes { shapes, fill_rule } => {
                let mut doc = mask::Document::<()>::new();
                let layer = doc.push_layer(mask::Layer::new(&self.label, self.role, Side::None));
                for contours in shapes {
                    // Keep every shape's rings together: filling its inner
                    // rings independently would turn holes into material.
                    doc.push_shape(layer, *fill_rule, contours.clone());
                }
                Ok(pcb_ir::render::svg(&doc, &options))
            }
        }
    }
}

fn native_artwork(
    design: &Design<'_>,
    layer: &str,
) -> Result<
    pcb_ir::dialects::artwork::Document<ipc2581::types::LayerFunction, Option<ipc2581::Symbol>>,
> {
    Ok(geometry::render::layer_artwork(
        design.imported,
        layer,
        design.scope,
        false,
        design.resolution,
    )?
    .artwork)
}

/// The scene is the whole checked layout, drawn from its root Step's design,
/// which holds everything the layout places.
pub(super) fn export(
    designs: &[Design<'_>],
    layout: &LayoutContext,
    rules: &[RuleResult],
    frames: &[Frame],
    findings: &[Finding],
) -> Result<Scene> {
    let design = &designs[0];
    let sources = scene_passes(rules, designs)?;
    let mut bounds = scene_bounds(layout.bounding_box, &sources);
    for finding in findings {
        let frame = &frames[finding.frame as usize];
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
            // A site is in its Step's frame and occurs wherever that is placed.
            bounds = frame
                .placements
                .iter()
                .map(|placement| {
                    let [m00, m10, m01, m11, m02, m12] = placement.transform;
                    site_bounds.transformed(Affine2 {
                        m00,
                        m01,
                        m02,
                        m10,
                        m11,
                        m12,
                    })
                })
                .fold(bounds, BBox::union);
            for feature in rule
                .view
                .features
                .iter()
                .filter(|&&feature| feature != "stackup")
            {
                ensure!(
                    sources
                        .iter()
                        .any(|source| source.feature == *feature
                            && pass_applies(source, &site.layers)),
                    "DFM site {} has no matching {} context in its declared layers",
                    site.id,
                    feature
                );
            }
        }
    }
    if bounds.is_empty() {
        bounds = BBox::new(Point::ZERO, Point::new(100.0, 100.0));
    } else if bounds.width() == 0.0 || bounds.height() == 0.0 {
        // A lone line or point still needs a positive SVG viewport. This
        // display padding never changes the site's measured bounding box.
        bounds = bounds.expand(0.5);
    }
    ensure!(bounds.is_valid(), "DFM scene has invalid bounds");
    let passes = sources
        .iter()
        .enumerate()
        .map(|(pass, source)| {
            Ok(ScenePass {
                label: source.label.clone(),
                feature: source.feature,
                layer: source.layer.clone(),
                color: source.color,
                svg: source.svg(design, bounds, pass)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Scene {
        schema_version: 1,
        bounds: bounds.into(),
        passes,
    })
}

/// One drill layer of the whole layout: each Step's own holes and slots are a
/// block, placed wherever the layout places the Step.
fn placed_drills(designs: &[Design<'_>], layer: &str) -> artwork::Document<(), ()> {
    let mut artwork = artwork::Document::new();
    let drawn = artwork.push_layer(artwork::Layer::new(
        format!("{layer} drills / routes"),
        LayerRole::Drill,
        Side::None,
    ));
    for design in designs {
        let holes = design
            .holes
            .iter()
            .filter(|hole| hole.branch.is_none() && hole.layer.name == layer)
            .collect::<Vec<_>>();
        let slots = design
            .slots
            .iter()
            .filter(|slot| slot.branch.is_none() && slot.layer.name == layer)
            .collect::<Vec<_>>();
        if holes.is_empty() && slots.is_empty() {
            continue;
        }
        let block = artwork.push_block();
        for hole in holes {
            let aperture = artwork.push_aperture(artwork::Aperture::circle(hole.diameter_mm));
            let flash = artwork::Geometry::Flash {
                aperture,
                transform: Affine2::translation(hole.center),
            };
            artwork.push_block_object(block, artwork::Object::new(Polarity::Dark, flash));
        }
        // Match the check's independently filled contour union; source
        // curves are retained instead of polygonized again.
        for contour in slots.iter().flat_map(|slot| &slot.native_outline) {
            let path = artwork.push_path(
                Paint::Fill {
                    rule: FillRule::EvenOdd,
                },
                [contour.clone()],
            );
            let region = artwork::Geometry::Region { path };
            artwork.push_block_object(block, artwork::Object::new(Polarity::Dark, region));
        }
        for &placement in &design.placements {
            let (transform, _) = design.placed(placement);
            let instance = artwork::Geometry::Instance { block, transform };
            artwork.push_object(drawn, artwork::Object::new(Polarity::Dark, instance));
        }
    }
    artwork::normalize_bounds(&mut artwork);
    artwork
}

fn pass_applies(source: &GeometryPass, layers: &[LayerRef]) -> bool {
    source
        .layer
        .as_ref()
        .is_none_or(|layer| layers.iter().any(|candidate| candidate.name == *layer))
}

fn scene_bounds(layout: Option<ReportBBox>, sources: &[GeometryPass]) -> BBox {
    sources.iter().map(|source| source.bounds).fold(
        layout.map(ReportBBox::as_bbox).unwrap_or_default(),
        BBox::union,
    )
}

fn scene_passes(rules: &[RuleResult], designs: &[Design<'_>]) -> anyhow::Result<Vec<GeometryPass>> {
    let design = &designs[0];
    let layout = &design.imported.geometry;
    let wanted = rules
        .iter()
        .flat_map(|rule| rule.view.features.iter().copied())
        .collect::<BTreeSet<_>>();
    let mut passes = Vec::new();
    if wanted.contains("copper") {
        for layer in &design.copper_layers {
            passes.push(GeometryPass::layer(
                layer.layer.name.clone(),
                "copper",
                LayerRole::Copper,
                "#d87822",
                Some(layer.layer.name.clone()),
                layer.image.bbox,
            ));
        }
    }
    if wanted.contains("mask_openings") {
        for layer in &design.mask_layers {
            passes.push(GeometryPass::layer(
                format!("{} openings", layer.layer.name),
                "mask_openings",
                LayerRole::Soldermask,
                "#159447",
                Some(layer.layer.name.clone()),
                layer.image.bbox,
            ));
        }
    }
    if wanted.contains("drills") {
        let layers = designs
            .iter()
            .flat_map(|design| {
                let holes = design.holes.iter().map(|hole| &hole.layer.name);
                holes.chain(design.slots.iter().map(|slot| &slot.layer.name))
            })
            .collect::<BTreeSet<_>>();
        for layer in layers {
            passes.push(GeometryPass::placed(
                "drills",
                "#5c7cfa",
                layer.clone(),
                placed_drills(designs, layer),
            ));
        }
    }
    if wanted.contains("scores") {
        let mut layers = BTreeMap::<String, Vec<Vec<ContourBuf>>>::new();
        // Every Step draws its own lines, wherever the layout places it.
        for design in designs {
            for &placement in &design.placements {
                let (placed, _) = design.placed(placement);
                for score in &design.scores {
                    layers
                        .entry(score.layer.name.clone())
                        .or_default()
                        .push(vec![ContourBuf::new(vec![
                            PathCmd::move_to(placed.transform_point(score.start)),
                            PathCmd::line_to(placed.transform_point(score.end)),
                        ])]);
                }
            }
        }
        for (layer, shapes) in layers {
            passes.push(GeometryPass::shapes(
                format!("{layer} centerlines"),
                "scores",
                LayerRole::Profile,
                "#333333",
                Some(layer),
                FillRule::NonZero,
                shapes,
            ));
        }
    }
    // Even a clean report retains its physical frame for navigation. Board
    // scope must only show the canonical definition, never the root panel.
    let profile_set = if design.scope == ArtworkScope::Board {
        ProfileSet::BoardOutlines
    } else {
        ProfileSet::FabricationOutlines
    };
    let outlines = profile_occurrences_for(layout, profile_set)
        .into_iter()
        .map(|occurrence| {
            let mut contours = layout
                .transformed_path_contours(occurrence.profile.outer_path, occurrence.transform);
            for cutout in occurrence.profile.cutouts.slice(&layout.profile_cutouts) {
                contours
                    .extend(layout.transformed_path_contours(cutout.path, occurrence.transform));
            }
            Ok::<_, anyhow::Error>(contours)
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
        .collect::<Vec<_>>();
    passes.push(GeometryPass::shapes(
        "Physical outlines".into(),
        "board_outlines",
        LayerRole::Profile,
        "#333333",
        None,
        FillRule::NonZero,
        outlines,
    ));
    if wanted.contains("array_outlines") && design.scope != ArtworkScope::Board {
        let arrays = design
            .board_arrays
            .iter()
            .map(|array| array.instance_index)
            .collect::<BTreeSet<_>>();
        let outlines =
            profile_occurrences_for(layout, ProfileSet::LayoutBoundaries)
                .into_iter()
                .filter(|occurrence| {
                    occurrence
                        .instance
                        .is_none_or(|index| arrays.contains(&index))
                })
                .map(|occurrence| {
                    // Retain native profile arcs instead of reconstructing the
                    // check's tessellated array region for display.
                    Ok::<_, anyhow::Error>(layout.transformed_path_contours(
                        occurrence.profile.outer_path,
                        occurrence.transform,
                    ))
                })
                .collect::<anyhow::Result<Vec<_>>>()?
                .into_iter()
                .collect();
        passes.push(GeometryPass::shapes(
            "Array / panel outlines".into(),
            "array_outlines",
            LayerRole::Profile,
            "#333333",
            None,
            FillRule::NonZero,
            outlines,
        ));
    }
    Ok(passes)
}

#[cfg(test)]
mod tests {
    use super::super::{pdk, rules};
    use super::*;
    use crate::ipc2581::Ipc2581;
    use pcb_ir::geom::{ContourSet, Resolution};

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

    const MASK_PDK: &str = r#"schema_version = 2
      default_profile = "test"
      [pdk]
      id = "mask-scene"
      name = "Mask scene"
      revision = "1"
      [profiles.test]
      name = "Test"
      [[rules.soldermask.web]]
      id = "mask-web"
      limit = { minimum = "0.1 mm" }
    "#;

    #[test]
    fn native_mask_scene_preserves_openings_voids_and_world_coordinates() {
        let resolution = Resolution::default();

        let ipc = Ipc2581::parse(MASK_BOARD).unwrap();
        let rules = rules::lower(&pdk::Pdk::parse(MASK_PDK).unwrap(), None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
        let design = Design::board(&imported, &rules, resolution);
        let artwork = native_artwork(&design, "F.Mask").unwrap();
        let rendered = pcb_ir::dialects::artwork::compose_to_mask(&artwork, resolution).unwrap();
        let contours = rendered
            .shapes(&rendered.layers[0])
            .iter()
            .flat_map(|shape| rendered.arena.path_contours(shape))
            .collect::<Vec<_>>();
        let image = ContourSet::from_contours(&contours, FillRule::NonZero, resolution).unwrap();
        let samples = [Point::ZERO, Point::new(8.0, 0.0), Point::new(15.0, 0.0)];
        assert_eq!(image.contains_points_batch(&samples), [false, true, false]);
        assert_eq!(
            image.contains_points_batch(&samples),
            design.mask_layers[0].image.contains_points_batch(&samples)
        );
        assert!((image.area() - design.mask_layers[0].image.area()).abs() < 1e-8);

        let pass = GeometryPass::layer(
            "F.Mask openings".into(),
            "mask_openings",
            LayerRole::Soldermask,
            "#159447",
            Some("F.Mask".into()),
            image.bbox,
        );
        let bounds = BBox::new(Point::new(-20.0, -20.0), Point::new(20.0, 20.0));
        let svg = pass.svg(&design, bounds, 0).unwrap();
        assert!(svg.contains("viewBox='-20 -20 40 40'"));
        assert_eq!(svg.matches("scale(1 -1)").count(), 1);
        assert!(svg.contains("<mask "));
        assert!(svg.contains("M-10 -10"));
        assert!(!svg.contains("<image"));
    }

    #[test]
    fn outlines_remain_full_native_paths_outside_any_site() {
        let resolution = Resolution::default();

        let ipc = Ipc2581::parse(MASK_BOARD).unwrap();
        let rules = rules::lower(&pdk::Pdk::parse(MASK_PDK).unwrap(), None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
        let design = Design::board(&imported, &rules, resolution);
        let outline = ContourSet::rectangle(
            BBox::new(Point::new(-50.0, -50.0), Point::new(50.0, 50.0)),
            resolution,
        );
        let pass = GeometryPass::shapes(
            "Physical outlines".into(),
            "board_outlines",
            LayerRole::Profile,
            "#333333",
            None,
            FillRule::NonZero,
            vec![outline.to_contours()],
        );
        // Even a viewport wholly inside the board does not remove its distant
        // perimeter from the vector document. Panning can always reach it.
        let viewport = BBox::new(Point::new(-2.0, -2.0), Point::new(2.0, 2.0));
        let svg = pass.svg(&design, viewport, 0).unwrap();
        assert!(svg.contains("data-board-outline='true'"));
        assert!(svg.contains("-50"));
        assert!(svg.contains("50"));
        assert!(svg.contains("fill='none'"));
    }

    #[test]
    fn drills_are_drawn_once_for_their_step_and_placed_with_it() {
        let resolution = Resolution::default();
        let ipc = Ipc2581::parse(
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
        )
        .unwrap();
        let pdk = MASK_PDK.replace(
            "[[rules.soldermask.web]]\n      id = \"mask-web\"",
            "[[rules.drilling.hole_diameter]]\n      id = \"via\"\n      select = { hole = \"via\" }",
        );
        let rules = rules::lower(&pdk::Pdk::parse(&pdk).unwrap(), None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
        let designs =
            Design::frames(&imported, ArtworkScope::ArrayFlattened, &rules, resolution).unwrap();
        let drills = placed_drills(&designs, "DRILL");
        let pass = GeometryPass::placed("drills", "#5c7cfa", "DRILL".into(), drills);
        assert_eq!(
            pass.bounds,
            BBox::new(Point::new(3.5, -30.5), Point::new(240.5, 6.5)),
            "every placement counts toward the scene"
        );
        let svg = pass.svg(&designs[0], pass.bounds, 0).unwrap();
        assert_eq!(svg.matches(" A").count(), 2 * 4, "two analytic circles");
        assert!(svg.contains("matrix(1 0 0 1 40 -30)"), "in its own Step");
        for x in [100, 150, 200] {
            assert!(
                svg.contains(&format!("matrix(1 0 0 1 {x} 0)")),
                "placed at {x}"
            );
        }
    }

    #[test]
    fn full_scene_bounds_and_layer_matching_do_not_depend_on_a_site() {
        let bounds = BBox::new(Point::new(-12.0, 3.0), Point::new(240.0, 180.0));
        let pass = GeometryPass::layer(
            "F.Cu".into(),
            "copper",
            LayerRole::Copper,
            "#d87822",
            Some("F.Cu".into()),
            bounds,
        );
        let layer = |name: &str| LayerRef {
            name: name.into(),
            function: "CONDUCTOR".into(),
            side: None,
        };
        assert!(pass_applies(&pass, &[layer("F.Cu")]));
        assert!(!pass_applies(&pass, &[layer("B.Cu")]));
        assert!(!pass_applies(&pass, &[]));
        assert_eq!(scene_bounds(None, &[pass]), bounds);
    }
}
