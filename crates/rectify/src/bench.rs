//! Benchmark the solver against a set of `.kicad_mod` files on disk.
//!
//! Each footprint's stored `(rotate ...)` / `(offset ...)` is treated as
//! ground truth. Before solving, the benchmark gives the solver a deterministic
//! randomized initial `(rotate ...)` / `(offset ...)` so solver priors cannot
//! accidentally use the answer key as their starting point. A footprint passes
//! when (a) the predicted rotation matches the stored transform and (b) the
//! predicted offset is within the L∞ tolerance of the stored offset (applied
//! uniformly to X, Y, Z).
//!
//! Two preset modes:
//!
//! | Mode     | Offset tolerance | Z-rotation equivalence |
//! |----------|------------------|------------------------|
//! | `loose`  | 0.20 mm L∞       | allowed                |
//! | `strict` | 0.10 mm L∞       | **not** allowed        |
//!
//! In addition to thresholded pass/fail counts, the bench emits a continuous
//! `reward_score` in `[0, 1]` so optimization runs can see progress before a
//! footprint flips all the way from fail to pass.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::Serialize;

use crate::footprint::{self, FootprintData, FootprintKind, PadKind, PadShape};
use crate::fs_util;
use crate::pose::{
    EulerPose, KICAD_ROTATION_ORDER, KICAD_ROTATION_SIGNS, RotationMatch, candidate_poses,
    classify_rotation, matrix_key_for_pose,
};
use crate::progress::batch_bar;
use crate::solver;

/// Offset tolerance for `--mode loose` (default).
const LOOSE_TOLERANCE_MM: f64 = 0.20;
/// Offset tolerance for `--mode strict`.
const STRICT_TOLERANCE_MM: f64 = 0.10;
/// Stable default seed for benchmark initial-transform randomization.
pub const DEFAULT_INITIAL_TRANSFORM_SEED: u64 = 1;

const RANDOM_OFFSET_XY_MIN_ABS_MM: f64 = 1.0;
const RANDOM_OFFSET_XY_MAX_ABS_MM: f64 = 5.0;
const RANDOM_OFFSET_Z_MIN_ABS_MM: f64 = 0.5;
const RANDOM_OFFSET_Z_MAX_ABS_MM: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchMode {
    Loose,
    Strict,
}

impl BenchMode {
    pub fn tolerance_mm(self) -> f64 {
        match self {
            BenchMode::Loose => LOOSE_TOLERANCE_MM,
            BenchMode::Strict => STRICT_TOLERANCE_MM,
        }
    }

    /// Strict mode requires exact rotation (no Z-rotation equivalence),
    /// so that future improvements to pin-1 / marking detection can be
    /// measured. Loose mode allows Z-rotation equivalence since the
    /// solver currently cannot distinguish symmetric orientations.
    pub fn allows_z_rotation(self) -> bool {
        matches!(self, BenchMode::Loose)
    }

    pub fn label(self) -> &'static str {
        match self {
            BenchMode::Loose => "loose",
            BenchMode::Strict => "strict",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchKindFilter {
    All,
    Smd,
    Tht,
    Mixed,
}

impl BenchKindFilter {
    fn label(self) -> &'static str {
        match self {
            BenchKindFilter::All => "all",
            BenchKindFilter::Smd => "smd",
            BenchKindFilter::Tht => "tht",
            BenchKindFilter::Mixed => "mixed",
        }
    }

    fn matches(self, kind: FootprintKind) -> bool {
        match self {
            BenchKindFilter::All => true,
            BenchKindFilter::Smd => kind == FootprintKind::SmdOnly,
            BenchKindFilter::Tht => kind == FootprintKind::ThtOnly,
            BenchKindFilter::Mixed => kind == FootprintKind::Mixed,
        }
    }
}

pub struct Args {
    pub paths: Vec<PathBuf>,
    pub mode: BenchMode,
    pub kind: BenchKindFilter,
    pub limit: Option<usize>,
    pub jobs: Option<usize>,
    pub jsonl: bool,
    pub randomize_initial_transform: bool,
    pub initial_transform_seed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EvalRecord {
    pub(crate) path: String,
    pub(crate) status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) footprint_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pipeline: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) model_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) predicted_rotate: Option<[i32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) predicted_offset: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) predicted_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) predicted_threshold_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) translation_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) initial_rotate: Option<[i32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) initial_offset: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) repo_rotate: Option<[i32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) repo_offset: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) offset_l_inf_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) xy_l_inf_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) z_diff_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) rotation_match: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reward_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

#[derive(Default, Serialize)]
struct ChoiceStats {
    pipeline: BTreeMap<String, usize>,
    translation_source: BTreeMap<String, usize>,
    threshold_mm: BTreeMap<String, usize>,
    rotation_match: BTreeMap<String, usize>,
    status_by_pipeline: BTreeMap<String, BTreeMap<String, usize>>,
    status_by_translation_source: BTreeMap<String, BTreeMap<String, usize>>,
}

impl ChoiceStats {
    fn add(&mut self, record: &EvalRecord) {
        let status = record.status.to_string();
        if let Some(pipeline) = record.pipeline {
            inc(&mut self.pipeline, pipeline);
            inc_nested(&mut self.status_by_pipeline, pipeline, &status);
        }
        if let Some(source) = record.translation_source.as_deref() {
            inc(&mut self.translation_source, source);
            inc_nested(&mut self.status_by_translation_source, source, &status);
        }
        if let Some(threshold) = record.predicted_threshold_mm {
            inc(&mut self.threshold_mm, &format!("{threshold:.2}"));
        }
        if let Some(rotation) = record.rotation_match {
            inc(&mut self.rotation_match, rotation);
        }
    }
}

fn inc(map: &mut BTreeMap<String, usize>, key: &str) {
    *map.entry(key.to_string()).or_insert(0) += 1;
}

fn inc_nested(map: &mut BTreeMap<String, BTreeMap<String, usize>>, outer: &str, inner: &str) {
    inc(map.entry(outer.to_string()).or_default(), inner);
}

#[derive(Default)]
struct BenchStats {
    pass: usize,
    fail: usize,
    skip: usize,
    error: usize,
    exact_rotation: usize,
    z_rotation: usize,
    mismatch_rotation: usize,
    reward_sum: f64,
    offset_l_inf_errors_mm: Vec<f64>,
    xy_l_inf_errors_mm: Vec<f64>,
    z_diff_errors_mm: Vec<f64>,
}

impl BenchStats {
    fn add(&mut self, record: &EvalRecord) {
        match record.status {
            "pass" => self.pass += 1,
            "fail" => self.fail += 1,
            "skip" => self.skip += 1,
            "error" => self.error += 1,
            _ => {}
        }
        match record.rotation_match {
            Some("exact") => self.exact_rotation += 1,
            Some("z_rotation") => self.z_rotation += 1,
            Some("mismatch") => self.mismatch_rotation += 1,
            _ => {}
        }
        if let Some(reward) = record.reward_score {
            self.reward_sum += reward;
        }
        if let Some(l_inf) = record.offset_l_inf_mm {
            self.offset_l_inf_errors_mm.push(l_inf);
        }
        if let Some(xy) = record.xy_l_inf_mm {
            self.xy_l_inf_errors_mm.push(xy);
        }
        if let Some(z) = record.z_diff_mm {
            self.z_diff_errors_mm.push(z);
        }
    }

    fn inferred(&self) -> usize {
        self.pass + self.fail
    }

    fn total(&self) -> usize {
        self.pass + self.fail + self.skip + self.error
    }

    fn pass_rate(&self) -> f64 {
        rate(self.pass, self.inferred())
    }

    fn reward_score(&self) -> f64 {
        rate_f64(self.reward_sum, self.inferred())
    }

    fn to_json(&self) -> serde_json::Value {
        let inferred = self.inferred();
        serde_json::json!({
            "total": self.total(),
            "inferred": inferred,
            "pass": self.pass,
            "fail": self.fail,
            "skip": self.skip,
            "error": self.error,
            "pass_rate": self.pass_rate(),
            "reward_score": self.reward_score(),
            "exact_rotation_rate": rate(self.exact_rotation, inferred),
            "z_rotation_rate": rate(self.z_rotation, inferred),
            "rotation_mismatch_rate": rate(self.mismatch_rotation, inferred),
            "mean_offset_l_inf_mm": mean(&self.offset_l_inf_errors_mm),
            "median_offset_l_inf_mm": quantile_nearest_rank(&self.offset_l_inf_errors_mm, 0.50),
            "p95_offset_l_inf_mm": quantile_nearest_rank(&self.offset_l_inf_errors_mm, 0.95),
            "mean_xy_l_inf_mm": mean(&self.xy_l_inf_errors_mm),
            "median_xy_l_inf_mm": quantile_nearest_rank(&self.xy_l_inf_errors_mm, 0.50),
            "p95_xy_l_inf_mm": quantile_nearest_rank(&self.xy_l_inf_errors_mm, 0.95),
            "mean_z_diff_mm": mean(&self.z_diff_errors_mm),
            "median_z_diff_mm": quantile_nearest_rank(&self.z_diff_errors_mm, 0.50),
            "p95_z_diff_mm": quantile_nearest_rank(&self.z_diff_errors_mm, 0.95),
        })
    }
}

pub fn run(args: Args) -> Result<()> {
    if args.paths.is_empty() {
        bail!("rectify bench requires at least one path (file or directory)");
    }
    if let Some(jobs) = args.jobs {
        rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build_global()
            .ok();
    }

    let mut footprints: Vec<PathBuf> = Vec::new();
    for root in &args.paths {
        fs_util::collect_footprints(root, &mut footprints)
            .with_context(|| format!("scanning {}", root.display()))?;
    }
    footprints.sort();
    footprints.dedup();
    if args.kind != BenchKindFilter::All {
        footprints.retain(|path| match footprint::parse(path) {
            Ok(fp) => args.kind.matches(fp.footprint_kind()),
            // Keep malformed footprints in filtered runs so they are reported
            // as errors rather than silently disappearing from the benchmark.
            Err(_) => true,
        });
    }

    let total = args.limit.unwrap_or(footprints.len()).min(footprints.len());
    let tolerance = args.mode.tolerance_mm();
    let mode_label = args.mode.label();
    let kind_label = args.kind.label();
    eprintln!(
        "rectify bench ({mode_label}, kind={kind_label}): {total} footprints, ±{tolerance} mm L∞"
    );
    if args.randomize_initial_transform {
        eprintln!(
            "  initial transforms randomized with seed={} (XY delta {:.1}..{:.1} mm, Z delta {:.1}..{:.1} mm)",
            args.initial_transform_seed,
            RANDOM_OFFSET_XY_MIN_ABS_MM,
            RANDOM_OFFSET_XY_MAX_ABS_MM,
            RANDOM_OFFSET_Z_MIN_ABS_MM,
            RANDOM_OFFSET_Z_MAX_ABS_MM,
        );
    } else {
        eprintln!("  initial transforms use the stored footprint values");
    }

    let selected: Vec<PathBuf> = footprints.into_iter().take(total).collect();
    let bar = batch_bar(total as u64, "bench", args.jsonl);
    let records: Vec<EvalRecord> = selected
        .par_iter()
        .map(|path| {
            let record = evaluate_one(
                path,
                args.mode,
                args.randomize_initial_transform,
                args.initial_transform_seed,
            );
            if !args.jsonl {
                bar.set_message(format!(
                    "{:<6} {}",
                    record.status,
                    display_path(path, &args.paths)
                ));
            }
            bar.inc(1);
            record
        })
        .collect();
    bar.finish_and_clear();

    let mut stats = BenchStats::default();
    let mut by_kind: BTreeMap<&'static str, BenchStats> = BTreeMap::new();
    let mut choices = ChoiceStats::default();
    for record in &records {
        stats.add(record);
        choices.add(record);
        if let Some(kind) = record.footprint_kind {
            by_kind.entry(kind).or_default().add(record);
        }
        if args.jsonl {
            println!("{}", serde_json::to_string(record)?);
        }
    }

    let inferred = stats.inferred();
    let pass = stats.pass;
    let fail = stats.fail;
    let skip = stats.skip;
    let error = stats.error;
    let pass_rate = stats.pass_rate();
    let reward_score = stats.reward_score();
    let exact_rotation_rate = rate(stats.exact_rotation, inferred);
    let p95_offset_l_inf_mm = quantile_nearest_rank(&stats.offset_l_inf_errors_mm, 0.95);
    let by_footprint_kind = by_kind
        .iter()
        .map(|(kind, stats)| ((*kind).to_string(), stats.to_json()))
        .collect::<serde_json::Map<_, _>>();
    let summary = serde_json::json!({
        "kind": "summary",
        "mode": mode_label,
        "kind_filter": kind_label,
        "tolerance_mm": tolerance,
        "randomized_initial_transform": args.randomize_initial_transform,
        "initial_transform_seed": args.initial_transform_seed,
        "total": total,
        "inferred": inferred,
        "pass": pass,
        "fail": fail,
        "skip": skip,
        "error": error,
        "pass_rate": pass_rate,
        "reward_score": reward_score,
        "exact_rotation_rate": exact_rotation_rate,
        "z_rotation_rate": rate(stats.z_rotation, inferred),
        "rotation_mismatch_rate": rate(stats.mismatch_rotation, inferred),
        "mean_offset_l_inf_mm": mean(&stats.offset_l_inf_errors_mm),
        "median_offset_l_inf_mm": quantile_nearest_rank(&stats.offset_l_inf_errors_mm, 0.50),
        "p95_offset_l_inf_mm": p95_offset_l_inf_mm,
        "mean_xy_l_inf_mm": mean(&stats.xy_l_inf_errors_mm),
        "median_xy_l_inf_mm": quantile_nearest_rank(&stats.xy_l_inf_errors_mm, 0.50),
        "p95_xy_l_inf_mm": quantile_nearest_rank(&stats.xy_l_inf_errors_mm, 0.95),
        "mean_z_diff_mm": mean(&stats.z_diff_errors_mm),
        "median_z_diff_mm": quantile_nearest_rank(&stats.z_diff_errors_mm, 0.50),
        "p95_z_diff_mm": quantile_nearest_rank(&stats.z_diff_errors_mm, 0.95),
        "by_footprint_kind": by_footprint_kind,
        "choices": &choices,
    });
    if args.jsonl {
        println!("{}", summary);
    } else {
        // Machine-readable metric line on stdout for autoresearch / CI.
        // Skipped in --jsonl mode to keep the stdout stream valid JSONL
        // (the summary record already contains pass_rate).
        println!("METRIC pass_rate={pass_rate:.6}");
        println!("METRIC reward_score={reward_score:.6}");
        println!("METRIC exact_rotation_rate={exact_rotation_rate:.6}");
        println!("METRIC p95_offset_l_inf_mm={p95_offset_l_inf_mm:.6}");
        for (kind, stats) in &by_kind {
            println!("METRIC {kind}_pass_rate={:.6}", stats.pass_rate());
            println!("METRIC {kind}_reward_score={:.6}", stats.reward_score());
            println!(
                "METRIC {kind}_p95_offset_l_inf_mm={:.6}",
                quantile_nearest_rank(&stats.offset_l_inf_errors_mm, 0.95)
            );
        }
    }

    let pct = |num: usize, denom: usize| {
        if denom == 0 {
            0.0
        } else {
            100.0 * (num as f64) / (denom as f64)
        }
    };
    eprintln!(
        "rectify bench summary ({mode_label}, kind={kind_label}): pass={pass}/{inferred} ({:.2}%) \
         fail={fail} skip={skip} error={error} reward={reward_score:.4} \
         exact_rot={:.2}% p95_l_inf={p95_offset_l_inf_mm:.3} mm",
        pct(pass, inferred),
        exact_rotation_rate * 100.0,
    );
    for (kind, stats) in &by_kind {
        let inferred = stats.inferred();
        eprintln!(
            "  {kind}: pass={}/{} ({:.2}%) fail={} skip={} error={} reward={:.4}",
            stats.pass,
            inferred,
            pct(stats.pass, inferred),
            stats.fail,
            stats.skip,
            stats.error,
            stats.reward_score(),
        );
    }
    log_choice_counts("pipeline", &choices.pipeline);
    log_choice_counts("translation_source", &choices.translation_source);
    log_choice_counts("threshold_mm", &choices.threshold_mm);
    log_choice_counts("rotation_match", &choices.rotation_match);
    log_nested_choice_counts(
        "translation_source_status",
        &choices.status_by_translation_source,
    );
    Ok(())
}

fn log_choice_counts(label: &str, counts: &BTreeMap<String, usize>) {
    if counts.is_empty() {
        return;
    }
    eprintln!("  {label} choices:");
    for (key, count) in counts {
        eprintln!("    {key}: {count}");
    }
}

fn log_nested_choice_counts(label: &str, counts: &BTreeMap<String, BTreeMap<String, usize>>) {
    if counts.is_empty() {
        return;
    }
    eprintln!("  {label} choices:");
    for (key, inner) in counts {
        let parts: Vec<String> = inner
            .iter()
            .map(|(status, count)| format!("{status}={count}"))
            .collect();
        eprintln!("    {key}: {}", parts.join(", "));
    }
}

fn rate(num: usize, denom: usize) -> f64 {
    if denom == 0 {
        0.0
    } else {
        (num as f64) / (denom as f64)
    }
}

fn rate_f64(num: f64, denom: usize) -> f64 {
    if denom == 0 {
        0.0
    } else {
        num / (denom as f64)
    }
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / (values.len() as f64)
    }
}

fn quantile_nearest_rank(values: &[f64], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let q = q.clamp(0.0, 1.0);
    let rank = ((sorted.len() as f64) * q).ceil().max(1.0) as usize - 1;
    sorted[rank]
}

fn rotation_credit(rotation: RotationMatch) -> f64 {
    match rotation {
        RotationMatch::Exact => 1.0,
        RotationMatch::ZRotation => 0.5,
        RotationMatch::Mismatch => 0.0,
    }
}

fn offset_credit(l_inf_mm: f64, tolerance_mm: f64) -> f64 {
    1.0 / (1.0 + l_inf_mm / tolerance_mm.max(f64::EPSILON))
}

fn reward_score(rotation: RotationMatch, l_inf_mm: f64, tolerance_mm: f64) -> f64 {
    0.5 * rotation_credit(rotation) + 0.5 * offset_credit(l_inf_mm, tolerance_mm)
}

fn display_path(path: &Path, roots: &[PathBuf]) -> String {
    for root in roots {
        if let Ok(rel) = path.strip_prefix(root) {
            return rel.display().to_string();
        }
    }
    path.display().to_string()
}

pub(crate) fn evaluate_one(
    path: &Path,
    mode: BenchMode,
    randomize_initial_transform: bool,
    initial_transform_seed: u64,
) -> EvalRecord {
    let rel = path.display().to_string();
    EvalRecord {
        path: rel,
        ..try_evaluate(
            path,
            mode,
            randomize_initial_transform,
            initial_transform_seed,
        )
    }
}

fn empty_record(status: &'static str) -> EvalRecord {
    EvalRecord {
        path: String::new(),
        status,
        footprint_kind: None,
        pipeline: None,
        model_path: None,
        predicted_rotate: None,
        predicted_offset: None,
        predicted_score: None,
        predicted_threshold_mm: None,
        translation_source: None,
        initial_rotate: None,
        initial_offset: None,
        repo_rotate: None,
        repo_offset: None,
        offset_l_inf_mm: None,
        xy_l_inf_mm: None,
        z_diff_mm: None,
        rotation_match: None,
        reward_score: None,
        error: None,
    }
}

fn try_evaluate(
    path: &Path,
    mode: BenchMode,
    randomize_initial_transform: bool,
    initial_transform_seed: u64,
) -> EvalRecord {
    let mut fp: FootprintData = match footprint::parse(path) {
        Ok(f) => f,
        Err(err) => {
            return EvalRecord {
                error: Some(format!("{err:#}")),
                ..empty_record("error")
            };
        }
    };
    let footprint_kind = fp.footprint_kind();
    let footprint_kind_label = footprint_kind.label();
    let pipeline = pipeline_label(footprint_kind, None);
    let model_spec = match fp.require_model() {
        Ok(m) => m.clone(),
        Err(_) => {
            return EvalRecord {
                footprint_kind: Some(footprint_kind_label),
                pipeline: Some(pipeline),
                ..empty_record("skip")
            };
        }
    };
    let repo_rotate = model_spec.rotate;
    let repo_offset = model_spec.offset;
    let model_path = model_spec.path.clone();
    let initial_transform = if randomize_initial_transform {
        randomized_initial_transform(path, &fp, repo_rotate, repo_offset, initial_transform_seed)
    } else {
        InitialTransform {
            rotate: repo_rotate,
            offset: repo_offset,
        }
    };
    if let Some(model) = fp.model.as_mut() {
        model.rotate = initial_transform.rotate;
        model.offset = initial_transform.offset;
    }
    let best = match solver::solve_best(&fp) {
        Ok(best) => best,
        Err(err) => {
            return EvalRecord {
                footprint_kind: Some(footprint_kind_label),
                pipeline: Some(pipeline),
                model_path: Some(model_path.clone()),
                error: Some(format!("{err:#}")),
                ..empty_record("error")
            };
        }
    };
    let predicted = best.pose;
    let pipeline = pipeline_label(footprint_kind, Some(best.translation_source.as_str()));
    let predicted_offset = [best.translation[0], -best.translation[1], best.z_offset];
    let raw_rotation = classify_rotation(predicted, repo_rotate);
    let rotation = effective_rotation_match(&fp, predicted, repo_rotate, raw_rotation);

    // L∞ across all three axes, uniform tolerance. In loose mode, Z-rotation
    // equivalent poses are accepted, so compare the offset in the best
    // equivalent Z frame rather than penalizing a valid 90/180/270-degree
    // in-plane representation.
    let offset_errors = offset_errors(predicted_offset, repo_offset, rotation, mode);
    let l_inf = offset_errors.l_inf_mm;
    let xy_l_inf = offset_errors.xy_l_inf_mm;
    let z_diff = offset_errors.z_diff_mm;
    let rotation_ok = if mode.allows_z_rotation() {
        rotation.is_equivalent()
    } else {
        rotation == RotationMatch::Exact
    };
    let offset_ok = l_inf <= mode.tolerance_mm();
    let status = if rotation_ok && offset_ok {
        "pass"
    } else {
        "fail"
    };
    let reward = reward_score(rotation, l_inf, mode.tolerance_mm());

    EvalRecord {
        path: String::new(),
        status,
        footprint_kind: Some(footprint_kind_label),
        pipeline: Some(pipeline),
        model_path: Some(model_path),
        predicted_rotate: Some([predicted.x, predicted.y, predicted.z]),
        predicted_offset: Some(predicted_offset),
        predicted_score: Some(best.score),
        predicted_threshold_mm: Some(best.threshold_mm),
        translation_source: Some(best.translation_source),
        initial_rotate: Some([
            initial_transform.rotate.x,
            initial_transform.rotate.y,
            initial_transform.rotate.z,
        ]),
        initial_offset: Some(initial_transform.offset),
        repo_rotate: Some([repo_rotate.x, repo_rotate.y, repo_rotate.z]),
        repo_offset: Some(repo_offset),
        offset_l_inf_mm: Some(l_inf),
        xy_l_inf_mm: Some(xy_l_inf),
        z_diff_mm: Some(z_diff),
        rotation_match: Some(match rotation {
            RotationMatch::Exact => "exact",
            RotationMatch::ZRotation => "z_rotation",
            RotationMatch::Mismatch => "mismatch",
        }),
        reward_score: Some(reward),
        error: None,
    }
}

fn effective_rotation_match(
    fp: &FootprintData,
    predicted: EulerPose,
    repo: EulerPose,
    rotation: RotationMatch,
) -> RotationMatch {
    if rotation != RotationMatch::ZRotation {
        return rotation;
    }
    let Some(delta_z) = z_rotation_delta(predicted, repo) else {
        return rotation;
    };
    if physical_drills_allow_z_rotation(fp, delta_z) {
        rotation
    } else {
        RotationMatch::Mismatch
    }
}

fn z_rotation_delta(predicted: EulerPose, repo: EulerPose) -> Option<i32> {
    let predicted_key = matrix_key_for_pose(predicted, KICAD_ROTATION_ORDER, KICAD_ROTATION_SIGNS);
    for &delta_z in &[90, 180, 270] {
        let rotated = EulerPose::new(repo.x, repo.y, repo.z + delta_z);
        let rotated_key = matrix_key_for_pose(rotated, KICAD_ROTATION_ORDER, KICAD_ROTATION_SIGNS);
        if predicted_key == rotated_key {
            return Some(delta_z);
        }
    }
    None
}

fn physical_drills_allow_z_rotation(fp: &FootprintData, delta_z: i32) -> bool {
    if fp.physical_drills.len() <= fp.connected_holes.len() {
        return true;
    }
    drill_pattern_allows_z_rotation(&fp.physical_drills, delta_z)
}

fn drill_pattern_allows_z_rotation(drills: &[PadShape], delta_z: i32) -> bool {
    const CENTER_TOL_MM: f64 = 0.05;
    const SIZE_TOL_MM: f64 = 0.05;

    if drills.len() <= 1 {
        return true;
    }
    let count = drills.len() as f64;
    let cx = drills.iter().map(|p| p.at[0]).sum::<f64>() / count;
    let cy = drills.iter().map(|p| p.at[1]).sum::<f64>() / count;
    let angle = (delta_z as f64).to_radians();
    let (sin_a, cos_a) = angle.sin_cos();
    let mut used = vec![false; drills.len()];

    for drill in drills {
        let x = drill.at[0] - cx;
        let y = drill.at[1] - cy;
        let rx = cx + cos_a * x - sin_a * y;
        let ry = cy + sin_a * x + cos_a * y;

        let mut matched = None;
        for (idx, candidate) in drills.iter().enumerate() {
            if used[idx] {
                continue;
            }
            if !drill_shapes_match_under_z_rotation(drill, candidate, delta_z, SIZE_TOL_MM) {
                continue;
            }
            let dist = (candidate.at[0] - rx)
                .abs()
                .max((candidate.at[1] - ry).abs());
            if dist <= CENTER_TOL_MM {
                matched = Some(idx);
                break;
            }
        }
        let Some(idx) = matched else {
            return false;
        };
        used[idx] = true;
    }
    true
}

fn drill_shapes_match_under_z_rotation(
    source: &PadShape,
    candidate: &PadShape,
    delta_z: i32,
    tol_mm: f64,
) -> bool {
    if source.kind != candidate.kind {
        return false;
    }
    let mut source_size = source.size;
    if matches!(
        source.kind,
        PadKind::Rect | PadKind::RoundRect | PadKind::Trapezoid | PadKind::Oval
    ) && delta_z.rem_euclid(180) != 0
    {
        source_size = [source.size[1], source.size[0]];
    }
    (source_size[0] - candidate.size[0]).abs() <= tol_mm
        && (source_size[1] - candidate.size[1]).abs() <= tol_mm
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct InitialTransform {
    rotate: EulerPose,
    offset: [f64; 3],
}

fn randomized_initial_transform(
    path: &Path,
    fp: &FootprintData,
    repo_rotate: EulerPose,
    repo_offset: [f64; 3],
    seed: u64,
) -> InitialTransform {
    let key = format!(
        "{}\n{}\n{}\n{}:{}:{}\n{:.6}:{:.6}:{:.6}",
        path.display(),
        fp.name,
        fp.model
            .as_ref()
            .map(|model| model.filename.as_str())
            .unwrap_or(""),
        repo_rotate.x,
        repo_rotate.y,
        repo_rotate.z,
        repo_offset[0],
        repo_offset[1],
        repo_offset[2],
    );
    randomized_initial_transform_for_key(&key, repo_rotate, repo_offset, seed)
}

fn randomized_initial_transform_for_key(
    key: &str,
    repo_rotate: EulerPose,
    repo_offset: [f64; 3],
    seed: u64,
) -> InitialTransform {
    let mut state = fnv1a64_with_seed(key.as_bytes(), seed);
    let mismatched_poses: Vec<EulerPose> = candidate_poses()
        .into_iter()
        .filter(|&pose| classify_rotation(pose, repo_rotate) == RotationMatch::Mismatch)
        .collect();
    let rotate = if mismatched_poses.is_empty() {
        repo_rotate
    } else {
        let idx = (splitmix64(&mut state) as usize) % mismatched_poses.len();
        mismatched_poses[idx]
    };
    let offset = [
        repo_offset[0]
            + signed_delta(
                &mut state,
                RANDOM_OFFSET_XY_MIN_ABS_MM,
                RANDOM_OFFSET_XY_MAX_ABS_MM,
            ),
        repo_offset[1]
            + signed_delta(
                &mut state,
                RANDOM_OFFSET_XY_MIN_ABS_MM,
                RANDOM_OFFSET_XY_MAX_ABS_MM,
            ),
        repo_offset[2]
            + signed_delta(
                &mut state,
                RANDOM_OFFSET_Z_MIN_ABS_MM,
                RANDOM_OFFSET_Z_MAX_ABS_MM,
            ),
    ];
    InitialTransform { rotate, offset }
}

fn fnv1a64_with_seed(bytes: &[u8], seed: u64) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ seed;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn unit_f64(state: &mut u64) -> f64 {
    const SCALE: f64 = 1.0 / ((1u64 << 53) as f64);
    ((splitmix64(state) >> 11) as f64) * SCALE
}

fn signed_delta(state: &mut u64, min_abs: f64, max_abs: f64) -> f64 {
    let sign = if splitmix64(state) & 1 == 0 {
        -1.0
    } else {
        1.0
    };
    sign * (min_abs + unit_f64(state) * (max_abs - min_abs))
}

fn pipeline_label(footprint_kind: FootprintKind, translation_source: Option<&str>) -> &'static str {
    match footprint_kind {
        FootprintKind::SmdOnly => "smd_contact",
        FootprintKind::Mixed => "mixed_hole_align",
        FootprintKind::ThtOnly => {
            if translation_source == Some("tht_pin_island_fft") {
                "tht_pin_island"
            } else {
                "tht_hole_fallback"
            }
        }
        FootprintKind::Other => "other",
    }
}

fn l_infinity(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dx = (a[0] - b[0]).abs();
    let dy = (a[1] - b[1]).abs();
    let dz = (a[2] - b[2]).abs();
    dx.max(dy).max(dz)
}

fn xy_l_infinity(a: [f64; 3], b: [f64; 3]) -> f64 {
    let dx = (a[0] - b[0]).abs();
    let dy = (a[1] - b[1]).abs();
    dx.max(dy)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct OffsetErrors {
    pub(crate) l_inf_mm: f64,
    pub(crate) xy_l_inf_mm: f64,
    pub(crate) z_diff_mm: f64,
}

pub(crate) fn offset_errors(
    predicted: [f64; 3],
    repo: [f64; 3],
    rotation: RotationMatch,
    mode: BenchMode,
) -> OffsetErrors {
    let candidates: &[i32] = if mode.allows_z_rotation() && rotation == RotationMatch::ZRotation {
        &[0, 90, 180, 270]
    } else {
        &[0]
    };
    candidates
        .iter()
        .map(|&z| {
            let rotated = rotate_offset_z(predicted, z);
            OffsetErrors {
                l_inf_mm: l_infinity(rotated, repo),
                xy_l_inf_mm: xy_l_infinity(rotated, repo),
                z_diff_mm: (rotated[2] - repo[2]).abs(),
            }
        })
        .min_by(|a, b| {
            a.l_inf_mm
                .partial_cmp(&b.l_inf_mm)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(OffsetErrors {
            l_inf_mm: l_infinity(predicted, repo),
            xy_l_inf_mm: xy_l_infinity(predicted, repo),
            z_diff_mm: (predicted[2] - repo[2]).abs(),
        })
}

fn rotate_offset_z(offset: [f64; 3], degrees: i32) -> [f64; 3] {
    match degrees.rem_euclid(360) {
        90 => [-offset[1], offset[0], offset[2]],
        180 => [-offset[0], -offset[1], offset[2]],
        270 => [offset[1], -offset[0], offset[2]],
        _ => offset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reward_score_gives_partial_credit_before_pass_threshold() {
        let reward = reward_score(RotationMatch::Exact, 0.15, STRICT_TOLERANCE_MM);
        assert!(reward > 0.5, "expected smooth partial credit, got {reward}");
        assert!(
            reward < 1.0,
            "reward should remain below perfect, got {reward}"
        );
    }

    #[test]
    fn quantile_nearest_rank_uses_sorted_offsets() {
        let values = [0.4, 0.1, 0.2, 0.3];
        assert_eq!(quantile_nearest_rank(&values, 0.50), 0.2);
        assert_eq!(quantile_nearest_rank(&values, 0.95), 0.4);
    }

    #[test]
    fn randomized_initial_transform_is_stable_and_not_the_answer_key() {
        let repo_rotate = EulerPose::new(0, 0, 0);
        let repo_offset = [1.0, 2.0, 3.0];
        let first =
            randomized_initial_transform_for_key("same-footprint", repo_rotate, repo_offset, 7);
        let second =
            randomized_initial_transform_for_key("same-footprint", repo_rotate, repo_offset, 7);

        assert_eq!(first, second);
        assert_eq!(
            classify_rotation(first.rotate, repo_rotate),
            RotationMatch::Mismatch
        );
        assert!(xy_l_infinity(first.offset, repo_offset) >= RANDOM_OFFSET_XY_MIN_ABS_MM);
        assert!((first.offset[2] - repo_offset[2]).abs() >= RANDOM_OFFSET_Z_MIN_ABS_MM);
    }

    #[test]
    fn randomized_initial_transform_changes_with_seed() {
        let repo_rotate = EulerPose::new(0, 0, 0);
        let repo_offset = [1.0, 2.0, 3.0];
        let first =
            randomized_initial_transform_for_key("same-footprint", repo_rotate, repo_offset, 7);
        let second =
            randomized_initial_transform_for_key("same-footprint", repo_rotate, repo_offset, 8);

        assert_ne!(first, second);
    }

    #[test]
    fn pipeline_label_distinguishes_footprint_family_paths() {
        assert_eq!(
            pipeline_label(FootprintKind::SmdOnly, Some("fft_pad")),
            "smd_contact"
        );
        assert_eq!(
            pipeline_label(FootprintKind::Mixed, Some("hole_align")),
            "mixed_hole_align"
        );
        assert_eq!(
            pipeline_label(FootprintKind::ThtOnly, Some("tht_pin_island_fft")),
            "tht_pin_island"
        );
        assert_eq!(
            pipeline_label(FootprintKind::ThtOnly, Some("hole_align")),
            "tht_hole_fallback"
        );
    }

    #[test]
    fn loose_z_rotation_offset_error_uses_best_equivalent_xy_frame() {
        let errors = offset_errors(
            [-8.75, 0.87, 0.65],
            [8.77, -0.84, 0.65],
            RotationMatch::ZRotation,
            BenchMode::Loose,
        );

        assert!(errors.l_inf_mm < 0.04, "got {errors:?}");
    }

    #[test]
    fn strict_z_rotation_offset_error_uses_raw_xy_frame() {
        let errors = offset_errors(
            [-8.75, 0.87, 0.65],
            [8.77, -0.84, 0.65],
            RotationMatch::ZRotation,
            BenchMode::Strict,
        );

        assert!(errors.l_inf_mm > 10.0, "got {errors:?}");
    }

    fn circular_drill(x: f64, y: f64) -> PadShape {
        PadShape {
            kind: PadKind::Circle,
            at: [x, y],
            size: [1.02, 1.02],
            angle_deg: 0.0,
        }
    }

    #[test]
    fn asymmetric_physical_drills_break_z_rotation_equivalence() {
        let drills = [
            circular_drill(0.0, 0.0),
            circular_drill(0.0, -3.0),
            circular_drill(-3.0, -3.94),
            circular_drill(3.0, -3.94),
        ];

        assert!(!drill_pattern_allows_z_rotation(&drills, 180));
    }

    #[test]
    fn symmetric_physical_drills_allow_z_rotation_equivalence() {
        let drills = [
            circular_drill(-1.0, -1.0),
            circular_drill(1.0, -1.0),
            circular_drill(-1.0, 1.0),
            circular_drill(1.0, 1.0),
        ];

        assert!(drill_pattern_allows_z_rotation(&drills, 180));
    }
}
