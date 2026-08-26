use anyhow::{Context, Result};
use clap::ValueEnum;
use log::{debug, warn};
use pcb_kicad::{KiCadCliBuilder, ensure_board_compatible_with_installed_kicad};
use pcb_layout::utils as layout_utils;
use pcb_ui::{Colorize, Spinner, Style, StyledText};

use crate::bom::generate_bom_with_fallback;
use crate::bundle::{self, MetadataInput, SourceBundlePlan};
use pcb_zen::WorkspaceInfo;
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
    Bom,
    Gerbers,
    Cpl,
    Assembly,
    Odb,
    Ipc2581,
    Step,
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
    schematic: pcb_sch::Schematic,
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

    fn has_layout(&self) -> bool {
        self.layout.is_some()
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

type TaskFn = fn(&ReleaseInfo, &Spinner) -> Result<()>;

/// Manufacturing work that runs only after the release preflight is accepted.
const MANUFACTURING_TASKS: &[(ArtifactType, &str, TaskFn)] = &[
    (
        ArtifactType::Gerbers,
        "Generating gerber files",
        generate_gerbers,
    ),
    (
        ArtifactType::Cpl,
        "Generating pick-and-place file",
        generate_cpl,
    ),
    (
        ArtifactType::Assembly,
        "Generating assembly drawings",
        generate_assembly_drawings,
    ),
    (ArtifactType::Odb, "Generating ODB++ files", generate_odb),
    (
        ArtifactType::Ipc2581,
        "Generating IPC-2581 file",
        generate_ipc2581,
    ),
    (
        ArtifactType::Step,
        "Generating STEP model",
        generate_step_model,
    ),
    (
        ArtifactType::Vrml,
        "Generating VRML model",
        generate_vrml_model,
    ),
];

const ODB_EXPORT_PRECISION: &str = "4";

const FINALIZATION_TASKS: &[(&str, TaskFn)] = &[
    ("Writing release metadata", write_metadata),
    ("Creating release archive", zip_release),
];

/// Get manufacturing tasks as (name, function) pairs, filtered by exclusions and layout availability
fn get_manufacturing_tasks(
    excluded: &[ArtifactType],
    has_layout: bool,
) -> Vec<(&'static str, TaskFn)> {
    if !has_layout {
        return Vec::new();
    }

    MANUFACTURING_TASKS
        .iter()
        .filter(|(artifact, _, _)| !excluded.contains(artifact))
        .map(|(_, name, task)| (*name, *task))
        .collect()
}

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
        "{}: {} ({}) {name}",
        format_cumulative_time(cumulative_duration),
        "✓".green(),
        format_task_duration(task_duration)
    );
    Ok(output)
}

/// Execute a list of tasks with proper error handling and UI feedback
fn execute_tasks(info: &ReleaseInfo, tasks: &[(&str, TaskFn)], start_time: Instant) -> Result<()> {
    for (name, task) in tasks {
        execute_task(info, name, start_time, task)?;
    }
    Ok(())
}

/// Build a release for a board file. Used by `pcb publish --board`.
/// If version is provided (e.g. "v1.2.3"), uses that. Otherwise uses git commit hash.
/// Takes pre-resolved workspace info to avoid duplicate resolution.
/// Returns the path to the created release zip file.
pub fn build_board_release(
    workspace: WorkspaceInfo,
    zen_path: PathBuf,
    board_name: String,
    suppress: Vec<String>,
    version: Option<String>,
    exclude: Vec<ArtifactType>,
) -> Result<PathBuf> {
    let start_time = Instant::now();

    let release_info = {
        let info_spinner = Spinner::builder("Gathering release information").start();

        let package_url = workspace.package_url_for_zen(&zen_path);

        info_spinner.set_message("Resolving dependencies");
        let resolution = crate::resolve::resolve(Some(&zen_path), false)?;
        info_spinner.set_message("Evaluating zen file");

        // Evaluate the zen file (still needed for schematic)
        // Pass resolution so Module() paths resolve correctly
        let eval_result = pcb_zen::eval(&zen_path, resolution.clone(), Default::default());

        if eval_result.diagnostics.has_errors() || eval_result.output.is_none() {
            info_spinner.suspend(|| {
                let mut diagnostics = eval_result.diagnostics.clone();
                let passes = crate::build::create_diagnostics_passes(&[], &[]);
                diagnostics.apply_passes(&passes);
            });
            info_spinner.finish();
            anyhow::bail!("Evaluation failed");
        }

        info_spinner.finish();

        let eval_output = eval_result.output.unwrap();

        let workspace_root = &resolution.workspace_info.root;

        // Get git hash for metadata
        let git_hash = git::rev_parse_head(workspace_root).unwrap_or_else(|| "unknown".to_string());

        // Use provided version, or fall back to short git hash
        let version = version.unwrap_or_else(|| {
            git::rev_parse_short_head(workspace_root).unwrap_or_else(|| "unknown".to_string())
        });

        // Create release staging directory in workspace root with flat structure
        let staging_dir = workspace_root
            .join(".pcb/releases")
            .join(format!("{}-{}", board_name, version));

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

        let schematic = eval_output.to_schematic()?;

        let info = ReleaseInfo {
            zen_path,
            board_name,
            version,
            git_hash,
            staging_dir,
            layout,
            schematic,
            output_dir,
            output_name,
            suppress,
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

    if let Some(layout) = &release_info.layout {
        let kicad_pcb_path = layout_utils::KiCadLayoutFiles {
            kicad_pro: release_info.workspace_root().join(&layout.kicad_pro_rel),
        }
        .kicad_pcb();
        ensure_board_compatible_with_installed_kicad(&kicad_pcb_path)?;
    }

    run_release_preflight(&release_info, &exclude, start_time)?;

    let manufacturing_tasks = get_manufacturing_tasks(&exclude, release_info.has_layout());
    execute_tasks(&release_info, &manufacturing_tasks, start_time)?;

    // Execute finalization tasks
    execute_tasks(&release_info, FINALIZATION_TASKS, start_time)?;

    // Calculate archive path
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

    Ok(zip_path)
}

/// Display release information summary
fn display_release_info(info: &ReleaseInfo) {
    eprintln!(
        "{}",
        "Release Summary".to_string().with_style(Style::Blue).bold()
    );
    let mut table = comfy_table::Table::new();
    table
        .load_preset(comfy_table::presets::UTF8_BORDERS_ONLY)
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

/// Get KiCad CLI version
pub(crate) struct DiscoveredLayout {
    pub(crate) layout_dir: PathBuf,
    kicad_files: layout_utils::KiCadLayoutFiles,
}

/// Discover layout info from zen evaluation output.
/// Returns None if no layout_path property exists or the layout directory doesn't contain KiCad files.
pub(crate) fn discover_layout_from_output(output: &EvalOutput) -> Result<Option<DiscoveredLayout>> {
    let properties = output.sch_module.properties();

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
fn copy_sources(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
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
fn substitute_variables(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
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
    excluded: &[ArtifactType],
    start_time: Instant,
) -> Result<()> {
    execute_task(
        info,
        "Copying source files and dependencies",
        start_time,
        copy_sources,
    )?;
    let mut diagnostics = execute_task(
        info,
        "Generating netlist from staged sources",
        start_time,
        validate_build,
    )?;
    execute_task(
        info,
        "Substituting version variables",
        start_time,
        substitute_variables,
    )?;

    if info.has_layout() && !excluded.contains(&ArtifactType::Drc) {
        diagnostics.diagnostics.extend(
            execute_task(info, "Running KiCad DRC checks", start_time, run_kicad_drc)?.diagnostics,
        );
    }
    if !excluded.contains(&ArtifactType::Bom) {
        diagnostics.diagnostics.extend(
            execute_task(
                info,
                "Generating design BOM",
                start_time,
                generate_design_bom,
            )?
            .diagnostics,
        );
    }

    execute_task(
        info,
        "Reviewing release preflight",
        start_time,
        |info, spinner| review_release_preflight(info, spinner, &mut diagnostics),
    )
}

fn review_release_preflight(
    info: &ReleaseInfo,
    spinner: &Spinner,
    diagnostics: &mut Diagnostics,
) -> Result<()> {
    spinner.suspend(|| crate::drc::render_diagnostics(diagnostics, &info.suppress));
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
fn validate_build(info: &ReleaseInfo, spinner: &Spinner) -> Result<Diagnostics> {
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
        diagnostics,
        ..
    } = build_result;
    if diagnostics.error_count() > 0 {
        std::process::exit(1);
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

    Ok(diagnostics)
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
        let issue = if availability.no_match {
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
                    format!("Strict BOM matching does not recognize {part} ({designators})"),
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

fn check_bom_offers(info: &ReleaseInfo, spinner: &Spinner, bom: &pcb_sch::bom::Bom) -> Diagnostics {
    let mut sourcing_bom = bom.filter_excluded();
    sourcing_bom.entries.retain(|_, entry| !entry.dnp);
    if sourcing_bom.is_empty() {
        return Diagnostics::default();
    }

    spinner.set_message("Checking strict BOM offers");
    let ctx = pcb_diode_api::WorkspaceContext::from_path(&info.zen_path);
    match pcb_diode_api::fetch_and_populate_availability_with_context(
        &ctx,
        None,
        &mut sourcing_bom,
        true,
    ) {
        Ok(()) => bom_offer_diagnostics(&info.zen_path, &sourcing_bom),
        Err(error) => pcb_zen_core::Diagnostics {
            diagnostics: vec![pcb_zen_core::Diagnostic::categorized(
                &info.zen_path.to_string_lossy(),
                &format!("Could not check strict BOM offers: {error:#}"),
                "bom.sourceability.check_failed",
                starlark::errors::EvalSeverity::Warning,
            )],
        },
    }
}

/// Generate design BOM JSON file (with optional KiCad fallback if layout exists)
fn generate_design_bom(info: &ReleaseInfo, spinner: &Spinner) -> Result<Diagnostics> {
    // Generate BOM entries from the schematic
    let bom = info.schematic.bom();

    // Create bom directory in staging
    let bom_dir = info.staging_dir.join("bom");
    fs::create_dir_all(&bom_dir)?;

    // Apply fallback logic only if layout exists
    let layout_path = info
        .layout
        .as_ref()
        .map(|l| info.workspace_root().join(l.layout_dir_rel()));
    let final_bom = generate_bom_with_fallback(bom, layout_path.as_deref())?;

    let diagnostics = check_bom_offers(info, spinner, &final_bom);

    // Write design BOM as JSON
    let bom_file = bom_dir.join("design_bom.json");
    let mut file = fs::File::create(&bom_file)?;
    write!(file, "{}", final_bom.ungrouped_json())?;

    Ok(diagnostics)
}

/// Write release metadata to JSON file
fn write_metadata(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
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
        bom_strict: info.workspace_info().workspace_config().bom.strict,
    })
}

fn archive_zip_path(info: &ReleaseInfo) -> PathBuf {
    info.output_dir.join(&info.output_name)
}

/// Create zip archive of release staging directory
fn zip_release(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
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
        let rel_name = path
            .strip_prefix(base_path)?
            .to_string_lossy()
            .replace('\\', "/");
        if should_skip_release_zip_path(&rel_name) {
            continue;
        }
        if path.is_dir() {
            add_directory_to_zip(zip, &path, base_path)?;
        } else {
            zip.start_file(rel_name, FileOptions::<()>::default())?;
            std::io::copy(&mut fs::File::open(&path)?, zip)?;
        }
    }
    Ok(())
}

fn should_skip_release_zip_path(rel_name: &str) -> bool {
    matches!(rel_name, "src/.pcb/stdlib" | "src/.pcb/stdlib.lock")
        || rel_name.starts_with("src/.pcb/stdlib/")
}

/// Generate gerber files
fn generate_gerbers(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for gerber generation")?;

    // Generate gerber files to a temporary directory
    let gerbers_dir = manufacturing_dir.join("gerbers_temp");
    fs::create_dir_all(&gerbers_dir)?;

    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("gerbers")
        .arg("--output")
        .arg(gerbers_dir.to_string_lossy())
        .arg("--use-drill-file-origin")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate gerber files")?;

    // KiCad's default Gerber layer set has changed across releases. Export
    // fabrication drawings explicitly in isolation so its job file cannot
    // replace the primary manufacturing job file.
    let fab_gerbers_dir = manufacturing_dir.join("fab_gerbers_temp");
    fs::create_dir_all(&fab_gerbers_dir)?;
    let fab_export_result = (|| -> Result<()> {
        KiCadCliBuilder::new()
            .command("pcb")
            .subcommand("export")
            .subcommand("gerbers")
            .arg("--output")
            .arg(fab_gerbers_dir.to_string_lossy())
            .arg("--layers")
            .arg("F.Fab,B.Fab")
            .arg("--use-drill-file-origin")
            .arg(kicad_pcb_path.to_string_lossy())
            .run()
            .context("Failed to generate fabrication drawing gerbers")?;
        copy_assembly_drawing_gerbers(&fab_gerbers_dir, &gerbers_dir)?;
        Ok(())
    })();
    let fab_cleanup_result = fs::remove_dir_all(&fab_gerbers_dir);
    fab_export_result?;
    fab_cleanup_result?;

    // Generate drill files (separate PTH/NPTH) with PDF map(s)
    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("drill")
        .arg("--output")
        .arg(gerbers_dir.to_string_lossy())
        .arg("--format")
        .arg("excellon")
        .arg("--drill-origin")
        .arg("plot")
        .arg("--excellon-zeros-format")
        .arg("decimal")
        .arg("--excellon-units")
        .arg("mm")
        .arg("--excellon-separate-th")
        .arg("--generate-map")
        .arg("--map-format")
        .arg("pdf")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate drill files")?;

    // Generate drill map(s) as Gerber X2 as well (for CAM tooling that prefers Gerber over PDF)
    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("drill")
        .arg("--output")
        .arg(gerbers_dir.to_string_lossy())
        .arg("--format")
        .arg("excellon")
        .arg("--drill-origin")
        .arg("plot")
        .arg("--excellon-zeros-format")
        .arg("decimal")
        .arg("--excellon-units")
        .arg("mm")
        .arg("--excellon-separate-th")
        .arg("--generate-map")
        .arg("--map-format")
        .arg("gerberx2")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate gerber drill map(s)")?;

    // Create gerbers.zip from the temp directory
    create_gerbers_zip(&gerbers_dir, &manufacturing_dir.join("gerbers.zip"))?;

    // Clean up temp directory
    fs::remove_dir_all(&gerbers_dir)?;

    Ok(())
}

fn copy_assembly_drawing_gerbers(source_dir: &Path, destination_dir: &Path) -> Result<usize> {
    let mut copied = 0;
    for entry in fs::read_dir(source_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("gbr") {
            continue;
        }
        let contents = fs::read_to_string(&path)?;
        if !contents.contains("%TF.FileFunction,AssemblyDrawing,") {
            continue;
        }
        fs::copy(&path, destination_dir.join(entry.file_name()))?;
        copied += 1;
    }
    merge_assembly_drawing_job_attributes(source_dir, destination_dir)?;
    Ok(copied)
}

fn merge_assembly_drawing_job_attributes(source_dir: &Path, destination_dir: &Path) -> Result<()> {
    let Some(source_job) = find_gerber_job_file(source_dir)? else {
        return Ok(());
    };
    let Some(destination_job) = find_gerber_job_file(destination_dir)? else {
        return Ok(());
    };
    let source: serde_json::Value = serde_json::from_slice(&fs::read(&source_job)?)?;
    let mut destination: serde_json::Value = serde_json::from_slice(&fs::read(&destination_job)?)?;
    let source_files = source
        .get("FilesAttributes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry
                .get("FileFunction")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|function| function.starts_with("AssemblyDrawing,"))
        })
        .cloned()
        .collect::<Vec<_>>();
    let destination_files = destination
        .get_mut("FilesAttributes")
        .and_then(serde_json::Value::as_array_mut)
        .context("primary Gerber job file has no FilesAttributes array")?;
    for source_entry in source_files {
        let source_path = source_entry.get("Path").and_then(serde_json::Value::as_str);
        if let Some(existing) = destination_files
            .iter_mut()
            .find(|entry| entry.get("Path").and_then(serde_json::Value::as_str) == source_path)
        {
            *existing = source_entry;
        } else {
            destination_files.push(source_entry);
        }
    }
    let mut serialized = serde_json::to_string_pretty(&destination)?;
    serialized.push('\n');
    fs::write(destination_job, serialized)?;
    Ok(())
}

fn find_gerber_job_file(directory: &Path) -> Result<Option<PathBuf>> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_file()
            && path.extension().and_then(|extension| extension.to_str()) == Some("gbrjob")
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// Generate pick-and-place file
fn generate_cpl(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for CPL generation")?;

    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("pos")
        .arg("--format")
        .arg("csv")
        .arg("--units")
        .arg("mm")
        .arg("--use-drill-file-origin")
        .arg("--output")
        .arg(manufacturing_dir.join("cpl.csv").to_string_lossy())
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate pick-and-place file")?;

    // Fix CPL CSV header to match expected format
    fix_cpl_header(&manufacturing_dir.join("cpl.csv"))?;

    Ok(())
}

/// Generate assembly drawings (front and back PDFs)
fn generate_assembly_drawings(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for assembly drawings")?;

    // Generate front assembly drawing
    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("pdf")
        .arg("--output")
        .arg(
            manufacturing_dir
                .join("assembly_front.pdf")
                .to_string_lossy(),
        )
        .arg("--layers")
        .arg("F.Fab,Edge.Cuts")
        .arg("--include-border-title")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate front assembly drawing")?;

    // Generate back assembly drawing
    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("pdf")
        .arg("--output")
        .arg(
            manufacturing_dir
                .join("assembly_back.pdf")
                .to_string_lossy(),
        )
        .arg("--layers")
        .arg("B.Fab,Edge.Cuts")
        .arg("--mirror")
        .arg("--include-border-title")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate back assembly drawing")?;

    Ok(())
}

/// Create a ZIP archive from gerber files directory
fn create_gerbers_zip(gerbers_dir: &Path, zip_path: &Path) -> Result<()> {
    let zip_file = fs::File::create(zip_path)?;
    let buffered = BufWriter::with_capacity(256 * 1024, zip_file);
    let mut zip = zip::ZipWriter::new(buffered);

    for entry in fs::read_dir(gerbers_dir)? {
        let entry = entry?;
        let path = entry.path();
        // Skip symlinks for safety
        if path.is_symlink() {
            continue;
        }
        if path.is_file() {
            let name = path.file_name().unwrap().to_string_lossy();
            zip.start_file(name, zip::write::FileOptions::<()>::default())?;
            std::io::copy(&mut fs::File::open(&path)?, &mut zip)?;
        }
    }
    zip.finish()?;
    Ok(())
}

/// Fix the CPL CSV header to match expected format
fn fix_cpl_header(cpl_path: &Path) -> Result<()> {
    let content = fs::read_to_string(cpl_path)?;
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() > 1 {
        let fixed_content = format!(
            "Designator,Val,Package,Mid X,Mid Y,Rotation,Layer\n{}",
            lines[1..].join("\n")
        );
        fs::write(cpl_path, fixed_content)?;
    }
    Ok(())
}

/// Generate ODB++ files
fn generate_odb(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for ODB++ generation")?;
    let odb_path = manufacturing_dir.join("odb.zip");

    KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("odb")
        .arg("--output")
        .arg(odb_path.to_string_lossy())
        .arg("--units")
        .arg("mm")
        .arg("--precision")
        .arg(ODB_EXPORT_PRECISION)
        .arg("--compression")
        .arg("zip")
        .arg(kicad_pcb_path.to_string_lossy())
        .run()
        .context("Failed to generate ODB++ files")?;

    Ok(())
}

/// Generate IPC-2581 file
fn generate_ipc2581(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let manufacturing_dir = info.staging_dir.join("manufacturing");
    fs::create_dir_all(&manufacturing_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for IPC-2581 generation")?;
    let ipc2581_path = manufacturing_dir.join("ipc2581.xml");

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

    // Generate HTML export from the IPC-2581 XML file (silently, without printing)
    let ipc2581_html_path = manufacturing_dir.join("ipc2581.html");
    let ipc_content = pcb_ipc2581_tools::utils::file::load_ipc_file(&ipc2581_path)
        .context("Failed to load IPC-2581 file for HTML export")?;
    let ipc = pcb_ipc2581_tools::ipc2581::Ipc2581::parse(&ipc_content)
        .context("Failed to parse IPC-2581 file for HTML export")?;
    let accessor = pcb_ipc2581_tools::accessors::IpcAccessor::new(&ipc);
    let html = pcb_ipc2581_tools::commands::html_export::generate_html(
        &accessor,
        pcb_ipc2581_tools::UnitFormat::Mm,
    )
    .context("Failed to generate HTML from IPC-2581")?;
    fs::write(&ipc2581_html_path, html).context("Failed to write IPC-2581 HTML export")?;

    Ok(())
}

/// Generate STEP model
fn generate_step_model(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
    let models_dir = info.staging_dir.join("3d");
    fs::create_dir_all(&models_dir)?;

    let kicad_pcb_path = info
        .staged_pcb_path()
        .context("No layout directory for STEP model generation")?;

    // Create a temp file to capture and discard verbose KiCad output
    let devnull = tempfile::tempfile()?;

    // Generate STEP model - KiCad CLI has platform-specific exit code issues
    let step_path = models_dir.join("model.step");
    let step_result = KiCadCliBuilder::new()
        .command("pcb")
        .subcommand("export")
        .subcommand("step")
        .arg("--subst-models")
        .arg("--force")
        .arg("--output")
        .arg(step_path.to_string_lossy())
        .arg("--no-dnp")
        // FIXME: kicad-imported projects have unspecified footprints, so allow these temporarily
        // .arg("--no-unspecified")
        .arg("--include-silkscreen")
        .arg(kicad_pcb_path.to_string_lossy())
        .log_file(devnull)
        .suppress_error_output(true)
        .run();

    if let Err(e) = step_result {
        if step_path.exists() {
            warn!("KiCad CLI reported error but STEP file was created: {e}");
        } else {
            return Err(e).context("Failed to generate STEP model");
        }
    }

    Ok(())
}

/// Generate VRML model
fn generate_vrml_model(info: &ReleaseInfo, _spinner: &Spinner) -> Result<()> {
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
fn run_kicad_drc(info: &ReleaseInfo, spinner: &Spinner) -> Result<Diagnostics> {
    let mut diagnostics = pcb_zen_core::Diagnostics::default();
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
        &mut diagnostics,
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
    report.add_to_diagnostics(&mut diagnostics, &display_pcb_file.to_string_lossy());

    pcb_zen_core::SuppressPass::new(info.suppress.clone()).apply(&mut diagnostics);
    if diagnostics.error_count() > 0 {
        spinner.suspend(|| crate::drc::render_diagnostics(&mut active_errors(&diagnostics), &[]));
        std::process::exit(1);
    }

    Ok(diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn release_zip_skips_materialized_stdlib() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let staging_dir = temp_dir.path().join("stage");
        fs::create_dir_all(staging_dir.join("src/.pcb/stdlib"))?;
        fs::write(staging_dir.join("src/.pcb/stdlib/interfaces.zen"), "")?;
        fs::write(staging_dir.join("src/.pcb/stdlib.lock"), "")?;
        fs::write(staging_dir.join("src/Feign.zen"), "")?;

        let zip_path = temp_dir.path().join("out.zip");
        let file = fs::File::create(&zip_path)?;
        let mut zip = ZipWriter::new(BufWriter::new(file));
        add_directory_to_zip(&mut zip, &staging_dir, &staging_dir)?;
        zip.finish()?;

        let file = fs::File::open(&zip_path)?;
        let mut archive = zip::ZipArchive::new(file)?;
        let mut names = Vec::new();
        for i in 0..archive.len() {
            names.push(archive.by_index(i)?.name().to_string());
        }

        assert!(names.contains(&"src/Feign.zen".to_string()));
        assert!(names.iter().all(|name| !should_skip_release_zip_path(name)));

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
            matcher: None,
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
                    no_match: true,
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
                "Strict BOM matching does not recognize Acme UNKNOWN (U1)",
            ]
        );
    }

    #[test]
    fn copies_only_explicit_assembly_drawing_gerbers() -> Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let source = temp_dir.path().join("source");
        let destination = temp_dir.path().join("destination");
        fs::create_dir_all(&source)?;
        fs::create_dir_all(&destination)?;
        fs::write(
            source.join("layout-F_Fab.gbr"),
            "%TF.FileFunction,AssemblyDrawing,Top*%\nM02*\n",
        )?;
        fs::write(
            source.join("layout-F_Cu.gtl"),
            "%TF.FileFunction,Copper,L1,Top*%\nM02*\n",
        )?;
        fs::write(
            source.join("layout-job.gbrjob"),
            r#"{
  "FilesAttributes": [
    {"Path":"layout-F_Fab.gbr","FileFunction":"AssemblyDrawing,Top","FilePolarity":"Positive"}
  ]
}"#,
        )?;
        fs::write(
            destination.join("layout-job.gbrjob"),
            r#"{
  "FilesAttributes": [
    {"Path":"layout-F_Cu.gtl","FileFunction":"Copper,L1,Top","FilePolarity":"Positive"}
  ]
}"#,
        )?;

        assert_eq!(copy_assembly_drawing_gerbers(&source, &destination)?, 1);

        assert!(destination.join("layout-F_Fab.gbr").is_file());
        assert!(!destination.join("layout-F_Cu.gtl").exists());
        let job: serde_json::Value =
            serde_json::from_slice(&fs::read(destination.join("layout-job.gbrjob"))?)?;
        let files = job["FilesAttributes"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|entry| {
            entry["FileFunction"] == "AssemblyDrawing,Top" && entry["Path"] == "layout-F_Fab.gbr"
        }));
        Ok(())
    }
}
