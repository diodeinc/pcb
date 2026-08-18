//! PDK-driven manufacturability checks for IPC-2581 geometry.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use sha2::{Digest, Sha256};

use crate::LayoutTarget;
use crate::ipc2581::Ipc2581;
use crate::utils::file as file_utils;

mod checks;
mod design;
mod pdk;
mod report;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DfmReportFormat {
    Json,
}

#[derive(Debug)]
pub struct CheckOptions {
    pub pdk: PathBuf,
    pub output: Option<PathBuf>,
    pub format: DfmReportFormat,
    pub layout_target: LayoutTarget,
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

    let content = file_utils::load_ipc_file(file)?;
    let ipc = Ipc2581::parse(&content).context("failed to parse IPC-2581 file")?;
    let design = design::Design::extract(&ipc, &pdk, options.layout_target.artwork_scope())?;
    let checked = checks::run(&design, &pdk);

    let summary = summarize(&checked);
    let failed = summary.findings > 0;
    let report = report::DfmReport {
        schema_version: report::REPORT_SCHEMA_VERSION,
        generated_at: chrono::Utc::now().to_rfc3339(),
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
        summary,
        rules: checked.rules,
        findings: checked.findings,
    };

    let rendered = match options.format {
        DfmReportFormat::Json => serde_json::to_string_pretty(&report)?,
    };
    write_report(options.output.as_deref(), &rendered)?;

    if failed {
        bail!(
            "DFM check failed with {} finding(s)",
            report.summary.findings
        );
    }
    eprintln!(
        "✓ DFM check passed ({} configured rule(s))",
        report.summary.rules_configured
    );
    Ok(())
}

fn summarize(checked: &checks::Results) -> report::Summary {
    report::Summary {
        rules_configured: checked.rules.len(),
        rules_passed: checked
            .rules
            .iter()
            .filter(|rule| matches!(rule.status, report::RuleStatus::Pass))
            .count(),
        rules_failed: checked
            .rules
            .iter()
            .filter(|rule| matches!(rule.status, report::RuleStatus::Fail))
            .count(),
        rules_skipped: checked
            .rules
            .iter()
            .filter(|rule| matches!(rule.status, report::RuleStatus::Skipped))
            .count(),
        findings: checked.findings.len(),
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
