//! The rule engine: one evaluator per measurement kind, uniform bookkeeping.
//!
//! Every rule reduces to enumerating subjects and taking one [`Distance`]
//! per subject. Each check lives in its own module, whose docstring defines
//! the measurement mathematically, and returns those measurements with the
//! report identity of what was measured. The engine here owns everything
//! else — the verdict against the limit, finding text, witness roles, skip
//! reasons, checked counts, finding order, stable ids, waivers, statuses —
//! so a check only measures, and may assume its subject pools are non-empty.
//!
//! A check measures one [`Design`]: one Step with everything it places. It
//! measures the Step's own subjects, and a pair of subjects unless both lie
//! inside one placement ([`spans`](super::design::spans)), which that
//! placement's own design measures. The engine runs every rule over every
//! design and tells each finding which one it came from.

mod annular_ring;
mod board_array_spacing;
mod copper_clearance;
mod drilled_board_edge_clearance;
mod hole_aspect_ratio;
mod hole_clearance;
mod hole_diameter;
mod hole_pair_clearance;
mod layer_count;
mod linework_clearance;
mod plated_slot_enclosure;
mod slot_clearance;
mod slot_width;
mod thin_regions;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use ipc2581::Symbol;
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::geom::dfm::{COMPARISON_EPSILON_MM, Distance};
use pcb_ir::geom::{BBox, Point};
use pcb_ir::import::ipc2581::LayoutOccurrenceId;
use sha2::{Digest, Sha256};

use super::design::{Design, Hole, HoleClass, Slot};
use super::pdk::SlotPlating;
use super::report::{
    DrillSpan, Evidence, Finding, LayerRef, Location, Measurement, MeasurementKind, ReportPoint,
    RuleResult, RuleStatus, Severity, Site, SourceLocator, Subject, Unresolved, Witness,
};
use super::rules::{Comparison, Linework, Pools, Rule, RuleKind};
use super::waivers::{self, WaiverFile, WaiverOutcome};

#[derive(Default)]
pub(super) struct Results {
    pub(super) rules: Vec<RuleResult>,
    /// What findings name as their frame: a design, by index, and the
    /// placements of its Step, by index, that they hold at. Each design's
    /// frame of all its placements comes first, at the design's own index.
    pub(super) frames: Vec<(u32, Vec<u32>)>,
    pub(super) findings: Vec<Finding>,
    pub(super) shared_evidence: Vec<Evidence>,
    pub(super) waivers: Option<WaiverOutcome>,
}

/// One subject measured by a check: the distance and what it is about.
struct Measured {
    distance: Distance,
    /// The extent the finding points at: the subject, or the violating piece.
    bbox: BBox,
    layers: Vec<LayerRef>,
    subjects: Vec<Subject>,
    evidence: Vec<Evidence>,
    sites: Vec<MeasuredSite>,
}

/// Geometry of one connected failing region or one failing layer. It does not
/// change the representative measurement or identity of its containing finding.
struct MeasuredSite {
    distance: Distance,
    bbox: BBox,
    layers: Vec<LayerRef>,
    subjects: Vec<Subject>,
    evidence: Vec<Evidence>,
    measurement_kind: MeasurementKind,
    note: Option<String>,
}

impl MeasuredSite {
    fn new(
        distance: Distance,
        bbox: BBox,
        layers: Vec<LayerRef>,
        evidence: Vec<Evidence>,
        measurement_kind: MeasurementKind,
    ) -> Self {
        Self {
            distance,
            bbox,
            layers,
            evidence,
            measurement_kind,
            subjects: Vec::new(),
            note: None,
        }
    }
}

/// What one check did for one rule. `checked` is the number of subjects
/// decided, including those a broad phase proved clear without measuring;
/// `measured` holds every candidate the engine must still judge.
struct Evaluation {
    checked: usize,
    measured: Vec<Measured>,
}

struct CountEvaluation {
    actual: u32,
    layers: Vec<LayerRef>,
    subjects: Vec<Subject>,
}

struct RatioMeasured {
    actual_ratio: f64,
    drilled_span_thickness_mm: f64,
    finished_hole_diameter_mm: f64,
    thickness_source: &'static str,
    center: Point,
    bbox: BBox,
    layers: Vec<LayerRef>,
    subjects: Vec<Subject>,
    evidence: Vec<Evidence>,
    note: String,
}

struct RatioEvaluation {
    checked: usize,
    measured: Vec<RatioMeasured>,
    incomplete_reason: Option<String>,
    assumptions: Vec<String>,
}

enum RuleEvaluation {
    /// Each evaluation with the placements of the design's Step it holds at,
    /// by index; `None` is all of them.
    Distance(Vec<(Option<Vec<u32>>, Evaluation)>),
    Count(CountEvaluation),
    Ratio(RatioEvaluation),
}

impl From<Evaluation> for RuleEvaluation {
    fn from(evaluation: Evaluation) -> Self {
        Self::Distance(vec![(None, evaluation)])
    }
}

pub(super) fn run(
    rules: &[Rule],
    designs: &[Design],
    waiver_file: Option<&WaiverFile>,
    today: NaiveDate,
) -> anyhow::Result<Results> {
    let mut results = Results {
        frames: designs
            .iter()
            .enumerate()
            .map(|(index, design)| (index as u32, (0..design.placements.len() as u32).collect()))
            .collect(),
        ..Results::default()
    };
    let annular_rules = rules
        .iter()
        .filter(|rule| matches!(rule.kind, RuleKind::AnnularRing(_)))
        .map(|rule| rule.id.as_str())
        .collect::<HashSet<_>>();
    for rule in rules {
        let mut result = RuleResult::new(rule);
        // A rule is evaluated in the design of every Step. One design that
        // cannot certify it leaves it uncertified; it measures nothing only
        // when no design holds a subject for it.
        let mut incomplete = Vec::new();
        let mut not_applicable = None;
        for (index, design) in designs.iter().enumerate() {
            let unevaluated = unevaluated(rule, design).or_else(|| {
                // A measurement that fails leaves its own rule uncertified.
                judge_in(rule, (index as u32, design), &mut result, &mut results)
                    .unwrap_or_else(|error| Some((RuleStatus::Incomplete, format!("{error:#}"))))
            });
            match unevaluated {
                Some((RuleStatus::Incomplete, reason)) if !incomplete.contains(&reason) => {
                    incomplete.push(reason);
                }
                Some((RuleStatus::NotApplicable, reason)) => {
                    not_applicable.get_or_insert(reason);
                }
                _ => {}
            }
        }
        if !incomplete.is_empty() {
            result.leave_unevaluated(RuleStatus::Incomplete, incomplete.join("; "));
        } else if result.checked == 0 {
            // A nominally populated pool can still yield nothing to measure
            // (e.g. hole pairs with disjoint spans); an unexercised rule must
            // not read as validated.
            result.leave_unevaluated(
                RuleStatus::NotApplicable,
                not_applicable.unwrap_or_else(|| {
                    format!(
                        "no measurable {} subjects in the selected layout target",
                        rule.kind.semantics().subject
                    )
                }),
            );
        }
        results.rules.push(result);
    }
    results.rules = report_uncovered(rules, std::mem::take(&mut results.rules), designs);
    // Every exercised fixture also checks the reporting contract. A spatial
    // failure without a local site must never masquerade as a stackup check.
    #[cfg(test)]
    for finding in &results.findings {
        assert_eq!(
            !finding.sites.is_empty(),
            matches!(
                finding.measurement,
                Measurement::Distance { .. } | Measurement::Ratio { .. }
            ),
            "finding {} violates the spatial-site contract",
            finding.rule_id,
        );
    }
    let waiver_aliases = assign_ids(&mut results.findings, &annular_rules);
    results.shared_evidence = share_evidence(&mut results.findings, &results.frames, designs);
    results.waivers =
        waiver_file.map(|file| waivers::apply(&mut results.findings, file, &waiver_aliases, today));

    let mut per_rule: HashMap<&str, (usize, usize)> = HashMap::new();
    for finding in &results.findings {
        let (total, waived) = per_rule.entry(finding.rule_id.as_str()).or_default();
        *total += 1;
        *waived += usize::from(finding.waived);
    }
    for result in &mut results.rules {
        let (total, waived) = per_rule.get(result.id.as_str()).copied().unwrap_or((0, 0));
        result.finish(total, waived);
    }
    Ok(results)
}

/// The frame of a design at some placements of its Step, all for `None`.
fn frame_at(frames: &mut Vec<(u32, Vec<u32>)>, design: u32, placements: Option<Vec<u32>>) -> u32 {
    let Some(placements) = placements else {
        return design;
    };
    let known = frames
        .iter()
        .position(|frame| (frame.0, &frame.1) == (design, &placements));
    known.unwrap_or_else(|| {
        frames.push((design, placements));
        frames.len() - 1
    }) as u32
}

/// Evaluate a rule in one design and judge what it measured: findings and
/// unresolved measurements join the rule's, and each subject decided counts
/// once for every placement it is decided at. Returns why the rule stays
/// unevaluated here, if it does.
fn judge_in(
    rule: &Rule,
    (index, design): (u32, &Design),
    result: &mut RuleResult,
    results: &mut Results,
) -> anyhow::Result<Option<(RuleStatus, String)>> {
    let Results {
        frames, findings, ..
    } = results;
    match evaluate(rule, design)? {
        RuleEvaluation::Distance(evaluations) => {
            debug_assert_eq!(rule.comparison, Comparison::Minimum);
            let limit = rule.limit.length().millimeters();
            for (placements, evaluation) in evaluations {
                let frame = frame_at(frames, index, placements);
                for measured in evaluation.measured {
                    match judge(&measured.distance, limit) {
                        Judgement::Violates => findings.push(Finding {
                            frame,
                            ..finding(rule, measured)
                        }),
                        Judgement::Unresolved => result.unresolved.push(Unresolved {
                            frame,
                            actual_mm: measured.distance.mm,
                            uncertainty_mm: measured.distance.uncertainty_mm,
                            point: measured.distance.midpoint().into(),
                            layers: measured
                                .layers
                                .into_iter()
                                .map(|layer| layer.name)
                                .collect(),
                        }),
                        Judgement::Meets => {}
                    }
                }
                result.checked += evaluation.checked * frames[frame as usize].1.len();
            }
        }
        RuleEvaluation::Count(evaluation) => {
            let limit = rule.limit.count();
            if violates_count(evaluation.actual, rule.comparison, limit) {
                findings.push(Finding {
                    frame: index,
                    ..count_finding(rule, evaluation, limit)
                });
            }
            result.checked += design.placements.len();
        }
        RuleEvaluation::Ratio(evaluation) => {
            debug_assert_eq!(rule.comparison, Comparison::Maximum);
            for assumption in evaluation.assumptions {
                if !result.assumptions.contains(&assumption) {
                    result.assumptions.push(assumption);
                }
            }
            if let Some(reason) = evaluation.incomplete_reason {
                return Ok(Some((RuleStatus::Incomplete, reason)));
            }
            let maximum = rule.limit.ratio();
            findings.extend(
                evaluation
                    .measured
                    .into_iter()
                    .filter(|measured| exceeds(measured, maximum))
                    .map(|measured| Finding {
                        frame: index,
                        ..ratio_finding(rule, measured, maximum)
                    }),
            );
            result.checked += evaluation.checked * design.placements.len();
        }
    }
    Ok(None)
}

/// Build the report's shared-evidence table. A site names its board profile
/// by its design's outline pool index; the table holds each referenced
/// profile once, in design and pool order, so report size follows the
/// findings rather than findings times the outline every one measures to.
fn share_evidence(
    findings: &mut [Finding],
    frames: &[(u32, Vec<u32>)],
    designs: &[Design],
) -> Vec<Evidence> {
    fn references<'a>(
        findings: &'a mut [Finding],
        frames: &'a [(u32, Vec<u32>)],
    ) -> impl Iterator<Item = (u32, &'a mut u32)> {
        findings.iter_mut().flat_map(|finding| {
            let design = frames[finding.frame as usize].0;
            finding
                .sites
                .iter_mut()
                .flat_map(|site| &mut site.evidence)
                .filter_map(move |evidence| Some((design, evidence.shared.as_mut()?)))
        })
    }
    let outlines = references(findings, frames)
        .map(|(design, index)| (design, *index))
        .collect::<std::collections::BTreeSet<_>>();
    for (design, index) in references(findings, frames) {
        *index = outlines.range(..(design, *index)).count() as u32;
    }
    outlines
        .into_iter()
        .map(|(design, index)| {
            drilled_board_edge_clearance::profile_evidence(
                &designs[design as usize].board_outlines[index as usize],
            )
        })
        .collect()
}

/// How a measured distance stands against a minimum.
#[derive(PartialEq)]
enum Judgement {
    Meets,
    /// Below the limit, but by less than the measurement's own uncertainty.
    Unresolved,
    Violates,
}

/// The one verdict: a distance violates a minimum when it is certainly
/// below it, beyond both its own geometric uncertainty and the comparison
/// epsilon. One that is below it only within that uncertainty is neither a
/// violation nor a pass, and is reported as unresolved.
fn judge(distance: &Distance, limit_mm: f64) -> Judgement {
    let limit_mm = limit_mm - COMPARISON_EPSILON_MM;
    if distance.certainly_below(limit_mm) {
        Judgement::Violates
    } else if distance.mm < limit_mm {
        Judgement::Unresolved
    } else {
        Judgement::Meets
    }
}

fn violates(distance: &Distance, limit_mm: f64) -> bool {
    judge(distance, limit_mm) == Judgement::Violates
}

/// A ratio exceeds its maximum when the drilled depth exceeds the depth the
/// maximum allows for that diameter, by the same comparison epsilon: a span
/// summed from decimal layer thicknesses must not fail a limit it sits on.
fn exceeds(measured: &RatioMeasured, maximum: f64) -> bool {
    measured.drilled_span_thickness_mm
        > maximum * measured.finished_hole_diameter_mm + COMPARISON_EPSILON_MM
}

fn violates_count(actual: u32, comparison: Comparison, limit: u32) -> bool {
    match comparison {
        Comparison::Minimum => actual < limit,
        Comparison::Maximum => actual > limit,
    }
}

/// The one policy for a rule that is not evaluated, in order of what can be
/// known. `Incomplete`: the rule applies, or might, but something it reads
/// could not be built or resolved, so its limit is not certified.
/// `NotApplicable`: the design holds nothing for it to measure.
fn unevaluated(rule: &Rule, design: &Design) -> Option<(RuleStatus, String)> {
    let pools = rule.pools(!design.imported.stackups.is_empty());
    let blocked = |pools: Pools| {
        let reasons = design
            .blockers
            .iter()
            .filter(|blocker| blocker.pools.intersects(pools))
            .map(|blocker| blocker.reason.as_str())
            .collect::<Vec<_>>();
        (!reasons.is_empty()).then(|| (RuleStatus::Incomplete, reasons.join("; ")))
    };
    // Conditions on the stackup cannot be decided without one.
    if pools.intersects(Pools::STACKUP)
        && let Some(blocked) = blocked(Pools::STACKUP)
    {
        return Some(blocked);
    }
    if !rule.conditions.applies_to_design(design) {
        return Some((
            RuleStatus::NotApplicable,
            "rule conditions do not apply to this stackup".to_owned(),
        ));
    }
    if let Some(blocked) = blocked(pools)
        .or_else(|| unresolved_span(rule, design).map(|reason| (RuleStatus::Incomplete, reason)))
    {
        return Some(blocked);
    }
    let layers = (pools.intersects(Pools::COPPER)
        && design
            .copper_layers
            .iter()
            .all(|layer| !rule.conditions.applies_to_layer(layer)))
    .then_some("applicable copper layers")
    .or_else(|| {
        (pools.intersects(Pools::MASKS) && design.mask_layers.is_empty())
            .then_some("soldermask layers")
    })
    .map(|what| format!("no {what} in the selected layout target"));
    missing_subjects(rule.kind, design)
        .or(layers)
        .map(|reason| (RuleStatus::NotApplicable, reason))
}

/// Follow each authored rule's results with one more when its cases leave
/// part of the design outside all of them. Cases must not overlap but need
/// not cover: a copper layer or stackup that no case matches lies outside the
/// capability the PDK states, so it is uncertified, never silently unchecked.
fn report_uncovered(
    rules: &[Rule],
    results: Vec<RuleResult>,
    designs: &[Design],
) -> Vec<RuleResult> {
    // Layers and the stackup are the layout's, the same in every design.
    let design = &designs[0];
    let mut results = results.into_iter();
    rules
        .chunk_by(|left, right| left.authored_id == right.authored_id)
        .flat_map(|cases| {
            let mut reported = results.by_ref().take(cases.len()).collect::<Vec<_>>();
            // Cases already incomplete say why; without subjects nothing is unchecked.
            let decidable = reported
                .iter()
                .all(|result| !matches!(result.status, RuleStatus::Incomplete))
                && designs
                    .iter()
                    .any(|design| missing_subjects(cases[0].kind, design).is_none());
            if let Some(reason) = decidable.then(|| uncovered(cases, design)).flatten() {
                // The strictest tier the cases declare is the one left uncertified.
                let tier = cases
                    .iter()
                    .min_by_key(|case| case.severity != Severity::Error)
                    .expect("an authored rule lowers to at least one rule");
                let mut result = RuleResult::new(&Rule {
                    id: tier.authored_id.clone(),
                    ..tier.clone()
                });
                result.leave_unevaluated(RuleStatus::Incomplete, reason);
                reported.push(result);
            }
            reported
        })
        .collect()
}

/// What in the design no case of one authored rule applies to.
fn uncovered(cases: &[Rule], design: &Design) -> Option<String> {
    let applicable = cases
        .iter()
        .filter(|case| case.conditions.applies_to_design(design))
        .collect::<Vec<_>>();
    if applicable.is_empty() {
        let layers = design
            .stackup
            .as_ref()
            .map_or(0, |stackup| stackup.copper_layers.len());
        return Some(format!(
            "no case applies to a design with {layers} copper layer(s)"
        ));
    }
    if !cases[0].kind.semantics().pools.intersects(Pools::COPPER) {
        return None;
    }
    let layers = design
        .copper_layers
        .iter()
        .filter(|layer| {
            applicable
                .iter()
                .all(|case| !case.conditions.applies_to_layer(layer))
        })
        .map(|layer| {
            let position = match layer.position {
                super::pdk::LayerPosition::Outer => "outer",
                super::pdk::LayerPosition::Inner => "inner",
            };
            match layer.copper_weight_oz {
                Some(weight) => format!("'{}' ({position}, {weight:.2} oz)", layer.layer.name),
                None => format!("'{}' ({position})", layer.layer.name),
            }
        })
        .collect::<Vec<_>>();
    (!layers.is_empty())
        .then(|| format!("no case applies to copper layer(s) {}", layers.join(", ")))
}

/// Why a design holds nothing for a rule kind to measure, whatever case
/// conditions select among its layers.
fn missing_subjects(kind: RuleKind, design: &Design) -> Option<String> {
    let what = match kind {
        // The stackup is the layout's: its root Step's design measures it.
        RuleKind::CopperLayerCount => {
            (design.placements[0] != LayoutOccurrenceId::Root).then(|| "stackup".to_owned())
        }
        RuleKind::BoardArrayPairClearance if design.scope != ArtworkScope::ArrayFlattened => {
            return Some("board-array spacing requires --layout-target board-array".to_owned());
        }
        RuleKind::BoardArrayPairClearance => (design.board_arrays.len() < 2)
            .then(|| "two or more direct board-array instances".to_owned()),
        RuleKind::HoleDiameter(class)
        | RuleKind::HoleAspectRatio(class)
        | RuleKind::AnnularRing(class)
        | RuleKind::HoleToCopperClearance(class) => design
            .holes
            .iter()
            .all(|hole| hole.class != class)
            .then(|| format!("{} holes", class.label())),
        RuleKind::HolePairClearance(first, second) => {
            (!has_hole_pair(design, first, second)).then(|| {
                format!(
                    "an eligible {}-to-{} hole pair",
                    first.label(),
                    second.label()
                )
            })
        }
        RuleKind::HoleToBoardEdgeClearance(class) => design
            .holes
            .iter()
            .all(|hole| hole.class != class)
            .then(|| format!("{} holes", class.label())),
        RuleKind::PlatedSlotEnclosure => design
            .slots
            .iter()
            .all(|slot| !slot_matches(slot.plating, SlotPlating::Plated))
            .then(|| "plated routed slots".to_owned()),
        RuleKind::SlotWidth(plating) | RuleKind::SlotToCopperClearance(plating) => design
            .slots
            .iter()
            .all(|slot| !slot_matches(slot.plating, plating))
            .then(|| format!("{} routed slots", slot_plating_label(plating))),
        RuleKind::SlotToBoardEdgeClearance(plating) => design
            .slots
            .iter()
            .all(|slot| !slot_matches(slot.plating, plating))
            .then(|| format!("{} routed slots", slot_plating_label(plating))),
        RuleKind::LineworkToCopperClearance(Linework::VScore) => (design.scores.is_empty()
            && design.inherited_scores.is_empty())
        .then(|| "V-score centerlines".to_owned()),
        RuleKind::LineworkToCopperClearance(Linework::BoardEdge) => design
            .board_outlines
            .iter()
            .all(|outline| !outline.is_board())
            .then(|| "board profile outlines".to_owned()),
        RuleKind::CopperFeatureWidth | RuleKind::CopperClearance | RuleKind::SoldermaskWeb => None,
    };
    what.map(|what| format!("no {what} in the selected layout target"))
}

/// A rule measuring on the copper layers a drill spans cannot be certified
/// for a drill whose declared span does not resolve in the physical stackup:
/// which layers it meets would be a guess.
fn unresolved_span(rule: &Rule, design: &Design) -> Option<String> {
    let unresolved = |span: &DrillSpan| span.interpretation == "assumed_whole_stack";
    let applies = |span: &DrillSpan| {
        design
            .copper_layers
            .iter()
            .enumerate()
            .any(|(index, layer)| {
                span.contains_copper(index) && rule.conditions.applies_to_layer(layer)
            })
    };
    match rule.kind {
        RuleKind::HoleToCopperClearance(class) => design
            .holes
            .iter()
            .find(|hole| {
                hole.class == class
                    && hole.branch.is_none()
                    && unresolved(&hole.drill_span)
                    && applies(&hole.drill_span)
            })
            .map(|hole| {
                format!(
                    "{} hole on layer '{}' at ({:.6}, {:.6}) has no resolvable drill span",
                    hole.class.label(),
                    hole.layer.name,
                    hole.center.x,
                    hole.center.y
                )
            }),
        RuleKind::SlotToCopperClearance(_) | RuleKind::PlatedSlotEnclosure => design
            .slots
            .iter()
            .find(|slot| {
                let selected = match rule.kind {
                    RuleKind::SlotToCopperClearance(plating) => slot_matches(slot.plating, plating),
                    _ => slot_matches(slot.plating, SlotPlating::Plated),
                };
                selected
                    && slot.branch.is_none()
                    && unresolved(&slot.drill_span)
                    && applies(&slot.drill_span)
            })
            .map(|slot| {
                format!(
                    "routed slot on layer '{}' has no resolvable drill span",
                    slot.layer.name
                )
            }),
        _ => None,
    }
}

fn evaluate(rule: &Rule, design: &Design) -> anyhow::Result<RuleEvaluation> {
    let limit = || rule.limit.length().millimeters();
    Ok(match rule.kind {
        RuleKind::CopperLayerCount => RuleEvaluation::Count(layer_count::evaluate(design)),
        RuleKind::HoleDiameter(class) => hole_diameter::evaluate(limit(), class, design).into(),
        RuleKind::HoleAspectRatio(class) => {
            RuleEvaluation::Ratio(hole_aspect_ratio::evaluate(class, &rule.conditions, design))
        }
        RuleKind::SlotWidth(plating) => slot_width::evaluate(limit(), plating, design)?.into(),
        RuleKind::HolePairClearance(first, second) => {
            hole_pair_clearance::evaluate(limit(), first, second, design)?.into()
        }
        RuleKind::HoleToBoardEdgeClearance(class) => {
            drilled_board_edge_clearance::evaluate_holes(limit(), class, design)?.into()
        }
        RuleKind::SlotToBoardEdgeClearance(plating) => {
            drilled_board_edge_clearance::evaluate_slots(limit(), plating, design)?.into()
        }
        RuleKind::AnnularRing(class) => {
            annular_ring::evaluate(limit(), class, &rule.conditions, design)?.into()
        }
        RuleKind::PlatedSlotEnclosure => {
            plated_slot_enclosure::evaluate(limit(), &rule.conditions, design)?.into()
        }
        RuleKind::HoleToCopperClearance(class) => {
            hole_clearance::evaluate(limit(), class, &rule.conditions, design)?.into()
        }
        RuleKind::SlotToCopperClearance(plating) => {
            slot_clearance::evaluate(limit(), plating, &rule.conditions, design)?.into()
        }
        RuleKind::LineworkToCopperClearance(linework) => RuleEvaluation::Distance(
            linework_clearance::evaluate(limit(), linework, &rule.conditions, design)?,
        ),
        RuleKind::BoardArrayPairClearance => board_array_spacing::evaluate(limit(), design)?.into(),
        RuleKind::CopperFeatureWidth => {
            thin_regions::copper_feature_width(limit(), &rule.conditions, design)?.into()
        }
        RuleKind::CopperClearance => {
            copper_clearance::evaluate(limit(), &rule.conditions, design)?.into()
        }
        RuleKind::SoldermaskWeb => thin_regions::soldermask_web(limit(), design)?.into(),
    })
}

pub(super) fn slot_matches(
    actual: pcb_ir::dialects::ipc::PlatingKind,
    expected: SlotPlating,
) -> bool {
    matches!(
        (actual, expected),
        (
            pcb_ir::dialects::ipc::PlatingKind::Plated,
            SlotPlating::Plated
        ) | (
            pcb_ir::dialects::ipc::PlatingKind::NonPlated,
            SlotPlating::Nonplated
        )
    )
}

fn slot_plating_label(plating: SlotPlating) -> &'static str {
    match plating {
        SlotPlating::Plated => "plated",
        SlotPlating::Nonplated => "non-plated",
    }
}

fn has_hole_pair(design: &Design, first: HoleClass, second: HoleClass) -> bool {
    if first == second {
        design
            .holes
            .iter()
            .filter(|hole| hole.class == first)
            .take(2)
            .count()
            == 2
    } else {
        design.holes.iter().any(|hole| hole.class == first)
            && design.holes.iter().any(|hole| hole.class == second)
    }
}

/// Render one violating measurement as a finding. Titles, message shape,
/// and witness roles come from the rule kind; the location is the measured
/// distance itself.
fn finding(rule: &Rule, measured: Measured) -> Finding {
    let limit = rule.limit.length().millimeters();
    let semantics = rule.kind.semantics();
    let [first_role, second_role] = semantics
        .witness_roles
        .expect("distance-valued rules define witness roles");
    let distance = measured.distance;
    let sites = measured
        .sites
        .into_iter()
        .filter(|site| violates(&site.distance, limit))
        .map(|site| Site {
            id: String::new(),
            measurement: Measurement::minimum_distance(site.distance.mm, limit),
            measurement_kind: site.measurement_kind,
            uncertainty_mm: site.distance.uncertainty_mm,
            witnesses: vec![
                Witness::new(first_role, site.distance.first),
                Witness::new(second_role, site.distance.second),
            ],
            bounding_box: site.bbox.into(),
            layers: site.layers,
            subjects: if site.subjects.is_empty() {
                measured.subjects.clone()
            } else {
                site.subjects
            },
            evidence: site.evidence,
            note: site.note,
        })
        .collect();
    let layer_names = measured
        .layers
        .iter()
        .map(|layer| layer.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let on_layers = measured
        .layers
        .first()
        .map(|_| format!(" on {layer_names}"))
        .unwrap_or_default();
    Finding {
        id: String::new(),
        rule_id: rule.id.clone(),
        severity: rule.severity,
        waived: false,
        waiver_reason: None,
        title: semantics.finding_title,
        message: format!(
            "{} is {:.6} mm{on_layers}; the PDK requires at least {limit:.6} mm",
            semantics.quantity_label, distance.mm
        ),
        measurement: Measurement::minimum_distance(distance.mm, limit),
        location: Location {
            point: Some(distance.midpoint().into()),
            bounding_box: Some(measured.bbox.into()),
            witnesses: vec![
                Witness::new(first_role, distance.first),
                Witness::new(second_role, distance.second),
            ],
        },
        layers: measured.layers,
        subjects: measured.subjects,
        evidence: measured.evidence,
        sites,
        frame: 0,
    }
}

fn count_finding(rule: &Rule, measured: CountEvaluation, limit: u32) -> Finding {
    let (title, requirement, measurement) = match rule.comparison {
        Comparison::Minimum => (
            "Copper layer count is below the minimum",
            format!("requires at least {limit}"),
            Measurement::minimum_count(measured.actual, limit),
        ),
        Comparison::Maximum => (
            "Copper layer count exceeds the maximum",
            format!("permits at most {limit}"),
            Measurement::maximum_count(measured.actual, limit),
        ),
    };
    Finding {
        id: String::new(),
        rule_id: rule.id.clone(),
        severity: rule.severity,
        waived: false,
        waiver_reason: None,
        title: title.to_owned(),
        message: format!(
            "copper layer count is {}; the PDK {requirement}",
            measured.actual
        ),
        measurement,
        location: Location::default(),
        layers: measured.layers,
        subjects: measured.subjects,
        evidence: Vec::new(),
        sites: Vec::new(),
        frame: 0,
    }
}

fn ratio_finding(rule: &Rule, measured: RatioMeasured, maximum: f64) -> Finding {
    let semantics = rule.kind.semantics();
    let measurement = Measurement::maximum_ratio(
        measured.actual_ratio,
        maximum,
        measured.drilled_span_thickness_mm,
        measured.finished_hole_diameter_mm,
        measured.thickness_source,
    );
    let site = Site {
        id: String::new(),
        measurement: measurement.clone(),
        measurement_kind: MeasurementKind::AspectRatio,
        uncertainty_mm: 0.0,
        witnesses: Vec::new(),
        bounding_box: measured.bbox.into(),
        layers: measured.layers.clone(),
        subjects: measured.subjects.clone(),
        evidence: measured.evidence.clone(),
        note: Some(measured.note),
    };
    Finding {
        id: String::new(),
        rule_id: rule.id.clone(),
        severity: rule.severity,
        waived: false,
        waiver_reason: None,
        title: semantics.finding_title,
        message: format!(
            "{} is {:.6} ({:.6} mm drilled span / {:.6} mm finished diameter); the PDK permits at most {maximum:.6}; thickness source is {}",
            semantics.quantity_label,
            measured.actual_ratio,
            measured.drilled_span_thickness_mm,
            measured.finished_hole_diameter_mm,
            measured.thickness_source,
        ),
        measurement,
        location: Location {
            point: Some(measured.center.into()),
            bounding_box: Some(measured.bbox.into()),
            witnesses: Vec::new(),
        },
        layers: measured.layers,
        subjects: measured.subjects,
        evidence: measured.evidence,
        sites: vec![site],
        frame: 0,
    }
}

/// The copper a plated drilled feature owns, for holes and slots alike.
///
/// A canonical land link can mean unique overlap rather than identity, and
/// proximity never implies ownership: a linked land is the feature's own only
/// with the same stated padstack and no contradicting net. The feature then
/// owns, within its Step occurrence, its net and the nets of those lands on
/// any layer, and the netless pads that are those lands.
pub(super) struct Ownership {
    step: Option<Symbol>,
    instance: Option<u32>,
    nets: Vec<Symbol>,
    lands: Vec<pcb_ir::import::physical::LandId>,
}

impl Ownership {
    pub(super) fn of(
        design: &Design,
        net: Option<Symbol>,
        padstack: Option<Symbol>,
        step: Option<Symbol>,
        instance: Option<u32>,
        links: &[super::design::HoleLand],
    ) -> Self {
        let lands = links
            .iter()
            .map(|link| {
                &design.copper_layers[link.copper_index as usize].lands[link.land_index as usize]
            })
            .filter(|land| padstack == Some(land.padstack))
            .filter(|land| net.zip(land.net).is_none_or(|(own, land)| own == land))
            .collect::<Vec<_>>();
        Self {
            step,
            instance,
            nets: net
                .into_iter()
                .chain(lands.iter().filter_map(|land| land.net))
                .collect(),
            lands: lands.iter().map(|land| land.id).collect(),
        }
    }

    pub(super) fn owns(&self, conductor: super::design::ConductorId) -> bool {
        use super::design::ConductorId;
        match conductor {
            ConductorId::Net {
                step,
                instance,
                net,
            } => step == self.step && instance == self.instance && self.nets.contains(&net),
            ConductorId::Isolated { occurrence, .. } => {
                self.lands.iter().any(|land| land.0 == occurrence)
            }
            ConductorId::Auxiliary { .. } | ConductorId::Unattributed { .. } => false,
        }
    }
}

/// The Step's own holes of one plating class, with their indices into the
/// hole pool: the subjects of every rule that measures a hole on its own.
fn holes_of_class<'a>(design: &'a Design<'a>, class: HoleClass) -> Vec<(usize, &'a Hole)> {
    design
        .holes
        .iter()
        .enumerate()
        .filter(|(_, hole)| hole.class == class && hole.branch.is_none())
        .collect()
}

/// The Step's own slots of one plating class, with their indices into the
/// slot pool.
fn slots_of_plating<'a>(
    design: &'a Design<'a>,
    plating: SlotPlating,
) -> impl Iterator<Item = (usize, &'a Slot)> {
    design
        .slots
        .iter()
        .enumerate()
        .filter(move |(_, slot)| slot_matches(slot.plating, plating) && slot.branch.is_none())
}

/// The shared subject shape of every drilled feature (holes and slots).
#[allow(clippy::too_many_arguments)]
fn drilled_subject(
    design: &Design,
    role: &'static str,
    kind: &'static str,
    net: Option<Symbol>,
    padstack: Option<Symbol>,
    step: Option<Symbol>,
    layer: &LayerRef,
    set_index: u32,
    feature_index: u32,
) -> Subject {
    Subject {
        role,
        kind,
        net: design.resolve(net),
        padstack_ref: design.resolve(padstack),
        source: Some(SourceLocator {
            step: design.resolve(step),
            layer: Some(layer.name.clone()),
            set_index: Some(set_index),
            feature_index: Some(feature_index),
            instance_index: None,
        }),
        ..Subject::default()
    }
}

fn hole_subject(design: &Design, hole: &Hole, role: &'static str) -> Subject {
    let mut subject = drilled_subject(
        design,
        role,
        hole.class.subject_kind(),
        hole.net,
        hole.padstack,
        hole.step,
        &hole.layer,
        hole.source_set_index,
        hole.source_feature_index,
    );
    subject.provenance = Some(hole.provenance.clone());
    subject.anchor = Some(hole.center.into());
    subject.drill_span = Some(hole.drill_span.clone());
    subject
}

fn slot_subject(design: &Design, slot: &Slot, role: &'static str) -> Subject {
    let mut subject = drilled_subject(
        design,
        role,
        "routed_slot",
        slot.net,
        slot.padstack,
        slot.step,
        &slot.layer,
        slot.source_set_index,
        slot.source_feature_index,
    );
    subject.provenance = Some(slot.provenance.clone());
    subject.anchor = Some(slot.bbox.center().into());
    // Width and board-edge checks need not resolve the physical stackup.
    // Do not present their declaration-order fallback as a physical span.
    subject.drill_span = design.stackup.as_ref().map(|_| slot.drill_span.clone());
    subject
}

/// This projection is the original v1 subject serialization, including field
/// order and nulls. New diagnostic metadata must never silently re-key waivers.
#[derive(serde::Serialize)]
struct LegacySubject<'a> {
    role: &'static str,
    kind: &'static str,
    name: &'a Option<String>,
    reference_designator: &'a Option<String>,
    pin: &'a Option<String>,
    net: &'a Option<String>,
    padstack_ref: &'a Option<String>,
    source: &'a Option<SourceLocator>,
}

impl<'a> From<&'a Subject> for LegacySubject<'a> {
    fn from(subject: &'a Subject) -> Self {
        Self {
            role: subject.role,
            kind: subject.kind,
            name: &subject.name,
            reference_designator: &subject.reference_designator,
            pin: &subject.pin,
            net: &subject.net,
            padstack_ref: &subject.padstack_ref,
            source: &subject.source,
        }
    }
}

/// What identifies a subject across equivalent exports: who it is, not how
/// the file happened to name or number it. Generated IPC primitive names,
/// padstack ids, and set/feature indices change between clean exports of the
/// same board and are excluded. A drilled subject is further identified by
/// where the source drills it, in whole micrometres.
#[derive(serde::Serialize)]
struct StableSubject<'a> {
    role: &'static str,
    kind: &'static str,
    reference_designator: &'a Option<String>,
    pin: &'a Option<String>,
    net: &'a Option<String>,
    source: Option<StableSource<'a>>,
    drill_span: &'a Option<DrillSpan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    anchor: Option<[i64; 2]>,
}

#[derive(serde::Serialize)]
struct StableSource<'a> {
    step: &'a Option<String>,
    layer: &'a Option<String>,
    instance_index: Option<u32>,
}

impl<'a> From<&'a Subject> for StableSubject<'a> {
    fn from(subject: &'a Subject) -> Self {
        Self {
            role: subject.role,
            kind: subject.kind,
            reference_designator: &subject.reference_designator,
            pin: &subject.pin,
            net: &subject.net,
            source: subject.source.as_ref().map(|source| StableSource {
                step: &source.step,
                layer: &source.layer,
                instance_index: source.instance_index,
            }),
            drill_span: &subject.drill_span,
            anchor: subject.anchor.map(micrometres),
        }
    }
}

/// Identity coordinates are whole micrometres: far above the noise that
/// equivalent geometry differs by, far below anything that tells two
/// violations apart. Any grid has cell edges where noise still flips a
/// coordinate; offsetting them a quarter micrometre keeps them off the
/// half-micrometre lattice, where midpoints of gridded CAD coordinates fall.
fn micrometres(point: ReportPoint) -> [i64; 2] {
    [point.x, point.y].map(|millimetres| (millimetres * 1000.0 + 0.25).floor() as i64)
}

/// The layers a finding spans, each named once.
fn layers<'a>(layers: impl IntoIterator<Item = &'a LayerRef>) -> Vec<LayerRef> {
    let mut layers = layers.into_iter().cloned().collect::<Vec<_>>();
    layers.dedup_by(|left, right| left.name == right.name);
    layers
}

/// Sort findings into rule/location order and give each an id hashed from
/// what it is about: its rule, its stable subjects, its layers, and where it
/// is, in whole micrometres. A drilled subject carries its own source
/// location; only a finding without one is placed by its measured point,
/// which depends on which of several equally near boundaries was the witness.
/// No raw float and no evidence geometry enters an id, so noise-level
/// coordinate changes do not re-key findings and strand their waivers. The
/// measured value is excluded too: a waived violation that changes magnitude
/// in place keeps its waiver.
///
/// Ids released earlier hashed raw coordinates and export-specific indices.
/// Each is still computed, exactly as it was, and returned as an alias of the
/// finding's id so a waiver written against it keeps matching.
fn assign_ids(findings: &mut [Finding], annular_rules: &HashSet<&str>) -> HashMap<String, String> {
    findings.sort_by(|left, right| {
        left.rule_id
            .cmp(&right.rule_id)
            .then_with(|| left.frame.cmp(&right.frame))
            .then_with(|| compare_locations(&left.location, &right.location))
    });
    let short = |fingerprint: &[u8]| hex::encode(&Sha256::digest(fingerprint)[..6]);
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut released_seen: HashMap<String, u32> = HashMap::new();
    let mut waiver_aliases = HashMap::new();
    for finding in findings.iter_mut() {
        let subjects = finding
            .subjects
            .iter()
            .map(StableSubject::from)
            .collect::<Vec<_>>();
        let placed_by_subject = subjects.iter().any(|subject| subject.anchor.is_some());
        let digest = short(
            &serde_json::to_vec(&(
                &finding.rule_id,
                &subjects,
                &finding.layers,
                finding
                    .location
                    .point
                    .filter(|_| !placed_by_subject)
                    .map(micrometres),
            ))
            .expect("finding identity serializes"),
        );
        let repeat = seen
            .entry(digest.clone())
            .and_modify(|n| *n += 1)
            .or_insert(1);
        finding.id = if *repeat == 1 {
            format!("dfm-{digest}")
        } else {
            format!("dfm-{digest}-{repeat}")
        };

        for released in released_fingerprints(finding, annular_rules) {
            let released_id = format!("dfm-{}", short(released.as_bytes()));
            let repeat = released_seen
                .entry(released_id.clone())
                .and_modify(|n| *n += 1)
                .or_insert(1);
            if *repeat == 1 {
                if released_id != finding.id {
                    waiver_aliases.insert(released_id, finding.id.clone());
                }
            } else {
                // An ordinal id can move when equivalent findings are
                // reordered. Do not transfer either waiver ambiguously.
                waiver_aliases.remove(&released_id);
            }
        }

        let mut sites_seen: HashMap<String, usize> = HashMap::new();
        for site in &mut finding.sites {
            let bounds = site.bounding_box;
            let digest = short(
                &serde_json::to_vec(&(
                    &site.layers,
                    &site.measurement_kind,
                    site.subjects
                        .iter()
                        .map(StableSubject::from)
                        .collect::<Vec<_>>(),
                    [micrometres(bounds.min), micrometres(bounds.max)],
                ))
                .expect("site identity serializes"),
            );
            let ordinal = sites_seen
                .entry(digest.clone())
                .and_modify(|n| *n += 1)
                .or_insert(1);
            site.id = format!("{}-site-{digest}", finding.id);
            if *ordinal > 1 {
                site.id.push_str(&format!("-{ordinal}"));
            }
        }
    }
    waiver_aliases
}

/// The identity records of every released id format, byte for byte. The first
/// served every rule; annular findings then moved to their drilled hole, with
/// the subject projection that is now stable identity minus its anchor.
fn released_fingerprints(finding: &Finding, annular_rules: &HashSet<&str>) -> Vec<String> {
    let original = serde_json::to_string(&(
        &finding.rule_id,
        finding
            .subjects
            .iter()
            .map(LegacySubject::from)
            .collect::<Vec<_>>(),
        &finding.layers,
        &finding.location.point,
    ));
    let annular = annular_rules
        .contains(finding.rule_id.as_str())
        .then(|| {
            finding
                .evidence
                .iter()
                .find(|evidence| evidence.role == "drilled_hole")
        })
        .flatten()
        .map(|hole| {
            serde_json::to_string(&(
                &finding.rule_id,
                finding
                    .subjects
                    .iter()
                    .map(|subject| StableSubject {
                        anchor: None,
                        ..StableSubject::from(subject)
                    })
                    .collect::<Vec<_>>(),
                &finding.layers,
                &hole.center,
                &hole.diameter,
            ))
        });
    std::iter::once(original)
        .chain(annular)
        .map(|fingerprint| fingerprint.expect("released finding identity serializes"))
        .collect()
}

fn compare_locations(left: &Location, right: &Location) -> Ordering {
    match (left.point, right.point) {
        (Some(left), Some(right)) => left
            .x
            .total_cmp(&right.x)
            .then_with(|| left.y.total_cmp(&right.y)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::dfm::report::ReportPoint;

    fn assign_ids(findings: &mut [Finding]) -> HashMap<String, String> {
        super::assign_ids(findings, &HashSet::new())
    }

    fn finding_at(x: f64) -> Finding {
        Finding {
            id: String::new(),
            rule_id: "rule".to_owned(),
            severity: super::super::report::Severity::Error,
            waived: false,
            waiver_reason: None,
            title: String::new(),
            message: String::new(),
            measurement: Measurement::minimum_distance(0.0, 1.0),
            location: Location {
                point: Some(ReportPoint { x, y: 0.0 }),
                bounding_box: None,
                witnesses: Vec::new(),
            },
            layers: Vec::new(),
            subjects: Vec::new(),
            evidence: Vec::new(),
            sites: Vec::new(),
            frame: 0,
        }
    }

    #[test]
    fn a_shortfall_inside_the_measurement_uncertainty_is_unresolved_not_passed() {
        let measured = |mm: f64| Distance::with_uncertainty(mm, Point::ZERO, Point::ZERO, 0.003);
        assert!(judge(&measured(0.0960), 0.1) == Judgement::Violates);
        assert!(judge(&measured(0.0985), 0.1) == Judgement::Unresolved);
        assert!(judge(&measured(0.1000), 0.1) == Judgement::Meets);
        // Unit conversion noise on the limit itself is not a shortfall.
        assert!(judge(&measured(0.1 - 1e-9), 0.1) == Judgement::Meets);
        // An exact measurement is never unresolved.
        let exact = Distance::exact(0.0999, Point::ZERO, Point::ZERO);
        assert!(judge(&exact, 0.1) == Judgement::Violates);
    }

    #[test]
    fn a_ratio_sitting_on_its_maximum_does_not_exceed_it() {
        let ratio = |thickness_mm: f64| RatioMeasured {
            actual_ratio: thickness_mm / 0.1,
            drilled_span_thickness_mm: thickness_mm,
            finished_hole_diameter_mm: 0.1,
            thickness_source: "test",
            center: Point::ZERO,
            bbox: BBox::from_point(Point::ZERO),
            layers: Vec::new(),
            subjects: Vec::new(),
            evidence: Vec::new(),
            note: String::new(),
        };
        // Summed decimal layer thicknesses: 0.1 + 0.2 is 0.30000000000000004.
        let summed = ratio(0.1 + 0.2);
        assert!(summed.actual_ratio > 3.0);
        assert!(!exceeds(&summed, 3.0));
        assert!(exceeds(&ratio(0.31), 3.0));
    }

    #[test]
    fn ids_stay_with_their_location_when_findings_are_added() {
        let mut two = vec![finding_at(2.0), finding_at(1.0)];
        assign_ids(&mut two);
        let id_at = |findings: &[Finding], x: f64| {
            findings
                .iter()
                .find(|finding| finding.location.point.unwrap().x == x)
                .unwrap()
                .id
                .clone()
        };
        assert_ne!(id_at(&two, 1.0), id_at(&two, 2.0));
        // Location disambiguates in the hash itself: no ordinal suffix.
        assert_eq!(id_at(&two, 2.0).matches('-').count(), 1);

        // A new violation earlier in sort order must not move existing ids.
        let mut three = vec![finding_at(0.5), finding_at(1.0), finding_at(2.0)];
        assign_ids(&mut three);
        assert_eq!(id_at(&two, 1.0), id_at(&three, 1.0));
        assert_eq!(id_at(&two, 2.0), id_at(&three, 2.0));
    }

    #[test]
    fn visual_metadata_does_not_change_the_id_and_the_released_id_still_resolves() {
        let mut finding = finding_at(1.0);
        finding.subjects.push(Subject {
            role: "hole",
            kind: "via_hole",
            ..Subject::default()
        });
        let aliases = assign_ids(std::slice::from_mut(&mut finding));
        let id = finding.id.clone();
        // Independently computed from the pre-sites v1 JSON identity record.
        assert_eq!(aliases.get("dfm-bee136ee7a39"), Some(&id));
        finding.subjects[0].provenance = Some(SourceLocator {
            step: Some("board".into()),
            layer: Some("DRILL".into()),
            set_index: Some(3),
            feature_index: Some(1),
            instance_index: Some(7),
        });
        finding
            .evidence
            .push(Evidence::circle("hole", Point::new(1.0, 0.0), 0.1));
        finding.sites.push(Site {
            id: String::new(),
            measurement: Measurement::minimum_distance(0.1, 0.2),
            measurement_kind: MeasurementKind::Diameter,
            uncertainty_mm: 0.0,
            witnesses: Vec::new(),
            bounding_box: BBox::from_point(Point::new(1.0, 0.0)).expand(0.05).into(),
            layers: Vec::new(),
            subjects: finding.subjects.clone(),
            evidence: finding.evidence.clone(),
            note: None,
        });
        let aliases = assign_ids(std::slice::from_mut(&mut finding));
        assert_eq!(finding.id, id);
        assert_eq!(aliases.get("dfm-bee136ee7a39"), Some(&id));
        assert!(finding.sites[0].id.starts_with(&format!("{id}-site-")));
    }

    #[test]
    fn a_waiver_written_against_a_released_id_still_applies() {
        use crate::commands::dfm::waivers::{Waiver, WaiverFile, apply};
        let mut finding = finding_at(1.0);
        finding.subjects.push(Subject {
            role: "hole",
            kind: "via_hole",
            ..Subject::default()
        });
        let aliases = assign_ids(std::slice::from_mut(&mut finding));
        assert_ne!(finding.id, "dfm-bee136ee7a39");
        let file = WaiverFile {
            waiver: vec![Waiver {
                finding: "dfm-bee136ee7a39".to_owned(),
                reason: "approved by fab".to_owned(),
                expires: None,
            }],
        };
        let outcome = apply(
            std::slice::from_mut(&mut finding),
            &file,
            &aliases,
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        );
        assert!(finding.waived);
        assert_eq!(outcome.applied, 1);
        assert!(outcome.unmatched.is_empty());
    }

    #[test]
    fn noise_level_coordinate_changes_do_not_rekey_findings_or_sites() {
        let site = |center: Point, subjects: Vec<Subject>| Site {
            id: String::new(),
            measurement: Measurement::minimum_distance(0.1, 0.2),
            measurement_kind: MeasurementKind::Clearance,
            uncertainty_mm: 0.0,
            witnesses: Vec::new(),
            bounding_box: BBox::from_point(center).expand(0.05).into(),
            layers: Vec::new(),
            subjects,
            evidence: vec![Evidence::circle("hole", center, 0.1)],
            note: None,
        };
        // Placed by its drilled subject; and by its measured point, here the
        // midpoint of gridded coordinates, on the half-micrometre lattice.
        for anchored in [true, false] {
            let ids = |noise: f64| {
                let center = Point::new(12.3455 + noise, -4.0005 - noise);
                let subject = Subject {
                    role: "hole",
                    kind: "via_hole",
                    anchor: anchored.then(|| center.into()),
                    ..Subject::default()
                };
                let mut finding = finding_at(0.0);
                // The witness of an anchored finding may be any equally near point.
                finding.location.point = Some(if anchored {
                    Point::new(12.0 + 1e3 * noise, -4.0).into()
                } else {
                    center.into()
                });
                finding.subjects.push(subject.clone());
                finding.sites.push(site(center, vec![subject]));
                assign_ids(std::slice::from_mut(&mut finding));
                (finding.id.clone(), finding.sites[0].id.clone())
            };
            let reference = ids(0.0);
            for noise in [1e-12, -1e-12, 3e-10] {
                assert_eq!(ids(noise), reference, "anchored {anchored}, noise {noise}");
            }
            assert_ne!(
                ids(0.002).0,
                reference.0,
                "two micrometres away is elsewhere"
            );
        }
    }

    #[test]
    fn annular_ids_ignore_generated_export_identity_and_evidence_paths() {
        let mut finding = finding_at(1.0);
        finding.rule_id = "annular".to_owned();
        finding.subjects.push(Subject {
            role: "land",
            kind: "padstack_land",
            name: Some("OVAL_32".into()),
            reference_designator: Some("U1".into()),
            pin: Some("3".into()),
            net: Some("GND".into()),
            padstack_ref: Some("PADSTACK_17".into()),
            source: Some(SourceLocator {
                step: Some("board".into()),
                layer: Some("F.Cu".into()),
                set_index: Some(8),
                feature_index: Some(13),
                instance_index: Some(2),
            }),
            drill_span: Some(DrillSpan {
                first_copper_index: 0,
                last_copper_index: 1,
                interpretation: "declared",
            }),
            anchor: Some(Point::new(2.0, 3.0).into()),
            ..Subject::default()
        });
        finding
            .evidence
            .push(Evidence::circle("drilled_hole", Point::new(2.0, 3.0), 0.2));
        finding.sites.push(Site {
            id: String::new(),
            measurement: Measurement::minimum_distance(0.1, 0.2),
            measurement_kind: MeasurementKind::MissingCopper,
            uncertainty_mm: 0.0,
            witnesses: Vec::new(),
            bounding_box: BBox::from_point(Point::new(1.0, 0.0)).expand(0.2).into(),
            layers: Vec::new(),
            subjects: finding.subjects.clone(),
            evidence: vec![Evidence {
                role: "missing_copper",
                kind: "region",
                paths: vec![vec![
                    Point::new(0.9, 0.0).into(),
                    Point::new(1.1, 0.0).into(),
                ]],
                ..Evidence::default()
            }],
            note: None,
        });
        let annular = HashSet::from(["annular"]);
        let aliases = super::assign_ids(std::slice::from_mut(&mut finding), &annular);
        let finding_id = finding.id.clone();
        let site_id = finding.sites[0].id.clone();
        for released in ["dfm-ed8c542f1d5c", "dfm-96a22f500f68"] {
            assert_eq!(
                aliases.get(released),
                Some(&finding_id),
                "both released annular id formats remain waiver aliases"
            );
        }

        for subject in [&mut finding.subjects[0], &mut finding.sites[0].subjects[0]] {
            subject.name = Some("OVAL_10".into());
            subject.padstack_ref = Some("PADSTACK_4".into());
            let source = subject.source.as_mut().unwrap();
            source.set_index = Some(10);
            source.feature_index = Some(29);
        }
        finding.sites[0].evidence[0].paths = vec![vec![
            Point::new(0.95, -0.05).into(),
            Point::new(1.05, 0.05).into(),
        ]];
        finding.location.point = Some(Point::new(9.0, 9.0).into());
        super::assign_ids(std::slice::from_mut(&mut finding), &annular);

        assert_eq!(finding.id, finding_id);
        assert_eq!(finding.sites[0].id, site_id);

        let mut different_hole = finding_at(9.0);
        different_hole.rule_id = "annular".to_owned();
        different_hole.subjects = finding.subjects.clone();
        different_hole.subjects[0].anchor = Some(Point::new(5.0, 3.0).into());
        different_hole.layers = finding.layers.clone();
        different_hole
            .evidence
            .push(Evidence::circle("drilled_hole", Point::new(5.0, 3.0), 0.2));
        super::assign_ids(std::slice::from_mut(&mut different_hole), &annular);
        assert_ne!(different_hole.id, finding.id);
    }

    fn placed_hole(instance: u32) -> Finding {
        let center = Point::new(1.0, 2.0);
        let subject = Subject {
            role: "hole",
            kind: "via_hole",
            provenance: Some(SourceLocator {
                step: Some("board".into()),
                layer: Some("DRILL".into()),
                set_index: Some(0),
                feature_index: Some(4),
                instance_index: Some(instance),
            }),
            ..Subject::default()
        };
        let mut finding = finding_at(center.x);
        finding.subjects.push(subject.clone());
        finding.sites.push(Site {
            id: String::new(),
            measurement: Measurement::minimum_distance(0.1, 0.2),
            measurement_kind: MeasurementKind::Diameter,
            uncertainty_mm: 0.0,
            witnesses: Vec::new(),
            bounding_box: BBox::from_point(center).expand(0.05).into(),
            layers: Vec::new(),
            subjects: vec![subject],
            evidence: vec![Evidence::circle("hole", center, 0.1)],
            note: None,
        });
        finding
    }

    #[test]
    fn native_display_metadata_preserves_finding_and_site_ids() {
        use super::super::report::{DisplayCircle, EvidenceDisplay};
        let mut finding = placed_hole(4);
        assign_ids(std::slice::from_mut(&mut finding));
        let original_finding = finding.id.clone();
        let original_site = finding.sites[0].id.clone();
        let circle = DisplayCircle {
            center: Point::new(1.0, 2.0).into(),
            diameter: 0.1,
        };
        for display in [
            EvidenceDisplay::Path {
                paths: vec!["M1 2 A0.1 0.1 0 0 1 1.1 2.1 Z".into()],
                fill_rule: "evenodd",
            },
            EvidenceDisplay::RoundStroke {
                paths: vec![vec![Point::ZERO.into(), Point::new(1.0, 1.0).into()]],
                width_mm: 0.2,
            },
            EvidenceDisplay::CircleMinusLayer {
                center: circle.center,
                diameter: circle.diameter,
                layer: "F.Cu".into(),
            },
            EvidenceDisplay::CircleIntersection {
                first: circle,
                second: circle,
            },
        ] {
            finding.sites[0].evidence[0].display = Some(display);
            assign_ids(std::slice::from_mut(&mut finding));
            assert_eq!(finding.id, original_finding);
            assert_eq!(finding.sites[0].id, original_site);
        }
    }
}
