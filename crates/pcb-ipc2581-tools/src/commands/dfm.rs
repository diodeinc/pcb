//! PDK-driven manufacturability checks for IPC-2581 geometry.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::LayoutTarget;
use crate::ipc2581::Ipc2581;
use crate::utils::file as file_utils;

mod checks;
mod design;
mod pdk;
mod report;
mod rules;
mod waivers;

#[derive(Debug)]
pub struct CheckOptions {
    pub pdk: PathBuf,
    pub waivers: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub layout_target: LayoutTarget,
}

/// A parsed waiver file plus the identity the report echoes back.
struct LoadedWaivers {
    path: String,
    sha256: String,
    file: waivers::WaiverFile,
}

pub fn execute_check(file: &Path, options: &CheckOptions) -> Result<()> {
    let input_bytes = std::fs::read(file)
        .with_context(|| format!("failed to read IPC-2581 file {}", file.display()))?;
    let pdk_bytes = std::fs::read(&options.pdk)
        .with_context(|| format!("failed to read PDK file {}", options.pdk.display()))?;
    let pdk_source = std::str::from_utf8(&pdk_bytes)
        .with_context(|| format!("PDK file {} is not UTF-8", options.pdk.display()))?;
    let pdk = pdk::Pdk::parse(pdk_source)
        .with_context(|| format!("failed to parse PDK file {}", options.pdk.display()))?;

    let rules = rules::lower(&pdk)
        .with_context(|| format!("failed to lower PDK file {}", options.pdk.display()))?;
    if rules.is_empty() {
        bail!(
            "PDK {} configures no DFM rules; add at least one capability",
            options.pdk.display()
        );
    }

    let waivers = options
        .waivers
        .as_deref()
        .map(|path| -> Result<LoadedWaivers> {
            let bytes = std::fs::read(path)
                .with_context(|| format!("failed to read waiver file {}", path.display()))?;
            let source = std::str::from_utf8(&bytes)
                .with_context(|| format!("waiver file {} is not UTF-8", path.display()))?;
            let file = waivers::WaiverFile::parse(source)
                .with_context(|| format!("failed to parse waiver file {}", path.display()))?;
            Ok(LoadedWaivers {
                path: path.display().to_string(),
                sha256: sha256(&bytes),
                file,
            })
        })
        .transpose()?;

    let generated_at = generation_time();
    let content = file_utils::ipc_text(file, &input_bytes)?;
    let ipc = Ipc2581::parse(&content).context("failed to parse IPC-2581 file")?;
    let design = design::Design::extract(&ipc, &rules, options.layout_target.artwork_scope())?;
    let checked = checks::run(
        &rules,
        &design,
        &ipc,
        waivers.as_ref().map(|loaded| &loaded.file),
        generated_at.date_naive(),
    );

    let summary = summarize(&checked);
    let failed = summary.errors > 0;
    let report = report::DfmReport {
        schema_version: report::REPORT_SCHEMA_VERSION,
        generated_at: generated_at.to_rfc3339(),
        verdict: if failed {
            report::Verdict::Fail
        } else {
            report::Verdict::Pass
        },
        tool: report::ToolIdentity {
            name: "pcb",
            version: env!("CARGO_PKG_VERSION"),
        },
        input: report::FileIdentity {
            path: file.display().to_string(),
            sha256: sha256(&input_bytes),
            size_bytes: input_bytes.len() as u64,
        },
        pdk: report::PdkIdentity::from_pdk(
            &pdk,
            options.pdk.display().to_string(),
            sha256(&pdk_bytes),
        ),
        layout_target: match options.layout_target {
            LayoutTarget::Board => "board",
            LayoutTarget::BoardArray => "board_array",
        },
        coordinate_system: report::CoordinateSystem {
            unit: "mm",
            axes: "x_right_y_up",
            origin: "ipc_2581_design",
        },
        waivers: checked
            .waivers
            .as_ref()
            .zip(waivers.as_ref())
            .map(|(outcome, loaded)| report::WaiversApplied {
                path: loaded.path.clone(),
                sha256: loaded.sha256.clone(),
                applied: outcome.applied,
                expired: outcome.expired.clone(),
                unmatched: outcome.unmatched.clone(),
            }),
        summary,
        rules: checked.rules,
        findings: checked.findings,
    };

    let rendered = serde_json::to_string_pretty(&report)?;
    write_report(options.output.as_deref(), &rendered)?;

    let summary = &report.summary;
    if failed {
        bail!(
            "DFM check failed with {} error finding(s){}",
            summary.errors,
            annotations(summary)
        );
    }
    eprintln!(
        "✓ DFM check passed ({} rule(s){})",
        summary.rules_configured,
        annotations(summary)
    );
    Ok(())
}

/// The non-verdict counts worth surfacing next to the pass/fail line.
fn annotations(summary: &report::Summary) -> String {
    let mut notes = String::new();
    if summary.rules_skipped > 0 {
        notes.push_str(&format!(", {} skipped", summary.rules_skipped));
    }
    if summary.warnings > 0 {
        notes.push_str(&format!(", {} warning(s)", summary.warnings));
    }
    if summary.waived > 0 {
        notes.push_str(&format!(", {} waived", summary.waived));
    }
    notes
}

/// Report generation time, honoring `SOURCE_DATE_EPOCH` so CI reports can be
/// byte-stable.
fn generation_time() -> chrono::DateTime<chrono::Utc> {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|epoch| epoch.parse::<i64>().ok())
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .unwrap_or_else(chrono::Utc::now)
}

fn summarize(checked: &checks::Results) -> report::Summary {
    let status_count = |status: fn(&report::RuleStatus) -> bool| {
        checked
            .rules
            .iter()
            .filter(|rule| status(&rule.status))
            .count()
    };
    report::Summary {
        rules_configured: checked.rules.len(),
        rules_passed: status_count(|status| matches!(status, report::RuleStatus::Pass)),
        rules_warned: status_count(|status| matches!(status, report::RuleStatus::Warning)),
        rules_failed: status_count(|status| matches!(status, report::RuleStatus::Fail)),
        rules_skipped: status_count(|status| matches!(status, report::RuleStatus::Skipped)),
        findings: checked.findings.len(),
        errors: checked
            .findings
            .iter()
            .filter(|finding| !finding.waived && finding.severity == report::Severity::Error)
            .count(),
        warnings: checked
            .findings
            .iter()
            .filter(|finding| !finding.waived && finding.severity == report::Severity::Warning)
            .count(),
        waived: checked
            .findings
            .iter()
            .filter(|finding| finding.waived)
            .count(),
    }
}

fn write_report(output: Option<&Path>, report: &str) -> Result<()> {
    match output {
        Some(path) => std::fs::write(path, format!("{report}\n"))
            .with_context(|| format!("failed to write DFM report to {}", path.display())),
        None => {
            let mut stdout = std::io::stdout().lock();
            writeln!(stdout, "{report}").context("failed to write DFM report to stdout")
        }
    }
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
