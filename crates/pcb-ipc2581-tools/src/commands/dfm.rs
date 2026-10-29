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
#[cfg(feature = "cli")]
mod store;

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
/// no filesystem, environment, or clock access; supplying the same inputs
/// yields the same report.
#[derive(Debug)]
pub struct CheckRequest<'a> {
    pub input: report::FileIdentity,
    pub pdk: PdkSource<'a>,
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
/// `incomplete`, never passed. Only invalid inputs return an error, and a
/// flexible design the PDK has no qualified profile for: none of its rules
/// can judge it.
pub fn check(
    imported: &ImportedDesign,
    request: CheckRequest<'_>,
    resolution: Resolution,
) -> Result<DfmReport> {
    check_with_scene(imported, request, resolution, false)
}

/// [`check`], also drawing the report's scene when `scene` is set. Only the
/// database reads the scene, so a summary-only run skips building it.
fn check_with_scene(
    imported: &ImportedDesign,
    request: CheckRequest<'_>,
    resolution: Resolution,
    scene: bool,
) -> Result<DfmReport> {
    let (pdk_path, pdk_source, explicit_profile) = match request.pdk {
        PdkSource::Builtin(name) => {
            let pdk = builtin_pdks::find(name)
                .with_context(|| format!("unknown built-in PDK '{name}'"))?;
            (format!("builtin:{}", pdk.name), pdk.source, pdk.profile)
        }
        PdkSource::Toml(source) => (source.path.to_owned(), source.source, None),
    };
    ensure!(
        pdk_source.len() <= MAX_PDK_BYTES,
        "PDK {pdk_path} exceeds the {MAX_PDK_BYTES} byte limit"
    );
    let pdk =
        pdk::Pdk::parse(pdk_source).with_context(|| format!("failed to parse PDK {pdk_path}"))?;
    let scope = request.layout_target.artwork_scope();
    let flexible = design::flexible_technology(imported);
    let technology = flexible.as_ref().map(|(technology, _)| *technology);
    let selection = match explicit_profile {
        Some(profile) => report::ProfileSelection::explicit(profile),
        None => select_profile(&pdk, imported, scope, resolution, technology)
            .with_context(|| format!("failed to select a profile of PDK {pdk_path}"))?,
    };
    // A flexible board is not a rigid one with odd properties: no rigid
    // limit qualifies it, so a kit without a profile for it cannot check it.
    if let (Some((technology, materials)), Some(profile)) =
        (&flexible, pdk.profiles.get(&selection.profile))
        && !qualifies(profile, Some(*technology))
    {
        bail!(
            "the stackup's dielectric is {}, a {} design, and PDK {pdk_path} ('{}') has no qualified {} process: profile '{}' lists technologies [{}]. No rule of this PDK applies; check the design against a PDK with a {} profile",
            materials.join(", "),
            technology.label(),
            pdk.pdk.name,
            technology.label(),
            selection.profile,
            profile
                .technologies
                .iter()
                .map(|technology| technology.label())
                .collect::<Vec<_>>()
                .join(", "),
            technology.label(),
        );
    }
    let mut rules = rules::lower(&pdk, Some(&selection.profile))
        .with_context(|| format!("failed to lower PDK {pdk_path}"))?;
    if rules.is_empty() {
        bail!("PDK {pdk_path} configures no DFM rules; add at least one capability");
    }
    rules.push(rules::board_outline());
    let designs = design::Design::frames(imported, scope, &rules, resolution)?;
    let checked = checks::run(&rules, &designs)?;
    let summary = summarize(&checked);
    let layout = designs[0].report_layout();
    let frames = checked
        .frames
        .iter()
        .map(|(design, placements)| designs[*design as usize].report_frame(placements))
        .collect::<Vec<_>>();
    let scene = scene
        .then(|| {
            scene::export(
                &designs,
                &layout,
                &checked.rules,
                &frames,
                &checked.findings,
            )
        })
        .transpose()?;
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
        tool: report::ToolIdentity::PCB,
        input: request.input,
        pdk: report::PdkIdentity::from_pdk(
            &pdk,
            selection,
            pdk_path,
            sha256(pdk_source.as_bytes()),
            pdk_source.to_owned(),
        ),
        layout_target: layout_target_label(request.layout_target),
        layout,
        coordinate_system: report::CoordinateSystem {
            unit: "mm",
            axes: "x_right_y_up",
            origin: "ipc_2581_design",
        },
        summary,
        rules: checked.rules,
        frames,
        findings: checked.findings,
        scene,
    })
}

/// The profile a kit applies to a design when no profile is named: the first
/// executable profile whose support the design meets, the default first and
/// the rest in name order. A kit of one profile always runs it, so its
/// support rules report what the design lacks. When the design cannot be
/// qualified for a profile considered before one it meets, because what the
/// support reads is missing, or when it meets none, the default runs and its
/// support rules report what the design lacks.
fn select_profile(
    pdk: &pdk::Pdk,
    imported: &ImportedDesign,
    scope: pcb_ir::dialects::ipc::ArtworkScope,
    resolution: Resolution,
    technology: Option<pdk::Technology>,
) -> Result<report::ProfileSelection> {
    let candidates = std::iter::once(pdk.default_profile.as_str())
        .chain(
            pdk.profiles
                .keys()
                .map(String::as_str)
                .filter(|name| *name != pdk.default_profile),
        )
        .filter(|name| pdk.profiles[*name].status == pdk::ProfileStatus::Executable)
        .filter(|name| qualifies(&pdk.profiles[*name], technology))
        .collect::<Vec<_>>();
    let default = report::ProfileSelection::automatic(&pdk.default_profile, Vec::new());
    match candidates.as_slice() {
        // Lowering reports a kit with no executable profile.
        [] => return Ok(default),
        [only] => return Ok(report::ProfileSelection::automatic(only, Vec::new())),
        _ => {}
    }
    let support = candidates
        .iter()
        .map(|name| rules::support(&pdk.profiles[*name]))
        .collect::<Vec<_>>();
    let designs = design::Design::frames(imported, scope, &support.concat(), resolution)?;
    let mut rejected = Vec::new();
    for (name, support) in candidates.into_iter().zip(&support) {
        let checked = checks::run(support, &designs)?;
        if checked.rules.iter().any(report::RuleResult::blocks_verdict) {
            return Ok(default);
        }
        if checked.findings.is_empty() {
            return Ok(report::ProfileSelection::automatic(name, rejected));
        }
        rejected.push(report::RejectedProfile {
            profile: name.to_owned(),
            reasons: checked
                .findings
                .into_iter()
                .map(|finding| finding.message)
                .collect(),
        });
    }
    rejected.retain(|profile| profile.profile != pdk.default_profile);
    Ok(report::ProfileSelection::automatic(
        &pdk.default_profile,
        rejected,
    ))
}

/// Whether a profile qualifies a design of `technology`. A flexible design
/// needs a profile that lists its technology: one that lists none states no
/// process, so it is never taken for a flex one. A design not shown to be
/// flexible is unconstrained, since a rigid stackup rarely states its
/// material and `technologies` was descriptive metadata before.
fn qualifies(profile: &pdk::Profile, technology: Option<pdk::Technology>) -> bool {
    technology.is_none_or(|technology| profile.technologies.contains(&technology))
}

#[cfg(feature = "cli")]
#[derive(Debug)]
pub struct CheckOptions {
    pub pdk: PathBuf,
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
    for source in [Some(file), pdk_file].into_iter().flatten() {
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
    write_report(options, &report.summary_record(), || {
        store::database(&report)
    })?;

    let summary = &report.summary;
    let selection = &report.pdk.profile_selection;
    for rejected in &selection.rejected {
        eprintln!(
            "PDK profile '{}' does not fit: {}",
            rejected.profile,
            rejected.reasons.join("; ")
        );
    }
    if !selection.rejected.is_empty() {
        eprintln!("checking with PDK profile '{}'", selection.profile);
    }
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
/// The caller must already have checked `options` with [`validate_output`].
#[cfg(feature = "cli")]
pub fn write_error_report(
    file: &Path,
    options: &CheckOptions,
    error: &anyhow::Error,
) -> Result<()> {
    let incomplete = serde_json::json!({
        "schema_version": report::REPORT_SCHEMA_VERSION,
        "generated_at": generation_time().to_rfc3339(),
        "verdict": "incomplete",
        "tool": report::ToolIdentity::PCB,
        "input": { "path": file.display().to_string() },
        "pdk": { "path": options.pdk.display().to_string() },
        "layout_target": layout_target_label(options.layout_target),
        "error": { "message": format!("{error:#}") },
    });
    write_report(options, &incomplete, || {
        store::incomplete_database(&incomplete)
    })
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
    let generated_at = generation_time();
    let content = file_utils::ipc_text(file, &input_bytes)?;
    let ipc = Ipc2581::parse(&content).context("failed to parse IPC-2581 file")?;
    drop(content);
    drop(input_bytes);
    let imported =
        import_design(&ipc, resolution).context("failed to import IPC-2581 physical design")?;
    check_with_scene(
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
            layout_target: options.layout_target,
            generated_at,
        },
        resolution,
        options.output.is_some(),
    )
}

/// How a report names its layout target.
fn layout_target_label(target: LayoutTarget) -> &'static str {
    match target {
        LayoutTarget::Board => "board",
        LayoutTarget::BoardArray => "board_array",
    }
}

/// The non-verdict counts worth surfacing next to the pass/fail line.
#[cfg(feature = "cli")]
fn annotations(summary: &report::Summary) -> String {
    [
        (summary.rules_incomplete, "not evaluated"),
        (summary.rules_not_applicable, "not applicable"),
        (summary.warnings, "warning(s)"),
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
    let findings = |severity: Severity| {
        let findings = checked.findings.iter();
        findings
            .filter(|finding| finding.severity == severity)
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
        errors: findings(Severity::Error),
        warnings: findings(Severity::Warning),
        unresolved: checked.rules.iter().map(|rule| rule.unresolved.len()).sum(),
    }
}

/// Write the database to the output path, if any, replacing it only once
/// the whole file is written; then print the summary as one line of JSON.
#[cfg(feature = "cli")]
fn write_report(
    options: &CheckOptions,
    summary: &impl Serialize,
    database: impl FnOnce() -> Result<Vec<u8>>,
) -> Result<()> {
    if let Some(path) = options.output.as_deref() {
        let bytes = database()?;
        atomicwrites::AtomicFile::new(path, atomicwrites::OverwriteBehavior::AllowOverwrite)
            .write(|file| file.write_all(&bytes))
            .with_context(|| format!("failed to replace DFM report {}", path.display()))?;
    }
    let mut line = serde_json::to_vec(summary)?;
    line.push(b'\n');
    pcb_ui::write_stdout(|stdout| stdout.write_all(&line))
        .context("failed to write the DFM summary to stdout")
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
            layout_target,
            generated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        }
    }

    /// The report of `xml` checked against a PDK source, with its scene.
    pub fn report(xml: &str, pdk: &str, target: LayoutTarget) -> DfmReport {
        check_with_scene(
            &import(xml),
            request(xml, pdk, target),
            Resolution::default(),
            true,
        )
        .unwrap()
    }

    /// The engine's results for the board of `xml`.
    pub fn run_board(xml: &str, pdk: &str) -> checks::Results {
        let (imported, rules) = (import(xml), rules(pdk));
        let design = design::Design::board(&imported, &rules, Resolution::default());
        checks::run(&rules, std::slice::from_ref(&design)).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;
    use pcb_ir::geom::path::{ContourBuf, PathCmd};
    use pcb_ir::geom::{FillRule, LineCap, Paint, Point, Span, StrokeStyle};

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
            assert!(!rules::lower(&parsed, builtin.profile).unwrap().is_empty());
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

                let limits = |of: fn(&rules::RuleKind) -> bool| {
                    let rules = rules.iter().filter(|rule| of(&rule.kind));
                    rules
                        .map(|rule| rule.limit.length().millimeters())
                        .collect::<Vec<_>>()
                };
                use rules::RuleKind::*;
                assert_eq!(
                    limits(|kind| matches!(kind, HoleToCopperClearance(_))),
                    [copper_clearance; 3]
                );
                assert_eq!(
                    limits(|kind| matches!(kind, HoleToBoardEdgeClearance(_))),
                    [edge_clearance; 3]
                );
                assert_eq!(
                    limits(|kind| matches!(kind, SlotToBoardEdgeClearance(_))),
                    [edge_clearance; 2]
                );
            }
        }

        let jlc = builtin_pdks::find("jlcpcb-1oz").unwrap();
        let parsed = pdk::Pdk::parse(jlc.source).unwrap();
        let rules = rules::lower(&parsed, jlc.profile).unwrap();
        let mask = rules
            .iter()
            .find(|rule| rule.id == "jlc.soldermask.minimum_web.preferred")
            .unwrap();
        assert_eq!(mask.limit.length().millimeters(), 0.10);
        let two_layer = rules
            .iter()
            .find(|rule| rule.id == "jlc.copper.minimum_feature_width.2-layer-1oz")
            .unwrap();
        assert_eq!(two_layer.limit.length().millimeters(), 0.10);
        assert_eq!(two_layer.conditions.minimum_copper_layers, Some(2));
        assert_eq!(two_layer.conditions.maximum_copper_layers, Some(2));
        let multilayer = rules
            .iter()
            .find(|rule| rule.id == "jlc.copper.minimum_feature_width.multilayer-outer-1oz")
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

    #[cfg(feature = "cli")]
    #[test]
    fn database_report_is_reproducible_and_holds_every_finding() {
        let ipc = builtin_pdks::find("ipc").unwrap();
        let board = BOARD.replace(
            "</Profile>",
            r#"<Cutout>
              <PolyBegin x="10" y="1.3"/><PolyStepSegment x="20" y="1.3"/>
              <PolyStepSegment x="20" y="5"/><PolyStepSegment x="10" y="5"/>
              <PolyStepSegment x="10" y="1.3"/>
            </Cutout></Profile>"#,
        );
        let report = check_with_pdk(&board, LayoutTarget::Board, ipc.source);
        assert!(!report.findings.is_empty());
        let bytes = store::database(&report).unwrap();
        assert_eq!(bytes, store::database(&report).unwrap());

        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .deserialize_read_exact("main", bytes.as_slice(), bytes.len(), true)
            .unwrap();
        let count = |sql: &str| {
            connection
                .query_row(sql, [], |row| row.get::<_, i64>(0))
                .map(|count| count as usize)
        };
        assert_eq!(
            count("SELECT count(*) FROM findings").unwrap(),
            report.findings.len()
        );
        assert_eq!(
            count("SELECT count(*) FROM sites").unwrap(),
            report
                .findings
                .iter()
                .map(|finding| finding.sites.len())
                .sum::<usize>()
        );
        assert_eq!(
            count("SELECT count(*) FROM sites, json_each(sites.evidence) e JOIN shapes s ON s.id = e.value ->> 1").unwrap(),
            report
                .findings
                .iter()
                .flat_map(|finding| &finding.sites)
                .map(|site| site.evidence.len())
                .sum::<usize>()
        );
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
              <PolyBegin x="5.15" y="2"/><PolyStepSegment x="8" y="2"/>
              <PolyStepSegment x="8" y="8"/><PolyStepSegment x="5.15" y="8"/>
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
        let board = BOARD
            .replace(copper, mask_web)
            .replace("overallThickness=\"0.07\"", "overallThickness=\"1.6\"");
        let standard = builtin_pdks::find("standard").unwrap();

        for (gap, warnings) in [(0.08, 1), (0.15, 0)] {
            let xml = board.replace("5.15", &(5.0 + gap).to_string());
            let results = check_with_pdk(&xml, LayoutTarget::Board, standard.source);
            let mask_rule = rule(&results, "soldermask.minimum_web.preferred");

            assert!(matches!(results.verdict, report::Verdict::Pass));
            assert_eq!(results.summary.errors, 0);
            assert_eq!(results.summary.warnings, warnings, "{gap} mm mask web");
            assert_eq!(mask_rule.severity, report::Severity::Warning);
            assert_eq!(mask_rule.finding_count, warnings);
        }
    }

    #[test]
    fn standard_reports_incomplete_assembly_data_by_diagnostic_kind() {
        const ASSEMBLY: &str = include_str!("../assembly/testdata/report.xml");
        let run = |xml: &str, pdk| {
            let imported = fixtures::import(xml);
            super::check(
                &imported,
                CheckRequest {
                    input: report::FileIdentity::new("assembly.xml", xml.as_bytes()),
                    pdk,
                    layout_target: LayoutTarget::BoardArray,
                    generated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
                },
                Resolution::default(),
            )
            .unwrap()
        };

        let incomplete = run(ASSEMBLY, PdkSource::Builtin("standard"));
        let missing_population = rule(&incomplete, "assembly.missing_population");
        assert!(matches!(
            missing_population.status,
            report::RuleStatus::Fail
        ));
        assert_eq!(missing_population.finding_count, 1);
        assert!(matches!(incomplete.verdict, report::Verdict::Fail));
        let finding = incomplete
            .findings
            .iter()
            .find(|finding| finding.rule_id == missing_population.id)
            .unwrap();
        assert_eq!(
            finding.subjects[0].reference_designator.as_deref(),
            Some("U2")
        );
        assert_eq!(
            incomplete.frames[finding.frame as usize].placements.len(),
            2
        );

        let standard = builtin_pdks::find("standard").unwrap();
        let from_file = run(
            ASSEMBLY,
            PdkSource::Toml(TextSource {
                path: "standard.toml",
                source: standard.source,
            }),
        );
        assert_eq!(
            rule(&from_file, "assembly.missing_population").finding_count,
            1
        );

        let complete_xml = ASSEMBLY.replace(
            r#"<RefDes name="U2" packageRef="pkg-smt" layerRef="TOP"/>"#,
            r#"<RefDes name="U2" packageRef="pkg-smt" populate="true" layerRef="TOP"/>"#,
        );
        let complete = run(&complete_xml, PdkSource::Builtin("standard"));
        assert!(
            complete
                .rules
                .iter()
                .filter(|rule| rule.id.starts_with("assembly."))
                .all(|rule| matches!(rule.status, report::RuleStatus::Pass))
        );
        assert!(
            complete
                .findings
                .iter()
                .all(|finding| !finding.rule_id.starts_with("assembly."))
        );

        const U1_COPPER_LAND: &str = r#"<Set><Pad padstackDefRef="smt-padstack"><Location x="2" y="2"/><StandardPrimitiveRef id="land"/><PinRef componentRef="U1" pin="1"/></Pad></Set>"#;
        assert!(complete_xml.contains(U1_COPPER_LAND));
        let missing_termination_xml = complete_xml.replacen(U1_COPPER_LAND, "", 1);
        let missing_termination = run(&missing_termination_xml, PdkSource::Builtin("standard"));
        let rule = rule(
            &missing_termination,
            "assembly.missing_physical_terminations",
        );
        assert!(matches!(rule.status, report::RuleStatus::Fail));
        assert_eq!(rule.finding_count, 1);
        assert_eq!(
            missing_termination
                .findings
                .iter()
                .find(|finding| finding.rule_id == rule.id)
                .unwrap()
                .subjects[0]
                .reference_designator
                .as_deref(),
            Some("U1")
        );
    }

    #[test]
    fn assembly_termination_extraction_failure_blocks_only_its_rule() {
        const ASSEMBLY: &str = include_str!("../assembly/testdata/report.xml");
        let mut imported = fixtures::import(ASSEMBLY);
        let top = imported.layer_id("TOP").unwrap();
        let document_layer = imported
            .step_layers
            .iter()
            .find(|layer| layer.layer == top)
            .unwrap()
            .document_layer as usize;
        let feature = imported.geometry.layers[document_layer].features.start as usize;
        let start = imported.geometry.arena.paths.len() as u32;
        imported.geometry.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 1.0)),
                PathCmd::close(),
            ])],
        );
        imported.geometry.push_path(
            Paint::Stroke(StrokeStyle::new(0.1, LineCap::Round)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 1.0)),
            ])],
        );
        imported.geometry.features[feature].paths = Span::new(start, 2);

        let standard = builtin_pdks::find("standard").unwrap();
        let pdk = pdk::Pdk::parse(standard.source).unwrap();
        let rules = rules::lower(&pdk, standard.profile).unwrap();
        let designs = design::Design::frames(
            &imported,
            LayoutTarget::BoardArray.artwork_scope(),
            &rules,
            Resolution::default(),
        )
        .unwrap();
        let checked = checks::run(&rules, &designs).unwrap();
        let status = |id| {
            &checked
                .rules
                .iter()
                .find(|rule| rule.id == id)
                .unwrap()
                .status
        };

        assert!(matches!(
            status("assembly.missing_physical_terminations"),
            &report::RuleStatus::Incomplete
        ));
        assert!(matches!(
            status("assembly.missing_population"),
            &report::RuleStatus::Fail
        ));
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
    fn in_memory_report_keeps_source_identity() {
        let pdk_source = PDK.replace("minimum = 2", "minimum = 3");
        let report = super::check(
            &fixtures::import(BOARD),
            CheckRequest {
                generated_at: NaiveDate::from_ymd_opt(2026, 8, 30)
                    .unwrap()
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc(),
                ..fixtures::request(BOARD, &pdk_source, LayoutTarget::Board)
            },
            Resolution::default(),
        )
        .unwrap();
        assert!(matches!(report.verdict, report::Verdict::Fail));
        assert_eq!(report.summary.errors, 1);
        assert_eq!(report.input.sha256, sha256(BOARD.as_bytes()));
        assert_eq!(report.pdk.sha256, sha256(pdk_source.as_bytes()));
        assert_eq!(report.generated_at, "2026-08-30T00:00:00+00:00");
    }

    #[cfg(feature = "cli")]
    #[test]
    fn cli_report_matches_in_memory_report_for_compressed_input() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("board.xml.zst");
        let pdk = directory.path().join("custom.toml");
        let output = directory.path().join("report.dfm");
        let pdk_source = PDK.replace("minimum = 2", "minimum = 3");
        let bytes = zstd::encode_all(BOARD.as_bytes(), 0).unwrap();
        std::fs::write(&input, &bytes).unwrap();
        std::fs::write(&pdk, &pdk_source).unwrap();
        let outcome = execute_check(
            &input,
            &CheckOptions {
                pdk: pdk.clone(),
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
        let cli = std::fs::read(output).unwrap();
        let generated_at: String = rusqlite::Connection::open_in_memory()
            .and_then(|mut connection| {
                connection.deserialize_read_exact("main", cli.as_slice(), cli.len(), true)?;
                connection.query_row(
                    "SELECT value ->> '$' FROM report WHERE key = 'generated_at'",
                    [],
                    |row| row.get(0),
                )
            })
            .unwrap();
        let mut report = check_with_pdk(BOARD, LayoutTarget::Board, &pdk_source);
        report.input = report::FileIdentity::new(input.display().to_string(), &bytes);
        report.pdk.path = pdk.display().to_string();
        report.generated_at = generated_at;
        assert_eq!(cli, store::database(&report).unwrap());
    }

    #[cfg(feature = "cli")]
    #[test]
    fn a_failed_report_write_leaves_the_destination_and_no_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let options = |output: PathBuf| CheckOptions {
            pdk: "standard".into(),
            output: Some(output),
            layout_target: LayoutTarget::Board,
        };
        let summary = serde_json::json!({"verdict": "incomplete"});

        let file = directory.path().join("report.dfm");
        std::fs::write(&file, b"previous report").unwrap();
        let error = write_report(&options(file.clone()), &summary, || {
            anyhow::bail!("database failed")
        })
        .unwrap_err();
        assert!(error.to_string().contains("database failed"));
        assert_eq!(std::fs::read(file).unwrap(), b"previous report");

        // A destination that cannot be replaced: a directory.
        let occupied = directory.path().join("occupied");
        std::fs::create_dir(&occupied).unwrap();
        std::fs::write(occupied.join("sentinel"), b"untouched").unwrap();
        let error =
            write_report(&options(occupied.clone()), &summary, || Ok(Vec::new())).unwrap_err();
        assert!(error.to_string().contains("failed to replace DFM report"));
        assert_eq!(
            std::fs::read(occupied.join("sentinel")).unwrap(),
            b"untouched"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    /// `BOARD` as a 1.6 mm board with `copper_mm` foil and a 0.12 mm net trace.
    fn weighted_board(copper_mm: &str) -> String {
        BOARD
            .replace("overallThickness=\"0.07\"", "overallThickness=\"1.6\"")
            .replace("thickness=\"0.035\"", &format!("thickness=\"{copper_mm}\""))
            .replace("lineWidth=\"0.2\"", "lineWidth=\"0.12\"")
            .replace(
                r#"<Set polarity="POSITIVE">"#,
                r#"<Set polarity="POSITIVE" net="N1">"#,
            )
    }

    fn check_builtin(xml: &str, name: &str) -> Result<DfmReport> {
        super::check(
            &fixtures::import(xml),
            CheckRequest {
                pdk: PdkSource::Builtin(name),
                ..fixtures::request(xml, "", LayoutTarget::Board)
            },
            Resolution::default(),
        )
    }

    #[test]
    fn standard_keeps_five_mil_copper_limits_for_both_copper_weights() {
        for copper in ["0.035", "0.07"] {
            for (width, gap, width_fails, gap_fails) in [
                (0.11, 0.20, true, false),
                (0.14, 0.20, false, false),
                (0.20, 0.11, false, true),
                (0.20, 0.14, false, false),
            ] {
                let y = 1.0 + width + gap;
                let second = format!(
                    r#"<LayerFeature layerRef="TOP"><Set net="N2"><Features>
                      <Line startX="1" startY="{y}" endX="29" endY="{y}">
                        <LineDesc lineWidth="{width}" lineEnd="ROUND"/>
                      </Line>
                    </Features></Set></LayerFeature>"#
                );
                let xml = weighted_board(copper)
                    .replace("lineWidth=\"0.12\"", &format!("lineWidth=\"{width}\""))
                    .replace("</Step>", &format!("{second}</Step>"));
                let checked = check_builtin(&xml, "standard").unwrap();
                assert_eq!(
                    rule(&checked, "copper.minimum_feature_width").status,
                    if width_fails {
                        report::RuleStatus::Fail
                    } else {
                        report::RuleStatus::Pass
                    },
                    "{copper} mm foil, {width} mm width"
                );
                assert_eq!(
                    rule(&checked, "copper.minimum_copper_clearance").status,
                    if gap_fails {
                        report::RuleStatus::Fail
                    } else {
                        report::RuleStatus::Pass
                    },
                    "{copper} mm foil, {gap} mm gap"
                );
                assert_eq!(
                    matches!(checked.verdict, report::Verdict::Fail),
                    width_fails || gap_fails,
                    "{:?}",
                    checked.findings
                );
            }
        }
    }

    #[test]
    fn standard_qualifies_copper_and_thickness_no_more_loosely_than_jlc() {
        for (top, bottom, thickness, passes) in [
            ("0.035", "0.035", "1.6", true),
            ("0.07", "0.07", "1.6", true),
            ("0.035", "0.07", "1.6", false),
            ("0.105", "0.105", "1.6", false),
            ("0.035", "0.035", "2.4", false),
        ] {
            let xml = weighted_board(top)
                .replace("lineWidth=\"0.12\"", "lineWidth=\"0.2\"")
                .replace(
                    &format!("layerOrGroupRef=\"BOTTOM\" thickness=\"{top}\""),
                    &format!("layerOrGroupRef=\"BOTTOM\" thickness=\"{bottom}\""),
                )
                .replace(
                    "overallThickness=\"1.6\"",
                    &format!("overallThickness=\"{thickness}\""),
                );
            for pdk in ["standard", "jlc"] {
                let checked = check_builtin(&xml, pdk).unwrap();
                assert_eq!(
                    matches!(checked.verdict, report::Verdict::Pass),
                    passes,
                    "{pdk}: {top}/{bottom} mm copper, {thickness} mm board: {:?}",
                    checked.findings
                );
            }
        }
    }

    /// Four layers in JLC's default 1.6 mm stackup, a 1 mm hole at (10, 10),
    /// and N1 lands unless nonplated.
    fn jlc_drilled_board(plating: &str, ring_mm: f64, outer_copper_mm: &str) -> String {
        let mut xml = weighted_board(outer_copper_mm)
            .replace("lineWidth=\"0.12\"", "lineWidth=\"0.2\"")
            .replace(r#"sequence="1"/>"#, r#"sequence="6"/>"#)
            .replace(
                r#"<Layer name="BOTTOM""#,
                r#"<Layer name="PP1" layerFunction="DIELPREG" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="IN1" layerFunction="SIGNAL" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="CORE" layerFunction="DIELCORE" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="IN2" layerFunction="SIGNAL" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="PP2" layerFunction="DIELPREG" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE"><Span fromLayer="TOP" toLayer="BOTTOM"/></Layer>
      <Layer name="BOTTOM""#,
            )
            .replace(
                r#"<StackupLayer layerOrGroupRef="BOTTOM""#,
                r#"<StackupLayer layerOrGroupRef="PP1" thickness="0.203" sequence="1"/>
          <StackupLayer layerOrGroupRef="IN1" thickness="0.035" sequence="2"/>
          <StackupLayer layerOrGroupRef="CORE" thickness="1.03" sequence="3"/>
          <StackupLayer layerOrGroupRef="IN2" thickness="0.035" sequence="4"/>
          <StackupLayer layerOrGroupRef="PP2" thickness="0.203" sequence="5"/>
          <StackupLayer layerOrGroupRef="BOTTOM""#,
            );
        let mut features = format!(
            r#"<LayerFeature layerRef="DRILL"><Set net="N1"><Hole name="H1" diameter="1" platingStatus="{plating}" x="10" y="10"/></Set></LayerFeature>"#
        );
        if plating != "NONPLATED" {
            let (low, high) = (9.5 - ring_mm, 10.5 + ring_mm);
            for layer in ["TOP", "IN1", "IN2", "BOTTOM"] {
                features.push_str(&format!(
                    r#"<LayerFeature layerRef="{layer}"><Set net="N1"><Features><Contour><Polygon>
                      <PolyBegin x="{low}" y="{low}"/><PolyStepSegment x="{high}" y="{low}"/>
                      <PolyStepSegment x="{high}" y="{high}"/><PolyStepSegment x="{low}" y="{high}"/>
                      <PolyStepSegment x="{low}" y="{low}"/>
                    </Polygon></Contour></Features></Set></LayerFeature>"#
                ));
            }
        }
        xml = xml.replace("</Step>", &format!("{features}</Step>"));
        xml
    }

    #[test]
    fn standard_via_drill_and_ring_have_tight_review_bands() {
        for copper in ["0.035", "0.07"] {
            for (diameter, ring, errors, warnings) in [
                (0.189, 0.125, 1, 0),
                (0.190, 0.125, 0, 1),
                (0.195, 0.125, 0, 1),
                (0.200, 0.125, 0, 0),
                // Keep the error case outside the existing curve uncertainty.
                (1.0, 0.110, 1, 0),
                (1.0, 0.115, 0, 1),
                (1.0, 0.120, 0, 1),
                (1.0, 0.125, 0, 0),
            ] {
                let xml = jlc_drilled_board("VIA", ring, copper).replace(
                    r#"<Hole name="H1" diameter="1""#,
                    &format!(r#"<Hole name="H1" diameter="{diameter}""#),
                );
                let checked = check_builtin(&xml, "standard").unwrap();
                assert_eq!(
                    (checked.summary.errors, checked.summary.warnings),
                    (errors, warnings),
                    "{copper} mm foil, {diameter} mm drill, {ring} mm ring: {:?}",
                    checked.findings
                );
                assert_eq!(matches!(checked.verdict, report::Verdict::Fail), errors > 0);
            }
        }
    }

    #[test]
    fn builtin_routed_edges_have_tight_review_bands() {
        for (pdk, minimum, target) in [("standard", 0.371, 0.381), ("jlc", 0.19, 0.20)] {
            for copper in ["0.035", "0.07"] {
                for (gap, errors, warnings) in [
                    (minimum - 0.005, 1, 0),
                    (minimum, 0, 1),
                    ((minimum + target) / 2.0, 0, 1),
                    (target, 0, 0),
                ] {
                    let y = gap + 0.1;
                    let xml = weighted_board(copper)
                        .replace("lineWidth=\"0.12\"", "lineWidth=\"0.2\"")
                        .replace("startY=\"1\"", &format!("startY=\"{y}\""))
                        .replace("endY=\"1\"", &format!("endY=\"{y}\""));
                    let checked = check_builtin(&xml, pdk).unwrap();
                    assert_eq!(
                        (checked.summary.errors, checked.summary.warnings),
                        (errors, warnings),
                        "{pdk}: {copper} mm foil, {gap} mm gap: {:?}",
                        checked.findings
                    );
                    assert_eq!(matches!(checked.verdict, report::Verdict::Fail), errors > 0);
                }
            }
        }
    }

    #[test]
    fn builtin_pth_annular_rings_follow_their_copper_weight_policy() {
        for (copper, ring, jlc_fails, standard_fails) in [
            ("0.035", 0.16, false, true),
            ("0.035", 0.19, false, false),
            ("0.035", 0.23, false, false),
            ("0.07", 0.23, true, false),
            ("0.07", 0.27, false, false),
        ] {
            for (pdk, fails) in [("jlc", jlc_fails), ("standard", standard_fails)] {
                let checked =
                    check_builtin(&jlc_drilled_board("PLATED", ring, copper), pdk).unwrap();
                assert_eq!(
                    matches!(checked.verdict, report::Verdict::Fail),
                    fails,
                    "{pdk}: {copper} mm foil, {ring} mm ring: {:?}",
                    checked.findings
                );
                assert_eq!(checked.summary.errors, usize::from(fails));
            }
        }

        // 2 oz rings from 0.250 mm to JLC's 10 mil warn; much smaller rings still fail.
        for (ring, errors, warnings) in [
            (0.125, 1, 0),
            (0.245, 1, 0),
            (0.250, 0, 1),
            (0.252, 0, 1),
            (0.254, 0, 0),
        ] {
            let checked = check_builtin(&jlc_drilled_board("PLATED", ring, "0.07"), "jlc").unwrap();
            let id = "jlc.copper.minimum_pth_annular_ring_2oz";
            assert_eq!(
                (
                    rule(&checked, id).finding_count,
                    rule(&checked, &format!("{id}.preferred")).finding_count
                ),
                (errors, warnings),
                "{ring} mm ring: {:?}",
                checked.findings
            );
            assert_eq!(matches!(checked.verdict, report::Verdict::Fail), errors > 0);
        }
    }

    #[test]
    fn jlc_checks_drill_to_copper_clearance_by_hole_class_and_layer() {
        for (plating, ring, layer, id, below, above) in [
            ("VIA", 0.08, "TOP", "via", 0.19, 0.22),
            ("VIA", 0.08, "IN1", "via", 0.19, 0.22),
            ("NONPLATED", 0.0, "TOP", "npth", 0.189, 0.22),
            ("NONPLATED", 0.0, "IN1", "npth", 0.189, 0.22),
            ("NONPLATED", 0.0, "TOP", "npth.preferred", 0.19, 0.20),
            ("NONPLATED", 0.0, "IN1", "npth.preferred", 0.195, 0.201),
            ("PLATED", 0.16, "TOP", "pth.outer", 0.265, 0.27),
            ("PLATED", 0.16, "TOP", "pth.outer.preferred", 0.275, 0.35),
            ("PLATED", 0.16, "IN1", "pth.inner", 0.29, 0.32),
        ] {
            for (gap, fails) in [(below, true), (above, false)] {
                let left = 10.5 + gap;
                let right = left + 1.0;
                let copper = format!(
                    r#"<LayerFeature layerRef="{layer}"><Set net="N2"><Features><Contour><Polygon>
                      <PolyBegin x="{left}" y="9"/><PolyStepSegment x="{right}" y="9"/>
                      <PolyStepSegment x="{right}" y="11"/><PolyStepSegment x="{left}" y="11"/>
                      <PolyStepSegment x="{left}" y="9"/>
                    </Polygon></Contour></Features></Set></LayerFeature>"#
                );
                let xml = jlc_drilled_board(plating, ring, "0.035")
                    .replace("</Step>", &format!("{copper}</Step>"));
                let checked = check_builtin(&xml, "jlc").unwrap();
                let result = rule(&checked, &format!("jlc.copper.minimum_hole_clearance.{id}"));
                assert_eq!(
                    result.finding_count,
                    usize::from(fails),
                    "{plating} on {layer}, {gap} mm gap: {:?}",
                    checked.findings
                );
                let warning = id.ends_with(".preferred");
                assert_eq!(
                    result.severity,
                    if warning {
                        report::Severity::Warning
                    } else {
                        report::Severity::Error
                    }
                );
                assert_eq!(
                    matches!(checked.verdict, report::Verdict::Fail),
                    fails && !warning,
                    "{plating} on {layer}, {gap} mm gap: {:?}",
                    checked.findings
                );
                if plating == "NONPLATED" {
                    let standard = check_builtin(&xml, "standard").unwrap();
                    assert_eq!(
                        (standard.summary.errors, standard.summary.warnings),
                        (
                            usize::from(fails && !warning),
                            usize::from(fails && warning)
                        ),
                        "standard: {plating} on {layer}, {gap} mm gap: {:?}",
                        standard.findings
                    );
                }
            }
        }
    }

    #[test]
    fn jlc_distinguishes_published_and_inferred_hole_pair_limits() {
        for (first, second, gap, severity) in [
            ("VIA", "VIA", 0.15, Some(report::Severity::Error)),
            ("VIA", "VIA", 0.1989, Some(report::Severity::Error)),
            ("VIA", "VIA", 0.199, Some(report::Severity::Warning)),
            // Export precision just short of JLC's 0.20 mm.
            ("VIA", "VIA", 0.199998, Some(report::Severity::Warning)),
            ("VIA", "VIA", 0.20, None),
            ("VIA", "VIA", 0.35, None),
            ("PLATED", "PLATED", 0.35, Some(report::Severity::Error)),
            ("VIA", "PLATED", 0.35, Some(report::Severity::Warning)),
            ("VIA", "NONPLATED", 0.35, Some(report::Severity::Warning)),
            ("PLATED", "NONPLATED", 0.35, Some(report::Severity::Warning)),
            (
                "NONPLATED",
                "NONPLATED",
                0.35,
                Some(report::Severity::Warning),
            ),
            ("PLATED", "PLATED", 0.47, None),
            ("VIA", "PLATED", 0.47, None),
        ] {
            let x = 11.0 + gap;
            let hole = format!(
                r#"<LayerFeature layerRef="DRILL"><Set net="N1"><Hole name="H2" diameter="1" platingStatus="{second}" x="{x}" y="10"/></Set></LayerFeature>"#
            );
            let xml = jlc_drilled_board(first, 0.23, "0.035")
                .replace("</Step>", &format!("{hole}</Step>"));
            let checked = check_builtin(&xml, "jlc").unwrap();
            let findings = checked
                .findings
                .iter()
                .filter(|finding| {
                    finding
                        .rule_id
                        .starts_with("jlc.drilling.minimum_hole_clearance.")
                })
                .collect::<Vec<_>>();
            assert_eq!(findings.len(), usize::from(severity.is_some()));
            if let Some(severity) = severity {
                assert_eq!(findings[0].severity, severity, "{first}/{second}");
                assert!((findings[0].measurement.actual_mm().unwrap() - gap).abs() < 1e-8);
            }
        }
    }

    #[test]
    fn jlc_requires_copper_clearance_from_nonplated_routed_slots() {
        for (gap, errors, warnings) in [
            (0.185, 1, 0),
            (0.190, 0, 1),
            (0.195, 0, 1),
            (0.200, 0, 0),
            (0.220, 0, 0),
        ] {
            let left = 11.5 + gap;
            let right = left + 1.0;
            let features = format!(
                r#"<LayerFeature layerRef="ROUT"><Set><SlotCavity name="S1" platingStatus="NONPLATED"><Location x="10" y="10"/><Oval width="3" height="1"/></SlotCavity></Set></LayerFeature>
                <LayerFeature layerRef="TOP"><Set net="N2"><Features><Contour><Polygon>
                  <PolyBegin x="{left}" y="9"/><PolyStepSegment x="{right}" y="9"/>
                  <PolyStepSegment x="{right}" y="11"/><PolyStepSegment x="{left}" y="11"/>
                  <PolyStepSegment x="{left}" y="9"/>
                </Polygon></Contour></Features></Set></LayerFeature>"#
            );
            for copper in ["0.035", "0.07"] {
                let xml = weighted_board(copper)
                    .replace("lineWidth=\"0.12\"", "lineWidth=\"0.2\"")
                    .replace(
                        r#"<Layer name="BOTTOM""#,
                        r#"<Layer name="ROUT" layerFunction="ROUT" side="ALL" polarity="POSITIVE"><Span fromLayer="TOP" toLayer="BOTTOM"/></Layer><Layer name="BOTTOM""#,
                    )
                    .replace("</Step>", &format!("{features}</Step>"));
                let checked = check_builtin(&xml, "jlc").unwrap();
                assert_eq!(
                    (checked.summary.errors, checked.summary.warnings),
                    (errors, warnings),
                    "{copper} mm foil, {gap} mm gap: {:?}",
                    checked.findings
                );
                assert_eq!(matches!(checked.verdict, report::Verdict::Fail), errors > 0);
            }
        }
    }

    #[test]
    fn jlc_checks_a_board_against_the_profile_its_copper_weight_qualifies_for() {
        let width = |report: &DfmReport, case: &str| {
            let id = format!("jlc.copper.minimum_feature_width.{case}");
            rule(report, &id).status == report::RuleStatus::Pass
        };

        let one_ounce = check_builtin(&weighted_board("0.035"), "jlc").unwrap();
        assert_eq!(one_ounce.pdk.profile, "one-ounce");
        assert!(one_ounce.pdk.profile_selection.rejected.is_empty());
        assert!(width(&one_ounce, "2-layer-1oz"));
        assert!(matches!(one_ounce.verdict, report::Verdict::Pass));

        // The same 0.12 mm trace in 2 oz copper is below JLC's 0.16 mm.
        let two_ounce = check_builtin(&weighted_board("0.07"), "jlc").unwrap();
        assert_eq!(two_ounce.pdk.profile, "two-ounce");
        let [rejected] = two_ounce.pdk.profile_selection.rejected.as_slice() else {
            panic!("only the default profile precedes two-ounce");
        };
        assert_eq!(rejected.profile, "one-ounce");
        assert!(
            rejected
                .reasons
                .iter()
                .any(|reason| reason.contains("'TOP'"))
        );
        assert!(!width(&two_ounce, "2-layer-2oz"));
        assert!(matches!(two_ounce.verdict, report::Verdict::Fail));

        // Naming the 1 oz profile still runs it, and its support fails.
        let explicit = check_builtin(&weighted_board("0.07"), "jlcpcb-1oz").unwrap();
        assert_eq!(explicit.pdk.profile_selection.mode, "explicit");
        let support = rule(&explicit, "profile.support.outer_copper_weight");
        assert!(matches!(support.status, report::RuleStatus::Fail));
        assert_eq!(support.checked, 2);

        // 3 oz fits no profile, so the default runs and its support fails.
        let unfit = check_builtin(&weighted_board("0.105"), "jlc").unwrap();
        assert_eq!(unfit.pdk.profile, "one-ounce");
        let [rejected] = unfit.pdk.profile_selection.rejected.as_slice() else {
            panic!("two-ounce is rejected besides the default");
        };
        assert_eq!(rejected.profile, "two-ounce");
        let support = rule(&unfit, "profile.support.outer_copper_weight");
        assert!(matches!(support.status, report::RuleStatus::Fail));
        assert!(matches!(unfit.verdict, report::Verdict::Fail));
    }

    #[test]
    fn a_flexible_stackup_runs_only_a_profile_qualified_for_its_technology() {
        // A 2-layer board with one stated core material.
        let board = |materials: &[&str]| {
            let specs = materials
                .iter()
                .enumerate()
                .map(|(index, material)| {
                    format!(
                        r#"<Spec name="CORE{index}"><General type="MATERIAL"><Property text="{material}"/></General></Spec>"#
                    )
                })
                .collect::<String>();
            let cores = (0..materials.len())
                .map(|index| {
                    format!(
                        r#"<Layer name="CORE{index}" layerFunction="DIELCORE" side="INTERNAL" polarity="POSITIVE"/>"#
                    )
                })
                .collect::<String>();
            let layers = (0..materials.len())
                .map(|index| {
                    format!(
                        r#"<StackupLayer layerOrGroupRef="CORE{index}" thickness="0.025" tolPlus="0" tolMinus="0" sequence="{}"><SpecRef id="CORE{index}"/></StackupLayer>"#,
                        index + 1
                    )
                })
                .collect::<String>();
            BOARD
                .replace(
                    "<CadHeader units=\"MILLIMETER\"/>",
                    &format!(r#"<CadHeader units="MILLIMETER">{specs}</CadHeader>"#),
                )
                .replace(
                    r#"<Layer name="TOP""#,
                    &format!(r#"{cores}<Layer name="TOP""#),
                )
                .replace(
                    r#"<StackupLayer layerOrGroupRef="BOTTOM""#,
                    &format!(r#"{layers}<StackupLayer layerOrGroupRef="BOTTOM""#),
                )
        };

        // JLC's kit is rigid FR-4 only: a polyimide board is not judged by
        // its rigid copper-weight, thickness or soldermask limits, nor passed.
        let error = check_builtin(&board(&["Polyimide-Core"]), "jlc")
            .unwrap_err()
            .to_string();
        for detail in [
            "Polyimide-Core",
            "a flex design",
            "no qualified flex process",
        ] {
            assert!(error.contains(detail), "missing {detail}: {error}");
        }
        assert!(
            check_builtin(&board(&["Polyimide-Core"]), "jlcpcb-1oz").is_err(),
            "naming a rigid profile does not qualify it"
        );
        // A stated rigid material is checked as before.
        let rigid = check_builtin(&board(&["FR4-Core"]), "jlc").unwrap();
        assert_eq!(rigid.pdk.profile, "one-ounce");

        let pdk = r#"schema_version = 2
default_profile = "rigid"
[pdk]
id = "test"
name = "Test"
revision = "1"
[profiles.rigid]
name = "Rigid"
technologies = ["rigid"]
[profiles.flex]
name = "Flex"
technologies = ["flex"]
[[rules.stackup.board_thickness]]
id = "rigid-thickness"
profiles = ["rigid"]
limit = { nominal = ["1.6 mm"], tolerance = "0.1 mm" }
[[rules.stackup.board_thickness]]
id = "flex-thickness"
profiles = ["flex"]
limit = { nominal = ["0.07 mm"], tolerance = "0.01 mm" }
"#;
        let check = |xml: &str| {
            check_with_scene(
                &fixtures::import(xml),
                fixtures::request(xml, pdk, LayoutTarget::Board),
                Resolution::default(),
                false,
            )
        };
        let flex = check(&board(&["Polyimide-Core"])).unwrap();
        assert_eq!(flex.pdk.profile, "flex");
        assert!(matches!(flex.verdict, report::Verdict::Pass));
        assert!(flex.rules.iter().all(|rule| rule.id != "rigid-thickness"));
        let rigid = check(&board(&["FR4-Core"])).unwrap();
        assert_eq!(rigid.pdk.profile, "rigid");
        assert!(
            matches!(rigid.verdict, report::Verdict::Fail),
            "0.07 mm is not 1.6 mm"
        );
        // Polyimide bonded to FR-4 is rigid-flex, which neither profile is.
        let error = check(&board(&["Polyimide-Core", "FR4-Prepreg"]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("no qualified rigid_flex process"), "{error}");
        // A profile that states no technology is no flex process either.
        let unstated = check_with_scene(
            &fixtures::import(&board(&["Polyimide-Core"])),
            fixtures::request(
                &board(&["Polyimide-Core"]),
                &fixtures::pdk(
                    r#"[[rules.stackup.board_thickness]]
id = "thickness"
limit = { nominal = ["0.07 mm"], tolerance = "0.01 mm" }"#,
                ),
                LayoutTarget::Board,
            ),
            Resolution::default(),
            false,
        )
        .unwrap_err()
        .to_string();
        assert!(unstated.contains("lists technologies []"), "{unstated}");
    }

    #[test]
    fn board_properties_are_judged_against_what_the_pdk_allows() {
        let pdk = fixtures::pdk(
            r#"[[rules.stackup.board_thickness]]
id = "thickness"
limit = { nominal = ["0.8 mm", "1.6 mm"], tolerance = "0.1 mm", tolerance_percent = 10 }

[[rules.board.surface_finish]]
id = "finish"
limit = { allowed = ["hasl", "enig"] }

[[rules.board.soldermask_color]]
id = "color"
limit = { allowed = ["Green"] }

[[rules.board.size]]
id = "size"
limit = { minimum = ["10 mm", "5 mm"], maximum = ["40 mm", "25 mm"] }"#,
        );
        let status = |xml: &str, id: &str| {
            let report = check_with_pdk(xml, LayoutTarget::Board, &pdk);
            let status = &rule(&report, id).status;
            format!("{status:?}")
        };
        // Below 1 mm the absolute tolerance governs, above it the relative.
        for (thickness, verdict) in [
            ("0.9", "Pass"),
            ("0.91", "Fail"),
            ("1.76", "Pass"),
            ("1.77", "Fail"),
        ] {
            let xml = BOARD.replace(
                "overallThickness=\"0.07\"",
                &format!("overallThickness=\"{thickness}\""),
            );
            assert_eq!(status(&xml, "thickness"), verdict, "{thickness} mm");
        }

        // An unstated finish or colour is the fab's choice.
        assert_eq!(status(BOARD, "finish"), "NotApplicable");
        assert_eq!(status(BOARD, "color"), "NotApplicable");
        let specified = |finish: &str, color: &str| {
            BOARD
                .replace(
                    "<CadHeader units=\"MILLIMETER\"/>",
                    &format!(
                        r#"<CadHeader units="MILLIMETER">
      <Spec name="MASK"><General type="MATERIAL"><Property text="Color : {color}"/></General></Spec>
      <Spec name="FINISH"><SurfaceFinish type="{finish}"/></Spec>
      <Spec name="UNUSED"><SurfaceFinish type="OSP"/></Spec>
    </CadHeader>"#
                    ),
                )
                .replace(
                    r#"<Layer name="TOP""#,
                    r#"<Layer name="COATING_TOP" layerFunction="COATINGCOND" side="TOP" polarity="POSITIVE"/>
      <Layer name="TOP""#,
                )
                .replace(
                    r#"<StackupLayer layerOrGroupRef="TOP""#,
                    r#"<StackupLayer layerOrGroupRef="F.Mask" thickness="0.01" sequence="0"><SpecRef id="MASK"/></StackupLayer>
          <StackupLayer layerOrGroupRef="COATING_TOP" thickness="0" sequence="0"><SpecRef id="FINISH"/></StackupLayer>
          <StackupLayer layerOrGroupRef="TOP""#,
                )
        };
        // Only the coating layer's spec states the finish, not the unused OSP.
        assert_eq!(status(&specified("ENIG-N", "green"), "finish"), "Pass");
        assert_eq!(status(&specified("T", "green"), "finish"), "Pass");
        // IPC S does not say which of the two lead contents it is.
        assert_eq!(status(&specified("S", "green"), "finish"), "Incomplete");
        assert_eq!(status(&specified("S", "green"), "color"), "Pass");
        let rgb_only = specified("S", "green").replace(
            r#"<Property text="Color : green"/>"#,
            r#"<Color r="0" g="0" b="0"/>"#,
        );
        assert_eq!(status(&rgb_only, "color"), "Fail");
        // KiCad writes a custom colour as hex; it is named for the nearest.
        let report = check_with_pdk(&specified("OSP", "#191919e6"), LayoutTarget::Board, &pdk);
        let messages = report
            .findings
            .iter()
            .map(|finding| (finding.rule_id.as_str(), finding.message.as_str()))
            .collect::<Vec<_>>();
        assert!(messages.contains(&("finish", "surface finish is osp; the PDK allows hasl, enig")));
        assert!(messages.contains(&(
            "color",
            "soldermask layer 'F.Mask' is black; the PDK allows green"
        )));
        // A hex colour within an ink colour's bounds is that colour; one
        // outside every ink's bounds is unrecognized, so not allowed.
        assert_eq!(status(&specified("OSP", "#3C9650"), "color"), "Pass");
        assert_eq!(status(&specified("OSP", "#3C9650ZZ"), "color"), "Fail");
        let report = check_with_pdk(&specified("OSP", "#FF8C00"), LayoutTarget::Board, &pdk);
        assert!(report.findings.iter().any(|finding| finding.rule_id == "color"
            && finding.message
                == "soldermask layer 'F.Mask' is an unrecognized colour (#FF8C00); the PDK allows green"));

        // The 30 mm square board is wider than 25 mm, however it is turned.
        assert_eq!(status(BOARD, "size"), "Fail");
        let square = |side: &str| BOARD.replace("\"30\"", &format!("\"{side}\""));
        assert_eq!(status(&square("20"), "size"), "Pass");
        // A layout without an outline fails the baseline outline check
        // once; size has nothing to measure.
        let (start, end) = (
            BOARD.find("<Profile>").unwrap(),
            BOARD.find("</Profile>").unwrap(),
        );
        let unoutlined = BOARD.replace(&BOARD[start..end + "</Profile>".len()], "");
        assert_eq!(status(BOARD, "board.outline"), "Pass");
        assert_eq!(status(&unoutlined, "board.outline"), "Fail");
        assert_eq!(status(&unoutlined, "size"), "NotApplicable");
    }

    /// A 30 mm square board whose copper layers, `outer` or `inner` mm
    /// thick, are separated by prepregs `gaps` mm thick ("" is unstated).
    fn stacked_board(outer: &str, inner: &str, gaps: &[&str], thickness: &str) -> String {
        let (mut layers, mut stackup) = (String::new(), String::new());
        for index in 0..=gaps.len() {
            let (side, copper) = match index {
                0 => ("TOP", outer),
                _ if index == gaps.len() => ("BOTTOM", outer),
                _ => ("INTERNAL", inner),
            };
            let (name, sequence) = (index + 1, 2 * index);
            layers += &format!(
                r#"<Layer name="L{name}" layerFunction="SIGNAL" side="{side}" polarity="POSITIVE"/>"#
            );
            stackup += &format!(
                r#"<StackupLayer layerOrGroupRef="L{name}" thickness="{copper}" sequence="{sequence}"/>"#
            );
            if let Some(&gap) = gaps.get(index) {
                let gap = match gap {
                    "" => String::new(),
                    gap => format!(r#" thickness="{gap}""#),
                };
                layers += &format!(
                    r#"<Layer name="PP{name}" layerFunction="DIELPREG" side="INTERNAL" polarity="POSITIVE"/>"#
                );
                stackup += &format!(
                    r#"<StackupLayer layerOrGroupRef="PP{name}"{gap} sequence="{}"/>"#,
                    sequence + 1
                );
            }
        }
        format!(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/><LayerRef name="L1"/></Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>{layers}
    <Stackup name="Primary" overallThickness="{thickness}" whereMeasured="METAL" stackupStatus="PROPOSED">
      <StackupGroup name="Primary_Group" thickness="{thickness}">{stackup}</StackupGroup>
    </Stackup>
    <Step name="board" type="BOARD"><Datum x="0" y="0"/><Profile><Polygon>
      <PolyBegin x="0" y="0"/><PolyStepSegment x="30" y="0"/><PolyStepSegment x="30" y="30"/>
      <PolyStepSegment x="0" y="30"/><PolyStepSegment x="0" y="0"/>
    </Polygon></Profile></Step>
  </CadData></Ecad>
</IPC-2581>"#
        )
    }

    /// A rule's status and its finding or skip reason.
    fn judged(report: &DfmReport, id: &str) -> (String, Option<String>) {
        let result = rule(report, id);
        let finding = report.findings.iter().find(|finding| finding.rule_id == id);
        let message = finding.map(|finding| finding.message.clone());
        (
            format!("{:?}", result.status),
            message.or(result.skip_reason.clone()),
        )
    }

    #[test]
    fn dielectric_gaps_must_match_a_stackup_template_for_the_build() {
        let pdk = fixtures::pdk(
            r#"[[rules.stackup.dielectric_gaps]]
id = "gaps"
limit = { tolerance = "0.025 mm" }

[[stackup_builds]]
copper_layers = 4
board_thickness = "1.6 mm"
outer_copper_weight = "1 oz"
inner_copper_weight = "0.5 oz"
templates = [
  { name = "A", default = true, dielectric_gaps_mm = [0.2104, 1.065, 0.2104] },
  { name = "B", dielectric_gaps_mm = [0.1, 1.265, 0.12] },
]"#,
        );
        let check = |outer: &str, gaps: &[&str]| {
            let xml = stacked_board(outer, "0.0152", gaps, "1.6");
            judged(&check_with_pdk(&xml, LayoutTarget::Board, &pdk), "gaps")
        };
        let fail = |message: &str| ("Fail".to_owned(), Some(message.to_owned()));

        // The rule needs builds with inner layers and real gaps to match.
        for broken in [
            pdk[..pdk.find("[[stackup_builds]]").unwrap()].to_owned(),
            pdk.replace("copper_layers = 4", "copper_layers = 2"),
            pdk.replace("[0.1, 1.265, 0.12]", "[0.1, nan, 0.12]"),
        ] {
            assert!(pdk::Pdk::parse(&broken).is_err(), "{broken}");
        }

        assert_eq!(check("0.035", &["0.21", "1.065", "0.21"]).0, "Pass");
        assert_eq!(check("0.035", &["0.1", "1.265", "0.144"]).0, "Pass");
        assert_eq!(
            check("0.035", &["0.1", "1.265", "0.15"]),
            fail(
                "dielectric gaps match no stackup template for the 4-layer 1.6 mm, 1 oz outer / 0.5 oz inner build within ± 0.025 mm; closest is B: L3–L4 is 0.1500 mm, template 0.1200 mm; the build's default is A"
            )
        );
        assert_eq!(
            check("0.07", &["0.21", "1.065", "0.21"]),
            fail("no stackup template exists for a 4-layer 1.6 mm build with these copper weights")
        );
        assert_eq!(
            check("0.035", &["0.21", "", "0.21"]),
            (
                "Incomplete".to_owned(),
                Some("physical stackup layer 'PP2' has no thickness".to_owned())
            )
        );
        assert_eq!(
            check("0.035", &["1.5"]),
            (
                "NotApplicable".to_owned(),
                Some("the PDK lists no stackup templates for 2-layer boards".to_owned())
            )
        );
    }

    #[test]
    fn jlc_flags_the_f9wry_stackup_its_only_template_does_not_match() {
        let check = |gaps: &[&str]| {
            let xml = stacked_board("0.07", "0.07", gaps, "2.0");
            judged(
                &check_builtin(&xml, "jlc").unwrap(),
                "jlc.stackup.dielectric_gaps",
            )
        };
        let jlc = [
            "0.1415", "0.1", "0.125", "0.1", "0.125", "0.1", "0.125", "0.1", "0.125", "0.1",
            "0.1415",
        ];
        assert_eq!(check(&jlc), ("Pass".to_owned(), None));
        let f9wry = [
            "0.1", "0.1", "0.12", "0.1", "0.12", "0.1", "0.12", "0.1", "0.12", "0.1", "0.1",
        ];
        assert_eq!(
            check(&f9wry),
            (
                "Fail".to_owned(),
                Some("dielectric gaps match no stackup template for the 12-layer 2.0 mm, 2 oz outer / 2 oz inner build within ± 0.025 mm; closest is JLC122022-1080: L1–L2 is 0.1000 mm, template 0.1415 mm; L11–L12 is 0.1000 mm, template 0.1415 mm; the build has no default template".to_owned())
            )
        );
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
      <Profile><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="45" y="0"/><PolyStepSegment x="45" y="20"/><PolyStepSegment x="0" y="20"/><PolyStepSegment x="0" y="0"/></Polygon></Profile>
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
            format!("{:?}", board.findings[0].sites),
            format!("{:?}", array.findings[0].sites)
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
        let witness = finding.sites[0].witnesses[0].point;
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
