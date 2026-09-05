//! Full-scene vector data for external diagnostic viewers.
//!
//! The native PCB IR renderer retains arcs, polarity, apertures, and cutouts.
//! Every semantic layer is exported once in world millimeters. Check-owned
//! evidence keeps its measured geometry and uncertainty separately; a camera
//! never clips, reconstructs, or replaces the checked finding.

use pcb_ir::geom::GeometryAccuracy;
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use pcb_ir::dialects::ipc::{ArtworkScope, ProfileSet, profile_occurrences_for};
use pcb_ir::dialects::{LayerRole, Side, mask};
use pcb_ir::geom::path::{ContourBuf, PathCmd};
use pcb_ir::geom::{Affine2, BBox, FillRule, Point, shapes};
use pcb_ir::render::RenderOptions;

use super::design::Design;
use super::report::{Finding, LayerRef, LayoutContext, ReportBBox, RuleResult, Scene, ScenePass};
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
    /// Analytic drills and un-clipped physical profile paths.
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

    fn svg(&self, design: &Design<'_>, bounds: BBox, accuracy: GeometryAccuracy) -> Result<String> {
        let options = RenderOptions::default().with_viewport(bounds);
        match &self.source {
            GeometrySource::Layer => {
                let layer = self.layer.as_deref().context("artwork pass has no layer")?;
                let artwork = native_artwork(design, layer, accuracy)
                    .with_context(|| format!("failed to prepare DFM scene layer {layer}"))?;
                Ok(pcb_ir::render::artwork_svg(&artwork, &options, accuracy)?)
            }
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
    accuracy: GeometryAccuracy,
) -> Result<
    pcb_ir::dialects::artwork::Document<ipc2581::types::LayerFunction, Option<ipc2581::Symbol>>,
> {
    let geometry = geometry::render::prepare_layer(design.imported, layer, design.scope, accuracy)?;
    geometry::render::layer_artwork(&geometry, false, design.scope.profile_set(), accuracy)
}

pub(super) fn export(
    design: &Design<'_>,
    layout: &LayoutContext,
    rules: &[RuleResult],
    findings: &[Finding],
    accuracy: GeometryAccuracy,
) -> Result<Scene> {
    let sources = scene_passes(rules, design, accuracy)?;
    let mut bounds = scene_bounds(layout.bounding_box, &sources);
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
            bounds = bounds.union(site_bounds);
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
        .map(|source| {
            Ok(ScenePass {
                label: source.label.clone(),
                feature: source.feature,
                layer: source.layer.clone(),
                color: source.color,
                svg: source.svg(design, bounds, accuracy)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Scene {
        schema_version: 1,
        bounds: bounds.into(),
        passes,
    })
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

fn scene_passes(
    rules: &[RuleResult],
    design: &Design<'_>,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<Vec<GeometryPass>> {
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
        let mut layers = BTreeMap::<String, Vec<Vec<ContourBuf>>>::new();
        for hole in &design.holes {
            if let Some(circle) = shapes::circle(hole.diameter_mm) {
                layers
                    .entry(hole.layer.name.clone())
                    .or_default()
                    .push(vec![
                        circle.transformed(Affine2::translation(hole.center), accuracy)?,
                    ]);
            }
        }
        for slot in &design.slots {
            layers
                .entry(slot.layer.name.clone())
                .or_default()
                // Match the check's independently filled contour union;
                // source curves are retained instead of polygonized again.
                .extend(
                    slot.native_outline
                        .iter()
                        .cloned()
                        .map(|contour| vec![contour]),
                );
        }
        for (layer, shapes) in layers {
            passes.push(GeometryPass::shapes(
                format!("{layer} drills / routes"),
                "drills",
                LayerRole::Drill,
                "#5c7cfa",
                Some(layer),
                FillRule::EvenOdd,
                shapes,
            ));
        }
    }
    if wanted.contains("scores") {
        let mut layers = BTreeMap::<String, Vec<Vec<ContourBuf>>>::new();
        for score in &design.scores {
            layers
                .entry(score.layer.name.clone())
                .or_default()
                .push(vec![ContourBuf::new(vec![
                    PathCmd::move_to(score.start),
                    PathCmd::line_to(score.end),
                ])]);
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
            let mut contours = layout.transformed_path_contours(
                occurrence.profile.outer_path,
                occurrence.transform,
                accuracy,
            )?;
            for cutout in occurrence.profile.cutouts.slice(&layout.profile_cutouts) {
                contours.extend(layout.transformed_path_contours(
                    cutout.path,
                    occurrence.transform,
                    accuracy,
                )?);
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
        let outlines = profile_occurrences_for(layout, ProfileSet::LayoutBoundaries)
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
                    accuracy,
                )?)
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
    use pcb_ir::geom::{ContourSet, tol};

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
        let accuracy = GeometryAccuracy::default();

        let ipc = Ipc2581::parse(MASK_BOARD).unwrap();
        let rules = rules::lower(&pdk::Pdk::parse(MASK_PDK).unwrap(), None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, accuracy).unwrap();
        let design = Design::extract(&imported, ArtworkScope::Board, &rules, accuracy).unwrap();
        let artwork = native_artwork(&design, "F.Mask", accuracy).unwrap();
        let rendered = pcb_ir::dialects::artwork::compose_to_mask(&artwork, accuracy).unwrap();
        let contours = rendered
            .shapes(&rendered.layers[0])
            .iter()
            .flat_map(|shape| rendered.arena.path_contours(shape))
            .collect::<Vec<_>>();
        let image =
            ContourSet::from_contours(&contours, FillRule::NonZero, tol::REGION_MM, accuracy)
                .unwrap();
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
        let svg = pass.svg(&design, bounds, accuracy).unwrap();
        assert!(svg.contains("viewBox='-20 -20 40 40'"));
        assert_eq!(svg.matches("scale(1 -1)").count(), 1);
        assert!(svg.contains("<mask "));
        assert!(svg.contains("M-10 -10"));
        assert!(!svg.contains("<image"));
    }

    #[test]
    fn outlines_and_drills_remain_full_native_paths_outside_any_site() {
        let accuracy = GeometryAccuracy::default();

        let ipc = Ipc2581::parse(MASK_BOARD).unwrap();
        let rules = rules::lower(&pdk::Pdk::parse(MASK_PDK).unwrap(), None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc, accuracy).unwrap();
        let design = Design::extract(&imported, ArtworkScope::Board, &rules, accuracy).unwrap();
        let outline = ContourSet::rectangle(
            BBox::new(Point::new(-50.0, -50.0), Point::new(50.0, 50.0)),
            tol::REGION_MM,
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
        let svg = pass.svg(&design, viewport, accuracy).unwrap();
        assert!(svg.contains("data-board-outline='true'"));
        assert!(svg.contains("-50"));
        assert!(svg.contains("50"));
        assert!(svg.contains("fill='none'"));

        let circle = shapes::circle(1.0)
            .unwrap()
            .transformed(Affine2::translation(Point::new(40.0, -30.0)), accuracy)
            .unwrap();
        let drill = GeometryPass::shapes(
            "Drills".into(),
            "drills",
            LayerRole::Drill,
            "#5c7cfa",
            Some("Drill".into()),
            FillRule::EvenOdd,
            vec![vec![circle]],
        );
        let svg = drill.svg(&design, outline.bbox, accuracy).unwrap();
        assert!(svg.contains('A'), "round drills retain analytic arcs");
        assert!(svg.contains("40.5 -30"));
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
