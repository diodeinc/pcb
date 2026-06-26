//! `pcb rectify check` / `pcb rectify fix` — porcelain output for benchmark failures.
//!
//! Failure tiers (most to least severe):
//!   1. `rotation_mismatch`  — predicted rotation fails the selected audit mode
//!   2. `offset_mismatch`    — rotation passes, L∞ offset fails selected mode
//!
//! Default output groups failures by tier with up to `--top N` examples each.
//! `--jsonl` emits flagged audit rows, candidate correction records, apply
//! error records, and a trailing machine-readable summary for batch review
//! tooling.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use serde::Serialize;

use crate::bench::{self, BenchMode};
use crate::footprint::{self, FootprintKind};
use crate::fs_util;
use crate::patch;
use crate::pose::EulerPose;
use crate::progress::batch_bar;

pub struct Args {
    pub paths: Vec<PathBuf>,
    pub kind: AuditKindFilter,
    pub limit: Option<usize>,
    pub jobs: Option<usize>,
    pub jsonl: bool,
    pub top: usize,
    pub apply: bool,
    pub fail_on_flagged: bool,
    pub mode: BenchMode,
    pub randomize_initial_transform: bool,
    pub initial_transform_seed: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditKindFilter {
    All,
    Smd,
    Tht,
    Mixed,
}

impl AuditKindFilter {
    fn label(self) -> &'static str {
        match self {
            AuditKindFilter::All => "all",
            AuditKindFilter::Smd => "smd",
            AuditKindFilter::Tht => "tht",
            AuditKindFilter::Mixed => "mixed",
        }
    }

    fn matches(self, kind: FootprintKind) -> bool {
        match self {
            AuditKindFilter::All => true,
            AuditKindFilter::Smd => kind == FootprintKind::SmdOnly,
            AuditKindFilter::Tht => kind == FootprintKind::ThtOnly,
            AuditKindFilter::Mixed => kind == FootprintKind::Mixed,
        }
    }
}

struct TierInfo {
    key: &'static str,
    label: &'static str,
    description: &'static str,
}

const TIER_ORDER: &[TierInfo] = &[
    TierInfo {
        key: "rotation_mismatch",
        label: "ROTATION MISMATCH",
        description: "benchmark failed because rotation does not satisfy the selected mode",
    },
    TierInfo {
        key: "offset_mismatch",
        label: "OFFSET MISMATCH",
        description: "benchmark failed because L∞ offset exceeds the selected tolerance",
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Verdict {
    Ok,
    RotationMismatch,
    OffsetMismatch,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::RotationMismatch => "rotation_mismatch",
            Verdict::OffsetMismatch => "offset_mismatch",
        }
    }
}

#[derive(Debug, Serialize)]
struct AuditRecord {
    kind: &'static str,
    path: String,
    verdict: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    footprint_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pipeline: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    predicted_rotate: Option<[i32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    predicted_offset: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    predicted_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    predicted_threshold_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    translation_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    initial_rotate: Option<[i32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    initial_offset: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_rotate: Option<[i32; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repo_offset: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset_l_inf_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    xy_l_inf_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    z_diff_mm: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rotation_match: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reward_score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct CandidateCorrectionRecord {
    kind: &'static str,
    path: String,
    verdict: &'static str,
    footprint_kind: &'static str,
    model_path: String,
    current_rotate: [i32; 3],
    current_offset: [f64; 3],
    suggested_rotate: [i32; 3],
    suggested_offset: [f64; 3],
    predicted_score: f64,
    xy_l_inf_mm: f64,
    z_diff_mm: f64,
}

#[derive(Debug, Serialize)]
struct ApplyRecord {
    kind: &'static str,
    path: String,
    verdict: &'static str,
    footprint_kind: &'static str,
    model_path: String,
    suggested_rotate: [i32; 3],
    suggested_offset: [f64; 3],
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct AuditSummaryRecord {
    kind: &'static str,
    kind_filter: &'static str,
    mode: &'static str,
    tolerance_mm: f64,
    randomized_initial_transform: bool,
    initial_transform_seed: u64,
    total: usize,
    ok: usize,
    flagged: usize,
    skipped: usize,
    errors: usize,
    candidate_corrections: usize,
    applied_corrections: usize,
    apply_errors: usize,
    by_verdict: std::collections::BTreeMap<String, usize>,
}

pub fn run(args: Args) -> Result<()> {
    if args.paths.is_empty() {
        bail!("pcb rectify requires at least one path (file or directory)");
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
    if args.kind != AuditKindFilter::All {
        footprints.retain(|path| match footprint::parse(path) {
            Ok(fp) => args.kind.matches(fp.footprint_kind()),
            // Keep malformed footprints in filtered runs so they are reported
            // as errors rather than silently disappearing from the audit.
            Err(_) => true,
        });
    }

    let total = args.limit.unwrap_or(footprints.len()).min(footprints.len());
    let selected: Vec<PathBuf> = footprints.into_iter().take(total).collect();
    let single_human_output = selected.len() == 1 && !args.jsonl;
    let kind_label = args.kind.label();
    let mode_label = args.mode.label();
    let tolerance_mm = args.mode.tolerance_mm();
    if !single_human_output {
        eprintln!(
            "pcb rectify (kind={kind_label}, mode={mode_label}): scanning {total} footprints"
        );
        if args.randomize_initial_transform {
            eprintln!(
                "  evaluator uses randomized initial transforms with seed={}",
                args.initial_transform_seed
            );
        } else {
            eprintln!("  evaluator uses stored footprint transforms as initial transforms");
        }
    }

    let bar = batch_bar(total as u64, "audit", args.jsonl);
    let mut records: Vec<AuditRecord> = selected
        .par_iter()
        .map(|p| {
            let rec = evaluate_one(
                p,
                args.mode,
                args.randomize_initial_transform,
                args.initial_transform_seed,
            );
            bar.inc(1);
            rec
        })
        .collect();
    if single_human_output && let Some(record) = records.first_mut() {
        normalize_single_skip(record);
    }
    bar.finish_and_clear();

    let mut ok_count = 0usize;
    let mut skips = 0usize;
    let mut errors = 0usize;
    let mut tiers: std::collections::BTreeMap<&str, Vec<&AuditRecord>> =
        std::collections::BTreeMap::new();
    let mut verdict_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    let candidate_corrections: Vec<CandidateCorrectionRecord> = records
        .iter()
        .filter_map(candidate_correction_record)
        .collect();
    let apply_records = if args.apply {
        apply_candidate_corrections(&candidate_corrections)
    } else {
        Vec::new()
    };
    let applied_corrections = apply_records
        .iter()
        .filter(|rec| rec.status == "applied")
        .count();
    let apply_errors = apply_records
        .iter()
        .filter(|rec| rec.status == "error")
        .count();

    for record in &records {
        match record.verdict {
            "ok" => ok_count += 1,
            "skip" => skips += 1,
            "error" => errors += 1,
            _ => {
                tiers.entry(record.verdict).or_default().push(record);
                *verdict_counts
                    .entry(record.verdict.to_string())
                    .or_insert(0) += 1;
            }
        }
    }
    let flagged: usize = tiers.values().map(|v| v.len()).sum();
    let summary = AuditSummaryRecord {
        kind: "summary",
        kind_filter: kind_label,
        mode: mode_label,
        tolerance_mm,
        randomized_initial_transform: args.randomize_initial_transform,
        initial_transform_seed: args.initial_transform_seed,
        total,
        ok: ok_count,
        flagged,
        skipped: skips,
        errors,
        candidate_corrections: candidate_corrections.len(),
        applied_corrections,
        apply_errors,
        by_verdict: verdict_counts,
    };

    if args.jsonl {
        for record in &records {
            match record.verdict {
                "ok" | "skip" | "error" => {}
                _ => println!("{}", serde_json::to_string(record)?),
            }
        }
        for correction in &candidate_corrections {
            println!("{}", serde_json::to_string(correction)?);
        }
        for apply_record in &apply_records {
            if apply_record.status == "error" {
                println!("{}", serde_json::to_string(apply_record)?);
            }
        }
        println!("{}", serde_json::to_string(&summary)?);
    } else if single_human_output {
        if let Some(record) = records.first() {
            print_single_result(record, &apply_records, args.apply);
        }
    } else {
        for tier in TIER_ORDER {
            let Some(examples) = tiers.get(tier.key) else {
                continue;
            };
            let count = examples.len();
            let limit = if args.top == 0 { count } else { args.top };
            eprintln!("\n{} ({count}) — {}", tier.label, tier.description);
            for rec in examples.iter().take(limit) {
                println!("  {}  {}", rec.path, format_detail(rec));
            }
            if count > limit {
                eprintln!("  ... and {} more", count - limit);
            }
        }

        if args.apply && apply_errors > 0 {
            eprintln!("\nAPPLY ERRORS ({apply_errors})");
            for rec in &apply_records {
                if rec.status == "error" {
                    println!(
                        "  error {}  {}",
                        rec.path,
                        rec.error.as_deref().unwrap_or("unknown error"),
                    );
                }
            }
        }

        eprintln!();
        eprintln!(
            "  {total:>5} scanned    {ok:>5} ok    {flagged:>5} flagged    {skips:>5} skipped    {errors:>5} errors    {applied_corrections:>5} applied",
            ok = ok_count,
        );
    }
    if single_human_output {
        if apply_errors > 0 || errors > 0 || (args.fail_on_flagged && flagged > 0) {
            std::process::exit(1);
        }
        return Ok(());
    }
    if apply_errors > 0 {
        bail!("failed to apply {apply_errors} audit correction(s)");
    }
    if args.fail_on_flagged && (flagged > 0 || errors > 0) {
        bail!("rectify check found {flagged} flagged footprint(s) and {errors} error(s)");
    }
    Ok(())
}

fn normalize_single_skip(record: &mut AuditRecord) {
    if record.verdict != "skip" {
        return;
    }

    match footprint::parse(Path::new(&record.path)) {
        Ok(fp) if fp.require_model().is_err() => {
            record.error = Some(single_skip_reason(Path::new(&record.path)));
        }
        Ok(_) => {
            record.verdict = "error";
            record.error = Some("solver skipped footprint despite a usable STEP model".into());
        }
        Err(err) => {
            record.verdict = "error";
            record.error = Some(format!("{err:#}"));
        }
    }
}

fn single_skip_reason(path: &Path) -> String {
    match std::fs::read_to_string(path) {
        Ok(content) if has_model_block(&content) => ".wrl models unsupported".into(),
        _ => "no 3D model".into(),
    }
}

fn has_model_block(content: &str) -> bool {
    let bytes = content.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'(' {
            i += 1;
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i..i + 5) == Some(b"model")
            && bytes
                .get(i + 5)
                .is_none_or(|b| b.is_ascii_whitespace() || *b == b')')
        {
            return true;
        }
    }
    false
}

fn print_single_result(record: &AuditRecord, apply_records: &[ApplyRecord], apply: bool) {
    match record.verdict {
        "ok" => println!("{} OK", record.path),
        "skip" => println!(
            "{} SKIP: {}",
            record.path,
            record.error.as_deref().unwrap_or("no 3D model")
        ),
        "error" => println!(
            "{} Error: {}",
            record.path,
            record.error.as_deref().unwrap_or("solver failed")
        ),
        verdict if apply => {
            let apply_record = apply_records.iter().find(|rec| rec.path == record.path);
            match apply_record.map(|rec| rec.status) {
                Some("applied") => println!(
                    "{} OK: applied correction for {}: {}",
                    record.path,
                    verdict,
                    format_detail(record)
                ),
                Some("error") => {
                    let error = apply_record
                        .and_then(|rec| rec.error.as_deref())
                        .unwrap_or("failed to apply correction");
                    println!(
                        "{} Error: failed to apply correction for {}: {}",
                        record.path, verdict, error
                    );
                }
                _ => println!(
                    "{} Error: {}: {}; no correction candidate available",
                    record.path,
                    verdict,
                    format_detail(record)
                ),
            }
        }
        verdict => println!(
            "{} Error: {}: {}",
            record.path,
            verdict,
            format_detail(record)
        ),
    }
}

fn format_detail(rec: &AuditRecord) -> String {
    match rec.verdict {
        "rotation_mismatch" => {
            let pred = rec
                .predicted_rotate
                .map(|r| format!("({},{},{})", r[0], r[1], r[2]))
                .unwrap_or_default();
            let repo = rec
                .repo_rotate
                .map(|r| format!("({},{},{})", r[0], r[1], r[2]))
                .unwrap_or_default();
            format!("predicted={pred}  stored={repo}")
        }
        _ => {
            let l_inf = rec.offset_l_inf_mm.unwrap_or(0.0);
            let xy = rec.xy_l_inf_mm.unwrap_or(0.0);
            let z = rec.z_diff_mm.unwrap_or(0.0);
            format!("L∞={l_inf:.2} mm  Δxy={xy:.2} mm  Δz={z:.2} mm")
        }
    }
}

fn candidate_correction_record(rec: &AuditRecord) -> Option<CandidateCorrectionRecord> {
    if matches!(rec.verdict, "ok" | "skip" | "error") {
        return None;
    }
    Some(CandidateCorrectionRecord {
        kind: "candidate_correction",
        path: rec.path.clone(),
        verdict: rec.verdict,
        footprint_kind: rec.footprint_kind?,
        model_path: rec.model_path.clone()?,
        current_rotate: rec.repo_rotate?,
        current_offset: rec.repo_offset?,
        suggested_rotate: rec.predicted_rotate?,
        suggested_offset: rec.predicted_offset?,
        predicted_score: rec.predicted_score?,
        xy_l_inf_mm: rec.xy_l_inf_mm?,
        z_diff_mm: rec.z_diff_mm?,
    })
}

fn apply_candidate_corrections(corrections: &[CandidateCorrectionRecord]) -> Vec<ApplyRecord> {
    corrections
        .iter()
        .map(|correction| {
            let rotate = EulerPose::new(
                correction.suggested_rotate[0],
                correction.suggested_rotate[1],
                correction.suggested_rotate[2],
            );
            let result = (|| -> Result<()> {
                let content = std::fs::read_to_string(&correction.path)
                    .with_context(|| format!("read {}", correction.path))?;
                let patched = patch::patch_model_transform(
                    &content,
                    &correction.model_path,
                    rotate,
                    correction.suggested_offset,
                )?;
                std::fs::write(&correction.path, patched)
                    .with_context(|| format!("write {}", correction.path))?;
                Ok(())
            })();
            match result {
                Ok(()) => ApplyRecord {
                    kind: "applied_correction",
                    path: correction.path.clone(),
                    verdict: correction.verdict,
                    footprint_kind: correction.footprint_kind,
                    model_path: correction.model_path.clone(),
                    suggested_rotate: correction.suggested_rotate,
                    suggested_offset: correction.suggested_offset,
                    status: "applied",
                    error: None,
                },
                Err(err) => ApplyRecord {
                    kind: "applied_correction",
                    path: correction.path.clone(),
                    verdict: correction.verdict,
                    footprint_kind: correction.footprint_kind,
                    model_path: correction.model_path.clone(),
                    suggested_rotate: correction.suggested_rotate,
                    suggested_offset: correction.suggested_offset,
                    status: "error",
                    error: Some(format!("{err:#}")),
                },
            }
        })
        .collect()
}

fn evaluate_one(
    path: &Path,
    mode: BenchMode,
    randomize_initial_transform: bool,
    initial_transform_seed: u64,
) -> AuditRecord {
    bench_record_to_audit_record(
        bench::evaluate_one(
            path,
            mode,
            randomize_initial_transform,
            initial_transform_seed,
        ),
        mode,
    )
}

fn bench_record_to_audit_record(record: bench::EvalRecord, mode: BenchMode) -> AuditRecord {
    let verdict = match record.status {
        "pass" => Verdict::Ok.as_str(),
        "fail" => {
            if rotation_satisfies_mode(record.rotation_match, mode) {
                Verdict::OffsetMismatch.as_str()
            } else {
                Verdict::RotationMismatch.as_str()
            }
        }
        "skip" => "skip",
        "error" => "error",
        _ => "error",
    };

    AuditRecord {
        kind: "audit_record",
        path: record.path,
        verdict,
        footprint_kind: record.footprint_kind,
        pipeline: record.pipeline,
        model_path: record.model_path,
        predicted_rotate: record.predicted_rotate,
        predicted_offset: record.predicted_offset,
        predicted_score: record.predicted_score,
        predicted_threshold_mm: record.predicted_threshold_mm,
        translation_source: record.translation_source,
        initial_rotate: record.initial_rotate,
        initial_offset: record.initial_offset,
        repo_rotate: record.repo_rotate,
        repo_offset: record.repo_offset,
        offset_l_inf_mm: record.offset_l_inf_mm,
        xy_l_inf_mm: record.xy_l_inf_mm,
        z_diff_mm: record.z_diff_mm,
        rotation_match: record.rotation_match,
        reward_score: record.reward_score,
        error: record.error,
    }
}

fn rotation_satisfies_mode(rotation_match: Option<&str>, mode: BenchMode) -> bool {
    match rotation_match {
        Some("exact") => true,
        Some("z_rotation") => mode.allows_z_rotation(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flagged_record() -> AuditRecord {
        AuditRecord {
            kind: "audit_record",
            path: "foo.kicad_mod".into(),
            verdict: "offset_mismatch",
            footprint_kind: Some("smd_only"),
            pipeline: Some("smd_contact"),
            model_path: Some("pkg/foo.step".into()),
            predicted_rotate: Some([90, 0, 0]),
            predicted_offset: Some([1.0, 2.0, 3.0]),
            predicted_score: Some(0.9),
            predicted_threshold_mm: Some(0.1),
            translation_source: Some("fft".into()),
            initial_rotate: Some([180, 0, 0]),
            initial_offset: Some([4.0, 5.0, 6.0]),
            repo_rotate: Some([0, 0, 0]),
            repo_offset: Some([0.0, 0.0, 0.0]),
            offset_l_inf_mm: Some(0.25),
            xy_l_inf_mm: Some(0.25),
            z_diff_mm: Some(0.01),
            rotation_match: Some("exact"),
            reward_score: Some(0.8),
            error: None,
        }
    }

    fn eval_record(
        status: &'static str,
        rotation_match: Option<&'static str>,
    ) -> bench::EvalRecord {
        bench::EvalRecord {
            path: "foo.kicad_mod".into(),
            status,
            footprint_kind: Some("smd_only"),
            pipeline: Some("smd_contact"),
            model_path: Some("pkg/foo.step".into()),
            predicted_rotate: Some([0, 0, 90]),
            predicted_offset: Some([1.0, 2.0, 3.0]),
            predicted_score: Some(0.9),
            predicted_threshold_mm: Some(0.1),
            translation_source: Some("fft".into()),
            initial_rotate: Some([180, 0, 0]),
            initial_offset: Some([4.0, 5.0, 6.0]),
            repo_rotate: Some([0, 0, 0]),
            repo_offset: Some([1.0, 2.0, 3.3]),
            offset_l_inf_mm: Some(0.3),
            xy_l_inf_mm: Some(0.0),
            z_diff_mm: Some(0.3),
            rotation_match,
            reward_score: Some(0.7),
            error: None,
        }
    }

    #[test]
    fn candidate_correction_is_emitted_for_fixable_flagged_record() {
        let correction = candidate_correction_record(&flagged_record()).expect("correction");
        assert_eq!(correction.kind, "candidate_correction");
        assert_eq!(correction.model_path, "pkg/foo.step");
        assert_eq!(correction.suggested_rotate, [90, 0, 0]);
    }

    #[test]
    fn candidate_correction_is_not_emitted_for_ok_record() {
        let mut rec = flagged_record();
        rec.verdict = "ok";
        assert!(candidate_correction_record(&rec).is_none());
    }

    #[test]
    fn audit_verdict_comes_from_benchmark_rotation_rules() {
        let loose =
            bench_record_to_audit_record(eval_record("fail", Some("z_rotation")), BenchMode::Loose);
        assert_eq!(loose.verdict, "offset_mismatch");

        let strict = bench_record_to_audit_record(
            eval_record("fail", Some("z_rotation")),
            BenchMode::Strict,
        );
        assert_eq!(strict.verdict, "rotation_mismatch");
    }
}
