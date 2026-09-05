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
mod drilled_board_edge_clearance;
mod hole_aspect_ratio;
mod hole_clearance;
mod hole_diameter;
mod hole_pair_clearance;
mod layer_count;
mod linework_clearance;
mod slot_clearance;
mod slot_width;
mod thin_regions;

use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::NaiveDate;
use ipc2581::Symbol;
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::geom::dfm::Distance;
use pcb_ir::geom::{Affine2, BBox, Point};
use sha2::{Digest, Sha256};

use super::design::{Design, Hole, HoleClass, Slot};
use super::pdk::SlotPlating;
use super::report::{
    Evidence, Finding, LayerRef, Location, Measurement, MeasurementKind, ReportBBox, ReportPoint,
    RuleResult, RuleStatus, Site, SourceLocator, Subject, Witness,
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
    Distance(Evaluation),
    Count(CountEvaluation),
    Ratio(RatioEvaluation),
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
                    RuleEvaluation::Ratio(evaluation) => {
                        debug_assert_eq!(rule.comparison, Comparison::Maximum);
                        result.assumptions = evaluation.assumptions;
                        if let Some(reason) = evaluation.incomplete_reason {
                            result.skip(reason);
                        } else if evaluation.checked == 0 {
                            result.skip(format!(
                                "no measurable {} subjects in the selected layout target",
                                rule.kind.semantics().subject
                            ));
                        } else {
                            result.checked = evaluation.checked;
                            let maximum = rule.limit.ratio();
                            results.findings.extend(
                                evaluation
                                    .measured
                                    .into_iter()
                                    .filter(|measured| measured.actual_ratio > maximum)
                                    .map(|measured| ratio_finding(rule, measured, maximum)),
                            );
                        }
                    }
                }
            }
        }
        results.rules.push(result);
    }
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
    assign_ids(&mut results.findings);
    for finding in &mut results.findings {
        let instance = finding
            .subjects
            .first()
            .and_then(|subject| subject.provenance.as_ref())
            .and_then(|source| source.instance_index);
        finding.group_key = instance
            .and_then(|index| {
                design
                    .imported
                    .geometry
                    .layout
                    .instances
                    .get(index as usize)
            })
            .and_then(|instance| instance.transform.inverse())
            .and_then(|inverse| repeat_group_key(finding, inverse));
    }
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
    if !rule.conditions.applies_to_design(design) {
        return Some("rule conditions do not apply to this stackup".to_owned());
    }
    let subjects = match rule.kind {
        RuleKind::CopperLayerCount => None,
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
    let layers = (pools.copper
        && design
            .copper_layers
            .iter()
            .all(|layer| !rule.conditions.applies_to_layer(layer)))
    .then(|| "applicable copper layers".to_owned())
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
        RuleKind::HoleDiameter(class) => hole_diameter::evaluate(limit(), class, design).into(),
        RuleKind::HoleAspectRatio(class) => {
            RuleEvaluation::Ratio(hole_aspect_ratio::evaluate(class, &rule.conditions, design))
        }
        RuleKind::SlotWidth(plating) => slot_width::evaluate(limit(), plating, design).into(),
        RuleKind::HolePairClearance(first, second) => {
            hole_pair_clearance::evaluate(limit(), first, second, design).into()
        }
        RuleKind::HoleToBoardEdgeClearance(class) => {
            drilled_board_edge_clearance::evaluate_holes(limit(), class, design).into()
        }
        RuleKind::SlotToBoardEdgeClearance(plating) => {
            drilled_board_edge_clearance::evaluate_slots(limit(), plating, design).into()
        }
        RuleKind::AnnularRing(class) => {
            annular_ring::evaluate(limit(), class, &rule.conditions, design).into()
        }
        RuleKind::HoleToCopperClearance(class) => {
            hole_clearance::evaluate(limit(), class, &rule.conditions, design).into()
        }
        RuleKind::SlotToCopperClearance(plating) => {
            slot_clearance::evaluate(limit(), plating, &rule.conditions, design).into()
        }
        RuleKind::LineworkToCopperClearance(linework) => {
            linework_clearance::evaluate(limit(), linework, &rule.conditions, design).into()
        }
        RuleKind::BoardArrayPairClearance => board_array_spacing::evaluate(limit(), design).into(),
        RuleKind::CopperFeatureWidth => {
            thin_regions::copper_feature_width(limit(), &rule.conditions, design).into()
        }
        RuleKind::CopperClearance => {
            copper_clearance::evaluate(limit(), &rule.conditions, design).into()
        }
        RuleKind::SoldermaskWeb => thin_regions::soldermask_web(limit(), design).into(),
    }
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
        group_key: None,
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
        group_key: None,
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
        group_key: None,
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
    subject.drill_span = Some(slot.drill_span.clone());
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

/// The original evidence record, excluding display-only constructions.
/// Borrow the potentially large rings rather than cloning them to hash IDs.
#[derive(serde::Serialize)]
struct LegacyEvidence<'a> {
    role: &'static str,
    kind: &'static str,
    center: &'a Option<ReportPoint>,
    diameter: &'a Option<f64>,
    start: &'a Option<ReportPoint>,
    end: &'a Option<ReportPoint>,
    bounding_box: &'a Option<ReportBBox>,
    paths: &'a [Vec<ReportPoint>],
}

impl<'a> From<&'a Evidence> for LegacyEvidence<'a> {
    fn from(evidence: &'a Evidence) -> Self {
        Self {
            role: evidence.role,
            kind: evidence.kind,
            center: &evidence.center,
            diameter: &evidence.diameter,
            start: &evidence.start,
            end: &evidence.end,
            bounding_box: &evidence.bounding_box,
            paths: &evidence.paths,
        }
    }
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
            finding
                .subjects
                .iter()
                .map(LegacySubject::from)
                .collect::<Vec<_>>(),
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
        let mut sites_seen: HashMap<String, usize> = HashMap::new();
        for site in &mut finding.sites {
            let bytes = serde_json::to_vec(&(
                &site.layers,
                &site.measurement_kind,
                &site.bounding_box,
                site.evidence
                    .iter()
                    .map(LegacyEvidence::from)
                    .collect::<Vec<_>>(),
            ))
            .expect("site identity serializes");
            let digest = Sha256::digest(bytes);
            let short = hex::encode(&digest[..6]);
            let ordinal = sites_seen
                .entry(short.clone())
                .and_modify(|n| *n += 1)
                .or_insert(1);
            site.id = format!("{}-site-{short}", finding.id);
            if *ordinal > 1 {
                site.id.push_str(&format!("-{ordinal}"));
            }
        }
    }
}

/// Collapse only proven repeats of the same definition-local subjects, with
/// the same measured failure geometry. Cross-occurrence or unattributed
/// findings remain separate. This affects presentation only, never waivers.
fn repeat_group_key(finding: &Finding, inverse: Affine2) -> Option<String> {
    let instance = finding
        .subjects
        .first()?
        .provenance
        .as_ref()?
        .instance_index?;
    if finding.sites.is_empty() {
        return None;
    }
    let quantize = |n: f64| {
        let n = (n * 1_000_000.0).round() / 1_000_000.0;
        if n == 0.0 { 0.0 } else { n }
    };
    let point = |p: super::report::ReportPoint| {
        let p = inverse.transform_point(Point::new(p.x, p.y));
        [quantize(p.x), quantize(p.y)]
    };
    let bounds = |b: super::report::ReportBBox| {
        let b = b.as_bbox().transformed(inverse);
        [
            quantize(b.min.x),
            quantize(b.min.y),
            quantize(b.max.x),
            quantize(b.max.y),
        ]
    };
    let subject_identity = |subject: &Subject| {
        let source = subject.provenance.as_ref()?;
        if source.step.is_none() || source.instance_index != Some(instance) {
            return None;
        }
        Some(serde_json::json!([
            subject.role,
            subject.kind,
            subject.net,
            subject.padstack_ref,
            source.step,
            source.layer,
            source.set_index,
            source.feature_index,
            subject.drill_span
        ]))
    };
    let subjects = finding
        .subjects
        .iter()
        .map(subject_identity)
        .collect::<Option<Vec<_>>>()?;
    let sites = finding.sites.iter().map(|site| {
        if site.subjects.is_empty() {
            return None;
        }
        let subjects = site.subjects.iter().map(subject_identity).collect::<Option<Vec<_>>>()?;
        let measurement = match site.measurement {
            Measurement::Distance { actual_mm, required_mm, .. } => [quantize(actual_mm), quantize(required_mm)],
            Measurement::Count { actual_count, required_count, .. } => [f64::from(actual_count), f64::from(required_count)],
            Measurement::Ratio { actual_ratio, maximum_ratio, .. } => [quantize(actual_ratio), quantize(maximum_ratio)],
        };
        let evidence = site.evidence.iter().map(|evidence| serde_json::json!({
            "role": evidence.role, "kind": evidence.kind,
            "center": evidence.center.map(point), "diameter": evidence.diameter.map(quantize),
            "start": evidence.start.map(point), "end": evidence.end.map(point),
            "bounds": evidence.bounding_box.map(bounds),
            "paths": evidence.paths.iter().map(|path| path.iter().copied().map(point).collect::<Vec<_>>()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>();
        let witnesses = site.witnesses.iter().map(|witness| serde_json::json!([witness.role, point(witness.point)])).collect::<Vec<_>>();
        Some(serde_json::json!([site.measurement_kind, measurement, quantize(site.uncertainty_mm), site.layers, subjects, witnesses, evidence, site.note]))
    }).collect::<Option<Vec<_>>>()?;
    let bytes = serde_json::to_vec(&(&finding.rule_id, subjects, sites)).ok()?;
    Some(format!(
        "cause-{}",
        hex::encode(&Sha256::digest(bytes)[..10])
    ))
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
            sites: Vec::new(),
            group_key: None,
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

    #[test]
    fn visual_metadata_does_not_change_the_original_waiver_id() {
        let mut finding = finding_at(1.0);
        finding.subjects.push(Subject {
            role: "hole",
            kind: "via_hole",
            ..Subject::default()
        });
        assign_ids(std::slice::from_mut(&mut finding));
        // Independently computed from the pre-sites v1 JSON identity record.
        assert_eq!(finding.id, "dfm-bee136ee7a39");
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
        assign_ids(std::slice::from_mut(&mut finding));
        assert_eq!(finding.id, "dfm-bee136ee7a39");
        assert!(finding.sites[0].id.starts_with("dfm-bee136ee7a39-site-"));
    }

    fn repeated_hole(offset: f64, instance: u32) -> Finding {
        let center = Point::new(1.0 + offset, 2.0);
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
    fn native_display_metadata_preserves_site_ids_waivers_and_repeat_groups() {
        use super::super::report::{DisplayCircle, EvidenceDisplay};
        let mut finding = repeated_hole(0.0, 4);
        let evidence = &finding.sites[0].evidence[0];
        assert_eq!(
            serde_json::to_string(evidence).unwrap(),
            serde_json::to_string(&LegacyEvidence::from(evidence)).unwrap(),
            "the identity projection preserves the original field order and nulls"
        );
        assign_ids(std::slice::from_mut(&mut finding));
        let original_finding = finding.id.clone();
        let original_site = finding.sites[0].id.clone();
        let original_group = repeat_group_key(&finding, Affine2::IDENTITY).unwrap();
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
            assert_eq!(
                repeat_group_key(&finding, Affine2::IDENTITY).as_deref(),
                Some(original_group.as_str())
            );
        }
    }

    #[test]
    fn grouping_requires_the_same_definition_and_local_failure() {
        let first = repeated_hole(0.0, 0);
        let mut repeated = repeated_hole(30.0, 1);
        let local = Affine2::translation(Point::new(-30.0, 0.0));
        assert_eq!(
            repeat_group_key(&first, Affine2::IDENTITY),
            repeat_group_key(&repeated, local)
        );
        repeated.sites[0].subjects[0].net = Some("different contributor".into());
        assert_ne!(
            repeat_group_key(&first, Affine2::IDENTITY),
            repeat_group_key(&repeated, local),
            "local geometry alone cannot establish the same source contributors"
        );
        repeated.sites[0].subjects[0].net = None;
        repeated.sites[0].subjects[0]
            .provenance
            .as_mut()
            .unwrap()
            .instance_index = Some(0);
        assert!(
            repeat_group_key(&repeated, local).is_none(),
            "a secondary site's cross-occurrence subject prevents grouping"
        );
        repeated.sites[0].subjects[0]
            .provenance
            .as_mut()
            .unwrap()
            .instance_index = Some(1);
        repeated.subjects[0]
            .provenance
            .as_mut()
            .unwrap()
            .feature_index = Some(5);
        assert_ne!(
            repeat_group_key(&first, Affine2::IDENTITY),
            repeat_group_key(&repeated, local)
        );
        repeated.subjects.push(first.subjects[0].clone());
        assert!(
            repeat_group_key(&repeated, local).is_none(),
            "cross-occurrence findings cannot collapse into one board cause"
        );
        repeated.subjects[0].provenance = None;
        assert!(
            repeat_group_key(&repeated, local).is_none(),
            "missing provenance cannot be guessed from position"
        );
    }
}
