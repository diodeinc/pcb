//! The rule engine: one evaluator per measurement kind, uniform bookkeeping.
//!
//! Every rule reduces to enumerating subjects and taking one [`Distance`]
//! per subject. Each check lives in its own module, whose docstring defines
//! the measurement mathematically, and returns those measurements with the
//! report identity of what was measured. The engine here owns everything
//! else — the verdict against the limit, finding text, witness roles, skip
//! reasons, checked counts, finding order, stable ids, waivers, statuses —
//! so a check only measures, and may assume its subject pools are non-empty.

mod annular_ring;
mod board_array_spacing;
mod copper_clearance;
mod hole_diameter;
mod hole_pair_clearance;
mod layer_count;
mod linework_clearance;
mod slot_width;
mod thin_regions;

use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::NaiveDate;
use ipc2581::Symbol;
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::geom::BBox;
use pcb_ir::geom::dfm::Distance;
use sha2::{Digest, Sha256};

use super::design::{Design, Hole, HoleClass};
use super::report::{
    Evidence, Finding, LayerRef, Location, Measurement, RuleResult, RuleStatus, SourceLocator,
    Subject, Witness,
};
use super::rules::{Comparison, Linework, Rule, RuleKind};
use super::waivers::{self, WaiverFile, WaiverOutcome};

/// Absorbs floating-point unit conversion when a measurement sits exactly
/// on its limit.
const COMPARISON_EPSILON_MM: f64 = 1e-6;

#[derive(Default)]
pub(super) struct Results {
    pub(super) rules: Vec<RuleResult>,
    pub(super) findings: Vec<Finding>,
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

enum RuleEvaluation {
    Distance(Evaluation),
    Count(CountEvaluation),
}

impl From<Evaluation> for RuleEvaluation {
    fn from(evaluation: Evaluation) -> Self {
        Self::Distance(evaluation)
    }
}

pub(super) fn run(
    rules: &[Rule],
    design: &Design,
    waiver_file: Option<&WaiverFile>,
    today: NaiveDate,
) -> Results {
    let mut results = Results::default();
    for rule in rules {
        let mut result = RuleResult::new(rule);
        match skip_reason(rule, design) {
            Some(reason) => result.skip(reason),
            None => {
                let evaluation = evaluate(rule, design);
                match evaluation {
                    RuleEvaluation::Distance(evaluation) => {
                        debug_assert_eq!(rule.comparison, Comparison::Minimum);
                        let limit = rule.limit.length().millimeters();
                        match evaluation.checked {
                            // A nominally populated pool can still yield nothing to
                            // measure (e.g. hole pairs with disjoint spans); an
                            // unexercised rule must not read as validated.
                            0 => result.skip(format!(
                                "no measurable {} subjects in the selected layout target",
                                rule.kind.semantics().subject
                            )),
                            checked => {
                                result.checked = checked;
                                results.findings.extend(
                                    evaluation
                                        .measured
                                        .into_iter()
                                        .filter(|measured| violates(&measured.distance, limit))
                                        .map(|measured| finding(rule, measured)),
                                );
                            }
                        }
                    }
                    RuleEvaluation::Count(evaluation) => {
                        result.checked = 1;
                        let limit = rule.limit.count();
                        if violates_count(evaluation.actual, rule.comparison, limit) {
                            results
                                .findings
                                .push(count_finding(rule, evaluation, limit));
                        }
                    }
                }
            }
        }
        results.rules.push(result);
    }
    assign_ids(&mut results.findings);
    results.waivers = waiver_file.map(|file| waivers::apply(&mut results.findings, file, today));

    let mut per_rule: HashMap<&str, (usize, usize)> = HashMap::new();
    for finding in &results.findings {
        let (total, waived) = per_rule.entry(finding.rule_id.as_str()).or_default();
        *total += 1;
        *waived += usize::from(finding.waived);
    }
    for result in &mut results.rules {
        if !matches!(result.status, RuleStatus::Skipped) {
            let (total, waived) = per_rule.get(result.id.as_str()).copied().unwrap_or((0, 0));
            result.finish(total, waived);
        }
    }
    results
}

/// The one verdict: a distance violates a minimum when it is certainly
/// below it, beyond both its own geometric uncertainty and the comparison
/// epsilon.
fn violates(distance: &Distance, limit_mm: f64) -> bool {
    distance.certainly_below(limit_mm - COMPARISON_EPSILON_MM)
}

fn violates_count(actual: u32, comparison: Comparison, limit: u32) -> bool {
    match comparison {
        Comparison::Minimum => actual < limit,
        Comparison::Maximum => actual > limit,
    }
}

/// The one skip policy: a rule is skipped when its subject pool or a
/// required layer pool is empty for the selected layout target.
fn skip_reason(rule: &Rule, design: &Design) -> Option<String> {
    let subjects = match rule.kind {
        RuleKind::CopperLayerCount => None,
        RuleKind::BoardArrayPairClearance if design.scope != ArtworkScope::ArrayFlattened => {
            return Some("board-array spacing requires --layout-target board-array".to_owned());
        }
        RuleKind::BoardArrayPairClearance => (design.board_arrays.len() < 2)
            .then(|| "two or more direct board-array instances".to_owned()),
        RuleKind::HoleDiameter(class) | RuleKind::AnnularRing(class) => design
            .holes
            .iter()
            .all(|hole| hole.class != class)
            .then(|| format!("{} holes", class.label())),
        RuleKind::HolePairClearance => {
            (design.holes.len() < 2).then(|| "two or more holes".to_owned())
        }
        RuleKind::SlotWidth => design.slots.is_empty().then(|| "routed slots".to_owned()),
        RuleKind::LineworkToCopperClearance(Linework::VScore) => design
            .scores
            .is_empty()
            .then(|| "V-score centerlines".to_owned()),
        RuleKind::LineworkToCopperClearance(Linework::BoardEdge) => design
            .board_outlines
            .is_empty()
            .then(|| "board profile outlines".to_owned()),
        RuleKind::CopperFeatureWidth | RuleKind::CopperClearance | RuleKind::SoldermaskWeb => None,
    };
    let pools = rule.kind.semantics().pools;
    let layers = (pools.copper && design.copper_layers.is_empty())
        .then(|| "copper layers".to_owned())
        .or_else(|| {
            (pools.masks && design.mask_layers.is_empty()).then(|| "soldermask layers".to_owned())
        });
    subjects
        .or(layers)
        .map(|what| format!("no {what} in the selected layout target"))
}

fn evaluate(rule: &Rule, design: &Design) -> RuleEvaluation {
    let limit = || rule.limit.length().millimeters();
    match rule.kind {
        RuleKind::CopperLayerCount => RuleEvaluation::Count(layer_count::evaluate(design)),
        RuleKind::HoleDiameter(class) => hole_diameter::evaluate(class, design).into(),
        RuleKind::SlotWidth => slot_width::evaluate(design).into(),
        RuleKind::HolePairClearance => hole_pair_clearance::evaluate(limit(), design).into(),
        RuleKind::AnnularRing(class) => annular_ring::evaluate(limit(), class, design).into(),
        RuleKind::LineworkToCopperClearance(linework) => {
            linework_clearance::evaluate(limit(), linework, design).into()
        }
        RuleKind::BoardArrayPairClearance => board_array_spacing::evaluate(limit(), design).into(),
        RuleKind::CopperFeatureWidth => thin_regions::copper_feature_width(limit(), design).into(),
        RuleKind::CopperClearance => copper_clearance::evaluate(limit(), design).into(),
        RuleKind::SoldermaskWeb => thin_regions::soldermask_web(limit(), design).into(),
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
    }
}

/// Holes of one plating class, with their indices into the hole pool.
fn holes_of_class<'a>(design: &'a Design<'a>, class: HoleClass) -> Vec<(usize, &'a Hole)> {
    design
        .holes
        .iter()
        .enumerate()
        .filter(|(_, hole)| hole.class == class)
        .collect()
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
    drilled_subject(
        design,
        role,
        hole.class.subject_kind(),
        hole.net,
        hole.padstack,
        hole.step,
        &hole.layer,
        hole.source_set_index,
        hole.source_feature_index,
    )
}

/// The layers a finding spans, each named once.
fn layers<'a>(layers: impl IntoIterator<Item = &'a LayerRef>) -> Vec<LayerRef> {
    let mut layers = layers.into_iter().cloned().collect::<Vec<_>>();
    layers.dedup_by(|left, right| left.name == right.name);
    layers
}

/// Sort findings into rule/location order and give each an id hashed from
/// what it is about — rule, subjects, layers, and measured location. The id
/// is deterministic per input and survives revisions only while those facts
/// are unchanged: a violation whose representative point moves is a new
/// finding, so its stale waiver surfaces as unmatched. The measured value
/// is deliberately excluded — a waived violation that changes magnitude
/// in place keeps its waiver.
fn assign_ids(findings: &mut [Finding]) {
    findings.sort_by(|left, right| {
        left.rule_id
            .cmp(&right.rule_id)
            .then_with(|| compare_locations(&left.location, &right.location))
    });
    let mut seen: HashMap<String, u32> = HashMap::new();
    for finding in findings.iter_mut() {
        let fingerprint = serde_json::to_string(&(
            &finding.rule_id,
            &finding.subjects,
            &finding.layers,
            &finding.location.point,
        ))
        .expect("finding identity serializes");
        let digest = Sha256::digest(fingerprint.as_bytes());
        let short = hex::encode(&digest[..6]);
        let repeat = seen
            .entry(short.clone())
            .and_modify(|n| *n += 1)
            .or_insert(1);
        finding.id = if *repeat == 1 {
            format!("dfm-{short}")
        } else {
            format!("dfm-{short}-{repeat}")
        };
    }
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
        }
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
}
