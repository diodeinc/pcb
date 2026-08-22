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
    let design = design::Design::extract(&ipc, options.layout_target.artwork_scope(), &rules)?;
    let checked = checks::run(
        &rules,
        &design,
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

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use pcb_ir::dialects::ipc::ArtworkScope;

    use super::*;
    use crate::commands::EdgeInsetsMm;
    use crate::commands::board_array::{BoardArrayCreateOptions, create_board_array};
    use crate::commands::fab_panel::{FabPanelSpec, create_fab_panel};

    const BOARD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <LayerRef name="F.Mask"/>
    <LayerRef name="BOTTOM"/>
    <LayerRef name="B.Mask"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
      <Layer name="BOTTOM" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="B.Mask" layerFunction="SOLDERMASK" side="BOTTOM" polarity="POSITIVE"/>
      <Stackup name="Primary" overallThickness="0.07" tolPlus="0" tolMinus="0" whereMeasured="METAL" stackupStatus="PROPOSED">
        <StackupGroup name="Primary_Group" thickness="0.07" tolPlus="0" tolMinus="0">
          <StackupLayer layerOrGroupRef="TOP" thickness="0.035" tolPlus="0" tolMinus="0" sequence="0"/>
          <StackupLayer layerOrGroupRef="BOTTOM" thickness="0.035" tolPlus="0" tolMinus="0" sequence="1"/>
        </StackupGroup>
      </Stackup>
      <Step name="board" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="30" y="0"/>
            <PolyStepSegment x="30" y="30"/>
            <PolyStepSegment x="0" y="30"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <LayerFeature layerRef="TOP">
          <Set polarity="POSITIVE">
            <Features>
              <Line startX="1" startY="1" endX="29" endY="1">
                <LineDesc lineWidth="0.2" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

    const PDK: &str = r#"schema_version = 1

[pdk]
id = "scope-test"
name = "Scope test"
revision = "1"

[capabilities.copper]
minimum_vscore_to_copper_clearance = "0.5 mm"
minimum_board_edge_clearance = "0.5 mm"

[capabilities.panelization]
minimum_board_array_spacing = "300 mil"
"#;

    fn check(xml: &str, scope: ArtworkScope) -> checks::Results {
        let ipc = Ipc2581::parse(xml).unwrap();
        let pdk = pdk::Pdk::parse(PDK).unwrap();
        let rules = rules::lower(&pdk).unwrap();
        let design = design::Design::extract(&ipc, scope, &rules).unwrap();
        checks::run(
            &rules,
            &design,
            None,
            NaiveDate::from_ymd_opt(2026, 8, 21).unwrap(),
        )
    }

    fn rule<'a>(results: &'a checks::Results, id: &str) -> &'a report::RuleResult {
        results.rules.iter().find(|rule| rule.id == id).unwrap()
    }

    #[test]
    fn one_evaluator_scales_through_board_array_and_fab_panel_lowering() {
        let array = create_board_array(
            BOARD,
            &BoardArrayCreateOptions {
                columns: 2,
                rows: 2,
                board_margin_mm: EdgeInsetsMm::all(0.0),
                edge_rail_mm: EdgeInsetsMm::all(5.0),
            },
            false,
        )
        .unwrap()
        .xml;
        let fab = create_fab_panel(
            std::slice::from_ref(&array),
            &[0, 0],
            FabPanelSpec::default(),
            false,
        )
        .unwrap()
        .xml;

        let board_results = check(BOARD, ArtworkScope::Board);
        let array_results = check(&array, ArtworkScope::ArrayFlattened);
        let fab_results = check(&fab, ArtworkScope::ArrayFlattened);

        let board_edge = "copper.minimum_board_edge_clearance";
        let vscore = "copper.minimum_vscore_to_copper_clearance";
        let spacing = "panelization.minimum_board_array_spacing";

        assert_eq!(rule(&board_results, board_edge).checked, 2);
        assert_eq!(rule(&array_results, board_edge).checked, 8);
        assert_eq!(rule(&fab_results, board_edge).checked, 16);

        assert_eq!(rule(&board_results, vscore).checked, 0);
        assert!(rule(&array_results, vscore).checked > 0);
        assert_eq!(
            rule(&fab_results, vscore).checked,
            2 * rule(&array_results, vscore).checked
        );

        assert_eq!(rule(&board_results, spacing).checked, 0);
        assert_eq!(rule(&array_results, spacing).checked, 0);
        assert_eq!(rule(&fab_results, spacing).checked, 1);
        assert_eq!(rule(&fab_results, spacing).finding_count, 0);
        assert!(board_results.findings.is_empty());
        assert!(array_results.findings.is_empty());
        assert!(fab_results.findings.is_empty());
    }
}
