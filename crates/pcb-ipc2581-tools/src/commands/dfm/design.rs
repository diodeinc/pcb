use std::collections::HashMap;

use anyhow::{Context, Result};
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::ipc::{
    ArtworkScope, FeatureDomain, FeatureKind, LayoutStepKind, PlatingKind, ProfileOccurrenceRole,
    profile_occurrences_for, relief::vscore_lines_for,
};
use pcb_ir::geom::region::Ring;
use pcb_ir::geom::{BBox, ContourSet, Point, Polarity, tol};

use crate::geometry;
use crate::ipc2581::Ipc2581;
use crate::layers;

use super::pdk::Pdk;
use super::report::LayerRef;

pub(super) struct Design {
    pub scope: ArtworkScope,
    pub holes: Vec<Hole>,
    pub copper_layers: Vec<CopperLayer>,
    pub scores: Vec<Score>,
    pub board_outlines: Vec<BoardOutline>,
    pub board_arrays: Vec<BoardArray>,
}

impl Design {
    pub fn extract(ipc: &Ipc2581, pdk: &Pdk, scope: ArtworkScope) -> Result<Self> {
        let drilling = &pdk.capabilities.drilling;
        let copper = &pdk.capabilities.copper;
        let needs_holes = drilling.minimum_via_hole_diameter.is_some()
            || drilling.minimum_pth_hole_diameter.is_some()
            || drilling.minimum_npth_hole_diameter.is_some()
            || drilling.minimum_hole_to_hole_clearance.is_some()
            || copper.minimum_via_annular_ring.is_some()
            || copper.minimum_pth_annular_ring.is_some();
        let needs_copper = copper.minimum_via_annular_ring.is_some()
            || copper.minimum_pth_annular_ring.is_some()
            || copper.minimum_vscore_to_copper_clearance.is_some()
            || copper.minimum_board_edge_clearance.is_some();

        Ok(Self {
            scope,
            holes: if needs_holes {
                collect_holes(ipc, scope)?
            } else {
                Vec::new()
            },
            copper_layers: if needs_copper {
                collect_copper_layers(ipc, scope)?
            } else {
                Vec::new()
            },
            scores: if copper.minimum_vscore_to_copper_clearance.is_some() {
                collect_scores(ipc, scope)?
            } else {
                Vec::new()
            },
            board_outlines: if copper.minimum_board_edge_clearance.is_some() {
                collect_board_outlines(ipc, scope)?
            } else {
                Vec::new()
            },
            board_arrays: if pdk
                .capabilities
                .panelization
                .minimum_board_array_spacing
                .is_some()
                && scope == ArtworkScope::ArrayFlattened
            {
                collect_board_arrays(ipc)?
            } else {
                Vec::new()
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HoleClass {
    Via,
    Pth,
    Npth,
}

impl HoleClass {
    pub fn label(self) -> &'static str {
        match self {
            Self::Via => "via",
            Self::Pth => "PTH",
            Self::Npth => "NPTH",
        }
    }

    pub fn subject_kind(self) -> &'static str {
        match self {
            Self::Via => "via_hole",
            Self::Pth => "plated_hole",
            Self::Npth => "nonplated_hole",
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
    pub step: Option<String>,
    pub padstack_ref: Option<String>,
    pub net: Option<String>,
    pub source_set_index: u32,
    pub source_feature_index: u32,
    pub copper_layers: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub(super) struct Land {
    pub center: Point,
    pub bbox: BBox,
    pub step: Option<String>,
    pub padstack_ref: String,
    pub primitive_ref: Option<String>,
    pub net: Option<String>,
    pub reference_designator: Option<String>,
    pub pin: Option<String>,
    pub source_set_index: u32,
    pub source_feature_index: u32,
}

#[derive(Debug)]
pub(super) struct CopperLayer {
    pub layer: LayerRef,
    pub image: ContourSet,
    pub lands_by_padstack: HashMap<String, Vec<Land>>,
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
    pub contours: Vec<Ring>,
    pub bbox: BBox,
}

#[derive(Debug, Clone)]
pub(super) struct BoardArray {
    pub name: String,
    pub instance_index: u32,
    pub region: ContourSet,
}

fn collect_holes(ipc: &Ipc2581, scope: ArtworkScope) -> Result<Vec<Hole>> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut holes = Vec::new();
    for source_layer in ecad
        .cad_data
        .layers
        .iter()
        .filter(|layer| layer.layer_function == LayerFunction::Drill)
    {
        let layer_name = ipc.resolve(source_layer.name);
        let copper_layers = copper_layers_for_drill_layer(ipc, source_layer);
        let mut document = geometry::extract_layer_for_view(ipc, layer_name, scope)
            .with_context(|| format!("failed to extract drill layer '{layer_name}'"))?;
        pcb_ir::dialects::ipc::process::expand_feature_placement_groups(&mut document);
        for feature in document
            .features
            .iter()
            .filter(|feature| feature.kind == FeatureKind::Hole && feature.is_drill_like())
        {
            let Some(class) = hole_class(feature.intent.plating) else {
                continue;
            };
            if !(feature.outer_diameter.is_finite() && feature.outer_diameter > 0.0) {
                continue;
            }
            let radius = feature.outer_diameter / 2.0;
            holes.push(Hole {
                class,
                center: feature.center,
                diameter_mm: feature.outer_diameter,
                bbox: BBox::new(
                    Point::new(feature.center.x - radius, feature.center.y - radius),
                    Point::new(feature.center.x + radius, feature.center.y + radius),
                ),
                layer: layer_ref(layer_name, source_layer.layer_function),
                step: feature
                    .source_step_ref
                    .map(|symbol| ipc.resolve(symbol).to_owned()),
                padstack_ref: feature
                    .padstack_ref
                    .map(|symbol| ipc.resolve(symbol).to_owned()),
                net: feature.net.map(|symbol| ipc.resolve(symbol).to_owned()),
                source_set_index: feature.source.set_index,
                source_feature_index: feature.source.feature_index,
                copper_layers: copper_layers.clone(),
            });
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
    Ok(holes)
}

fn copper_layers_for_drill_layer(
    ipc: &Ipc2581,
    drill_layer: &ipc2581::types::Layer,
) -> Option<Vec<String>> {
    let span = drill_layer.span?;
    let from = span.from_layer?;
    let to = span.to_layer?;
    let layers = &ipc.ecad()?.cad_data.layers;
    let from_index = layers.iter().position(|layer| layer.name == from)?;
    let to_index = layers.iter().position(|layer| layer.name == to)?;
    let (start, end) = if from_index <= to_index {
        (from_index, to_index)
    } else {
        (to_index, from_index)
    };
    Some(
        layers[start..=end]
            .iter()
            .filter(|layer| layers::is_copper(layer.layer_function))
            .map(|layer| ipc.resolve(layer.name).to_owned())
            .collect(),
    )
}

fn hole_class(plating: PlatingKind) -> Option<HoleClass> {
    match plating {
        PlatingKind::Via | PlatingKind::ViaCapped => Some(HoleClass::Via),
        PlatingKind::Plated => Some(HoleClass::Pth),
        PlatingKind::NonPlated => Some(HoleClass::Npth),
        PlatingKind::Unknown | PlatingKind::None => None,
    }
}

fn collect_copper_layers(ipc: &Ipc2581, scope: ArtworkScope) -> Result<Vec<CopperLayer>> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    ecad.cad_data
        .layers
        .iter()
        .filter(|layer| layers::is_copper(layer.layer_function))
        .map(|layer| {
            let name = ipc.resolve(layer.name);
            let mut document = geometry::extract_layer_for_view(ipc, name, scope)
                .with_context(|| format!("failed to extract IPC-2581 copper layer '{name}'"))?;
            pcb_ir::dialects::ipc::process::expand_feature_placement_groups(&mut document);
            let mut lands_by_padstack: HashMap<String, Vec<Land>> = HashMap::new();
            for feature in document.features.iter().filter(|feature| {
                feature.kind == FeatureKind::Padstack
                    && feature.polarity == Polarity::Dark
                    && feature.intent.domain == FeatureDomain::Copper
            }) {
                let Some(padstack_ref) = feature.padstack_ref else {
                    continue;
                };
                let pin_ref = feature.pin_refs.slice(&document.pin_refs).first();
                let padstack_ref = ipc.resolve(padstack_ref).to_owned();
                lands_by_padstack
                    .entry(padstack_ref.clone())
                    .or_default()
                    .push(Land {
                        center: feature.center,
                        bbox: feature.bbox,
                        step: feature
                            .source_step_ref
                            .map(|step| ipc.resolve(step).to_owned()),
                        padstack_ref,
                        primitive_ref: feature
                            .primitive_ref
                            .map(|primitive| ipc.resolve(primitive.id()).to_owned()),
                        net: feature.net.map(|net| ipc.resolve(net).to_owned()),
                        reference_designator: pin_ref
                            .and_then(|pin| pin.component_ref)
                            .map(|component| ipc.resolve(component).to_owned()),
                        pin: pin_ref.map(|pin| ipc.resolve(pin.pin).to_owned()),
                        source_set_index: feature.source.set_index,
                        source_feature_index: feature.source.feature_index,
                    });
            }
            Ok(CopperLayer {
                layer: layer_ref(name, layer.layer_function),
                image: crate::copper_balance::composed_copper_image_from_document(document),
                lands_by_padstack,
            })
        })
        .collect()
}

fn collect_scores(ipc: &Ipc2581, scope: ArtworkScope) -> Result<Vec<Score>> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut scores = Vec::new();
    for layer in ecad.cad_data.layers.iter().filter(|layer| {
        matches!(
            layer.layer_function,
            LayerFunction::VCut | LayerFunction::Score
        )
    }) {
        let name = ipc.resolve(layer.name);
        let document = geometry::extract_layer_for_view(ipc, name, scope)
            .with_context(|| format!("failed to extract V-score layer '{name}'"))?;
        scores.extend(vscore_lines_for(&document).into_iter().map(|line| Score {
            start: line.start,
            end: line.end,
            layer: layer_ref(name, layer.layer_function),
        }));
    }
    Ok(scores)
}

fn collect_board_outlines(ipc: &Ipc2581, scope: ArtworkScope) -> Result<Vec<BoardOutline>> {
    let layout = geometry::extract_layout(ipc)?;
    Ok(profile_occurrences_for(&layout, scope.profile_set())
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
            let contours = layout
                .transformed_path_contours(occurrence.profile.outer_path, occurrence.transform);
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
        .collect())
}

fn collect_board_arrays(ipc: &Ipc2581) -> Result<Vec<BoardArray>> {
    let layout = geometry::extract_layout(ipc)?;
    let Some(root_step) = layout.layout.root_step else {
        return Ok(Vec::new());
    };
    Ok(layout
        .layout
        .instances
        .iter()
        .enumerate()
        .filter(|(_, instance)| {
            instance.parent_instance.is_none()
                && layout.layout.repeats[instance.repeat as usize].parent_step == root_step
                && layout.layout.steps[instance.child_step as usize].kind == LayoutStepKind::Panel
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
        .collect())
}

fn layer_ref(name: &str, function: LayerFunction) -> LayerRef {
    LayerRef {
        name: name.to_owned(),
        function: format!("{function:?}").to_ascii_lowercase(),
    }
}
