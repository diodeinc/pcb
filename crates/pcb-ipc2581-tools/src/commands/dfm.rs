//! PDK-driven manufacturability checks for IPC-2581 geometry.

use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use pcb_ir::import::ipc2581::import_design;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::LayoutTarget;
use crate::ipc2581::Ipc2581;
use crate::utils::file as file_utils;

mod builtin_pdks;
mod checks;
mod design;
mod pdk;
mod report;
mod rules;
mod scene;
mod waivers;

const MAX_REPORT_BYTES: usize = 128 * 1024 * 1024;
const MAX_PDK_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub struct CheckOptions {
    pub pdk: PathBuf,
    pub waivers: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub layout_target: LayoutTarget,
}

/// PDK source bytes plus the stable identity echoed into reports.
struct LoadedPdk {
    path: String,
    bytes: Cow<'static, [u8]>,
}

/// A parsed waiver file plus the identity the report echoes back.
struct LoadedWaivers {
    path: String,
    sha256: String,
    file: waivers::WaiverFile,
}

/// Reject an invalid destination before any layout preparation or output write.
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

fn build_report(file: &Path, options: &CheckOptions) -> Result<report::DfmReport> {
    let input_bytes = std::fs::read(file)
        .with_context(|| format!("failed to read IPC-2581 file {}", file.display()))?;
    let input = report::FileIdentity {
        path: file.display().to_string(),
        sha256: sha256(&input_bytes),
        size_bytes: input_bytes.len() as u64,
    };
    let content = file_utils::ipc_text(file, &input_bytes)?;
    let loaded_pdk = load_pdk(&options.pdk)?;
    let pdk_source = std::str::from_utf8(&loaded_pdk.bytes)
        .with_context(|| format!("PDK {} is not UTF-8", loaded_pdk.path))?;
    let pdk = pdk::Pdk::parse(pdk_source)
        .with_context(|| format!("failed to parse PDK {}", loaded_pdk.path))?;

    let rules =
        rules::lower(&pdk).with_context(|| format!("failed to lower PDK {}", loaded_pdk.path))?;
    if rules.is_empty() {
        bail!(
            "PDK {} configures no DFM rules; add at least one capability",
            loaded_pdk.path
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
    let ipc = Ipc2581::parse(&content).context("failed to parse IPC-2581 file")?;
    drop(content);
    drop(input_bytes);
    let imported = import_design(&ipc).context("failed to import IPC-2581 physical design")?;
    let design = design::Design::extract(&imported, options.layout_target.artwork_scope(), &rules)?;
    let checked = checks::run(
        &rules,
        &design,
        waivers.as_ref().map(|loaded| &loaded.file),
        generated_at.date_naive(),
    );

    let summary = summarize(&checked);
    let failed = summary.errors > 0;
    let layout = design.report_layout();
    let scene = scene::export(&design, &layout, &checked.rules, &checked.findings)?;
    Ok(report::DfmReport {
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
        input,
        pdk: report::PdkIdentity::from_pdk(
            &pdk,
            loaded_pdk.path,
            sha256(&loaded_pdk.bytes),
            pdk_source.to_owned(),
        ),
        layout_target: match options.layout_target {
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
        scene,
    })
}

fn load_pdk(reference: &Path) -> Result<LoadedPdk> {
    let loaded = if let Some(pdk) = reference.to_str().and_then(builtin_pdks::find) {
        LoadedPdk {
            path: format!("builtin:{}", pdk.name),
            bytes: Cow::Borrowed(pdk.source.as_bytes()),
        }
    } else {
        LoadedPdk {
            path: reference.display().to_string(),
            bytes: Cow::Owned(
                std::fs::read(reference)
                    .with_context(|| format!("failed to read PDK file {}", reference.display()))?,
            ),
        }
    };
    ensure!(
        loaded.bytes.len() <= MAX_PDK_BYTES,
        "PDK {} exceeds the {MAX_PDK_BYTES} byte limit",
        loaded.path
    );
    Ok(loaded)
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
        None => std::io::stdout()
            .lock()
            .write_all(&bytes)
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

    const PDK: &str = r#"schema_version = 1

[pdk]
id = "scope-test"
name = "Scope test"
revision = "1"

[capabilities.stackup]
minimum_copper_layer_count = 2
maximum_copper_layer_count = 4

[capabilities.copper]
minimum_vscore_to_copper_clearance = "0.5 mm"
minimum_board_edge_clearance = "0.5 mm"

[capabilities.panelization]
minimum_board_array_spacing = "300 mil"
"#;

    fn check(xml: &str, scope: ArtworkScope) -> checks::Results {
        check_with_pdk(xml, scope, PDK)
    }

    fn check_with_pdk(xml: &str, scope: ArtworkScope, pdk_source: &str) -> checks::Results {
        let ipc = Ipc2581::parse(xml).unwrap();
        let pdk = pdk::Pdk::parse(pdk_source).unwrap();
        let rules = rules::lower(&pdk).unwrap();
        let imported = import_design(&ipc).unwrap();
        let design = design::Design::extract(&imported, scope, &rules).unwrap();
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
    fn loads_the_embedded_standard_pdk() {
        let loaded = load_pdk(Path::new("standard")).unwrap();
        let parsed = pdk::Pdk::parse(std::str::from_utf8(&loaded.bytes).unwrap()).unwrap();

        assert_eq!(loaded.path, "builtin:standard");
        assert_eq!(parsed.pdk.id, "standard");
        assert_eq!(parsed.pdk.name, "Standard");
        assert_eq!(parsed.pdk.manufacturer.as_deref(), Some("Diode"));
        assert_eq!(parsed.pdk.process.as_deref(), Some("Standard"));
        assert_eq!(
            parsed.capabilities.stackup.minimum_copper_layer_count,
            Some(2)
        );
        assert_eq!(
            parsed.capabilities.stackup.maximum_copper_layer_count,
            Some(10)
        );
        assert!(!rules::lower(&parsed).unwrap().is_empty());
    }

    #[test]
    fn still_loads_a_pdk_from_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("custom.toml");
        std::fs::write(&path, PDK).unwrap();

        let loaded = load_pdk(&path).unwrap();

        assert_eq!(loaded.path, path.display().to_string());
        assert_eq!(&*loaded.bytes, PDK.as_bytes());
    }

    #[test]
    fn rejects_oversize_pdk_source() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("oversize.toml");
        std::fs::write(&path, vec![b' '; MAX_PDK_BYTES + 1]).unwrap();

        let error = load_pdk(&path).err().unwrap();

        assert!(error.to_string().contains("exceeds the 1048576 byte limit"));
    }

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
    fn copper_layer_count_checks_both_bounds_from_the_physical_stackup() {
        let below_minimum = check_with_pdk(
            BOARD,
            ArtworkScope::Board,
            &PDK.replace(
                "minimum_copper_layer_count = 2",
                "minimum_copper_layer_count = 3",
            ),
        );
        let minimum_rule = rule(&below_minimum, "stackup.minimum_copper_layer_count");
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
            ArtworkScope::Board,
            &PDK.replace(
                "minimum_copper_layer_count = 2\nmaximum_copper_layer_count = 4",
                "minimum_copper_layer_count = 1\nmaximum_copper_layer_count = 1",
            ),
        );
        let maximum_rule = rule(&above_maximum, "stackup.maximum_copper_layer_count");
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
    fn copper_layer_count_rejects_an_incomplete_physical_stackup() {
        let ipc = Ipc2581::parse(&BOARD.replace(
            "layerOrGroupRef=\"BOTTOM\"",
            "layerOrGroupRef=\"DIELECTRIC\"",
        ))
        .unwrap();
        let pdk = pdk::Pdk::parse(PDK).unwrap();
        let rules = rules::lower(&pdk).unwrap();
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
