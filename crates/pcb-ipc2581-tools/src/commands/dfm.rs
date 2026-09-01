//! PDK-driven manufacturability checks for IPC-2581 geometry.

#[cfg(feature = "cli")]
use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use pcb_ir::import::ipc2581::ImportedDesign;
#[cfg(any(feature = "cli", test))]
use pcb_ir::import::ipc2581::import_design;
#[cfg(feature = "cli")]
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::LayoutTarget;
#[cfg(any(feature = "cli", test))]
use crate::ipc2581::Ipc2581;
#[cfg(feature = "cli")]
use crate::utils::file as file_utils;

mod builtin_pdks;
mod checks;
mod design;
mod pdk;
pub mod report;
mod rules;
mod scene;
mod waivers;

#[cfg(feature = "cli")]
const MAX_REPORT_BYTES: usize = 128 * 1024 * 1024;
const MAX_PDK_BYTES: usize = 1024 * 1024;

pub use builtin_pdks::BuiltinPdk;
pub use report::DfmReport;

/// UTF-8 source and the caller-provided identity echoed into a report.
/// `path` is a label; the in-memory API never reads it from a filesystem.
#[derive(Debug, Clone, Copy)]
pub struct TextSource<'a> {
    pub path: &'a str,
    pub source: &'a str,
}

/// A bundled PDK name or a caller-provided TOML document.
#[derive(Debug)]
pub enum PdkSource<'a> {
    Builtin(&'a str),
    Toml(TextSource<'a>),
}

/// Inputs to one DFM run over an already imported physical design.
///
/// The host supplies the source identity and timestamp so this API performs
/// no filesystem, environment, or clock access. Waivers expire on the UTC
/// date of `generated_at`; supplying the same inputs yields the same report.
#[derive(Debug)]
pub struct CheckRequest<'a> {
    pub input: report::FileIdentity,
    pub pdk: PdkSource<'a>,
    pub waivers: Option<TextSource<'a>>,
    pub layout_target: LayoutTarget,
    pub generated_at: chrono::DateTime<chrono::Utc>,
}

/// The bundled PDKs, including their exact TOML source.
pub fn builtin_pdks() -> &'static [BuiltinPdk] {
    builtin_pdks::BUILTIN_PDKS
}

/// Run DFM in memory, reusing the canonical imported design.
///
/// Manufacturing violations are successful results with a `fail` verdict;
/// only invalid inputs or geometry that cannot be checked return an error.
pub fn check(imported: &ImportedDesign, request: CheckRequest<'_>) -> Result<DfmReport> {
    let (pdk_path, pdk_source, selected_profile) = match request.pdk {
        PdkSource::Builtin(name) => {
            let pdk = builtin_pdks::find(name)
                .with_context(|| format!("unknown built-in PDK '{name}'"))?;
            (
                format!("builtin:{}", pdk.name),
                pdk.source,
                Some(pdk.profile),
            )
        }
        PdkSource::Toml(source) => (source.path.to_owned(), source.source, None),
    };
    ensure!(
        pdk_source.len() <= MAX_PDK_BYTES,
        "PDK {pdk_path} exceeds the {MAX_PDK_BYTES} byte limit"
    );
    let pdk =
        pdk::Pdk::parse(pdk_source).with_context(|| format!("failed to parse PDK {pdk_path}"))?;
    let rules = rules::lower(&pdk, selected_profile)
        .with_context(|| format!("failed to lower PDK {pdk_path}"))?;
    if rules.is_empty() {
        bail!("PDK {pdk_path} configures no DFM rules; add at least one capability");
    }
    let waivers = request
        .waivers
        .map(|source| {
            waivers::WaiverFile::parse(source.source)
                .with_context(|| format!("failed to parse waiver file {}", source.path))
        })
        .transpose()?;

    let design = design::Design::extract(imported, request.layout_target.artwork_scope(), &rules)?;
    let checked = checks::run(
        &rules,
        &design,
        waivers.as_ref(),
        request.generated_at.date_naive(),
    );
    let summary = summarize(&checked);
    let layout = design.report_layout();
    let scene = scene::export(&design, &layout, &checked.rules, &checked.findings)?;
    Ok(DfmReport {
        schema_version: report::REPORT_SCHEMA_VERSION,
        generated_at: request.generated_at.to_rfc3339(),
        verdict: if summary.errors > 0 {
            report::Verdict::Fail
        } else {
            report::Verdict::Pass
        },
        tool: report::ToolIdentity {
            name: "pcb",
            version: env!("CARGO_PKG_VERSION"),
        },
        input: request.input,
        pdk: report::PdkIdentity::from_pdk(
            &pdk,
            selected_profile,
            pdk_path,
            sha256(pdk_source.as_bytes()),
            pdk_source.to_owned(),
        ),
        layout_target: match request.layout_target {
            LayoutTarget::Board => "board",
            LayoutTarget::BoardArray => "board_array",
        },
        layout,
        coordinate_system: report::CoordinateSystem {
            unit: "mm",
            axes: "x_right_y_up",
            origin: "ipc_2581_design",
        },
        waivers: checked
            .waivers
            .zip(request.waivers)
            .map(|(outcome, source)| report::WaiversApplied {
                path: source.path.to_owned(),
                sha256: sha256(source.source.as_bytes()),
                applied: outcome.applied,
                expired: outcome.expired,
                unmatched: outcome.unmatched,
            }),
        summary,
        rules: checked.rules,
        findings: checked.findings,
        scene,
    })
}

#[cfg(feature = "cli")]
#[derive(Debug)]
pub struct CheckOptions {
    pub pdk: PathBuf,
    pub waivers: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub layout_target: LayoutTarget,
}

/// Reject an invalid destination before any layout preparation or output write.
#[cfg(feature = "cli")]
pub fn validate_output(file: &Path, options: &CheckOptions) -> Result<()> {
    let Some(output) = options.output.as_deref() else {
        return Ok(());
    };
    let output_canonical = output.canonicalize().ok();
    // A .zen preparation error can occur before its layout path is resolved.
    // Reject board-file destinations up front so even an incomplete report
    // cannot replace that source, including through a differently named symlink.
    for target in [Some(output), output_canonical.as_deref()]
        .into_iter()
        .flatten()
    {
        ensure!(
            !target
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("kicad_pcb")),
            "DFM report output would overwrite a KiCad layout: {}",
            target.display()
        );
    }
    let pdk_file = options
        .pdk
        .to_str()
        .and_then(builtin_pdks::find)
        .is_none()
        .then_some(options.pdk.as_path());
    for source in [Some(file), pdk_file, options.waivers.as_deref()]
        .into_iter()
        .flatten()
    {
        ensure!(
            output != source
                && !output_canonical.as_ref().is_some_and(|output| source
                    .canonicalize()
                    .as_ref()
                    .ok()
                    == Some(output)),
            "DFM report output would overwrite source {}",
            source.display()
        );
    }
    Ok(())
}

#[cfg(feature = "cli")]
pub fn execute_check(file: &Path, options: &CheckOptions) -> Result<()> {
    validate_output(file, options)?;
    let report = match build_report(file, options) {
        Ok(checked) => checked,
        Err(error) => {
            write_error_report(file, options, &error)
                .with_context(|| format!("DFM check was incomplete: {error:#}"))?;
            return Err(error);
        }
    };
    write_report(options, &report)?;

    let summary = &report.summary;
    if summary.errors > 0 {
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

/// Preparation failures produce an incomplete report, never a passing result.
/// This also handles `.zen` layout/export errors before the IPC input exists.
#[cfg(feature = "cli")]
pub fn write_error_report(
    file: &Path,
    options: &CheckOptions,
    error: &anyhow::Error,
) -> Result<()> {
    validate_output(file, options)?;
    let incomplete = serde_json::json!({
        "schema_version": report::REPORT_SCHEMA_VERSION,
        "generated_at": generation_time().to_rfc3339(),
        "verdict": "incomplete",
        "tool": report::ToolIdentity {
            name: "pcb",
            version: env!("CARGO_PKG_VERSION"),
        },
        "input": { "path": file.display().to_string() },
        "pdk": { "path": options.pdk.display().to_string() },
        "layout_target": match options.layout_target {
            LayoutTarget::Board => "board",
            LayoutTarget::BoardArray => "board_array",
        },
        "error": { "message": format!("{error:#}") },
    });
    write_report(options, &incomplete)
}

#[cfg(feature = "cli")]
fn build_report(file: &Path, options: &CheckOptions) -> Result<DfmReport> {
    let input_bytes = std::fs::read(file)
        .with_context(|| format!("failed to read IPC-2581 file {}", file.display()))?;
    let input = report::FileIdentity::new(file.display().to_string(), &input_bytes);
    let pdk_path = options.pdk.display().to_string();
    let pdk_source = if builtin_pdks::find(&pdk_path).is_none() {
        let bytes = std::fs::read(&options.pdk)
            .with_context(|| format!("failed to read PDK file {pdk_path}"))?;
        Some(String::from_utf8(bytes).with_context(|| format!("PDK {pdk_path} is not UTF-8"))?)
    } else {
        None
    };
    let waivers = options
        .waivers
        .as_deref()
        .map(|path| -> Result<_> {
            let bytes = std::fs::read(path)
                .with_context(|| format!("failed to read waiver file {}", path.display()))?;
            let source = String::from_utf8(bytes)
                .with_context(|| format!("waiver file {} is not UTF-8", path.display()))?;
            Ok((path.display().to_string(), source))
        })
        .transpose()?;

    let generated_at = generation_time();
    let content = file_utils::ipc_text(file, &input_bytes)?;
    let ipc = Ipc2581::parse(&content).context("failed to parse IPC-2581 file")?;
    drop(content);
    drop(input_bytes);
    let imported = import_design(&ipc).context("failed to import IPC-2581 physical design")?;
    check(
        &imported,
        CheckRequest {
            input,
            pdk: match pdk_source.as_deref() {
                Some(source) => PdkSource::Toml(TextSource {
                    path: &pdk_path,
                    source,
                }),
                None => PdkSource::Builtin(&pdk_path),
            },
            waivers: waivers
                .as_ref()
                .map(|(path, source)| TextSource { path, source }),
            layout_target: options.layout_target,
            generated_at,
        },
    )
}

/// The non-verdict counts worth surfacing next to the pass/fail line.
#[cfg(feature = "cli")]
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
#[cfg(feature = "cli")]
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

#[cfg(feature = "cli")]
fn write_report(options: &CheckOptions, report: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(report)?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() <= MAX_REPORT_BYTES,
        "DFM report exceeds the {MAX_REPORT_BYTES} byte limit"
    );
    match options.output.as_deref() {
        Some(path) => {
            // Replace only after serialization and the complete write succeed.
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let mut temporary = tempfile::NamedTempFile::new_in(parent)
                .with_context(|| format!("failed to create DFM report in {}", parent.display()))?;
            temporary
                .write_all(&bytes)
                .with_context(|| format!("failed to write DFM report to {}", path.display()))?;
            temporary
                .as_file()
                .sync_all()
                .context("failed to flush DFM report to disk")?;
            temporary
                .persist(path)
                .map_err(|error| error.error)
                .with_context(|| format!("failed to replace DFM report {}", path.display()))?;
            Ok(())
        }
        None => pcb_ui::write_stdout(|stdout| stdout.write_all(&bytes))
            .context("failed to write DFM report to stdout"),
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

    const PDK: &str = r#"schema_version = 2
default_profile = "test"

[pdk]
id = "scope-test"
name = "Scope test"
revision = "1"

[profiles.test]
name = "Test"

[profiles.test.support]
copper_layers = { minimum = 2, maximum = 4 }

[[rules.copper.vscore_clearance]]
id = "copper.minimum_vscore_to_copper_clearance"
limit = { minimum = "0.5 mm" }

[[rules.copper.board_edge_clearance]]
id = "copper.minimum_board_edge_clearance"
limit = { minimum = "0.5 mm" }

[[rules.panelization.board_spacing]]
id = "panelization.minimum_board_array_spacing"
limit = { minimum = "300 mil" }
"#;

    fn check(xml: &str, target: LayoutTarget) -> DfmReport {
        check_with_pdk(xml, target, PDK)
    }

    fn check_with_pdk(xml: &str, target: LayoutTarget, pdk_source: &str) -> DfmReport {
        let ipc = Ipc2581::parse(xml).unwrap();
        let imported = import_design(&ipc).unwrap();
        super::check(
            &imported,
            CheckRequest {
                input: report::FileIdentity::new("board.xml", xml.as_bytes()),
                pdk: PdkSource::Toml(TextSource {
                    path: "pdk.toml",
                    source: pdk_source,
                }),
                waivers: None,
                layout_target: target,
                generated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            },
        )
        .unwrap()
    }

    fn rule<'a>(results: &'a DfmReport, id: &str) -> &'a report::RuleResult {
        results.rules.iter().find(|rule| rule.id == id).unwrap()
    }

    #[test]
    fn loads_the_embedded_pdks_and_keeps_ipc_profiles_non_executable() {
        let builtin = builtin_pdks()
            .iter()
            .find(|pdk| pdk.name == "standard")
            .unwrap();
        let parsed = pdk::Pdk::parse(builtin.source).unwrap();
        assert_eq!(parsed.pdk.id, "standard");
        assert_eq!(parsed.pdk.name, "Standard");
        assert_eq!(parsed.pdk.manufacturer.as_deref(), Some("Diode"));
        assert_eq!(parsed.pdk.process.as_deref(), Some("Standard"));
        assert_eq!(parsed.default_profile, "standard");
        let support = parsed.profiles["standard"]
            .support
            .copper_layers
            .as_ref()
            .unwrap();
        assert_eq!(support.minimum(), Some(2));
        assert_eq!(support.maximum(), Some(10));
        assert!(!rules::lower(&parsed, None).unwrap().is_empty());

        for builtin in builtin_pdks() {
            let parsed = pdk::Pdk::parse(builtin.source).unwrap();
            let lowered = rules::lower(&parsed, Some(builtin.profile));
            let (_, profile) = parsed.selected_profile(Some(builtin.profile)).unwrap();
            if profile.status == pdk::ProfileStatus::MetadataOnly {
                assert!(lowered.unwrap_err().to_string().contains("metadata-only"));
            } else {
                assert!(!lowered.unwrap().is_empty());
            }
        }

        let jlc = builtin_pdks()
            .iter()
            .find(|pdk| pdk.name == "jlcpcb-1oz-black-white")
            .unwrap();
        let parsed = pdk::Pdk::parse(jlc.source).unwrap();
        let rules = rules::lower(&parsed, Some(jlc.profile)).unwrap();
        let mask = rules
            .iter()
            .find(|rule| rule.id == "jlc.soldermask.minimum_web.black_white")
            .unwrap();
        assert_eq!(mask.limit.length().millimeters(), 0.13);
        assert!(
            rules
                .iter()
                .all(|rule| rule.id != "jlc.soldermask.minimum_web.standard_color")
        );
        let two_layer = rules
            .iter()
            .find(|rule| rule.id == "jlc.copper.minimum_feature_width.2-layer")
            .unwrap();
        assert_eq!(two_layer.limit.length().millimeters(), 0.10);
        assert_eq!(two_layer.conditions.minimum_copper_layers, Some(2));
        assert_eq!(two_layer.conditions.maximum_copper_layers, Some(2));
        let multilayer = rules
            .iter()
            .find(|rule| rule.id == "jlc.copper.minimum_feature_width.multilayer")
            .unwrap();
        assert_eq!(multilayer.limit.length().millimeters(), 0.09);
        assert_eq!(multilayer.conditions.minimum_copper_layers, Some(3));
        assert_eq!(multilayer.conditions.maximum_copper_layers, Some(32));
    }

    #[test]
    fn case_named_preferred_remains_a_required_tier() {
        let pdk = PDK.replace(
            "[[rules.copper.board_edge_clearance]]\nid = \"copper.minimum_board_edge_clearance\"\nlimit = { minimum = \"0.5 mm\" }",
            "[[rules.copper.board_edge_clearance]]\nid = \"copper.minimum_board_edge_clearance\"\ncases = [\n  { id = \"preferred\", when = { copper_layers = { exact = 2 } }, limit = { minimum = \"0.5 mm\" } },\n]",
        );

        let results = check_with_pdk(BOARD, LayoutTarget::Board, &pdk);
        let required = rule(&results, "copper.minimum_board_edge_clearance.preferred");

        assert_eq!(required.severity, report::Severity::Error);
        assert_eq!(required.tier, "required");
    }

    #[test]
    fn in_memory_report_keeps_source_identity_and_waiver_dates() {
        let imported = import_design(&Ipc2581::parse(BOARD).unwrap()).unwrap();
        let pdk_source = PDK.replace("minimum = 2", "minimum = 3");
        let run = |waivers, day| {
            super::check(
                &imported,
                CheckRequest {
                    input: report::FileIdentity::new("board.xml", BOARD.as_bytes()),
                    pdk: PdkSource::Toml(TextSource {
                        path: "pdk.toml",
                        source: &pdk_source,
                    }),
                    waivers,
                    layout_target: LayoutTarget::Board,
                    generated_at: NaiveDate::from_ymd_opt(2026, 8, day)
                        .unwrap()
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_utc(),
                },
            )
            .unwrap()
        };
        let initial = run(None, 30);
        assert!(matches!(initial.verdict, report::Verdict::Fail));
        assert_eq!(initial.summary.errors, 1);
        assert_eq!(initial.input.sha256, sha256(BOARD.as_bytes()));
        assert_eq!(initial.pdk.sha256, sha256(pdk_source.as_bytes()));
        assert_eq!(initial.generated_at, "2026-08-30T00:00:00+00:00");
        let id = &initial.findings[0].id;
        let source = format!(
            r#"[[waiver]]
finding = "{id}"
reason = "approved by fab"
expires = "2026-08-31"

[[waiver]]
finding = "dfm-stale"
reason = "old finding"
"#
        );
        let waivers = Some(TextSource {
            path: "waivers.toml",
            source: &source,
        });
        let active = run(waivers, 30);
        assert!(matches!(active.verdict, report::Verdict::Pass));
        assert_eq!(active.summary.errors, 0);
        assert_eq!(active.summary.findings, 1);
        assert_eq!(active.summary.waived, 1);
        assert_eq!(active.findings[0].id, *id);
        assert_eq!(
            active.findings[0].waiver_reason.as_deref(),
            Some("approved by fab")
        );
        assert!(active.findings[0].waived);
        let applied = active.waivers.unwrap();
        assert_eq!(applied.path, "waivers.toml");
        assert_eq!(applied.sha256, sha256(source.as_bytes()));
        assert_eq!(applied.applied, 1);
        assert!(applied.expired.is_empty());
        assert_eq!(applied.unmatched, ["dfm-stale"]);

        let expired = run(waivers, 31);
        assert!(matches!(expired.verdict, report::Verdict::Fail));
        assert_eq!(expired.summary.errors, 1);
        assert_eq!(expired.summary.waived, 0);
        assert!(!expired.findings[0].waived);
        assert_eq!(expired.waivers.unwrap().expired, std::slice::from_ref(id));
    }

    #[cfg(feature = "cli")]
    #[test]
    fn cli_report_matches_in_memory_report_for_compressed_input() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("board.xml.zst");
        let pdk = directory.path().join("custom.toml");
        let output = directory.path().join("report.json");
        let pdk_source = PDK.replace("minimum = 2", "minimum = 3");
        let bytes = zstd::encode_all(BOARD.as_bytes(), 0).unwrap();
        std::fs::write(&input, &bytes).unwrap();
        std::fs::write(&pdk, &pdk_source).unwrap();
        let error = execute_check(
            &input,
            &CheckOptions {
                pdk: pdk.clone(),
                waivers: None,
                output: Some(output.clone()),
                layout_target: LayoutTarget::Board,
            },
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("DFM check failed with 1 error finding(s)")
        );
        let cli: serde_json::Value =
            serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
        let mut report = check_with_pdk(BOARD, LayoutTarget::Board, &pdk_source);
        report.input = report::FileIdentity::new(input.display().to_string(), &bytes);
        report.pdk.path = pdk.display().to_string();
        report.generated_at = cli["generated_at"].as_str().unwrap().to_owned();
        assert_eq!(cli, serde_json::to_value(report).unwrap());
    }

    #[test]
    fn rejects_oversize_pdk_source() {
        let imported = import_design(&Ipc2581::parse(BOARD).unwrap()).unwrap();
        let source = " ".repeat(MAX_PDK_BYTES + 1);
        let error = super::check(
            &imported,
            CheckRequest {
                input: report::FileIdentity::new("board.xml", BOARD.as_bytes()),
                pdk: PdkSource::Toml(TextSource {
                    path: "oversize.toml",
                    source: &source,
                }),
                waivers: None,
                layout_target: LayoutTarget::Board,
                generated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("exceeds the 1048576 byte limit"));
    }

    #[cfg(feature = "cli")]
    #[test]
    fn report_serialization_failure_preserves_existing_output() {
        struct Unserializable;
        impl Serialize for Unserializable {
            fn serialize<S: serde::Serializer>(
                &self,
                _serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom("serialization failed"))
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("report.dfm.json");
        std::fs::write(&output, b"previous report").unwrap();
        let options = CheckOptions {
            pdk: "standard".into(),
            waivers: None,
            output: Some(output.clone()),
            layout_target: LayoutTarget::Board,
        };

        let error = write_report(&options, &Unserializable).unwrap_err();

        assert!(error.to_string().contains("serialization failed"));
        assert_eq!(std::fs::read(output).unwrap(), b"previous report");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(feature = "cli")]
    #[test]
    fn report_persistence_failure_preserves_destination_and_cleans_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("report.dfm.json");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("sentinel"), b"untouched").unwrap();
        let options = CheckOptions {
            pdk: "standard".into(),
            waivers: None,
            output: Some(output.clone()),
            layout_target: LayoutTarget::Board,
        };

        let error =
            write_report(&options, &serde_json::json!({"verdict": "incomplete"})).unwrap_err();

        assert!(error.to_string().contains("failed to replace DFM report"));
        assert_eq!(
            std::fs::read(output.join("sentinel")).unwrap(),
            b"untouched"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn profile_support_checks_both_layer_bounds_from_the_physical_stackup() {
        let below_minimum = check_with_pdk(
            BOARD,
            LayoutTarget::Board,
            &PDK.replace("minimum = 2", "minimum = 3"),
        );
        let minimum_rule = rule(&below_minimum, "profile.support.copper_layers.minimum");
        assert_eq!(minimum_rule.checked, 1);
        assert_eq!(minimum_rule.comparison, "minimum");
        assert_eq!(minimum_rule.limit.normalized_unit, "layers");
        assert_eq!(minimum_rule.limit.normalized_value, 3.0);
        assert!(matches!(minimum_rule.status, report::RuleStatus::Fail));
        let minimum_finding = below_minimum
            .findings
            .iter()
            .find(|finding| finding.rule_id == minimum_rule.id)
            .unwrap();
        assert!(matches!(
            minimum_finding.measurement,
            report::Measurement::Count {
                actual_count: 2,
                required_count: 3,
                margin_count: -1,
            }
        ));
        assert_eq!(
            minimum_finding
                .layers
                .iter()
                .map(|layer| layer.name.as_str())
                .collect::<Vec<_>>(),
            ["TOP", "BOTTOM"]
        );

        let above_maximum = check_with_pdk(
            BOARD,
            LayoutTarget::Board,
            &PDK.replace(
                "copper_layers = { minimum = 2, maximum = 4 }",
                "copper_layers = { maximum = 1 }",
            ),
        );
        let maximum_rule = rule(&above_maximum, "profile.support.copper_layers.maximum");
        assert_eq!(maximum_rule.checked, 1);
        assert_eq!(maximum_rule.comparison, "maximum");
        assert!(matches!(maximum_rule.status, report::RuleStatus::Fail));
        assert!(above_maximum.findings.iter().any(|finding| matches!(
            finding.measurement,
            report::Measurement::Count {
                actual_count: 2,
                required_count: 1,
                margin_count: -1,
            }
        )));
    }

    #[test]
    fn profile_support_rejects_an_incomplete_physical_stackup() {
        let ipc = Ipc2581::parse(&BOARD.replace(
            "layerOrGroupRef=\"BOTTOM\"",
            "layerOrGroupRef=\"DIELECTRIC\"",
        ))
        .unwrap();
        let pdk = pdk::Pdk::parse(PDK).unwrap();
        let rules = rules::lower(&pdk, None).unwrap();
        let imported = import_design(&ipc).unwrap();

        let error = design::Design::extract(&imported, ArtworkScope::Board, &rules)
            .err()
            .unwrap();

        assert!(
            error
                .to_string()
                .contains("omits declared copper layer(s): BOTTOM")
        );
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

        let board_results = check(BOARD, LayoutTarget::Board);
        let array_results = check(&array, LayoutTarget::BoardArray);
        let fab_results = check(&fab, LayoutTarget::BoardArray);

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
