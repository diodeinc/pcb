use anyhow::{Context, Result};
use clap::ValueEnum;
use log::{debug, warn};
use pcb_ipc2581_tools::manufacturing::{ManufacturingExportOptions, export_manufacturing_package};
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::geom::{GeometryAccuracy, Resolution};
use pcb_kicad::{KiCadCliBuilder, ensure_board_compatible_with_installed_kicad};
use pcb_layout::utils as layout_utils;
use pcb_ui::{Colorize, Spinner, Style, StyledText};
use serde::Serialize;

use crate::bundle::{self, MetadataInput, SourceBundlePlan};
use pcb_zen::workspace::WorkspaceInfoExt;
use pcb_zen_core::resolution::ResolutionResult;
use pcb_zen_core::{Diagnostics, DiagnosticsPass, EvalOutput};

use inquire::Confirm;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{BufWriter, Write};
use std::time::Instant;

use chrono::Utc;
use std::path::{Path, PathBuf};

use zip::{ZipWriter, write::FileOptions};

use pcb_zen::git;

#[derive(ValueEnum, Debug, Clone, PartialEq)]
#[value(rename_all = "lowercase")]
pub enum ArtifactType {
    Drc,
    Gerbers,
    Ipc2581,
    Vrml,
}

/// All information gathered during the release preparation phase
#[derive(Debug, Clone)]
struct ReleaseLayout {
    /// Path to the KiCad project file, relative to the workspace root.
    kicad_pro_rel: PathBuf,
}

impl ReleaseLayout {
    fn layout_dir_rel(&self) -> &Path {
        self.kicad_pro_rel.parent().unwrap_or(Path::new(""))
    }
}

struct ReleaseInfo {
    zen_path: PathBuf,
    board_name: String,
    version: String,
    git_hash: String,
    staging_dir: PathBuf,
    layout: Option<ReleaseLayout>,
    bom: pcb_sch::bom::Bom,
    output_dir: PathBuf,
    output_name: String,
    suppress: Vec<String>,
    resolution: ResolutionResult,
    root_package_url: Option<String>,
}

impl ReleaseInfo {
    fn workspace_info(&self) -> &pcb_zen::WorkspaceInfo {
        &self.resolution.workspace_info
    }

    fn workspace_root(&self) -> &Path {
        &self.resolution.workspace_info.root
    }

    fn staged_layout_dir(&self) -> Option<PathBuf> {
        self.layout
            .as_ref()
            .map(|l| self.staging_dir.join("src").join(l.layout_dir_rel()))
    }

    fn staged_kicad_files(&self) -> Option<layout_utils::KiCadLayoutFiles> {
        let layout = self.layout.as_ref()?;
        Some(layout_utils::KiCadLayoutFiles {
            kicad_pro: self.staging_dir.join("src").join(&layout.kicad_pro_rel),
        })
    }

    fn staged_pcb_path(&self) -> Option<PathBuf> {
        self.staged_kicad_files().map(|f| f.kicad_pcb())
    }
}

type TaskFn = fn(&ReleaseInfo) -> Result<()>;

const FINALIZATION_TASKS: &[(&str, TaskFn)] = &[
    ("Writing release metadata", write_metadata),
    ("Creating release archive", zip_release),
];

/// Format cumulative time as MM:SS
fn format_cumulative_time(seconds: f64) -> String {
    let total_secs = seconds as u64;
    let mins = total_secs / 60;
    let secs = total_secs % 60;
    format!("{:02}:{:02}", mins, secs)
}

/// Format task duration as seconds or minutes depending on the value
fn format_task_duration_value(seconds: f64) -> String {
    if seconds >= 60.0 {
        format!("{:4.1}m", seconds / 60.0)
    } else {
        format!("{:4.1}s", seconds)
    }
}

/// Format a task duration (dimmed if < 60s, red if >= 60s)
fn format_task_duration(seconds: f64) -> colored::ColoredString {
    let formatted = format_task_duration_value(seconds);
    if seconds >= 60.0 {
        formatted.red()
    } else {
        formatted.dimmed()
    }
}

fn confirm_continue_on_warnings(spinner: &Spinner, has_warnings: bool, message: &str) -> bool {
    if !has_warnings || !crate::tty::is_interactive() {
        return true;
    }

    spinner.suspend(|| {
        Confirm::new(message)
            .with_default(true)
            .prompt()
            .unwrap_or(false)
    })
}

fn execute_task<T>(
    info: &ReleaseInfo,
    name: &str,
    start_time: Instant,
    task: impl FnOnce(&ReleaseInfo, &Spinner) -> Result<T>,
) -> Result<T> {
    let spinner = Spinner::builder(name).start();
    let task_start = Instant::now();
    let output = task(info, &spinner)?;
    let task_duration = task_start.elapsed().as_secs_f64();
    let cumulative_duration = start_time.elapsed().as_secs_f64();

    spinner.finish();
    eprintln!(
        "{}: ({}) {name}",
        format_cumulative_time(cumulative_duration),
        format_task_duration(task_duration)
    );
    Ok(output)
}

/// Execute a list of tasks with proper error handling and UI feedback
fn execute_tasks(info: &ReleaseInfo, tasks: &[(&str, TaskFn)], start_time: Instant) -> Result<()> {
    for (name, task) in tasks {
        execute_task(info, name, start_time, |info, _| task(info))?;
    }
    Ok(())
}

fn release_blocked(diagnostics: &Diagnostics) -> bool {
    diagnostics.error_count() > 0
        || pcbc::kicad_schematic::has_unsuppressed_schematic_diagnostics(diagnostics)
}

fn release_diagnostics(
    diagnostics: &Diagnostics,
    workspace_root: &Path,
    staging: &Path,
) -> Vec<pcb_zen_core::diagnostics::DiagnosticReport> {
    let staged_src = staging.join("src").to_string_lossy().into_owned();
    let workspace_root = workspace_root.to_string_lossy();
    diagnostics
        .iter()
        .map(|diagnostic| {
            let mut report =
                pcb_zen_core::diagnostics::DiagnosticReport::from_diagnostic(diagnostic);
            // Release gating uses the outer severity, not the nested cause's.
            report.severity = diagnostic.severity;
            report.location = report.location.replace(&staged_src, &workspace_root);
            report.body = report.body.replace(&staged_src, &workspace_root);
            for frame in &mut report.stack {
                frame.location = frame.location.replace(&staged_src, &workspace_root);
                frame.message = frame.message.replace(&staged_src, &workspace_root);
            }
            report
        })
        .collect()
}

#[derive(Default, Serialize)]
#[serde(rename_all = "lowercase")]
enum StageStatus {
    #[default]
    Skipped,
    Passed,
    Failed,
}

#[derive(Default, Serialize)]
struct PreflightStages {
    build: StageStatus,
    layout: StageStatus,
}

pub struct BoardReleaseOptions {
    pub version: String,
    pub suppress: Vec<String>,
    pub exclude: Vec<ArtifactType>,
    pub check: bool,
    /// Query supplier offers for the BOM. Only versioned publishes need this;
    /// local builds stay offline.
    pub check_bom_offers: bool,
}

/// Run release preflight, then generate assets and an archive unless `check` is set.
/// Check mode returns no archive and leaves no persistent release staging.
pub fn build_board_release(
    workspace_root: &Path,
    zen_path: PathBuf,
    board_name: String,
    options: BoardReleaseOptions,
) -> Result<Option<PathBuf>> {
    let start_time = Instant::now();
    let temporary = options.check.then(tempfile::tempdir).transpose()?;
    if let Some(temporary) = &temporary {
        // A terminated check never drops its TempDir.
        let path = temporary.path().to_path_buf();
        ctrlc::set_handler(move || {
            let _ = fs::remove_dir_all(&path);
            std::process::exit(130);
        })
        .context("Failed to set termination handler")?;
    }
    let mut diagnostics = Diagnostics::default();
    let mut stages = PreflightStages::default();
    let outcome = preflight_board_release(
        zen_path.clone(),
        board_name,
        &options,
        temporary.as_ref().map(|dir| dir.path().join("release")),
        &mut diagnostics,
        &mut stages,
    );
    // Check mode stops here, including on preflight failure, before any assets.
    if let Some(temporary) = temporary {
        if let Err(error) = &outcome {
            diagnostics
                .diagnostics
                .push(pcb_zen_core::Diagnostic::categorized(
                    &zen_path.to_string_lossy(),
                    &format!("{error:#}"),
                    "release.preflight",
                    starlark::errors::EvalSeverity::Error,
                ));
        }
        let report = serde_json::json!({
            "schemaVersion": 1,
            "version": options.version,
            "stages": stages,
            "diagnostics": release_diagnostics(&diagnostics, workspace_root, &temporary.path().join("release")),
        });
        pcb_ui::write_stdout(|stdout| {
            serde_json::to_writer(&mut *stdout, &report)?;
            writeln!(stdout)?;
            Ok(())
        })?;
        outcome?.context("Evaluation failed")?;
        anyhow::ensure!(!release_blocked(&diagnostics), "Release preflight failed");
        return Ok(None);
    }
    let release_info = outcome?.context("Evaluation failed")?;
    execute_task(
        &release_info,
        "Reviewing release preflight",
        start_time,
        |info, spinner| review_release_preflight(info, spinner, &mut diagnostics),
    )?;

    generate_manufacturing(&release_info, &options.exclude, start_time)?;
    execute_tasks(&release_info, FINALIZATION_TASKS, start_time)?;
    let zip_path = archive_zip_path(&release_info);
    eprintln!(
        "{} {}",
        "✓".green(),
        format!("Release {} staged successfully", release_info.version).bold()
    );
    display_release_info(&release_info);
    eprintln!(
        "Archive: {}",
        zip_path.display().to_string().with_style(Style::Cyan)
    );
    Ok(Some(zip_path))
}

fn preflight_board_release(
    zen_path: PathBuf,
    board_name: String,
    options: &BoardReleaseOptions,
    staging_override: Option<PathBuf>,
    diagnostics: &mut Diagnostics,
    stages: &mut PreflightStages,
) -> Result<Option<ReleaseInfo>> {
    let start_time = Instant::now();
    stages.build = StageStatus::Failed;

    let release_info = {
        let info_spinner = Spinner::builder("Gathering release information").start();

        info_spinner.set_message("Resolving dependencies");
        let resolution = crate::resolve::resolve(Some(&zen_path), false)?;
        let package_url = resolution.workspace_info.package_url_for_zen(&zen_path);
        info_spinner.set_message("Evaluating zen file");

        // Evaluate the zen file for layout discovery and the authored design BOM.
        // Pass resolution so Module() paths resolve correctly
        let eval_result = pcb_zen::eval(&zen_path, resolution.clone(), Default::default());

        if eval_result.diagnostics.has_errors() || eval_result.output.is_none() {
            info_spinner.suspend(|| {
                let mut diagnostics = eval_result.diagnostics.clone();
                let passes = crate::build::create_diagnostics_passes(&[], &[]);
                diagnostics.apply_passes(&passes);
            });
            info_spinner.finish();
            diagnostics
                .diagnostics
                .extend(eval_result.diagnostics.diagnostics);
            anyhow::ensure!(diagnostics.has_errors(), "Evaluation produced no output");
            return Ok(None);
        }

        info_spinner.finish();

        let eval_output = eval_result.output.unwrap();

        let workspace_root = &resolution.workspace_info.root;

        // Get git hash for metadata
        let git_hash = git::rev_parse_head(workspace_root).unwrap_or_else(|| "unknown".to_string());

        let version = options.version.clone();

        // Create release staging directory in workspace root with flat structure
        let staging_dir = staging_override.unwrap_or_else(|| {
            workspace_root
                .join(".pcb/releases")
                .join(format!("{}-{}", board_name, version))
        });

        // Output directory and name use defaults
        let output_dir = workspace_root.join(".pcb/releases");
        let output_name = format!("{}-{}.zip", board_name, version);

        // Delete existing staging dir and recreate
        if staging_dir.exists() {
            debug!(
                "Removing existing staging directory: {}",
                staging_dir.display()
            );
            bundle::remove_dir_all_with_permissions(&staging_dir)?;
        }
        fs::create_dir_all(&staging_dir)?;

        let layout = match discover_layout_from_output(&eval_output)? {
            Some(discovered) => match discovered
                .kicad_files
                .kicad_pro
                .strip_prefix(workspace_root)
            {
                Ok(kicad_pro_rel) => Some(ReleaseLayout {
                    kicad_pro_rel: kicad_pro_rel.to_path_buf(),
                }),
                Err(_) => {
                    warn!(
                        "Layout path {} is outside workspace root, ignoring",
                        discovered.layout_dir.display()
                    );
                    None
                }
            },
            None => None,
        };

        let bom = eval_output.to_schematic()?.bom();

        let info = ReleaseInfo {
            zen_path,
            board_name,
            version,
            git_hash,
            staging_dir,
            layout,
            bom,
            output_dir,
            output_name,
            suppress: options.suppress.clone(),
            resolution,
            root_package_url: package_url,
        };

        let elapsed = start_time.elapsed().as_secs_f64();
        eprintln!(
            "{}: {} ({}) Release information gathered",
            format_cumulative_time(elapsed),
            "✓".green(),
            format_task_duration(elapsed),
        );

        info
    };

    run_release_preflight(&release_info, options, start_time, diagnostics, stages)?;
    Ok(Some(release_info))
}

/// Display release information summary
fn display_release_info(info: &ReleaseInfo) {
    eprintln!(
        "{}",
        "Release Summary".to_string().with_style(Style::Blue).bold()
    );
    let mut table = comfy_table::Table::new();
    table
        .load_style(comfy_table::presets::UTF8_BORDERS_ONLY)
        .set_content_arrangement(comfy_table::ContentArrangement::Dynamic);

    table.add_row(vec!["Release Type", "Full Release"]);
    table.add_row(vec!["Version", &info.version]);
    table.add_row(vec![
        "Git Hash",
        &info.git_hash[..8.min(info.git_hash.len())],
    ]);

    let zen_file = info
        .zen_path
        .strip_prefix(info.workspace_root())
        .unwrap_or(&info.zen_path)
        .display()
        .to_string();
    table.add_row(vec!["Zen File", &zen_file]);

    let staging_dir = info
        .staging_dir
        .strip_prefix(info.workspace_root())
        .unwrap_or(&info.staging_dir)
        .display()
        .to_string();
    table.add_row(vec!["Staging Dir", &staging_dir]);

    table.add_row(vec!["Platform", std::env::consts::OS]);
    table.add_row(vec!["Architecture", std::env::consts::ARCH]);
    table.add_row(vec!["CLI Version", env!("CARGO_PKG_VERSION")]);
    let kicad_version = pcb_kicad::get_kicad_version()
        .ok()
        .unwrap_or_else(|| "unknown".to_string());
    table.add_row(vec!["KiCad Version", &kicad_version]);

    let user = std::env::var("USER").unwrap_or_else(|_| "unknown".to_string());
    table.add_row(vec!["Created By", &user]);

    let timestamp = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    table.add_row(vec!["Created At", &timestamp]);

    println!("{table}");
}

struct DiscoveredLayout {
    layout_dir: PathBuf,
    kicad_files: layout_utils::KiCadLayoutFiles,
}

/// Discover layout info from zen evaluation output.
/// Returns None if no layout_path property exists or the layout directory doesn't contain KiCad files.
fn discover_layout_from_output(output: &EvalOutput) -> Result<Option<DiscoveredLayout>> {
    let properties = output.sch_module().properties();

    let Some(layout_path_value) = properties.get("layout_path") else {
        return Ok(None);
    };

    let layout_path_str = layout_path_value.to_string();
    let clean_path_str = layout_path_str.trim_matches('"');

    let layout_path = output.resolution().resolve_package_uri(clean_path_str)?;

    // Discover KiCad files (require a single top-level .kicad_pro).
    let discovered = layout_utils::discover_kicad_files(&layout_path)?;
    if discovered.is_none() {
        if layout_path.exists() {
            warn!(
                "Layout directory {} exists but has no discoverable KiCad project/layout files, skipping layout tasks",
                layout_path.display()
            );
        } else {
            debug!(
                "Layout path {} does not exist, skipping layout tasks",
                layout_path.display()
            );
        }
        return Ok(None);
    }

    debug!(
        "Extracted layout path: {} -> {}",
        clean_path_str,
        layout_path.display()
    );
    Ok(Some(DiscoveredLayout {
        layout_dir: layout_path,
        kicad_files: discovered.unwrap(),
    }))
}

/// Copy source files and vendor dependencies
fn copy_sources(info: &ReleaseInfo) -> Result<()> {
    bundle::stage_source_bundle(&SourceBundlePlan {
        resolution: &info.resolution,
        root_package_url: info.root_package_url.as_deref(),
        staged_src: &info.staging_dir.join("src"),
    })
}

fn update_kicad_pro_release_variables(
    kicad_pro_path: &Path,
    version: &str,
    git_hash: &str,
) -> Result<()> {
    // Read the existing .kicad_pro file
    let content = fs::read_to_string(kicad_pro_path).with_context(|| {
        format!(
            "Failed to read .kicad_pro file: {}",
            kicad_pro_path.display()
        )
    })?;

    // Parse as JSON
    let mut project: serde_json::Value = serde_json::from_str(&content).with_context(|| {
        format!(
            "Failed to parse .kicad_pro file as JSON: {}",
            kicad_pro_path.display()
        )
    })?;

    let project = project
        .as_object_mut()
        .context("KiCad project file root must be a JSON object")?;
    let text_vars = project
        .entry("text_variables")
        .or_insert_with(|| serde_json::json!({}));
    if !text_vars.is_object() {
        *text_vars = serde_json::json!({});
    }
    let text_vars = text_vars.as_object_mut().unwrap();

    for (key, value) in [("PCB_VERSION", version), ("PCB_GIT_HASH", git_hash)] {
        text_vars.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }

    // Write back to file with pretty formatting
    let mut updated_content = serde_json::to_string_pretty(&project)?;
    updated_content.push('\n');
    fs::write(kicad_pro_path, updated_content).with_context(|| {
        format!(
            "Failed to write updated .kicad_pro file: {}",
            kicad_pro_path.display()
        )
    })?;

    debug!("Updated text variables in: {}", kicad_pro_path.display());

    Ok(())
}

fn update_kicad_pcb_release_variables(
    kicad_pcb_path: &Path,
    version: &str,
    git_hash: &str,
) -> Result<()> {
    let content = fs::read_to_string(kicad_pcb_path).with_context(|| {
        format!(
            "Failed to read .kicad_pcb file: {}",
            kicad_pcb_path.display()
        )
    })?;
    let root = pcb_sexpr::parse(&content).map_err(|e| anyhow::anyhow!(e))?;
    let root_items = root
        .as_list()
        .context("KiCad PCB file root must be an S-expression list")?;

    let mut patches = pcb_sexpr::PatchSet::new();
    let mut inserted = String::new();
    for (key, value) in [("PCB_VERSION", version), ("PCB_GIT_HASH", git_hash)] {
        let value_node = root_items.iter().find_map(|item| {
            let items = item.as_list()?;
            (items.first().and_then(|item| item.as_sym()) == Some("property")
                && items.get(1).and_then(|item| item.as_str()) == Some(key))
            .then_some(items.get(2))
            .flatten()
        });
        if let Some(value_node) = value_node {
            patches.replace_string(value_node.span, value);
        } else {
            let property = pcb_sexpr::Sexpr::list(vec![
                pcb_sexpr::Sexpr::symbol("property"),
                pcb_sexpr::Sexpr::string(key),
                pcb_sexpr::Sexpr::string(value),
            ]);
            inserted.push('\n');
            inserted.push_str(&property.to_string());
        }
    }

    if !inserted.is_empty() {
        let insert_at = root_items
            .iter()
            .rev()
            .find_map(|item| {
                let items = item.as_list()?;
                match items.first().and_then(|item| item.as_sym()) {
                    Some("setup" | "layers" | "general") => Some(item.span.end),
                    _ => None,
                }
            })
            .unwrap_or_else(|| root.span.end.saturating_sub(1));
        patches.replace_raw(pcb_sexpr::Span::new(insert_at, insert_at), inserted);
    }

    let file = fs::File::create(kicad_pcb_path).with_context(|| {
        format!(
            "Failed to write updated .kicad_pcb file: {}",
            kicad_pcb_path.display()
        )
    })?;
    let mut writer = BufWriter::new(file);
    patches.write_to(&content, &mut writer)?;
    writer.flush().with_context(|| {
        format!(
            "Failed to flush updated .kicad_pcb file: {}",
            kicad_pcb_path.display()
        )
    })?;

    debug!("Updated release variables in: {}", kicad_pcb_path.display());
    Ok(())
}

/// Substitute release version and git hash placeholders in staged KiCad files.
fn substitute_variables(info: &ReleaseInfo) -> Result<()> {
    let Some(kicad_files) = info.staged_kicad_files() else {
        debug!("No layout directory, skipping variable substitution");
        return Ok(());
    };

    // Use short hash (7 chars) for variable substitution
    let short_hash = &info.git_hash[..7.min(info.git_hash.len())];

    let kicad_pro_path = kicad_files.kicad_pro.clone();
    update_kicad_pro_release_variables(&kicad_pro_path, &info.version, short_hash)?;
    update_kicad_pcb_release_variables(&kicad_files.kicad_pcb(), &info.version, short_hash)?;
    Ok(())
}

fn run_release_preflight(
    info: &ReleaseInfo,
    options: &BoardReleaseOptions,
    start_time: Instant,
    diagnostics: &mut Diagnostics,
    stages: &mut PreflightStages,
) -> Result<()> {
    execute_task(
        info,
        "Copying source files and dependencies",
        start_time,
        |info, _| copy_sources(info),
    )?;
    execute_task(
        info,
        "Generating netlist from staged sources",
        start_time,
        |info, spinner| validate_build(info, spinner, diagnostics),
    )?;
    if release_blocked(diagnostics) {
        return Ok(());
    }
    stages.build = StageStatus::Passed;
    execute_task(
        info,
        "Substituting version variables",
        start_time,
        |info, _| substitute_variables(info),
    )?;

    if let Some(layout) = &info.layout {
        stages.layout = StageStatus::Failed;
        ensure_board_compatible_with_installed_kicad(
            &layout_utils::KiCadLayoutFiles {
                kicad_pro: info.workspace_root().join(&layout.kicad_pro_rel),
            }
            .kicad_pcb(),
        )?;
        if options.exclude.contains(&ArtifactType::Drc) {
            stages.layout = StageStatus::Skipped;
        } else {
            execute_task(
                info,
                "Running KiCad DRC checks",
                start_time,
                |info, _spinner| run_kicad_drc(info, diagnostics),
            )?;
            if release_blocked(diagnostics) {
                return Ok(());
            }
            stages.layout = StageStatus::Passed;
        }
    }
    if options.check_bom_offers {
        diagnostics.extend(execute_task(
            info,
            "Checking BOM offers",
            start_time,
            |info, _| Ok(check_bom_offers(info)),
        )?);
    }

    // Process late-added BOM warnings before either JSON or interactive review.
    pcb_zen_core::FilterHiddenPass.apply(diagnostics);
    pcb_zen_core::SuppressPass::new(info.suppress.clone()).apply(diagnostics);
    Ok(())
}

fn review_release_preflight(
    info: &ReleaseInfo,
    spinner: &Spinner,
    diagnostics: &mut Diagnostics,
) -> Result<()> {
    spinner.suspend(|| crate::drc::render_diagnostics(diagnostics, &info.suppress, false));
    if pcbc::kicad_schematic::has_unsuppressed_schematic_diagnostics(diagnostics) {
        anyhow::bail!(
            "Linked KiCad schematic is not equivalent. Run `pcb apply schematic {}` before publishing.",
            info.zen_path.display()
        );
    }
    anyhow::ensure!(!release_blocked(diagnostics), "Release preflight failed");
    let warning_count = diagnostics.warning_count();
    if !confirm_continue_on_warnings(
        spinner,
        warning_count > 0,
        &format!(
            "Release preflight produced {warning_count} warning(s). Do you want to proceed with the release?"
        ),
    ) {
        std::process::exit(1);
    }
    Ok(())
}

fn active_errors(diagnostics: &Diagnostics) -> Diagnostics {
    Diagnostics {
        diagnostics: diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.is_error() && !diagnostic.suppressed)
            .cloned()
            .collect(),
    }
}

struct RenderBuildErrorsPass;

impl DiagnosticsPass for RenderBuildErrorsPass {
    fn apply(&self, diagnostics: &mut Diagnostics) {
        pcb_zen::diagnostics::RenderPass.apply(&mut active_errors(diagnostics));
    }
}

/// Validate that the staged zen file can be built successfully.
fn validate_build(
    info: &ReleaseInfo,
    spinner: &Spinner,
    diagnostics: &mut Diagnostics,
) -> Result<()> {
    // Calculate the zen file path in the staging directory
    let zen_file_rel = info
        .zen_path
        .strip_prefix(info.workspace_root())
        .context("Zen file must be within workspace root")?;
    let staged_src = info.staging_dir.join("src");
    let staged_zen_path = staged_src.join(zen_file_rel);

    debug!("Validating build of: {}", staged_zen_path.display());

    // Re-resolve in offline mode. Dependencies are vendored from eval1 by
    // copy_sources.
    let staged_resolution = crate::resolve::resolve(Some(&staged_zen_path), true)?;

    // Reuse the build pipeline without rendering; release preflight owns the combined report.
    let build_result = spinner.suspend(|| {
        let mut has_errors = false;
        let mut has_warnings = false;

        // Export diagnostics to JSON for release artifacts
        let mut passes = crate::build::create_diagnostics_processing_passes(&info.suppress, &[]);
        passes.push(Box::new(RenderBuildErrorsPass));
        passes.push(Box::new(pcb_zen_core::JsonExportPass::new(
            info.staging_dir.join("diagnostics.json"),
            zen_file_rel.display().to_string(),
        )));

        crate::build::BuildEvalState::new(staged_resolution).build(
            &staged_zen_path,
            Default::default(),
            passes,
            false, // don't deny warnings - we'll prompt user instead
            &mut has_errors,
            &mut has_warnings,
        )
    });

    let crate::build::BuildResult {
        schematic,
        diagnostics: build_diagnostics,
        ..
    } = build_result;
    diagnostics
        .diagnostics
        .extend(build_diagnostics.diagnostics);
    if release_blocked(diagnostics) {
        return Ok(());
    }

    // Write fp-lib-table with correct vendor/ paths to staged layout directory
    // The staged schematic has footprint paths pointing to src/vendor/ instead of .pcb/cache
    if let Some(ref schematic) = schematic {
        if let Some(staged_layout_dir) = info.staged_layout_dir()
            && staged_layout_dir.exists()
        {
            pcb_layout::utils::write_footprint_library_table(&staged_layout_dir, schematic)
                .context("Failed to write fp-lib-table for staged layout")?;
        }

        // Write RFC 8785 canonical netlist JSON to staging directory.
        let netlist_json = schematic.to_json().context("Failed to serialize netlist")?;
        fs::write(info.staging_dir.join("netlist.json"), &netlist_json)
            .context("Failed to write netlist.json")?;
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum BomOfferIssue {
    Unknown,
    NoOffers,
}

fn bom_part_label(entry: &pcb_sch::bom::BomEntry) -> String {
    if let Some(mpn) = entry.mpn.as_deref() {
        return match entry.manufacturer.as_deref() {
            Some(manufacturer) => format!("{manufacturer} {mpn}"),
            None => mpn.to_string(),
        };
    }

    match (entry.value.as_deref(), entry.package.as_deref()) {
        (Some(value), Some(package)) => format!("{value} {package}"),
        (Some(value), None) => value.to_string(),
        (None, Some(package)) => package.to_string(),
        (None, None) => "generic BOM part".to_string(),
    }
}

fn bom_offer_diagnostics(board_path: &Path, bom: &pcb_sch::bom::Bom) -> pcb_zen_core::Diagnostics {
    let mut groups = HashMap::<(BomOfferIssue, pcb_sch::bom::BomEntry), BTreeSet<String>>::new();

    for (path, entry) in &bom.entries {
        let availability = bom
            .availability
            .get(path)
            .expect("validated BOM match must include every requested path");
        let issue = if availability.match_status == Some(pcb_sch::bom::BomMatchStatus::Failed) {
            BomOfferIssue::Unknown
        } else if availability.offers.is_empty() {
            BomOfferIssue::NoOffers
        } else {
            continue;
        };
        groups
            .entry((issue, entry.clone()))
            .or_default()
            .insert(bom.designators[path].clone());
    }

    let mut groups = groups.into_iter().collect::<Vec<_>>();
    groups.sort_by(
        |((left_issue, _), left_refs), ((right_issue, _), right_refs)| {
            (left_refs.first(), left_issue).cmp(&(right_refs.first(), right_issue))
        },
    );

    let diagnostics = groups
        .into_iter()
        .map(|((issue, entry), designators)| {
            let designators = designators.into_iter().collect::<Vec<_>>().join(", ");
            let part = bom_part_label(&entry);
            let (kind, message) = match issue {
                BomOfferIssue::Unknown => (
                    "bom.sourceability.unknown",
                    format!("BOM matching does not recognize {part} ({designators})"),
                ),
                BomOfferIssue::NoOffers => (
                    "bom.sourceability.no_offers",
                    format!("No supplier offers found for {part} ({designators})"),
                ),
            };
            pcb_zen_core::Diagnostic::categorized(
                &board_path.to_string_lossy(),
                &message,
                kind,
                starlark::errors::EvalSeverity::Warning,
            )
        })
        .collect();

    pcb_zen_core::Diagnostics { diagnostics }
}

/// BOM offer findings are warnings, so every failure to check them is a warning too.
fn check_bom_offers(info: &ReleaseInfo) -> Diagnostics {
    match match_sourcing_bom(info) {
        Ok(None) => Diagnostics::default(),
        Ok(Some(bom)) => bom_offer_diagnostics(&info.zen_path, &bom),
        Err(error) => Diagnostics {
            diagnostics: vec![pcb_zen_core::Diagnostic::categorized(
                &info.zen_path.to_string_lossy(),
                &format!("Could not check BOM offers: {error:#}"),
                "bom.sourceability.check_failed",
                starlark::errors::EvalSeverity::Warning,
            )],
        },
    }
}

/// Match the placed BOM parts, or return `None` when there are none to source.
fn match_sourcing_bom(info: &ReleaseInfo) -> Result<Option<pcb_sch::bom::Bom>> {
    let mut bom = info.bom.filter_excluded();
    bom.entries.retain(|_, entry| !entry.dnp);
    if bom.is_empty() {
        return Ok(None);
    }

    let ctx = pcb_diode_api::WorkspaceContext::from_path(&info.zen_path);
    pcb_diode_api::match_bom_with_context(&ctx, None, &mut bom)?;
    anyhow::ensure!(
        bom.availability.len() == bom.entries.len(),
        "BOM matching could not complete"
    );
    Ok(Some(bom))
}

/// Write release metadata to JSON file
fn write_metadata(info: &ReleaseInfo) -> Result<()> {
    let board_description = info
        .workspace_info()
        .board_info_for_zen(&info.zen_path)
        .map(|b| b.description)
        .filter(|d: &String| !d.is_empty());

    bundle::write_metadata_json(&MetadataInput {
        name: &info.board_name,
        version: &info.version,
        git_hash: &info.git_hash,
        workspace_root: info.workspace_root(),
        staging_dir: &info.staging_dir,
        zen_path: &info.zen_path,
        layout_path: info.layout.as_ref().map(|layout| layout.layout_dir_rel()),
        description: board_description.as_deref(),
        include_kicad_version: true,
    })
}

fn archive_zip_path(info: &ReleaseInfo) -> PathBuf {
    info.output_dir.join(&info.output_name)
}

/// Create zip archive of release staging directory
fn zip_release(info: &ReleaseInfo) -> Result<()> {
    let zip_path = archive_zip_path(info);

    // Ensure output directory exists
    if let Some(parent) = zip_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let zip_file = fs::File::create(&zip_path)?;
    // Use buffered writer for better I/O performance
    let buffered = BufWriter::with_capacity(256 * 1024, zip_file);
    let mut zip = ZipWriter::new(buffered);
    add_directory_to_zip(&mut zip, &info.staging_dir, &info.staging_dir)?;
    zip.finish()?;
    Ok(())
}

/// Recursively add directory contents to zip
fn add_directory_to_zip<W: std::io::Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    dir: &Path,
    base_path: &Path,
) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        // Skip symlinks to avoid including external directories (e.g., .pcb/cache -> ~/.pcb/cache)
        if path.is_symlink() {
            continue;
        }
        if path.is_dir() {
            add_directory_to_zip(zip, &path, base_path)?;
        } else {
            let rel_name = path
                .strip_prefix(base_path)?
                .to_string_lossy()
                .replace('\\', "/");
            zip.start_file(rel_name, FileOptions::<()>::default())?;
            std::io::copy(&mut fs::File::open(&path)?, zip)?;
        }
    }
    Ok(())
}

/// Export IPC once, then derive the board's manufacturing package from it.
fn generate_manufacturing(
    info: &ReleaseInfo,
    excluded: &[ArtifactType],
    start_time: Instant,
) -> Result<()> {
    let Some(kicad_pcb_path) = info.staged_pcb_path() else {
        return Ok(());
    };
    let gerbers = !excluded.contains(&ArtifactType::Gerbers);
    let ipc2581 = !excluded.contains(&ArtifactType::Ipc2581);
    if gerbers || ipc2581 {
        let manufacturing_dir = info.staging_dir.join("manufacturing");
        fs::create_dir_all(&manufacturing_dir)?;
        // IPC is also an intermediate when its published artifact is excluded.
        let temporary = tempfile::tempdir()?;
        let ipc2581_path = temporary.path().join("ipc2581.xml");
        execute_task(info, "Generating IPC-2581 file", start_time, |_, _| {
            export_ipc2581(&kicad_pcb_path, &ipc2581_path)
        })?;
        if gerbers {
            execute_task(
                info,
                "Generating Gerber and drill files",
                start_time,
                |_, _| {
                    export_manufacturing_package(
                        &ipc2581_path,
                        &manufacturing_dir.join("gerbers.zip"),
                        &ManufacturingExportOptions {
                            view: ArtworkScope::Board,
                            include_auxiliary_layers: true,
                            relief_debug_dir: None,
                        },
                        Resolution::default().with_accuracy(GeometryAccuracy::micrometres(1)),
                    )?;
                    Ok(())
                },
            )?;
        }
        if ipc2581 {
            fs::copy(&ipc2581_path, manufacturing_dir.join("ipc2581.xml"))?;
        }
    }
    if !excluded.contains(&ArtifactType::Vrml) {
        execute_task(info, "Generating VRML model", start_time, |info, _| {
            generate_vrml_model(info)
        })?;
    }
    Ok(())
}

pub(crate) fn export_ipc2581(kicad_pcb_path: &Path, ipc2581_path: &Path) -> Result<()> {
    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("ipc2581")
        .arg("--output")
        .arg(ipc2581_path.to_string_lossy())
        .arg("--bom-col-int-id")
        .arg("Path")
        .arg("--bom-col-mfg-pn")
        .arg("Mpn")
        .arg("--bom-col-mfg")
        .arg("Manufacturer")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate IPC-2581 file")?;

    Ok(())
}

/// Generate VRML model
fn generate_vrml_model(info: &ReleaseInfo) -> Result<()> {
    let models_dir = info.staging_dir.join("3d");
    fs::create_dir_all(&models_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for VRML model generation")?;

    // Create a temp file to capture and discard verbose KiCad output
    let devnull = tempfile::tempfile()?;

    // Generate VRML model - KiCad CLI has platform-specific exit code issues
    let wrl_path = models_dir.join("model.wrl");
    let wrl_result = KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("vrml")
        .arg("--output")
        .arg(wrl_path.to_string_lossy())
        .arg("--units")
        .arg("mm")
        .arg("--no-dnp")
        // FIXME: kicad-imported projects have unspecified footprints, so allow these temporarily
        // .arg("--no-unspecified")
        .arg(kicad_pcb_path.to_string_lossy())
        .log_file(devnull)
        .suppress_error_output(true)
        .run();

    if let Err(e) = wrl_result {
        if wrl_path.exists() {
            warn!("KiCad CLI reported error but VRML file was created: {e}");
        } else {
            return Err(e).context("Failed to generate VRML model");
        }
    }

    Ok(())
}

/// Run KiCad DRC checks on the layout file
fn run_kicad_drc(info: &ReleaseInfo, diagnostics: &mut Diagnostics) -> Result<()> {
    let netlist_json_path = info.staging_dir.join("netlist.json");
    let netlist_json = fs::read_to_string(&netlist_json_path)
        .with_context(|| format!("Failed to read {}", netlist_json_path.display()))?;
    let staged_schematic: pcb_sch::Schematic = serde_json::from_str(&netlist_json)
        .with_context(|| format!("Failed to parse {}", netlist_json_path.display()))?;

    // Collect diagnostics from layout sync check (run on staged sources/layout).
    let Some(layout_result) = pcb_layout::process_layout(
        &staged_schematic,
        pcb_layout::LayoutOptions {
            check: true,
            ..Default::default()
        },
        diagnostics,
    )?
    else {
        anyhow::bail!("No layout directory for DRC checks");
    };
    let kicad_pcb_path = layout_result.pcb_file.clone();
    let display_pcb_file = layout_result.display_pcb_file().to_path_buf();
    let working_dir = kicad_pcb_path.parent();

    // Run DRC, writing raw KiCad JSON report to staging directory
    let drc_json_path = info.staging_dir.join("drc.json");
    let report = pcb_kicad::run_drc(&kicad_pcb_path, false, working_dir, &drc_json_path)?;
    report.add_to_diagnostics(diagnostics, &display_pcb_file.to_string_lossy());
    report.add_unconnected_items_to_diagnostics(diagnostics, &display_pcb_file.to_string_lossy());

    pcb_zen_core::SuppressPass::new(info.suppress.clone()).apply(diagnostics);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_check_respects_suppression_and_outer_severity() {
        use pcb_zen_core::Diagnostic;
        use starlark::errors::EvalSeverity::{Error, Warning};

        let error = Diagnostic::new("clearance", Error, Path::new("layout.kicad_pcb"));
        let mut suppressed = error.clone();
        suppressed.suppressed = true;
        let wrapped = Diagnostic::new("downgraded", Warning, Path::new("board.zen"))
            .with_child(Some(Box::new(error)));
        let diagnostics = Diagnostics {
            diagnostics: vec![suppressed, wrapped],
        };
        assert!(!release_blocked(&diagnostics));
        let findings =
            release_diagnostics(&diagnostics, Path::new("/repo"), Path::new("/tmp/release"));
        assert_eq!(
            serde_json::to_value(&findings[1]).unwrap()["severity"],
            "warning"
        );
        assert!(findings[0].suppressed);
    }

    #[test]
    fn update_kicad_pro_release_variables_adds_missing_release_variables() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let kicad_pro_path = temp_dir.path().join("layout.kicad_pro");

        fs::write(
            &kicad_pro_path,
            r#"{
  "text_variables": {
    "PCB_NAME": "Demo Board"
  }
}"#,
        )?;

        update_kicad_pro_release_variables(&kicad_pro_path, "1.2.3", "abcdef0")?;

        let content = fs::read_to_string(&kicad_pro_path)?;
        assert!(
            content.ends_with('\n'),
            "expected .kicad_pro to end with newline"
        );

        let project: serde_json::Value = serde_json::from_str(&content)?;
        let vars = project
            .get("text_variables")
            .and_then(|v| v.as_object())
            .expect("text_variables should exist");

        assert_eq!(
            vars.get("PCB_VERSION").and_then(|v| v.as_str()),
            Some("1.2.3")
        );
        assert_eq!(
            vars.get("PCB_GIT_HASH").and_then(|v| v.as_str()),
            Some("abcdef0")
        );
        assert_eq!(
            vars.get("PCB_NAME").and_then(|v| v.as_str()),
            Some("Demo Board")
        );

        Ok(())
    }

    #[test]
    fn update_kicad_pcb_release_variables_adds_missing_release_properties() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let kicad_pcb_path = temp_dir.path().join("layout.kicad_pcb");

        fs::write(
            &kicad_pcb_path,
            r#"(kicad_pcb
  (version 20240108)
  (generator "pcb")
  (general)
)
"#,
        )?;

        update_kicad_pcb_release_variables(&kicad_pcb_path, "1.2.3", "abcdef0")?;

        let content = fs::read_to_string(&kicad_pcb_path)?;
        assert!(content.contains(r#"(property "PCB_VERSION" "1.2.3")"#));
        assert!(content.contains(r#"(property "PCB_GIT_HASH" "abcdef0")"#));

        Ok(())
    }

    #[test]
    fn bom_offer_warnings_group_parts_and_prefer_unknown() {
        use pcb_sch::bom::{Availability, Bom, BomEntry, Offer};

        let part = |manufacturer: &str, mpn: &str| BomEntry {
            mpn: Some(mpn.to_string()),
            alternatives: Vec::new(),
            manufacturer: Some(manufacturer.to_string()),
            package: None,
            value: None,
            description: None,
            generic_data: None,
            dnp: false,
            skip_bom: false,
            properties: Default::default(),
        };
        let offer = || Offer {
            id: None,
            region: "US".to_string(),
            distributor: "test".to_string(),
            stock: 1,
            price: Some(1.0),
            part_id: None,
            mpn: None,
            manufacturer: None,
            datasheet_url: None,
            part_collections: Vec::new(),
        };

        let resistor = part("Yageo", "RC0603FR-0710KL");
        let unknown = part("Acme", "UNKNOWN");
        let sourceable = part("Murata", "GRM188R71C104KA01");
        let mut bom = Bom::new(
            HashMap::from([
                ("root.R1".to_string(), resistor.clone()),
                ("root.R2".to_string(), resistor),
                ("root.U1".to_string(), unknown),
                ("root.C1".to_string(), sourceable),
            ]),
            HashMap::from([
                ("root.R1".to_string(), "R1".to_string()),
                ("root.R2".to_string(), "R2".to_string()),
                ("root.U1".to_string(), "U1".to_string()),
                ("root.C1".to_string(), "C1".to_string()),
            ]),
        );
        bom.availability = HashMap::from([
            ("root.R1".to_string(), Availability::default()),
            ("root.R2".to_string(), Availability::default()),
            (
                "root.U1".to_string(),
                Availability {
                    match_status: Some(pcb_sch::bom::BomMatchStatus::Failed),
                    offers: vec![offer()],
                    ..Default::default()
                },
            ),
            (
                "root.C1".to_string(),
                Availability {
                    offers: vec![offer()],
                    ..Default::default()
                },
            ),
        ]);

        let diagnostics = bom_offer_diagnostics(Path::new("board.zen"), &bom);
        let warnings = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.body.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            warnings,
            vec![
                "No supplier offers found for Yageo RC0603FR-0710KL (R1, R2)",
                "BOM matching does not recognize Acme UNKNOWN (U1)",
            ]
        );
    }
}
