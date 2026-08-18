//! The rule engine: one evaluator per measurement kind, uniform bookkeeping.
//!
//! Every rule reduces to enumerating entities, taking one measurement per
//! subject, and comparing it against the rule's limit. Each check lives in
//! its own module, whose docstring defines the measurement mathematically.
//! The engine here owns the shared policy — skip reasons, checked counts,
//! finding order, stable ids, waivers, statuses — so each check only
//! measures, and may assume its subject pools are non-empty.

mod annular_ring;
mod board_array_spacing;
mod hole_diameter;
mod hole_pair_clearance;
mod linework_clearance;
mod slot_width;
mod thin_regions;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::OnceLock;

use chrono::NaiveDate;
use ipc2581::Symbol;
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::geom::BBox;
use pcb_ir::geom::dfm::{ClearanceMeasurement, RegionBoundaryIndex};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

use crate::ipc2581::Ipc2581;

use super::design::{Design, Hole, HoleClass};
use super::report::{
    Evidence, Finding, LayerRef, Location, Measurement, RuleResult, RuleStatus, SourceLocator,
    Subject, Witness,
};
use super::rules::{Linework, Rule, RuleKind};
use super::waivers::{self, WaiverFile, WaiverOutcome};

use thin_regions::Residue;

const COMPARISON_EPSILON_MM: f64 = 1e-6;

#[derive(Default)]
pub(super) struct Results {
    pub(super) rules: Vec<RuleResult>,
    pub(super) findings: Vec<Finding>,
    pub(super) waivers: Option<WaiverOutcome>,
}

pub(super) fn run(
    rules: &[Rule],
    design: &Design,
    ipc: &Ipc2581,
    waiver_file: Option<&WaiverFile>,
    today: NaiveDate,
) -> Results {
    let ctx = Context::new(design, ipc, rules);
    let mut results = Results::default();
    for rule in rules {
        let mut result = RuleResult::new(rule);
        match skip_reason(rule, design) {
            Some(reason) => result.skip(reason),
            None => match evaluate(rule, &ctx) {
                // A nominally populated pool can still yield nothing to
                // measure (e.g. hole pairs with disjoint spans); an
                // unexercised rule must not read as validated.
                (0, _) => result.skip(format!(
                    "no measurable {} subjects in the selected layout target",
                    rule.kind.subject()
                )),
                (checked, findings) => {
                    result.checked = checked;
                    results.findings.extend(findings);
                }
            },
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

/// The one skip policy: a rule is skipped when its subject pool or a
/// required layer pool is empty for the selected layout target.
fn skip_reason(rule: &Rule, design: &Design) -> Option<String> {
    let no = |what: &str| Some(format!("no {what} in the selected layout target"));
    match rule.kind {
        RuleKind::BoardArrayPairClearance if design.scope != ArtworkScope::ArrayFlattened => {
            return Some("board-array spacing requires --layout-target board-array".to_owned());
        }
        RuleKind::BoardArrayPairClearance if design.board_arrays.len() < 2 => {
            return no("two or more direct board-array instances");
        }
        RuleKind::HoleDiameter(class) | RuleKind::AnnularRing(class)
            if holes_of_class(design, class).is_empty() =>
        {
            return no(&format!("{} holes", class.label()));
        }
        RuleKind::HolePairClearance if design.holes.len() < 2 => {
            return no("two or more holes");
        }
        RuleKind::SlotWidth if design.slots.is_empty() => {
            return no("routed slots");
        }
        RuleKind::LineworkToCopperClearance(Linework::VScore) if design.scores.is_empty() => {
            return no("V-score centerlines");
        }
        RuleKind::LineworkToCopperClearance(Linework::BoardEdge)
            if design.board_outlines.is_empty() =>
        {
            return no("board profile outlines");
        }
        _ => {}
    }
    let needs = rule.kind.needs();
    if needs.copper && design.copper_layers.is_empty() {
        return no("copper layers");
    }
    if needs.masks && design.mask_layers.is_empty() {
        return no("soldermask layers");
    }
    None
}

fn evaluate(rule: &Rule, ctx: &Context) -> (usize, Vec<Finding>) {
    match rule.kind {
        RuleKind::HoleDiameter(class) => hole_diameter::evaluate(rule, class, ctx),
        RuleKind::SlotWidth => slot_width::evaluate(rule, ctx),
        RuleKind::HolePairClearance => hole_pair_clearance::evaluate(rule, ctx),
        RuleKind::AnnularRing(class) => annular_ring::evaluate(rule, class, ctx),
        RuleKind::LineworkToCopperClearance(linework) => {
            linework_clearance::evaluate(rule, linework, ctx)
        }
        RuleKind::BoardArrayPairClearance => board_array_spacing::evaluate(rule, ctx),
        RuleKind::ThinFeature(sel) => thin_regions::evaluate(rule, sel, Residue::Feature, ctx),
        RuleKind::ThinGap(sel) => thin_regions::evaluate(rule, sel, Residue::Gap, ctx),
    }
}

/// Shared evaluation state: the design, the interner for resolving entity
/// symbols into report strings, and one boundary index per copper layer,
/// built lazily and reused by every rule that queries copper.
struct Context<'a> {
    design: &'a Design,
    ipc: &'a Ipc2581,
    boundary_search_mm: f64,
    copper_boundaries: OnceLock<Vec<RegionBoundaryIndex>>,
}

impl<'a> Context<'a> {
    fn new(design: &'a Design, ipc: &'a Ipc2581, rules: &[Rule]) -> Self {
        // A pitch hint for the boundary grids, from the rule limits alone:
        // queries stay correct at any pitch, and sizing cells by hole radii
        // would let one large hole coarsen every fine-clearance query.
        let boundary_search_mm = rules
            .iter()
            .map(|rule| match rule.kind {
                RuleKind::AnnularRing(_) | RuleKind::LineworkToCopperClearance(_) => {
                    rule.limit.millimeters()
                }
                _ => 0.0,
            })
            .fold(1.0, f64::max);
        Self {
            design,
            ipc,
            boundary_search_mm,
            copper_boundaries: OnceLock::new(),
        }
    }

    fn copper_boundaries(&self) -> &[RegionBoundaryIndex] {
        let search_mm = self.boundary_search_mm;
        let copper_layers = &self.design.copper_layers;
        self.copper_boundaries.get_or_init(|| {
            copper_layers
                .par_iter()
                .map(|layer| RegionBoundaryIndex::new(&layer.image, search_mm))
                .collect()
        })
    }

    fn resolve(&self, symbol: Option<Symbol>) -> Option<String> {
        symbol.map(|symbol| self.ipc.resolve(symbol).to_owned())
    }
}

/// A clearance finding under construction: measurement plus report identity.
struct ClearanceViolation<'a> {
    rule: &'a Rule,
    title: &'static str,
    message: String,
    witness_roles: [&'static str; 2],
    clearance: ClearanceMeasurement,
    layers: Vec<LayerRef>,
    subjects: Vec<Subject>,
    evidence: Vec<Evidence>,
}

impl ClearanceViolation<'_> {
    fn into_finding(self) -> Finding {
        let bbox =
            BBox::from_point(self.clearance.first).union(BBox::from_point(self.clearance.second));
        Finding {
            title: self.title.to_owned(),
            message: self.message,
            measurement: Measurement::minimum(
                self.clearance.distance_mm,
                self.rule.limit.millimeters(),
            ),
            location: Location {
                point: Some(self.clearance.first.midpoint(self.clearance.second).into()),
                bounding_box: Some(bbox.into()),
                witnesses: vec![
                    Witness::new(self.witness_roles[0], self.clearance.first),
                    Witness::new(self.witness_roles[1], self.clearance.second),
                ],
            },
            layers: self.layers,
            subjects: self.subjects,
            evidence: self.evidence,
            ..blank_finding(self.rule)
        }
    }
}

/// The rule-derived identity fields every finding starts from.
fn blank_finding(rule: &Rule) -> Finding {
    Finding {
        id: String::new(),
        rule_id: rule.id.clone(),
        severity: rule.severity,
        waived: false,
        waiver_reason: None,
        title: String::new(),
        message: String::new(),
        measurement: Measurement::minimum(0.0, 0.0),
        location: Location::default(),
        layers: Vec::new(),
        subjects: Vec::new(),
        evidence: Vec::new(),
    }
}

fn holes_of_class(design: &Design, class: HoleClass) -> Vec<&Hole> {
    design
        .holes
        .iter()
        .filter(|hole| hole.class == class)
        .collect()
}

/// The shared subject shape of every drilled feature (holes and slots).
#[allow(clippy::too_many_arguments)]
fn drilled_subject(
    ctx: &Context,
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
        net: ctx.resolve(net),
        padstack_ref: ctx.resolve(padstack),
        source: Some(SourceLocator {
            step: ctx.resolve(step),
            layer: Some(layer.name.clone()),
            set_index: Some(set_index),
            feature_index: Some(feature_index),
            instance_index: None,
        }),
        ..Subject::default()
    }
}

fn hole_subject(ctx: &Context, hole: &Hole, role: &'static str) -> Subject {
    drilled_subject(
        ctx,
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

fn unique_layers(first: &LayerRef, second: &LayerRef) -> Vec<LayerRef> {
    if first.name == second.name {
        vec![first.clone()]
    } else {
        vec![first.clone(), second.clone()]
    }
}

fn violates_minimum(actual: f64, required: f64) -> bool {
    actual + COMPARISON_EPSILON_MM < required
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
            measurement: Measurement::minimum(0.0, 1.0),
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
