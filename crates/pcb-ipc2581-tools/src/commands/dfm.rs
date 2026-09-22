//! PDK-driven manufacturability checks for IPC-2581 geometry.

use pcb_ir::geom::Resolution;
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
/// Manufacturing violations are successful results with a `fail` verdict, and
/// so is a required rule that could not be evaluated: it is reported as
/// `incomplete`, never passed. Only invalid inputs return an error.
pub fn check(
    imported: &ImportedDesign,
    request: CheckRequest<'_>,
    resolution: Resolution,
) -> Result<DfmReport> {
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

    let designs = design::Design::frames(
        imported,
        request.layout_target.artwork_scope(),
        &rules,
        resolution,
    )?;
    let checked = checks::run(
        &rules,
        &designs,
        waivers.as_ref(),
        request.generated_at.date_naive(),
    )?;
    let summary = summarize(&checked);
    let layout = designs[0].report_layout();
    let frames = checked
        .frames
        .iter()
        .map(|(design, placements)| designs[*design as usize].report_frame(placements))
        .collect::<Vec<_>>();
    let scene = scene::export(
        &designs,
        &layout,
        &checked.rules,
        &frames,
        &checked.findings,
    )?;
    Ok(DfmReport {
        schema_version: report::REPORT_SCHEMA_VERSION,
        generated_at: request.generated_at.to_rfc3339(),
        verdict: if summary.errors > 0
            || checked.rules.iter().any(report::RuleResult::blocks_verdict)
        {
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
        frames,
        findings: checked.findings,
        shared_evidence: checked.shared_evidence,
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

#[cfg(feature = "cli")]
pub enum CheckOutcome {
    Passed,
    Failed(anyhow::Error),
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
pub fn execute_check(
    file: &Path,
    options: &CheckOptions,
    resolution: Resolution,
) -> Result<CheckOutcome> {
    validate_output(file, options)?;
    let report = match build_report(file, options, resolution) {
        Ok(checked) => checked,
        Err(error) => {
            write_error_report(file, options, &error)
                .with_context(|| format!("DFM check was incomplete: {error:#}"))?;
            return Ok(CheckOutcome::Failed(error));
        }
    };
    write_report(options, &report)?;

    let summary = &report.summary;
    // A rule that could not be evaluated is named, never just counted.
    for rule in &report.rules {
        if matches!(rule.status, report::RuleStatus::Incomplete) {
            eprintln!(
                "not evaluated: {}: {}",
                rule.id,
                rule.skip_reason.as_deref().unwrap_or_default()
            );
        }
    }
    if matches!(report.verdict, report::Verdict::Fail) {
        return Ok(CheckOutcome::Failed(anyhow::anyhow!(
            "DFM check failed with {} error finding(s){}",
            summary.errors,
            annotations(summary)
        )));
    }
    eprintln!(
        "✓ DFM check passed ({} rule(s){})",
        summary.rules_configured,
        annotations(summary)
    );
    Ok(CheckOutcome::Passed)
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
fn build_report(file: &Path, options: &CheckOptions, resolution: Resolution) -> Result<DfmReport> {
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
    let imported =
        import_design(&ipc, resolution).context("failed to import IPC-2581 physical design")?;
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
        resolution,
    )
}

/// The non-verdict counts worth surfacing next to the pass/fail line.
#[cfg(feature = "cli")]
fn annotations(summary: &report::Summary) -> String {
    [
        (summary.rules_incomplete, "not evaluated"),
        (summary.rules_not_applicable, "not applicable"),
        (summary.warnings, "warning(s)"),
        (summary.waived, "waived"),
        (summary.unresolved, "within measurement uncertainty"),
    ]
    .into_iter()
    .filter(|&(count, _)| count > 0)
    .map(|(count, what)| format!(", {count} {what}"))
    .collect()
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
    use report::{RuleStatus, Severity};
    let rules = |status: RuleStatus| {
        let rules = checked.rules.iter();
        rules.filter(|rule| rule.status == status).count()
    };
    let unwaived = |severity: Severity| {
        let findings = checked.findings.iter();
        findings
            .filter(|finding| !finding.waived && finding.severity == severity)
            .count()
    };
    report::Summary {
        rules_configured: checked.rules.len(),
        rules_passed: rules(RuleStatus::Pass),
        rules_warned: rules(RuleStatus::Warning),
        rules_failed: rules(RuleStatus::Fail),
        rules_not_applicable: rules(RuleStatus::NotApplicable),
        rules_incomplete: rules(RuleStatus::Incomplete),
        findings: checked.findings.len(),
        errors: unwaived(Severity::Error),
        warnings: unwaived(Severity::Warning),
        waived: checked
            .findings
            .iter()
            .filter(|finding| finding.waived)
            .count(),
        unresolved: checked.rules.iter().map(|rule| rule.unresolved.len()).sum(),
    }
}

/// The report as newline-terminated JSON of at most `limit` bytes.
/// Serialization stops at the limit: a panel's report can be many times over
/// it, and building all of that in memory only to refuse it cost gigabytes.
#[cfg(feature = "cli")]
fn serialize_within(report: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    struct Capped {
        bytes: Vec<u8>,
        limit: usize,
        exceeded: bool,
    }
    impl Write for Capped {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len() + buffer.len() > self.limit {
                self.exceeded = true;
                return Err(std::io::Error::other("report limit reached"));
            }
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut capped = Capped {
        bytes: Vec::new(),
        limit,
        exceeded: false,
    };
    let written = serde_json::to_writer_pretty(&mut capped, report)
        .map_err(anyhow::Error::from)
        .and_then(|()| Ok(capped.write_all(b"\n")?));
    ensure!(
        !capped.exceeded,
        "DFM report exceeds the {limit} byte limit"
    );
    written.map(|()| capped.bytes)
}

#[cfg(feature = "cli")]
fn write_report(options: &CheckOptions, report: &impl Serialize) -> Result<()> {
    let bytes = serialize_within(report, MAX_REPORT_BYTES)?;
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

/// What every DFM test runs its fixture through.
#[cfg(test)]
mod fixtures {
    use super::*;

    /// A PDK of one `test` profile holding the given rule tables.
    pub fn pdk(rules: &str) -> String {
        format!(
            "schema_version = 2\ndefault_profile = \"test\"\n[pdk]\nid = \"test\"\nname = \"Test\"\nrevision = \"1\"\n[profiles.test]\nname = \"Test\"\n{rules}\n"
        )
    }

    pub fn import(xml: &str) -> ImportedDesign {
        import_design(&Ipc2581::parse(xml).unwrap(), Resolution::default()).unwrap()
    }

    pub fn rules(pdk: &str) -> Vec<rules::Rule> {
        rules::lower(&pdk::Pdk::parse(pdk).unwrap(), None).unwrap()
    }

    pub fn request<'a>(xml: &str, pdk: &'a str, layout_target: LayoutTarget) -> CheckRequest<'a> {
        CheckRequest {
            input: report::FileIdentity::new("board.xml", xml.as_bytes()),
            pdk: PdkSource::Toml(TextSource {
                path: "pdk.toml",
                source: pdk,
            }),
            waivers: None,
            layout_target,
            generated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        }
    }

    /// The report of `xml` checked against a PDK source.
    pub fn report(xml: &str, pdk: &str, target: LayoutTarget) -> DfmReport {
        check(
            &import(xml),
            request(xml, pdk, target),
            Resolution::default(),
        )
        .unwrap()
    }

    /// The engine's results for the board of `xml`.
    pub fn run_board(xml: &str, pdk: &str) -> checks::Results {
        let (imported, rules) = (import(xml), rules(pdk));
        let design = design::Design::board(&imported, &rules, Resolution::default());
        checks::run(
            &rules,
            std::slice::from_ref(&design),
            None,
            chrono::NaiveDate::default(),
        )
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

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
        fixtures::report(xml, pdk_source, target)
    }

    /// A 2 x 2 V-scored array of `board` inside `rail_mm` rails.
    fn array_of(board: &str, rail_mm: f64) -> String {
        create_board_array(
            board,
            &BoardArrayCreateOptions {
                columns: 2,
                rows: 2,
                board_margin_mm: EdgeInsetsMm::all(0.0),
                edge_rail_mm: EdgeInsetsMm::all(rail_mm),
            },
            false,
            crate::commands::board_array::Separation::VScore,
            Resolution::default(),
        )
        .unwrap()
        .xml
    }

    /// A fabrication panel of two of `array`.
    fn fab_panel_of(array: &String) -> String {
        create_fab_panel(
            std::slice::from_ref(array),
            &[0, 0],
            FabPanelSpec::default(),
            false,
            Resolution::default(),
        )
        .unwrap()
        .xml
    }

    fn rule<'a>(results: &'a DfmReport, id: &str) -> &'a report::RuleResult {
        results.rules.iter().find(|rule| rule.id == id).unwrap()
    }

    #[test]
    fn loads_embedded_pdks_and_lowers_partial_ipc_baselines() {
        for builtin in builtin_pdks() {
            let parsed = pdk::Pdk::parse(builtin.source).unwrap();
            assert!(
                !rules::lower(&parsed, Some(builtin.profile))
                    .unwrap()
                    .is_empty()
            );
        }

        let parsed = pdk::Pdk::parse(builtin_pdks::find("ipc").unwrap().source).unwrap();
        for class in 1..=3 {
            for (level, maximum, copper_clearance, edge_clearance) in [
                ('a', 6.0, 0.25, 0.50),
                ('b', 8.0, 0.20, 0.40),
                ('c', 10.0, 0.15, 0.30),
            ] {
                let profile = format!("{class}{level}");
                let definition = &parsed.profiles[&profile];
                assert_eq!(definition.status, pdk::ProfileStatus::Executable);
                assert_eq!(definition.performance_class, Some(class));
                assert_eq!(
                    definition.producibility_level.unwrap().label(),
                    level.to_ascii_uppercase().to_string()
                );
                let rules = rules::lower(&parsed, Some(&profile)).unwrap();
                let aspect_ratio = rules
                    .iter()
                    .filter(|rule| matches!(rule.kind, rules::RuleKind::HoleAspectRatio(_)))
                    .collect::<Vec<_>>();
                assert_eq!(aspect_ratio.len(), 2);
                assert!(
                    aspect_ratio
                        .iter()
                        .all(|rule| rule.limit.ratio() == maximum)
                );
                assert!(aspect_ratio.iter().any(|rule| rule.id.contains("via")));
                assert!(aspect_ratio.iter().any(|rule| rule.id.contains("pth")));

                let hole_clearance = rules
                    .iter()
                    .filter(|rule| matches!(rule.kind, rules::RuleKind::HoleToCopperClearance(_)))
                    .collect::<Vec<_>>();
                assert_eq!(hole_clearance.len(), 3);
                assert!(
                    hole_clearance
                        .iter()
                        .all(|rule| rule.limit.length().millimeters() == copper_clearance)
                );

                let hole_to_edge = rules
                    .iter()
                    .filter(|rule| {
                        matches!(rule.kind, rules::RuleKind::HoleToBoardEdgeClearance(_))
                    })
                    .collect::<Vec<_>>();
                assert_eq!(hole_to_edge.len(), 3);
                assert!(
                    hole_to_edge
                        .iter()
                        .all(|rule| rule.limit.length().millimeters() == edge_clearance)
                );
                let slot_to_edge = rules
                    .iter()
                    .filter(|rule| {
                        matches!(rule.kind, rules::RuleKind::SlotToBoardEdgeClearance(_))
                    })
                    .collect::<Vec<_>>();
                assert_eq!(slot_to_edge.len(), 2);
                assert!(
                    slot_to_edge
                        .iter()
                        .all(|rule| rule.limit.length().millimeters() == edge_clearance)
                );
            }
        }

        let jlc = builtin_pdks::find("jlcpcb-1oz").unwrap();
        let parsed = pdk::Pdk::parse(jlc.source).unwrap();
        let rules = rules::lower(&parsed, Some(jlc.profile)).unwrap();
        let mask = rules
            .iter()
            .find(|rule| rule.id == "jlc.soldermask.minimum_web")
            .unwrap();
        assert_eq!(mask.limit.length().millimeters(), 0.13);
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
    fn ipc_copper_clearance_checks_profile_cutouts() {
        let ipc = builtin_pdks::find("ipc").unwrap();
        let id = "diode.ipc_baseline.copper.minimum_board_edge_clearance";
        let board = BOARD.replace(
            r#"<Set polarity="POSITIVE">"#,
            r#"<Set polarity="POSITIVE" net="N1">"#,
        );
        let clear = check_with_pdk(&board, LayoutTarget::Board, ipc.source);
        assert!(matches!(rule(&clear, id).status, report::RuleStatus::Pass));

        // The trace is 0.9 mm from the outer edge, but only 0.2 mm from
        // this cutout. Exercise import, profile extraction, and the shared
        // copper evaluator together rather than duplicating PCB IR geometry tests.
        let board = board.replace(
            "</Profile>",
            r#"<Cutout>
              <PolyBegin x="10" y="1.3"/><PolyStepSegment x="20" y="1.3"/>
              <PolyStepSegment x="20" y="5"/><PolyStepSegment x="10" y="5"/>
              <PolyStepSegment x="10" y="1.3"/>
            </Cutout></Profile>"#,
        );
        let results = check_with_pdk(&board, LayoutTarget::Board, ipc.source);
        let edge = rule(&results, id);
        assert!(matches!(edge.status, report::RuleStatus::Fail));
        assert_eq!(edge.finding_count, 1);
        let finding = results.findings.iter().find(|f| f.rule_id == id).unwrap();
        let actual = finding.measurement.actual_mm().unwrap();
        assert!(
            (actual - 0.2).abs() < pcb_ir::geom::tol::REGION_MM,
            "actual clearance: {actual}"
        );
        assert_eq!(finding.layers[0].name, "TOP");
        assert!(!finding.sites.is_empty());
    }

    #[test]
    fn standard_soldermask_web_warns_without_failing_verdict() {
        let mask_web = r#"
        <LayerFeature layerRef="F.Mask">
          <Set polarity="POSITIVE"><Features>
            <Contour><Polygon>
              <PolyBegin x="2" y="2"/><PolyStepSegment x="5" y="2"/>
              <PolyStepSegment x="5" y="8"/><PolyStepSegment x="2" y="8"/>
            </Polygon></Contour>
            <Contour><Polygon>
              <PolyBegin x="5.05" y="2"/><PolyStepSegment x="8" y="2"/>
              <PolyStepSegment x="8" y="8"/><PolyStepSegment x="5.05" y="8"/>
            </Polygon></Contour>
          </Features></Set>
        </LayerFeature>"#;
        let copper = r#"        <LayerFeature layerRef="TOP">
          <Set polarity="POSITIVE">
            <Features>
              <Line startX="1" startY="1" endX="29" endY="1">
                <LineDesc lineWidth="0.2" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>"#;
        let board = BOARD.replace(copper, mask_web);
        let standard = builtin_pdks::find("standard").unwrap();

        let results = check_with_pdk(&board, LayoutTarget::Board, standard.source);
        let mask_rule = rule(&results, "soldermask.minimum_web.preferred");

        assert!(matches!(results.verdict, report::Verdict::Pass));
        assert_eq!(results.summary.errors, 0);
        assert_eq!(results.summary.warnings, 1);
        assert_eq!(mask_rule.severity, report::Severity::Warning);
        assert!(matches!(mask_rule.status, report::RuleStatus::Warning));
        assert_eq!(mask_rule.finding_count, 1);
    }

    #[test]
    fn a_limit_inside_a_measurements_uncertainty_is_reported_unresolved() {
        let edge = "copper.minimum_board_edge_clearance";
        let with_limit = |limit_mm: f64| {
            check_with_pdk(
                BOARD,
                LayoutTarget::Board,
                &PDK.replace(
                    "id = \"copper.minimum_board_edge_clearance\"\nlimit = { minimum = \"0.5 mm\" }",
                    &format!(
                        "id = \"copper.minimum_board_edge_clearance\"\nlimit = {{ minimum = \"{limit_mm} mm\" }}"
                    ),
                ),
            )
        };
        // The round-capped trace is flattened, so its 0.9 mm clearance to the
        // board edge carries an uncertainty; read both from a certain failure.
        let failing = with_limit(2.0);
        let site = &failing.findings[0].sites[0];
        let (actual, uncertainty) = (site.measurement.actual_mm().unwrap(), site.uncertainty_mm);
        assert!(uncertainty > 0.0);

        let inside = with_limit(actual + uncertainty / 2.0);
        assert!(
            inside.findings.is_empty(),
            "tessellation alone could explain it"
        );
        let unresolved = &rule(&inside, edge).unresolved;
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].actual_mm, actual);
        assert_eq!(unresolved[0].uncertainty_mm, uncertainty);
        assert_eq!(unresolved[0].layers, ["TOP"]);
        assert_eq!(inside.summary.unresolved, 1);
        assert!(matches!(inside.verdict, report::Verdict::Pass));

        let beyond = with_limit(actual + uncertainty + 1e-4);
        assert_eq!(beyond.findings.len(), 1);
        assert!(rule(&beyond, edge).unresolved.is_empty());
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
        let imported = fixtures::import(BOARD);
        let pdk_source = PDK.replace("minimum = 2", "minimum = 3");
        let run = |waivers, day| {
            super::check(
                &imported,
                CheckRequest {
                    waivers,
                    generated_at: NaiveDate::from_ymd_opt(2026, 8, day)
                        .unwrap()
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_utc(),
                    ..fixtures::request(BOARD, &pdk_source, LayoutTarget::Board)
                },
                Resolution::default(),
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
        let outcome = execute_check(
            &input,
            &CheckOptions {
                pdk: pdk.clone(),
                waivers: None,
                output: Some(output.clone()),
                layout_target: LayoutTarget::Board,
            },
            Resolution::default(),
        )
        .unwrap();
        let CheckOutcome::Failed(error) = outcome else {
            panic!("expected the DFM check to fail");
        };
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

    #[cfg(feature = "cli")]
    #[test]
    fn a_failed_report_write_leaves_the_destination_and_no_temporary_file() {
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
        let options = |output: PathBuf| CheckOptions {
            pdk: "standard".into(),
            waivers: None,
            output: Some(output),
            layout_target: LayoutTarget::Board,
        };

        let file = directory.path().join("report.dfm.json");
        std::fs::write(&file, b"previous report").unwrap();
        let error = write_report(&options(file.clone()), &Unserializable).unwrap_err();
        assert!(error.to_string().contains("serialization failed"));
        assert_eq!(std::fs::read(file).unwrap(), b"previous report");

        // A destination that cannot be replaced: a directory.
        let occupied = directory.path().join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("sentinel"), b"untouched").unwrap();
        let report = serde_json::json!({"verdict": "incomplete"});
        let error = write_report(&options(occupied.clone()), &report).unwrap_err();
        assert!(error.to_string().contains("failed to replace DFM report"));
        assert_eq!(
            std::fs::read(occupied.join("sentinel")).unwrap(),
            b"untouched"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[cfg(feature = "cli")]
    #[test]
    fn serialization_stops_at_the_report_limit() {
        let report = serde_json::json!({"verdict": "fail", "findings": vec!["x"; 64]});
        let whole = serialize_within(&report, usize::MAX).unwrap();
        assert_eq!(whole.last(), Some(&b'\n'));
        assert_eq!(
            whole[..whole.len() - 1],
            serde_json::to_vec_pretty(&report).unwrap()
        );
        // The limit is inclusive of the trailing newline.
        assert_eq!(serialize_within(&report, whole.len()).unwrap(), whole);
        for limit in [whole.len() - 1, 16, 0] {
            let error = serialize_within(&report, limit).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("DFM report exceeds the {limit} byte limit")
            );
        }
    }

    #[test]
    fn profile_support_checks_both_layer_bounds_from_the_physical_stackup() {
        for (support, comparison, required) in [
            ("copper_layers = { minimum = 3, maximum = 4 }", "minimum", 3),
            ("copper_layers = { maximum = 1 }", "maximum", 1),
        ] {
            let pdk = PDK.replace("copper_layers = { minimum = 2, maximum = 4 }", support);
            let results = check_with_pdk(BOARD, LayoutTarget::Board, &pdk);
            let bound = rule(
                &results,
                &format!("profile.support.copper_layers.{comparison}"),
            );
            assert_eq!(bound.checked, 1);
            assert_eq!(bound.comparison, comparison);
            assert_eq!(bound.limit.normalized_unit, "layers");
            assert_eq!(bound.limit.normalized_value, f64::from(required));
            assert!(matches!(bound.status, report::RuleStatus::Fail));
            let [finding] = results.findings.as_slice() else {
                panic!("one bound is violated: {:?}", results.findings);
            };
            assert_eq!(finding.rule_id, bound.id);
            assert!(matches!(
                finding.measurement,
                report::Measurement::Count {
                    actual_count: 2,
                    required_count,
                    margin_count: -1,
                } if required_count == required
            ));
            assert_eq!(
                finding
                    .layers
                    .iter()
                    .map(|layer| layer.name.as_str())
                    .collect::<Vec<_>>(),
                ["TOP", "BOTTOM"]
            );
        }
    }

    #[test]
    fn an_incomplete_physical_stackup_blocks_only_the_rules_that_read_it() {
        let results = check(
            &BOARD.replace(
                "layerOrGroupRef=\"BOTTOM\"",
                "layerOrGroupRef=\"DIELECTRIC\"",
            ),
            LayoutTarget::Board,
        );

        // Layer-count support cannot be certified, so the verdict fails closed.
        assert!(matches!(results.verdict, report::Verdict::Fail));
        assert_eq!(results.summary.errors, 0);
        assert_eq!(results.summary.rules_incomplete, 2);
        for id in [
            "profile.support.copper_layers.minimum",
            "profile.support.copper_layers.maximum",
        ] {
            let support = rule(&results, id);
            assert!(matches!(support.status, report::RuleStatus::Incomplete));
            assert!(
                support
                    .skip_reason
                    .as_deref()
                    .unwrap()
                    .contains("omits declared copper layer(s): BOTTOM")
            );
        }
        // A rule that reads no stackup is still evaluated and reported.
        let edge = rule(&results, "copper.minimum_board_edge_clearance");
        assert!(matches!(edge.status, report::RuleStatus::Pass));
        assert_eq!(edge.checked, 2);
    }

    #[test]
    fn generated_array_tooling_holes_are_measured_to_their_own_rail() {
        let array = array_of(BOARD, 10.0);
        let fab = fab_panel_of(&array);
        let pdk = format!(
            "{PDK}
[[rules.copper.hole_clearance]]
id = \"npth-copper\"
select = {{ hole = \"npth\" }}
limit = {{ minimum = \"0.2 mm\" }}

[[rules.drilling.hole_to_board_edge_clearance]]
id = \"npth-edge\"
select = {{ hole = \"npth\" }}
limit = {{ minimum = \"0.5 mm\" }}
"
        );
        for (xml, arrays) in [(&array, 1), (&fab, 2)] {
            let results = check_with_pdk(xml, LayoutTarget::BoardArray, &pdk);
            let tooling = rule(&results, "npth-edge").checked;
            assert!(
                tooling > 0 && tooling.is_multiple_of(arrays),
                "{tooling} tooling holes"
            );
            // Through-board: every tooling hole meets both copper layers.
            assert_eq!(rule(&results, "npth-copper").checked, 2 * tooling);
            assert!(results.findings.is_empty(), "{:?}", results.findings);
        }
    }

    /// A board with two nets 0.1 mm apart, in a cell whose own NPTH hole sits
    /// 0.1 mm from the board's copper, in a panel of three cells whose own NPTH
    /// hole sits 0.1 mm from the last board's copper.
    const NESTED_PANEL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="panel"/>
    <LayerRef name="TOP"/><LayerRef name="BOTTOM"/><LayerRef name="DRILL"/>
  </Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
    <Layer name="BOTTOM" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
    <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
    <Stackup name="Primary" overallThickness="0.07" tolPlus="0" tolMinus="0" whereMeasured="METAL" stackupStatus="PROPOSED">
      <StackupGroup name="Primary_Group" thickness="0.07" tolPlus="0" tolMinus="0">
        <StackupLayer layerOrGroupRef="TOP" thickness="0.035" tolPlus="0" tolMinus="0" sequence="0"/>
        <StackupLayer layerOrGroupRef="BOTTOM" thickness="0.035" tolPlus="0" tolMinus="0" sequence="1"/>
      </StackupGroup>
    </Stackup>
    <Step name="board" type="BOARD"><Datum x="0" y="0"/>
      <Profile><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="10" y="0"/><PolyStepSegment x="10" y="10"/><PolyStepSegment x="0" y="10"/><PolyStepSegment x="0" y="0"/></Polygon></Profile>
      <LayerFeature layerRef="TOP">
        <Set net="A" polarity="POSITIVE"><Features><Contour><Polygon><PolyBegin x="2" y="2"/><PolyStepSegment x="4" y="2"/><PolyStepSegment x="4" y="8"/><PolyStepSegment x="2" y="8"/><PolyStepSegment x="2" y="2"/></Polygon></Contour></Features></Set>
        <Set net="B" polarity="POSITIVE"><Features><Contour><Polygon><PolyBegin x="4.1" y="2"/><PolyStepSegment x="9" y="2"/><PolyStepSegment x="9" y="8"/><PolyStepSegment x="4.1" y="8"/><PolyStepSegment x="4.1" y="2"/></Polygon></Contour></Features></Set>
      </LayerFeature>
    </Step>
    <Step name="cell" type="PALLET"><Datum x="0" y="0"/>
      <StepRepeat stepRef="board" x="1" y="1" nx="1" ny="1" dx="0" dy="0" angle="0" mirror="false"/>
      <LayerFeature layerRef="DRILL"><Set polarity="POSITIVE">
        <Hole name="bite" diameter="0.4" platingStatus="NONPLATED" x="10.3" y="6"/>
      </Set></LayerFeature>
    </Step>
    <Step name="panel" type="PALLET"><Datum x="0" y="0"/>
      <StepRepeat stepRef="cell" x="5" y="5" nx="3" ny="1" dx="12" dy="0" angle="0" mirror="false"/>
      <LayerFeature layerRef="DRILL"><Set polarity="POSITIVE">
        <Hole name="tooling" diameter="1" platingStatus="NONPLATED" x="39.6" y="12"/>
      </Set></LayerFeature>
    </Step>
  </CadData></Ecad>
</IPC-2581>"#;

    #[test]
    fn a_measurement_is_made_once_in_the_lowest_step_holding_its_subjects() {
        let pdk = r#"schema_version = 2
default_profile = "test"

[pdk]
id = "frames-test"
name = "Frames test"
revision = "1"

[profiles.test]
name = "Test"

[[rules.copper.clearance]]
id = "copper"
limit = { minimum = "0.2 mm" }

[[rules.copper.hole_clearance]]
id = "npth-copper"
select = { hole = "npth" }
limit = { minimum = "0.2 mm" }
"#;
        let array = check_with_pdk(NESTED_PANEL, LayoutTarget::BoardArray, pdk);
        let placements = |step: &str| {
            array
                .frames
                .iter()
                .find(|frame| frame.step == step)
                .unwrap()
                .placements
                .iter()
                .map(|placement| (placement.instance, placement.transform[4]))
                .collect::<Vec<_>>()
        };
        assert_eq!(placements("panel"), [(None, 0.0)]);
        assert_eq!(
            placements("cell"),
            [(Some(0), 5.0), (Some(1), 17.0), (Some(2), 29.0)]
        );
        assert_eq!(
            placements("board"),
            [(Some(3), 6.0), (Some(4), 18.0), (Some(5), 30.0)]
        );

        let found = array
            .findings
            .iter()
            .map(|finding| {
                let point = finding.location.point.unwrap();
                (
                    finding.rule_id.as_str(),
                    array.frames[finding.frame as usize].step.as_str(),
                    (point.x * 100.0).round() / 100.0,
                    finding
                        .measurement
                        .actual_mm()
                        .map(|mm| (mm * 1e6).round() / 1e6),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            found,
            [
                // The two nets are the board's own: found once, in its frame,
                // for all three boards.
                ("copper", "board", 4.05, Some(0.1)),
                // The tooling hole is the panel's and the copper a board's.
                ("npth-copper", "panel", 39.05, Some(0.1)),
                // Hole and board meet in the cell: once, in the cell's frame.
                ("npth-copper", "cell", 10.05, Some(0.1)),
            ]
        );
        // Each hole counts on both layers at every placement of its own Step.
        assert_eq!(rule(&array, "npth-copper").checked, 2 * (1 + 3));

        // The board is measured exactly as it is on its own.
        let board = check_with_pdk(NESTED_PANEL, LayoutTarget::Board, pdk);
        assert_eq!(board.findings.len(), 1);
        assert_eq!(board.findings[0].id, array.findings[0].id);
        assert_eq!(
            serde_json::to_value(&board.findings[0].sites).unwrap(),
            serde_json::to_value(&array.findings[0].sites).unwrap()
        );
    }

    #[test]
    fn neighbouring_placements_are_measured_against_each_other_where_both_are_placed() {
        // Copper and a mask opening reach 0.02 mm from both side edges of a
        // 10 mm board, so boards placed edge to edge leave 0.04 mm between.
        let rectangle = |x0: f64, x1: f64| {
            format!(
                r#"<Features><Contour><Polygon><PolyBegin x="{x0}" y="2"/><PolyStepSegment x="{x1}" y="2"/><PolyStepSegment x="{x1}" y="8"/><PolyStepSegment x="{x0}" y="8"/><PolyStepSegment x="{x0}" y="2"/></Polygon></Contour></Features>"#
            )
        };
        let layer = |name: &str, net: &str| {
            format!(
                r#"<LayerFeature layerRef="{name}"><Set{net} polarity="POSITIVE">{}</Set><Set{net} polarity="POSITIVE">{}</Set></LayerFeature>"#,
                rectangle(0.02, 3.0),
                rectangle(7.0, 9.98),
            )
        };
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="panel"/>
    <LayerRef name="TOP"/><LayerRef name="F.Mask"/>
  </Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
    <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
    <Step name="board" type="BOARD"><Datum x="0" y="0"/>{}{}</Step>
    <Step name="panel" type="PALLET"><Datum x="0" y="0"/>
      <StepRepeat stepRef="board" x="0" y="0" nx="3" ny="1" dx="10" dy="0" angle="0" mirror="false"/>
    </Step>
  </CadData></Ecad>
</IPC-2581>"#,
            layer("TOP", r#" net="N""#),
            layer("F.Mask", ""),
        );
        let pdk = r#"schema_version = 2
default_profile = "test"

[pdk]
id = "neighbours-test"
name = "Neighbours test"
revision = "1"

[profiles.test]
name = "Test"

[[rules.copper.clearance]]
id = "copper"
limit = { minimum = "0.1 mm" }

[[rules.soldermask.web]]
id = "web"
limit = { minimum = "0.1 mm" }
"#;
        let results = check_with_pdk(&xml, LayoutTarget::BoardArray, pdk);
        for rule_id in ["copper", "web"] {
            let found = results
                .findings
                .iter()
                .filter(|finding| finding.rule_id == rule_id)
                .map(|finding| {
                    let frame = &results.frames[finding.frame as usize];
                    assert_eq!((frame.step.as_str(), frame.placements.len()), ("panel", 1));
                    let point = finding.location.point.unwrap();
                    (
                        point.x.round(),
                        (finding.measurement.actual_mm().unwrap() * 1e6).round() / 1e6,
                    )
                })
                .collect::<Vec<_>>();
            // One net on every board: only what lies between two boards is
            // found, once for each pair of neighbours.
            assert_eq!(found, [(10.0, 0.04), (20.0, 0.04)], "{rule_id}");
        }
    }

    #[test]
    fn a_vscore_line_is_measured_once_by_the_board_it_crosses_everywhere() {
        // The trace's copper ends 0.3 mm from the board's bottom edge.
        let board = BOARD.replace(
            r#"startY="1" endX="29" endY="1""#,
            r#"startY="0.4" endX="29" endY="0.4""#,
        );
        let array = array_of(&board, 5.0);
        let results = check(&array, LayoutTarget::BoardArray);
        let vscore = results
            .findings
            .iter()
            .filter(|finding| finding.rule_id == "copper.minimum_vscore_to_copper_clearance")
            .collect::<Vec<_>>();
        let [finding] = vscore.as_slice() else {
            panic!("one line comes too close, to one layer: {vscore:?}");
        };
        assert!((finding.measurement.actual_mm().unwrap() - 0.3).abs() < 1e-8);
        let frame = &results.frames[finding.frame as usize];
        assert_eq!(frame.step, "board");
        assert_eq!(frame.placements.len(), 4, "the array scores every board");
        // The array draws the line; the board meets it in its own frame.
        assert_eq!(
            finding.subjects[0]
                .provenance
                .as_ref()
                .unwrap()
                .step
                .as_deref(),
            Some(results.frames[0].step.as_str())
        );
        let witness = finding.location.witnesses[0].point;
        assert!((0.0..=30.0).contains(&witness.x) && witness.y.abs() < 1e-9);
    }

    #[test]
    fn one_evaluator_scales_through_board_array_and_fab_panel_lowering() {
        let array = array_of(BOARD, 5.0);
        let fab = fab_panel_of(&array);

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
