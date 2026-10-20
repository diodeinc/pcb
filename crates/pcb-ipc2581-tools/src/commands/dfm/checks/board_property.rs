//! Properties of the whole board: copper weight, finished thickness, surface
//! finish, soldermask colour, and size, each judged against the values a
//! profile or rule allows.
//!
//! A property is categorical: it conforms or it does not, so each rule
//! permits no nonconforming property and reports one finding per property
//! that is not allowed. Copper weight compares standard weights, as weight
//! conditions do ([`nominal_copper_weight`]). Thickness conforms when it lies
//! within the tolerance of some nominal thickness. Size compares the
//! layout's extents long side to long side. A stackup property the file does
//! not state leaves the rule incomplete; it is never assumed allowed. A
//! surface finish or soldermask colour the file does not state is the fab's
//! default choice, so the rule has nothing to judge.

use ipc2581::types::LayerFunction;

use crate::accessors::{ColorInfo, format_ounces, nominal_copper_weight};
use crate::commands::dfm::design::Design;
use crate::commands::dfm::pdk::{CopperWeight, LayerPosition, SoldermaskColor, SurfaceFinish};
use crate::commands::dfm::report::{LayerRef, RuleStatus, Subject};
use crate::commands::dfm::rules::{Allowed, Rule, RuleKind, position_label};
use crate::layers;
use pcb_ir::geom::dfm::COMPARISON_EPSILON_MM;

pub(super) struct Evaluation {
    pub checked: usize,
    pub nonconforming: Vec<Nonconforming>,
    pub assumptions: Vec<String>,
}

pub(super) struct Nonconforming {
    pub message: String,
    pub layers: Vec<LayerRef>,
    pub subject: Subject,
}

/// Why a rule judged nothing: an `Incomplete` or `NotApplicable` status.
type Unjudged = (RuleStatus, String);

fn judged(checked: usize, nonconforming: impl IntoIterator<Item = Nonconforming>) -> Evaluation {
    Evaluation {
        checked,
        nonconforming: nonconforming.into_iter().collect(),
        assumptions: Vec::new(),
    }
}

fn incomplete(reason: impl Into<String>) -> Unjudged {
    (RuleStatus::Incomplete, reason.into())
}

fn unstated(reason: &str) -> Unjudged {
    (RuleStatus::NotApplicable, reason.to_owned())
}

fn subject(kind: &'static str, name: impl Into<String>) -> Subject {
    Subject {
        role: "measured",
        kind,
        name: Some(name.into()),
        ..Subject::default()
    }
}

pub(super) fn evaluate(rule: &Rule, design: &Design) -> Result<Evaluation, Unjudged> {
    let allowed_text = rule.limit.allowed().describe();
    match (rule.kind, rule.limit.allowed()) {
        (RuleKind::CopperWeight(position), Allowed::CopperWeights(allowed)) => {
            copper_weight(rule, position, allowed, &allowed_text, design)
        }
        (RuleKind::BoardThickness, Allowed::Thickness(limit)) => {
            let stackup = design
                .stackup
                .as_ref()
                .expect("thickness rules request stackup extraction");
            let thickness = stackup.total_thickness().map_err(incomplete)?.millimeters;
            let fits = limit.nominal.iter().any(|nominal| {
                let nominal = nominal.millimeters();
                (thickness - nominal).abs() <= limit.tolerance_mm(nominal) + COMPARISON_EPSILON_MM
            });
            let nonconforming = (!fits).then(|| Nonconforming {
                message: format!(
                    "finished board thickness is {thickness:.4} mm; the PDK allows {allowed_text}"
                ),
                layers: Vec::new(),
                subject: subject("stackup", stackup.name.clone()),
            });
            Ok(judged(1, nonconforming))
        }
        (RuleKind::SurfaceFinish, Allowed::Finishes(allowed)) => {
            surface_finish(allowed, &allowed_text, design)
        }
        (RuleKind::SoldermaskColor, Allowed::Colors(allowed)) => {
            soldermask_color(allowed, &allowed_text, design)
        }
        (RuleKind::BoardSize, Allowed::Size(limit)) => {
            // As for board-edge clearance: a layout without a profile, such
            // as a module's, is not a board to size.
            let bounds = design
                .report_layout()
                .bounding_box
                .ok_or_else(|| unstated("the selected layout target has no board profile"))?;
            let (width, height) = (bounds.max.x - bounds.min.x, bounds.max.y - bounds.min.y);
            let extents = [width.max(height), width.min(height)];
            let below = limit.minimum().is_some_and(|minimum| {
                (0..2).any(|side| extents[side] < minimum[side] - COMPARISON_EPSILON_MM)
            });
            let above = limit.maximum().is_some_and(|maximum| {
                (0..2).any(|side| extents[side] > maximum[side] + COMPARISON_EPSILON_MM)
            });
            let nonconforming = (below || above).then(|| Nonconforming {
                message: format!(
                    "board extents are {:.3} × {:.3} mm; the PDK allows {allowed_text}",
                    extents[0], extents[1]
                ),
                layers: Vec::new(),
                subject: subject("board", "layout profile"),
            });
            Ok(judged(1, nonconforming))
        }
        (kind, _) => unreachable!("{kind:?} is not a board property with this limit"),
    }
}

fn copper_weight(
    rule: &Rule,
    position: LayerPosition,
    allowed: &[CopperWeight],
    allowed_text: &str,
    design: &Design,
) -> Result<Evaluation, Unjudged> {
    let stackup = design
        .stackup
        .as_ref()
        .expect("copper-weight rules request stackup extraction");
    let assumed = match position {
        LayerPosition::Outer => rule.conditions.assumed_outer_copper_weight_oz,
        LayerPosition::Inner => rule.conditions.assumed_inner_copper_weight_oz,
    };
    let mut evaluation = judged(0, []);
    for (layer, stated) in stackup.copper_layers.iter().zip(&stackup.copper_weights_oz) {
        let layer_position = if layer.side == Some("inner") {
            LayerPosition::Inner
        } else {
            LayerPosition::Outer
        };
        if layer_position != position {
            continue;
        }
        let ounces = match (stated, assumed) {
            (Some(ounces), _) => *ounces,
            (None, Some(ounces)) => {
                evaluation.assumptions.push(format!(
                    "copper layer '{}' states no weight or thickness; used selected profile defaults.{}_copper_weight = {ounces} oz.",
                    layer.name,
                    position_label(position)
                ));
                ounces
            }
            (None, None) => {
                return Err(incomplete(format!(
                    "copper layer '{}' states no weight or thickness",
                    layer.name
                )));
            }
        };
        evaluation.checked += 1;
        let nominal = nominal_copper_weight(ounces, position == LayerPosition::Outer);
        if !allowed
            .iter()
            .any(|weight| Some(weight.ounces()) == nominal)
        {
            let weight = match nominal {
                Some(nominal) => format!("{} oz standard weight", format_ounces(nominal)),
                None => "no standard weight".to_owned(),
            };
            evaluation.nonconforming.push(Nonconforming {
                message: format!(
                    "{} copper layer '{}' is {ounces:.2} oz ({weight}); the PDK allows {allowed_text}",
                    position_label(position),
                    layer.name,
                ),
                layers: vec![layer.clone()],
                subject: subject("copper_layer", layer.name.clone()),
            });
        }
    }
    Ok(evaluation)
}

/// A layout without a profile has no board to fabricate.
pub(super) fn outline(design: &Design) -> Evaluation {
    let layout = design.report_layout();
    let missing = layout.bounding_box.is_none().then(|| Nonconforming {
        message: "the selected layout target has no board outline".to_owned(),
        layers: Vec::new(),
        subject: subject("board", layout.selected_step.unwrap_or_default()),
    });
    judged(1, missing)
}

/// The surface finish each coating layer's stackup spec states, judged once
/// per spec, with the coating layers that reference it.
fn surface_finish(
    allowed: &[SurfaceFinish],
    allowed_text: &str,
    design: &Design,
) -> Result<Evaluation, Unjudged> {
    use ipc2581::types::FinishType;
    let imported = design.imported;
    let mut finishes: Vec<(&str, FinishType, Vec<LayerRef>)> = Vec::new();
    for stackup_layer in imported.stackups.iter().flat_map(|stackup| &stackup.layers) {
        let Some(coating) = imported.layer_definitions.iter().find(|layer| {
            layer.name == stackup_layer.layer_ref
                && matches!(
                    layer.layer_function,
                    LayerFunction::CoatingCond | LayerFunction::CoatingNonCond
                )
        }) else {
            continue;
        };
        let Some((spec, finish)) = stackup_layer
            .spec_ref
            .and_then(|spec| imported.specs.get(&spec))
            .and_then(|spec| Some((imported.resolve(spec.name), spec.surface_finish.as_ref()?)))
        else {
            continue;
        };
        let layer = layer_ref(imported, coating);
        match finishes.iter_mut().find(|(name, ..)| *name == spec) {
            Some((.., layers)) => layers.push(layer),
            None => finishes.push((spec, finish.finish_type, vec![layer])),
        }
    }
    if finishes.is_empty() {
        return Err(unstated("the IPC-2581 file states no surface finish"));
    }
    let mut nonconforming = Vec::new();
    for (spec, finish, layers) in &finishes {
        let stated = match (finish, SurfaceFinish::from_ipc(*finish)) {
            (_, Some(family)) if allowed.contains(&family) => continue,
            (_, Some(family)) => family.label(),
            // Solder leveling of unstated lead content conforms only where
            // either lead content would.
            (FinishType::S, None) => match (
                allowed.contains(&SurfaceFinish::Hasl),
                allowed.contains(&SurfaceFinish::LeadFreeHasl),
            ) {
                (true, true) => continue,
                (false, false) => "solder leveling (IPC S)",
                _ => {
                    return Err(incomplete(format!(
                        "surface finish spec '{spec}' states solder leveling (IPC S) without its lead content; the PDK allows {allowed_text}"
                    )));
                }
            },
            (_, None) => "OTHER",
        };
        nonconforming.push(Nonconforming {
            message: format!("surface finish is {stated}; the PDK allows {allowed_text}"),
            layers: layers.clone(),
            subject: subject("surface_finish", *spec),
        });
    }
    Ok(judged(finishes.len(), nonconforming))
}

fn layer_ref(
    imported: &pcb_ir::import::ipc2581::ImportedDesign,
    layer: &ipc2581::types::Layer,
) -> LayerRef {
    LayerRef {
        name: imported.resolve(layer.name).to_owned(),
        function: layer.layer_function.as_str().to_owned(),
        side: match layers::ir_side(layer.side) {
            pcb_ir::dialects::Side::Top => Some("top"),
            pcb_ir::dialects::Side::Bottom => Some("bottom"),
            _ => None,
        },
    }
}

/// The colour each soldermask layer's stackup spec gives it, by colour term,
/// `Color : <name>` property, or RGB.
fn soldermask_color(
    allowed: &[SoldermaskColor],
    allowed_text: &str,
    design: &Design,
) -> Result<Evaluation, Unjudged> {
    let imported = design.imported;
    let stackup_layers = imported
        .stackups
        .iter()
        .flat_map(|stackup| &stackup.layers)
        .collect::<Vec<_>>();
    let masks = imported
        .layer_definitions
        .iter()
        .filter(|layer| layer.layer_function == LayerFunction::Soldermask)
        .collect::<Vec<_>>();
    if masks.is_empty() {
        return Err(unstated("the design has no soldermask layers"));
    }
    let mut nonconforming = Vec::new();
    let mut stated = 0;
    for mask in &masks {
        let name = imported.resolve(mask.name);
        let spec = stackup_layers
            .iter()
            .find(|layer| layer.layer_ref == mask.name)
            .and_then(|layer| imported.specs.get(&layer.spec_ref?));
        let Some(ink) = spec
            .map(|spec| ColorInfo::of_spec(spec, |symbol| imported.resolve(symbol)))
            .filter(ColorInfo::is_stated)
        else {
            continue;
        };
        stated += 1;
        // A stated colour no ink colour recognizes is not an allowed one.
        let color = ink.color_name().map(|color| SoldermaskColor::new(&color));
        if !color.as_ref().is_some_and(|color| allowed.contains(color)) {
            let layer = layer_ref(imported, mask);
            let stated = match &color {
                Some(color) => color.name().to_owned(),
                None => format!(
                    "an unrecognized colour ({})",
                    ink.hex_color()
                        .or_else(|| ink.name.clone())
                        .unwrap_or_default()
                ),
            };
            nonconforming.push(Nonconforming {
                message: format!(
                    "soldermask layer '{name}' is {stated}; the PDK allows {allowed_text}"
                ),
                layers: vec![layer],
                subject: subject("soldermask_layer", name),
            });
        }
    }
    if stated == 0 {
        return Err(unstated("the IPC-2581 file states no soldermask colour"));
    }
    Ok(judged(stated, nonconforming))
}
