//! Component placement data (pick-and-place).
//!
//! Placement is a deliberately compact lowering of the assembly dialect.

use std::collections::BTreeSet;

use anyhow::{Result, bail};

use super::assembly;
use crate::geom::{Affine2, Point};

pub use super::assembly::{Mount as PlacementMount, Population, Side as PlacementSide};

#[derive(Debug, Clone)]
pub struct Document {
    pub scope: assembly::Scope,
    pub step: Option<String>,
    pub components: Vec<Placement>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            scope: assembly::Scope::Board,
            step: None,
            components: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Placement {
    pub id: assembly::ComponentOccurrenceId,
    pub designator: String,
    pub value: Option<String>,
    pub package: Option<String>,
    pub bom_category: Option<assembly::BomCategory>,
    pub part: String,
    pub layer_ref: String,
    pub side: PlacementSide,
    pub mount: PlacementMount,
    pub at: Point,
    pub rotation_degrees: f64,
    pub mirror: bool,
    pub face_up: bool,
    pub scale: f64,
    pub population: Population,
}

/// Lower one canonical board from an assembly document.
///
/// Root-step components take precedence. Otherwise the source must contain
/// exactly one component-bearing repeated step.
pub fn lower_single_board(source: &assembly::Document) -> Result<Document> {
    let root_has_components = source.root_step.is_some_and(|root| {
        source
            .occurrences
            .iter()
            .any(|occurrence| source.components[occurrence.id.component.0 as usize].step == root)
    });
    let component_steps = source
        .occurrences
        .iter()
        .map(|occurrence| source.components[occurrence.id.component.0 as usize].step)
        .collect::<BTreeSet<_>>();
    let selected_step = if root_has_components {
        source.root_step
    } else {
        match component_steps
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .as_slice()
        {
            [] => None,
            [step] => Some(*step),
            steps => {
                let names = steps
                    .iter()
                    .map(|step| source.steps[*step as usize].name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "placement lowering found multiple component-bearing repeated Steps ({names}); single-board placement is ambiguous"
                );
            }
        }
    };
    let Some(selected_step) = selected_step else {
        return Ok(Document::default());
    };

    let mut components = Vec::new();
    let mut emitted = BTreeSet::new();
    for occurrence in &source.occurrences {
        let component = &source.components[occurrence.id.component.0 as usize];
        if component.step != selected_step || !emitted.insert(component.id) {
            continue;
        }
        let Some(designator) = component
            .designator
            .as_deref()
            .filter(|designator| !designator.is_empty())
        else {
            continue;
        };
        let bom_reference = source.preferred_bom_reference(component);
        if occurrence.population == Population::Conflicting {
            bail!("component '{designator}' has conflicting population state");
        }

        let bom_item = bom_reference.map(|reference| source.bom_item(reference));
        let bom_designator = bom_reference.map(|reference| source.bom_designator(reference));
        let package = bom_designator
            .and_then(|reference| reference.package_ref.clone())
            .or_else(|| {
                bom_item.and_then(|item| {
                    item.textual_characteristic("package")
                        .or_else(|| item.textual_characteristic("footprint"))
                        .map(str::to_owned)
                })
            })
            .or_else(|| component.package_ref.clone());
        let value = bom_item
            .and_then(|item| item.textual_characteristic("value"))
            .map(str::to_owned);
        let transform = occurrence
            .board_from_component
            .unwrap_or(occurrence.root_from_component);
        let placement = decompose_placement(transform)?;

        components.push(Placement {
            id: assembly::ComponentOccurrenceId {
                component: component.id,
                layout: assembly::LayoutOccurrenceId::Root,
            },
            designator: designator.to_owned(),
            value,
            package,
            bom_category: bom_item.and_then(|item| item.category),
            part: component.part.clone(),
            layer_ref: component.layer_ref.clone(),
            side: component.side,
            mount: component.mount,
            at: placement.at,
            rotation_degrees: placement.rotation_degrees,
            mirror: placement.mirror,
            face_up: component
                .source_transform
                .is_some_and(|transform| transform.face_up),
            scale: placement.scale,
            population: occurrence.population,
        });
    }

    Ok(Document {
        scope: assembly::Scope::Board,
        step: Some(source.steps[selected_step as usize].name.clone()),
        components,
    })
}

struct DecomposedPlacement {
    at: Point,
    rotation_degrees: f64,
    mirror: bool,
    scale: f64,
}

fn decompose_placement(transform: Affine2) -> Result<DecomposedPlacement> {
    let scale = transform.m00.hypot(transform.m10);
    let other_scale = transform.m01.hypot(transform.m11);
    let dot = transform.m00 * transform.m01 + transform.m10 * transform.m11;
    let epsilon = 1e-9 * scale.max(other_scale).max(1.0);
    if scale <= 0.0 || (scale - other_scale).abs() > epsilon || dot.abs() > epsilon {
        bail!("component occurrence transform is not a rigid uniform placement");
    }
    let mirror = transform.determinant() < 0.0;
    let signed_scale = if mirror { -scale } else { scale };
    Ok(DecomposedPlacement {
        at: Point::new(transform.m02, transform.m12),
        rotation_degrees: (transform.m10 / signed_scale)
            .atan2(transform.m00 / signed_scale)
            .to_degrees(),
        mirror,
        scale,
    })
}
