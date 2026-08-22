//! The checked design: fat entity pools extracted once from IPC-2581.
//!
//! Extraction resolves the semantics rules need — hole spans, hole-to-land
//! links, composed layer images — so the engine measures geometry without
//! re-deriving identity. Only the pools the configured rules read are built.
//! Entities carry interned [`Symbol`]s; strings are resolved only when a
//! finding is emitted.
//!
//! Copper layers form the design's stackup: the pool is ordered top to
//! bottom as the file lists it, and drill spans are resolved to inclusive
//! ordinal ranges over that pool.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use ipc2581::Symbol;
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::Side;
use pcb_ir::dialects::ipc::{
    ArtworkScope, FeatureDomain, FeatureKind, FeatureSpan, LayoutStepKind, PlatingKind,
    ProfileOccurrenceRole, profile_occurrences_for,
};
use pcb_ir::geom::region::Ring;
use pcb_ir::geom::{BBox, ContourSet, Point, Polarity, tol};
use rayon::prelude::*;

use crate::geometry::{self, GeometryDocument};
use crate::ipc2581::Ipc2581;
use crate::layers;

use super::report::LayerRef;
use super::rules::{Needs, Rule};

pub(super) struct Design {
    pub scope: ArtworkScope,
    pub holes: Vec<Hole>,
    pub slots: Vec<Slot>,
    pub copper_layers: Vec<CopperLayer>,
    pub mask_layers: Vec<MaskLayer>,
    pub scores: Vec<Score>,
    pub board_outlines: Vec<BoardOutline>,
    pub board_arrays: Vec<BoardArray>,
}

impl Design {
    pub fn extract(ipc: &Ipc2581, rules: &[Rule], scope: ArtworkScope) -> Result<Self> {
        let needs = super::rules::needs(rules);
        let (mut holes, slots) = if needs.holes || needs.slots {
            collect_drilled(ipc, scope, needs)?
        } else {
            (Vec::new(), Vec::new())
        };
        let copper_layers = if needs.copper {
            collect_copper_layers(ipc, scope)?
        } else {
            Vec::new()
        };
        link_lands(&mut holes, &copper_layers);
        let wants_arrays = needs.board_arrays && scope == ArtworkScope::ArrayFlattened;
        let layout = if needs.board_outlines || wants_arrays {
            Some(geometry::extract_layout(ipc)?)
        } else {
            None
        };
        Ok(Self {
            scope,
            holes,
            slots,
            copper_layers,
            mask_layers: if needs.masks {
                collect_mask_layers(ipc, scope)?
            } else {
                Vec::new()
            },
            scores: if needs.scores {
                collect_scores(ipc, scope)?
            } else {
                Vec::new()
            },
            board_outlines: match &layout {
                Some(layout) if needs.board_outlines => collect_board_outlines(ipc, layout, scope),
                _ => Vec::new(),
            },
            board_arrays: match &layout {
                Some(layout) if wants_arrays => collect_board_arrays(ipc, layout),
                _ => Vec::new(),
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HoleClass {
    Via,
    Pth,
    Npth,
    Unknown,
}

impl HoleClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Via => "via",
            Self::Pth => "PTH",
            Self::Npth => "NPTH",
            Self::Unknown => "unclassified",
        }
    }

    pub fn subject_kind(self) -> &'static str {
        match self {
            Self::Via => "via_hole",
            Self::Pth => "plated_hole",
            Self::Npth => "nonplated_hole",
            Self::Unknown => "unclassified_hole",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Hole {
    pub class: HoleClass,
    pub center: Point,
    pub diameter_mm: f64,
    pub bbox: BBox,
    pub layer: LayerRef,
    /// Inclusive ordinal range over the copper stackup the drill spans.
    /// `None` spans the whole board (or the span is unknown).
    pub copper_span: Option<(u16, u16)>,
    pub step: Option<Symbol>,
    pub padstack: Option<Symbol>,
    pub net: Option<Symbol>,
    /// Lands this hole owns, one per copper layer, linked at extraction.
    pub lands: Vec<HoleLand>,
    pub source_set_index: u32,
    pub source_feature_index: u32,
}

impl Hole {
    pub fn spans_copper(&self, copper_ordinal: u16) -> bool {
        self.copper_span
            .is_none_or(|(low, high)| (low..=high).contains(&copper_ordinal))
    }

    /// Whether two drill spans coexist at some board depth. Unknown spans
    /// conservatively overlap everything.
    pub fn span_overlaps(&self, other: &Hole) -> bool {
        match (self.copper_span, other.copper_span) {
            (Some((self_low, self_high)), Some((other_low, other_high))) => {
                self_low <= other_high && other_low <= self_high
            }
            _ => true,
        }
    }

    pub fn land_on(&self, copper_index: usize) -> Option<&HoleLand> {
        self.lands
            .iter()
            .find(|land| land.copper_index as usize == copper_index)
    }

    /// A plated hole must have copper enclosure on both terminal layers of
    /// its known span. Unknown/through-board spans conservatively terminate
    /// on the outermost copper layers.
    pub fn terminates_on(&self, copper_index: usize, copper_layer_count: usize) -> bool {
        if copper_layer_count == 0 {
            return false;
        }
        match self.copper_span {
            Some((low, high)) => copper_index == low as usize || copper_index == high as usize,
            None => copper_index == 0 || copper_index + 1 == copper_layer_count,
        }
    }
}

/// One hole's land on one copper layer, by pool indices.
#[derive(Debug, Clone, Copy)]
pub(super) struct HoleLand {
    pub copper_index: u32,
    pub land_index: u32,
}

/// The authoritative width basis retained for one routed slot.
#[derive(Debug, Clone)]
pub(super) enum SlotWidth {
    /// Exact source primitive width (normally an IPC `Oval`).
    Nominal(f64),
    /// Final materialized slot image when the source states only an outline.
    Geometry(ContourSet),
}

/// A routed slot on a drill layer. Primitive widths stay semantic; arbitrary
/// outlines retain their materialized filled geometry for local-width checks.
#[derive(Debug, Clone)]
pub(super) struct Slot {
    pub width: SlotWidth,
    pub center: Point,
    pub bbox: BBox,
    pub layer: LayerRef,
    pub step: Option<Symbol>,
    pub padstack: Option<Symbol>,
    pub net: Option<Symbol>,
    pub source_set_index: u32,
    pub source_feature_index: u32,
}

#[derive(Debug, Clone)]
pub(super) struct Land {
    pub center: Point,
    pub bbox: BBox,
    pub step: Option<Symbol>,
    pub padstack: Symbol,
    pub primitive_ref: Option<Symbol>,
    pub net: Option<Symbol>,
    pub reference_designator: Option<Symbol>,
    pub pin: Option<Symbol>,
    pub source_set_index: u32,
    pub source_feature_index: u32,
}

#[derive(Debug)]
pub(super) struct CopperLayer {
    pub layer: LayerRef,
    pub image: ContourSet,
    pub lands: Vec<Land>,
}

#[derive(Debug)]
pub(super) struct MaskLayer {
    pub layer: LayerRef,
    /// The composed image of the mask openings.
    pub image: ContourSet,
}

#[derive(Debug, Clone)]
pub(super) struct Score {
    pub start: Point,
    pub end: Point,
    pub layer: LayerRef,
}

#[derive(Debug, Clone)]
pub(super) struct BoardOutline {
    pub name: String,
    pub instance_index: Option<u32>,
    /// Outer profile plus cutout rings.
    pub contours: Vec<Ring>,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub(super) struct BoardArray {
    pub name: String,
    pub instance_index: u32,
    pub region: ContourSet,
}

fn collect_drilled(
    ipc: &Ipc2581,
    scope: ArtworkScope,
    needs: Needs,
) -> Result<(Vec<Hole>, Vec<Slot>)> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut holes = Vec::new();
    let mut slots = Vec::new();
    for source_layer in ecad.cad_data.layers.iter().filter(|layer| {
        matches!(
            layer.layer_function,
            LayerFunction::Drill | LayerFunction::Rout
        )
    }) {
        let layer_name = ipc.resolve(source_layer.name);
        let mut document = geometry::extract_layer_for_view(ipc, layer_name, scope)
            .with_context(|| format!("failed to extract drill layer '{layer_name}'"))?;
        pcb_ir::dialects::ipc::process::expand_feature_placement_groups(&mut document);
        for feature in document
            .features
            .iter()
            .filter(|feature| feature.is_drill_like())
        {
            match feature.kind {
                FeatureKind::Hole if needs.holes => {
                    if !feature.outer_diameter.is_finite() || feature.outer_diameter <= 0.0 {
                        bail!(
                            "drilled hole on layer '{layer_name}' at ({:.6}, {:.6}) has no positive finite diameter",
                            feature.center.x,
                            feature.center.y
                        );
                    }
                    let class = hole_class(feature.intent.plating);
                    if class == HoleClass::Unknown && needs.classified_holes {
                        bail!(
                            "drilled hole on layer '{layer_name}' at ({:.6}, {:.6}) has unknown plating; class-specific DFM rules cannot certify it",
                            feature.center.x,
                            feature.center.y
                        );
                    }
                    holes.push(Hole {
                        class,
                        center: feature.center,
                        diameter_mm: feature.outer_diameter,
                        bbox: BBox::from_point(feature.center).expand(feature.outer_diameter / 2.0),
                        layer: layer_ref(layer_name, source_layer.layer_function, None),
                        copper_span: copper_span(feature.intent.span, &ecad.cad_data.layers),
                        step: feature.source_step_ref,
                        padstack: feature.padstack_ref,
                        net: feature.net,
                        lands: Vec::new(),
                        source_set_index: feature.source.set_index,
                        source_feature_index: feature.source.feature_index,
                    });
                }
                FeatureKind::Slot if needs.slots => {
                    let width = if feature.outer_diameter.is_finite()
                        && feature.outer_diameter > 0.0
                    {
                        SlotWidth::Nominal(feature.outer_diameter)
                    } else {
                        let contours = document.placed_feature_contours(feature);
                        let geometry = ContourSet::from_filled_contours(&contours, tol::REGION_MM);
                        if geometry.is_empty() {
                            bail!(
                                "routed slot on layer '{layer_name}' at ({:.6}, {:.6}) has neither a nominal width nor measurable outline geometry",
                                feature.bbox.center().x,
                                feature.bbox.center().y
                            );
                        }
                        SlotWidth::Geometry(geometry)
                    };
                    slots.push(Slot {
                        width,
                        center: feature.bbox.center(),
                        bbox: feature.bbox,
                        layer: layer_ref(layer_name, source_layer.layer_function, None),
                        step: feature.source_step_ref,
                        padstack: feature.padstack_ref,
                        net: feature.net,
                        source_set_index: feature.source.set_index,
                        source_feature_index: feature.source.feature_index,
                    });
                }
                FeatureKind::Hole | FeatureKind::Slot => {
                    continue;
                }
                _ => {}
            }
        }
    }
    holes.sort_by(|left, right| {
        left.bbox
            .min
            .x
            .total_cmp(&right.bbox.min.x)
            .then_with(|| left.center.x.total_cmp(&right.center.x))
            .then_with(|| left.center.y.total_cmp(&right.center.y))
            .then_with(|| left.diameter_mm.total_cmp(&right.diameter_mm))
    });
    slots.sort_by(|left, right| {
        left.center
            .x
            .total_cmp(&right.center.x)
            .then_with(|| left.center.y.total_cmp(&right.center.y))
            .then_with(|| slot_width_hint(&left.width).total_cmp(&slot_width_hint(&right.width)))
    });
    Ok((holes, slots))
}

/// Resolve a drill span to an inclusive ordinal range over the copper
/// stackup. Unknown, through-board, and unresolvable spans widen to `None`;
/// so does a span that reaches no copper layer at all.
fn copper_span(span: FeatureSpan<Symbol>, layers: &[ipc2581::types::Layer]) -> Option<(u16, u16)> {
    let position = |name: Symbol| layers.iter().position(|layer| layer.name == name);
    let (low, high) = match span {
        FeatureSpan::Unknown | FeatureSpan::ThroughBoard => return None,
        FeatureSpan::Layer(layer) => {
            let index = position(layer)?;
            (index, index)
        }
        FeatureSpan::FromTo {
            from: Some(from),
            to: Some(to),
        } => {
            let from = position(from)?;
            let to = position(to)?;
            (from.min(to), from.max(to))
        }
        FeatureSpan::FromTo { .. } => return None,
    };
    let mut first = None;
    let mut last = None;
    let mut ordinal: u16 = 0;
    for (index, layer) in layers.iter().enumerate() {
        if !layers::is_copper(layer.layer_function) {
            continue;
        }
        if (low..=high).contains(&index) {
            first.get_or_insert(ordinal);
            last = Some(ordinal);
        }
        ordinal += 1;
    }
    Some((first?, last?))
}

fn slot_width_hint(width: &SlotWidth) -> f64 {
    match width {
        SlotWidth::Nominal(width) => *width,
        SlotWidth::Geometry(geometry) => geometry.bbox.width().min(geometry.bbox.height()),
    }
}

fn hole_class(plating: PlatingKind) -> HoleClass {
    match plating {
        PlatingKind::Via | PlatingKind::ViaCapped => HoleClass::Via,
        PlatingKind::Plated => HoleClass::Pth,
        PlatingKind::NonPlated => HoleClass::Npth,
        PlatingKind::Unknown | PlatingKind::None => HoleClass::Unknown,
    }
}

fn collect_copper_layers(ipc: &Ipc2581, scope: ArtworkScope) -> Result<Vec<CopperLayer>> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let copper_layers = ecad
        .cad_data
        .layers
        .iter()
        .filter(|layer| layers::is_copper(layer.layer_function))
        .collect::<Vec<_>>();
    let total = copper_layers.len();
    copper_layers
        .into_par_iter()
        .enumerate()
        .map(|(ordinal, layer)| {
            let name = ipc.resolve(layer.name);
            let mut document = geometry::extract_layer_for_view(ipc, name, scope)
                .with_context(|| format!("failed to extract IPC-2581 copper layer '{name}'"))?;
            pcb_ir::dialects::ipc::process::expand_feature_placement_groups(&mut document);
            let mut lands = Vec::new();
            for feature in document.features.iter().filter(|feature| {
                feature.kind == FeatureKind::Padstack
                    && feature.polarity == Polarity::Dark
                    && feature.intent.domain == FeatureDomain::Copper
            }) {
                let Some(padstack) = feature.padstack_ref else {
                    continue;
                };
                let pin_ref = feature.pin_refs.slice(&document.pin_refs).first();
                lands.push(Land {
                    center: feature.center,
                    bbox: feature.bbox,
                    step: feature.source_step_ref,
                    padstack,
                    primitive_ref: feature.primitive_ref.map(|primitive| primitive.id()),
                    net: feature.net,
                    reference_designator: pin_ref.and_then(|pin| pin.component_ref),
                    pin: pin_ref.map(|pin| pin.pin),
                    source_set_index: feature.source.set_index,
                    source_feature_index: feature.source.feature_index,
                });
            }
            // The file's side attribute is authoritative; the stackup
            // position is the fallback for files that omit it.
            let side =
                side_label(layers::ir_side(layer.side)).unwrap_or(stack_side(ordinal, total));
            Ok(CopperLayer {
                layer: layer_ref(name, layer.layer_function, Some(side)),
                image: crate::copper_balance::composed_copper_image_from_document(document),
                lands,
            })
        })
        .collect()
}

fn side_label(side: Side) -> Option<&'static str> {
    match side {
        Side::Top => Some("top"),
        Side::Bottom => Some("bottom"),
        Side::Inner => Some("inner"),
        Side::None => None,
    }
}

fn stack_side(ordinal: usize, total: usize) -> &'static str {
    if ordinal == 0 {
        "top"
    } else if ordinal + 1 == total {
        "bottom"
    } else {
        "inner"
    }
}

fn collect_mask_layers(ipc: &Ipc2581, scope: ArtworkScope) -> Result<Vec<MaskLayer>> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    ecad.cad_data
        .layers
        .iter()
        .filter(|layer| layer.layer_function == LayerFunction::Soldermask)
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|layer| {
            let name = ipc.resolve(layer.name);
            let document = geometry::extract_layer_for_view(ipc, name, scope)
                .with_context(|| format!("failed to extract soldermask layer '{name}'"))?;
            Ok(MaskLayer {
                layer: layer_ref(
                    name,
                    layer.layer_function,
                    side_label(layers::ir_side(layer.side)),
                ),
                image: crate::copper_balance::composed_copper_image_from_document(document),
            })
        })
        .collect()
}

/// Attach each hole to its land on every copper layer it spans: same
/// padstack, same step, compatible net, overlapping bounds, nearest center.
///
/// Lands are gridded by their bounds so each hole only meets the handful of
/// lands around it, not every instance of its padstack on the layer.
fn link_lands(holes: &mut [Hole], copper_layers: &[CopperLayer]) {
    const CELL_MM: f64 = 1.0;
    let cell = |value: f64| (value / CELL_MM).floor() as i64;
    let mut candidates: Vec<u32> = Vec::new();
    for (copper_index, copper) in copper_layers.iter().enumerate() {
        let mut grid: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
        for (land_index, land) in copper.lands.iter().enumerate() {
            for x in cell(land.bbox.min.x)..=cell(land.bbox.max.x) {
                for y in cell(land.bbox.min.y)..=cell(land.bbox.max.y) {
                    grid.entry((x, y)).or_default().push(land_index as u32);
                }
            }
        }
        for hole in holes.iter_mut() {
            if !hole.spans_copper(copper_index as u16) {
                continue;
            }
            let Some(padstack) = hole.padstack else {
                continue;
            };
            candidates.clear();
            for x in cell(hole.bbox.min.x)..=cell(hole.bbox.max.x) {
                for y in cell(hole.bbox.min.y)..=cell(hole.bbox.max.y) {
                    candidates.extend(grid.get(&(x, y)).into_iter().flatten().copied());
                }
            }
            candidates.sort_unstable();
            candidates.dedup();
            let best = candidates
                .iter()
                .map(|&land_index| (land_index, &copper.lands[land_index as usize]))
                .filter(|(_, land)| land.padstack == padstack)
                .filter(|(_, land)| land.step == hole.step)
                .filter(|(_, land)| {
                    land.net.is_none() || hole.net.is_none() || land.net == hole.net
                })
                .filter(|(_, land)| land.bbox.intersects(hole.bbox))
                .min_by(|(_, left), (_, right)| {
                    left.center
                        .distance_to(hole.center)
                        .total_cmp(&right.center.distance_to(hole.center))
                });
            if let Some((land_index, _)) = best {
                hole.lands.push(HoleLand {
                    copper_index: copper_index as u32,
                    land_index,
                });
            }
        }
    }
}

fn collect_scores(ipc: &Ipc2581, scope: ArtworkScope) -> Result<Vec<Score>> {
    Ok(geometry::vscore_lines(ipc, scope)?
        .into_iter()
        .map(|(layer, function, line)| Score {
            start: line.start,
            end: line.end,
            layer: layer_ref(ipc.resolve(layer), function, None),
        })
        .collect())
}

fn collect_board_outlines(
    ipc: &Ipc2581,
    layout: &GeometryDocument,
    scope: ArtworkScope,
) -> Vec<BoardOutline> {
    profile_occurrences_for(layout, scope.profile_set())
        .into_iter()
        .filter(|occurrence| {
            matches!(
                occurrence.role,
                ProfileOccurrenceRole::RootBoard
                    | ProfileOccurrenceRole::BoardDefinition
                    | ProfileOccurrenceRole::BoardInstance
            )
        })
        .filter_map(|occurrence| {
            let mut contours = layout
                .transformed_path_contours(occurrence.profile.outer_path, occurrence.transform);
            for cutout in occurrence.profile.cutouts.slice(&layout.profile_cutouts) {
                contours
                    .extend(layout.transformed_path_contours(cutout.path, occurrence.transform));
            }
            let rings = pcb_ir::geom::region::rings_from_contours(&contours);
            if rings.is_empty() {
                return None;
            }
            let bbox = pcb_ir::geom::region::rings_bbox(&rings);
            let name = occurrence
                .step
                .and_then(|step| layout.layout.steps.get(step as usize))
                .map(|step| ipc.resolve(step.source_step_ref).to_owned())
                .unwrap_or_else(|| "board".to_owned());
            Some(BoardOutline {
                name,
                instance_index: occurrence.instance,
                contours: rings,
                bbox,
            })
        })
        .collect()
}

fn collect_board_arrays(ipc: &Ipc2581, layout: &GeometryDocument) -> Vec<BoardArray> {
    let Some(root_step) = layout.layout.root_step else {
        return Vec::new();
    };
    // A panel-kind child that wraps exactly one board is per-board packaging
    // (a cell in a larger grid), not a sibling array to keep spacing from; a
    // one-board array still nests its own panel-kind cell.
    let wraps_single_board = |instance_index: usize| {
        let mut children = layout
            .layout
            .instances
            .iter()
            .filter(|child| child.parent_instance == Some(instance_index as u32));
        match (children.next(), children.next()) {
            (Some(only), None) => {
                layout.layout.steps[only.child_step as usize].kind == LayoutStepKind::Board
            }
            _ => false,
        }
    };
    layout
        .layout
        .instances
        .iter()
        .enumerate()
        .filter(|(instance_index, instance)| {
            instance.parent_instance.is_none()
                && layout.layout.repeats[instance.repeat as usize].parent_step == root_step
                && layout.layout.steps[instance.child_step as usize].kind == LayoutStepKind::Panel
                && !wraps_single_board(*instance_index)
        })
        .filter_map(|(instance_index, instance)| {
            let step = &layout.layout.steps[instance.child_step as usize];
            let contours = step
                .profiles
                .slice(&layout.profiles)
                .iter()
                .flat_map(|profile| {
                    layout.transformed_path_contours(profile.outer_path, instance.transform)
                })
                .collect::<Vec<_>>();
            let region = ContourSet::from_filled_contours(&contours, tol::REGION_MM);
            (!region.is_empty()).then(|| BoardArray {
                name: ipc.resolve(step.source_step_ref).to_owned(),
                instance_index: instance_index as u32,
                region,
            })
        })
        .collect()
}

fn layer_ref(name: &str, function: LayerFunction, side: Option<&'static str>) -> LayerRef {
    LayerRef {
        name: name.to_owned(),
        function: function.as_str().to_owned(),
        side,
    }
}

#[cfg(test)]
mod tests {
    use pcb_ir::geom::dfm::thin_features;

    use super::*;

    fn slot_fixture(shape: &str) -> Ipc2581 {
        Ipc2581::parse(&format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="ROUT"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="ROUT" layerFunction="ROUT" side="ALL" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <LayerFeature layerRef="ROUT">
          <Set>
            <SlotCavity name="S1" platingStatus="PLATED" plusTol="0" minusTol="0">
              {shape}
            </SlotCavity>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
        ))
        .unwrap()
    }

    #[test]
    fn retains_nominal_oval_slot_width() {
        let ipc = slot_fixture(
            r#"<Location x="10" y="20"/>
              <Oval width="1.8" height="0.6"/>"#,
        );

        let (_, slots) = collect_drilled(
            &ipc,
            ArtworkScope::Board,
            Needs {
                slots: true,
                ..Needs::default()
            },
        )
        .unwrap();

        assert_eq!(slots.len(), 1);
        let SlotWidth::Nominal(width) = slots[0].width else {
            panic!("oval slot must retain its nominal width")
        };
        assert!((width - 0.6).abs() < 1e-9);
    }

    #[test]
    fn retains_and_measures_outline_slot_geometry() {
        let ipc = slot_fixture(
            r#"<Outline>
                <Polygon>
                  <PolyBegin x="10" y="20"/>
                  <PolyStepSegment x="10.6" y="20"/>
                  <PolyStepSegment x="10.6" y="21.8"/>
                  <PolyStepSegment x="10" y="21.8"/>
                  <PolyStepSegment x="10" y="20"/>
                </Polygon>
                <LineDesc lineWidth="0" lineEnd="ROUND"/>
              </Outline>"#,
        );

        let (_, slots) = collect_drilled(
            &ipc,
            ArtworkScope::Board,
            Needs {
                slots: true,
                ..Needs::default()
            },
        )
        .unwrap();

        assert_eq!(slots.len(), 1);
        let SlotWidth::Geometry(geometry) = &slots[0].width else {
            panic!("outline slot must retain materialized geometry")
        };
        let violations = thin_features(geometry, 0.8);
        assert_eq!(violations.len(), 1);
        assert!(
            (violations[0].width_mm - 0.6).abs() < 1e-8,
            "measured width was {}",
            violations[0].width_mm
        );
    }
}
