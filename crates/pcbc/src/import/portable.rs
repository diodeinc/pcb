use super::{ImportSourceKind, PortableExtraFile, PortableKicadProject};
use anyhow::{Context, Result, bail};
use pcb_sexpr::{Sexpr, parse as parse_sexpr};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use zip::ZipWriter;

const KICAD_PRO_EXT: &str = "kicad_pro";
const KICAD_PCB_EXT: &str = "kicad_pcb";
const KICAD_SCH_EXT: &str = "kicad_sch";
const KICAD_SYM_EXT: &str = "kicad_sym";
const KICAD_MOD_EXT: &str = "kicad_mod";
const KICAD_DRU_EXT: &str = "kicad_dru";

const SYM_LIB_TABLE_FILE: &str = "sym-lib-table";
const FP_LIB_TABLE_FILE: &str = "fp-lib-table";
const KICAD_COMMON_JSON_FILE: &str = "kicad_common.json";

const MANIFEST_FILE_NAME: &str = "export_manifest.json";

#[derive(Default)]
struct SexprDiscovery {
    sheetfile_refs: BTreeSet<String>,
    symbol_ids: BTreeSet<String>,
    footprint_ids: BTreeSet<String>,
    model_refs: BTreeSet<String>,
}

struct KicadVariableResolver {
    vars: BTreeMap<String, String>,
}

struct ProjectLibraryTables {
    /// The project library tables that are actually present, for staging alongside the sources. The
    /// loader has already tested each path, so callers stage these rather than re-checking.
    existing_tables: Vec<PathBuf>,
    symbols: BTreeMap<String, String>,
    footprints: BTreeMap<String, String>,
    /// Footprint libraries registered in the user's global `fp-lib-table`. Registering a
    /// third-party library globally is the normal KiCad workflow, so a project table is often
    /// empty even though every footprint resolves in KiCad.
    global_footprints: BTreeMap<String, GlobalFootprintLibrary>,
}

struct GlobalFootprintLibrary {
    /// Directory of the global table, used as the base for relative URIs.
    table_dir: PathBuf,
    uri: String,
}

#[derive(Default)]
struct SchematicAssets {
    files: BTreeSet<PathBuf>,
    symbol_ids: BTreeSet<String>,
    footprint_ids: BTreeSet<String>,
    model_refs: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize)]
struct KicadProjectManifest {
    project_dir: String,
    source_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_file: Option<String>,
    root_schematic: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pcb_file: Option<String>,
    schematic_files: Vec<String>,
    files: Vec<String>,
    bundled_models: Vec<String>,
}

pub(super) fn discover_and_validate(kicad_input_abs: &Path) -> Result<PortableKicadProject> {
    match kicad_input_abs.extension().and_then(|ext| ext.to_str()) {
        Some(KICAD_PRO_EXT) => discover_project_and_validate(kicad_input_abs),
        Some(KICAD_SCH_EXT) => discover_schematic_and_validate(kicad_input_abs),
        _ => bail!(
            "Expected a .kicad_sch or .kicad_pro file path, got: {}",
            kicad_input_abs.display()
        ),
    }
}

fn load_project_library_tables(project_dir: &Path) -> Result<ProjectLibraryTables> {
    let sym_path = project_dir.join(SYM_LIB_TABLE_FILE);
    let fp_path = project_dir.join(FP_LIB_TABLE_FILE);
    let symbols = if sym_path.is_file() {
        parse_library_table(&sym_path, "sym_lib_table")?
    } else {
        BTreeMap::new()
    };
    let footprints = if fp_path.is_file() {
        parse_library_table(&fp_path, "fp_lib_table")?
    } else {
        BTreeMap::new()
    };
    Ok(ProjectLibraryTables {
        existing_tables: [sym_path, fp_path]
            .into_iter()
            .filter(|path| path.is_file())
            .collect(),
        symbols,
        footprints,
        global_footprints: load_global_footprint_table(),
    })
}

/// Footprint libraries from the user's global `fp-lib-table`, keyed by library nickname.
///
/// KiCad resolves a footprint through the project table *or* the global one, so a design whose
/// project table is empty still resolves in KiCad. Import reads global state only to locate the
/// referenced `.kicad_mod` files, which it then copies into the generated component packages, so
/// the machine's KiCad configuration is baked into the output at import time rather than becoming
/// an ongoing dependency of the generated board.
///
/// A malformed or unreadable global table is skipped rather than failing the import: it is machine
/// state the imported design does not own.
fn load_global_footprint_table() -> BTreeMap<String, GlobalFootprintLibrary> {
    global_footprint_table_from_paths(&discover_kicad_config_files(FP_LIB_TABLE_FILE))
}

/// Ascending KiCad version order, so a newer configuration wins on a nickname conflict.
fn global_footprint_table_from_paths(
    table_paths: &[PathBuf],
) -> BTreeMap<String, GlobalFootprintLibrary> {
    let mut libraries = BTreeMap::new();
    for table_path in table_paths {
        let Ok(entries) = parse_library_table(table_path, "fp_lib_table") else {
            continue;
        };
        let Some(table_dir) = table_path.parent().map(Path::to_path_buf) else {
            continue;
        };
        for (nickname, uri) in entries {
            libraries.insert(
                nickname,
                GlobalFootprintLibrary {
                    table_dir: table_dir.clone(),
                    uri,
                },
            );
        }
    }
    libraries
}

fn discover_schematic_assets(
    project_dir: &Path,
    root_schematic_abs: &Path,
    variable_resolver: &KicadVariableResolver,
) -> Result<SchematicAssets> {
    let mut assets = SchematicAssets::default();
    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::from([root_schematic_abs.to_path_buf()]);

    while let Some(current_abs) = queue.pop_front() {
        if !visited.insert(current_abs.clone()) {
            continue;
        }
        assets.files.insert(current_abs.clone());

        let content = fs::read_to_string(&current_abs)
            .with_context(|| format!("Failed to read schematic {}", current_abs.display()))?;
        let discovery = discover_from_sexpr_text(&content)
            .with_context(|| format!("Failed to parse schematic {}", current_abs.display()))?;
        assets.symbol_ids.extend(discovery.symbol_ids);
        assets.footprint_ids.extend(discovery.footprint_ids);
        assets.model_refs.extend(discovery.model_refs);

        for sheet_ref in discovery.sheetfile_refs {
            let base_dir = current_abs.parent().unwrap_or(project_dir);
            let child_abs =
                resolve_reference_path(project_dir, base_dir, &sheet_ref, variable_resolver)?;
            if child_abs.extension().and_then(|ext| ext.to_str()) != Some(KICAD_SCH_EXT) {
                bail!(
                    "Sheetfile reference must point to .kicad_sch, got '{}' in {}",
                    sheet_ref,
                    current_abs.display()
                );
            }
            queue.push_back(child_abs);
        }
    }

    Ok(assets)
}

fn resolve_project_library_assets(
    project_dir: &Path,
    variable_resolver: &KicadVariableResolver,
    tables: &ProjectLibraryTables,
    assets: &SchematicAssets,
    abs_files: &mut BTreeSet<PathBuf>,
) -> (BTreeMap<String, PathBuf>, BTreeSet<String>) {
    for identifier in &assets.symbol_ids {
        let Some((library_nickname, _)) = parse_library_identifier(identifier) else {
            continue;
        };
        let Some(uri) = tables.symbols.get(&library_nickname) else {
            continue;
        };
        if let Ok(path) = resolve_symbol_library_uri(project_dir, uri, variable_resolver) {
            abs_files.insert(path);
        }
    }

    let mut footprints = BTreeMap::new();
    let mut project_footprint_ids = BTreeSet::new();
    for identifier in &assets.footprint_ids {
        let Some((library_nickname, entry_name)) = parse_library_identifier(identifier) else {
            continue;
        };

        // An enabled project-table entry wins on a nickname conflict, matching KiCad. Disabled
        // project rows are omitted by `parse_library_table`, so lookup falls through to global.
        if let Some(uri) = tables.footprints.get(&library_nickname) {
            project_footprint_ids.insert(identifier.clone());
            if let Ok(path) = resolve_footprint_library_uri(
                project_dir,
                project_dir,
                uri,
                &entry_name,
                variable_resolver,
                false,
            ) {
                abs_files.insert(path.clone());
                footprints.insert(identifier.clone(), path);
            }
            continue;
        }

        // Global libraries live outside the project directory, so their files are not staged or
        // archived alongside the project sources; extraction reads the resolved file and embeds
        // the geometry in the generated component package.
        let Some(library) = tables.global_footprints.get(&library_nickname) else {
            continue;
        };
        if let Ok(path) = resolve_footprint_library_uri(
            project_dir,
            &library.table_dir,
            &library.uri,
            &entry_name,
            variable_resolver,
            true,
        ) {
            footprints.insert(identifier.clone(), path);
        }
    }
    (footprints, project_footprint_ids)
}

fn bundle_models(
    project_dir: &Path,
    model_refs: &BTreeSet<String>,
    variable_resolver: &KicadVariableResolver,
) -> Vec<PortableExtraFile> {
    let mut files = Vec::new();
    let mut used_paths = BTreeSet::new();
    for model_ref in model_refs {
        let Ok(source_path) = resolve_model_path(project_dir, model_ref, variable_resolver) else {
            continue;
        };
        let hint = model_archive_hint(model_ref, &source_path);
        files.push(PortableExtraFile {
            source_path,
            archive_relative_path: ensure_unique_archive_path(&mut used_paths, &hint),
        });
    }
    files
}

fn relative_sorted(project_dir: &Path, files: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    files
        .iter()
        .map(|path| to_relative(project_dir, path))
        .collect()
}

fn discover_project_and_validate(kicad_pro_abs: &Path) -> Result<PortableKicadProject> {
    if !kicad_pro_abs.exists() {
        bail!(
            "KiCad project file does not exist: {}",
            kicad_pro_abs.display()
        );
    }
    if !kicad_pro_abs.is_file()
        || kicad_pro_abs.extension().and_then(|ext| ext.to_str()) != Some(KICAD_PRO_EXT)
    {
        bail!(
            "Expected a .kicad_pro file path, got: {}",
            kicad_pro_abs.display()
        );
    }

    let kicad_pro_abs = kicad_pro_abs
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", kicad_pro_abs.display()))?;
    let project_dir = kicad_pro_abs.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Failed to determine project directory from {}",
            kicad_pro_abs.display()
        )
    })?;

    let kicad_pro_rel = to_relative(project_dir, &kicad_pro_abs);
    let project_name = kicad_pro_abs
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Failed to infer project name from .kicad_pro filename"))?
        .to_string();

    let kicad_pro_json = load_kicad_pro_json(&kicad_pro_abs)?;
    let variable_resolver = build_kicad_variable_resolver(project_dir, &kicad_pro_json);

    let kicad_refs = collect_kicad_refs_from_json(&kicad_pro_json);
    let primary_pcb_abs =
        resolve_primary_pcb_from_pro(project_dir, &project_name, &kicad_refs, &variable_resolver)?;
    let root_schematic_abs = resolve_root_schematic_from_pro(
        project_dir,
        &project_name,
        &kicad_refs,
        &variable_resolver,
    )?;

    // Validate root schematic UUID if present in project.
    if let Ok(root_uuid) = extract_root_uuid(&kicad_pro_json)
        && let Some(root_sch_uuid) = extract_first_schematic_uuid(&root_schematic_abs)?
        && root_sch_uuid != root_uuid
    {
        bail!(
            "Root schematic UUID mismatch: .kicad_pro says '{}', but '{}' has '{}'",
            root_uuid,
            root_schematic_abs.display(),
            root_sch_uuid
        );
    }

    let mut abs_files: BTreeSet<PathBuf> = BTreeSet::new();
    abs_files.insert(kicad_pro_abs.clone());
    abs_files.insert(primary_pcb_abs.clone());
    abs_files.insert(root_schematic_abs.clone());

    // KiCad loads project design rules by the project filename rather than an explicit project
    // reference. Preserve the conventional rules file in validation staging and the archive.
    let design_rules_name = format!("{project_name}.{KICAD_DRU_EXT}");
    let design_rules_path = project_dir.join(&design_rules_name);
    if design_rules_path.exists() {
        abs_files.insert(resolve_reference_path(
            project_dir,
            project_dir,
            &design_rules_name,
            &variable_resolver,
        )?);
    }

    let library_tables = load_project_library_tables(project_dir)?;
    abs_files.extend(library_tables.existing_tables.iter().cloned());

    // Include direct project references from .kicad_pro.
    for reference in &kicad_refs {
        let Some(ext) = extension_of_reference(reference) else {
            continue;
        };
        if !is_relevant_kicad_extension(&ext) {
            continue;
        }
        let resolved =
            resolve_reference_path(project_dir, project_dir, reference, &variable_resolver)?;
        abs_files.insert(resolved);
    }

    let mut referenced_assets =
        discover_schematic_assets(project_dir, &root_schematic_abs, &variable_resolver)?;
    abs_files.extend(referenced_assets.files.iter().cloned());

    // Include references embedded in the PCB in addition to schematic assets.
    let pcb_content = fs::read_to_string(&primary_pcb_abs)
        .with_context(|| format!("Failed to read {}", primary_pcb_abs.display()))?;
    let pcb_discovery = discover_from_sexpr_text(&pcb_content)
        .with_context(|| format!("Failed to parse {}", primary_pcb_abs.display()))?;
    referenced_assets
        .symbol_ids
        .extend(pcb_discovery.symbol_ids);
    referenced_assets
        .footprint_ids
        .extend(pcb_discovery.footprint_ids);
    referenced_assets
        .model_refs
        .extend(pcb_discovery.model_refs);

    let (resolved_project_footprints, project_footprint_ids) = resolve_project_library_assets(
        project_dir,
        &variable_resolver,
        &library_tables,
        &referenced_assets,
        &mut abs_files,
    );
    let extra_files_to_bundle = bundle_models(
        project_dir,
        &referenced_assets.model_refs,
        &variable_resolver,
    );
    let schematic_files_rel = relative_sorted(project_dir, &referenced_assets.files);

    let root_schematic_rel = to_relative(project_dir, &root_schematic_abs);
    let primary_kicad_pcb_rel = to_relative(project_dir, &primary_pcb_abs);
    let files_to_bundle_rel = relative_sorted(project_dir, &abs_files);

    // Emit a small manifest into the archive for reproducibility/debugging.
    let manifest = KicadProjectManifest {
        project_dir: project_dir.display().to_string(),
        source_kind: "project".to_string(),
        project_file: Some(path_to_posix_string(&kicad_pro_rel)),
        root_schematic: path_to_posix_string(&root_schematic_rel),
        pcb_file: Some(path_to_posix_string(&primary_kicad_pcb_rel)),
        schematic_files: schematic_files_rel
            .iter()
            .map(|p| path_to_posix_string(p))
            .collect(),
        files: files_to_bundle_rel
            .iter()
            .map(|p| path_to_posix_string(p))
            .collect(),
        bundled_models: extra_files_to_bundle
            .iter()
            .map(|f| f.archive_relative_path.clone())
            .collect(),
    };
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .context("Failed to serialize portable KiCad manifest")?;

    Ok(PortableKicadProject {
        project_dir: project_dir.to_path_buf(),
        project_name,
        source_kind: ImportSourceKind::Project,
        kicad_pro_rel: Some(kicad_pro_rel),
        root_schematic_rel,
        primary_kicad_pcb_rel: Some(primary_kicad_pcb_rel),
        schematic_files_rel,
        files_to_bundle_rel,
        resolved_project_footprints,
        project_footprint_ids,
        extra_files_to_bundle,
        manifest_json,
    })
}

fn discover_schematic_and_validate(kicad_sch_abs: &Path) -> Result<PortableKicadProject> {
    if !kicad_sch_abs.is_file()
        || kicad_sch_abs.extension().and_then(|ext| ext.to_str()) != Some(KICAD_SCH_EXT)
    {
        bail!(
            "Expected a .kicad_sch file path, got: {}",
            kicad_sch_abs.display()
        );
    }

    let kicad_sch_abs = kicad_sch_abs
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", kicad_sch_abs.display()))?;
    let project_dir = kicad_sch_abs.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Failed to determine schematic directory from {}",
            kicad_sch_abs.display()
        )
    })?;
    let project_name = kicad_sch_abs
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Failed to infer board name from .kicad_sch filename"))?
        .to_string();
    let root_schematic_rel = to_relative(project_dir, &kicad_sch_abs);

    let variable_resolver = build_kicad_variable_resolver(project_dir, &Value::Null);
    let library_tables = load_project_library_tables(project_dir)?;
    let schematic_assets =
        discover_schematic_assets(project_dir, &kicad_sch_abs, &variable_resolver)?;

    let mut abs_files = schematic_assets.files.clone();
    abs_files.extend(library_tables.existing_tables.iter().cloned());
    let (resolved_project_footprints, project_footprint_ids) = resolve_project_library_assets(
        project_dir,
        &variable_resolver,
        &library_tables,
        &schematic_assets,
        &mut abs_files,
    );
    let extra_files_to_bundle = bundle_models(
        project_dir,
        &schematic_assets.model_refs,
        &variable_resolver,
    );
    let schematic_files_rel = relative_sorted(project_dir, &schematic_assets.files);
    let files_to_bundle_rel = relative_sorted(project_dir, &abs_files);

    let manifest = KicadProjectManifest {
        project_dir: project_dir.display().to_string(),
        source_kind: "schematic".to_string(),
        project_file: None,
        root_schematic: path_to_posix_string(&root_schematic_rel),
        pcb_file: None,
        schematic_files: schematic_files_rel
            .iter()
            .map(|p| path_to_posix_string(p))
            .collect(),
        files: files_to_bundle_rel
            .iter()
            .map(|p| path_to_posix_string(p))
            .collect(),
        bundled_models: extra_files_to_bundle
            .iter()
            .map(|f| f.archive_relative_path.clone())
            .collect(),
    };
    let manifest_json = serde_json::to_string_pretty(&manifest)
        .context("Failed to serialize portable KiCad manifest")?;

    Ok(PortableKicadProject {
        project_dir: project_dir.to_path_buf(),
        project_name,
        source_kind: ImportSourceKind::Schematic,
        kicad_pro_rel: None,
        root_schematic_rel,
        primary_kicad_pcb_rel: None,
        schematic_files_rel,
        files_to_bundle_rel,
        resolved_project_footprints,
        project_footprint_ids,
        extra_files_to_bundle,
        manifest_json,
    })
}

fn private_tempdir(prefix: &str) -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    builder.tempdir()
}

pub(super) fn stage_project_files(project: &PortableKicadProject) -> Result<tempfile::TempDir> {
    // 0700: the staged tree is a copy of the user's KiCad sources in a shared temp directory.
    let temp = private_tempdir("pcb-import-kicad-sources-")
        .context("Failed to stage KiCad source files")?;
    for relative in &project.files_to_bundle_rel {
        let source = project.project_dir.join(relative);
        let destination = temp.path().join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(&source, &destination)
            .with_context(|| format!("Failed to stage KiCad source {}", source.display()))?;
    }
    Ok(temp)
}

/// Write the portable KiCad source archive using the established streaming project-import path.
pub(super) fn write_portable_zip(
    project: &PortableKicadProject,
    staged_root: &Path,
    output_zip: &Path,
) -> Result<()> {
    if let Some(parent) = output_zip.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create output directory for archive: {}",
                parent.display()
            )
        })?;
    }

    let output_file = fs::File::create(output_zip)
        .with_context(|| format!("Failed to create archive: {}", output_zip.display()))?;
    let mut zip = ZipWriter::new(BufWriter::new(output_file));

    for relative in &project.files_to_bundle_rel {
        let absolute = staged_root.join(relative);
        let archive_path = format!(
            "{}/{}",
            project.project_name,
            path_to_posix_string(relative)
        );
        add_file_to_zip(&mut zip, &absolute, &archive_path)?;
    }

    for extra in &project.extra_files_to_bundle {
        add_file_to_zip(
            &mut zip,
            &extra.source_path,
            &extra.archive_relative_path.replace('\\', "/"),
        )?;
    }

    zip.start_file(MANIFEST_FILE_NAME, zip::write::FileOptions::<()>::default())?;
    zip.write_all(project.manifest_json.as_bytes())
        .context("Failed to write project manifest to archive")?;

    zip.finish()
        .with_context(|| format!("Failed to finalize archive: {}", output_zip.display()))?;
    Ok(())
}

fn add_file_to_zip<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    source_path: &Path,
    archive_path: &str,
) -> Result<()> {
    let meta = fs::symlink_metadata(source_path)
        .with_context(|| format!("Failed to stat {}", source_path.display()))?;
    if meta.file_type().is_symlink() {
        bail!(
            "Symlinked referenced files are not supported: {}",
            source_path.display()
        );
    }
    if !meta.is_file() {
        bail!("Referenced path is not a file: {}", source_path.display());
    }

    zip.start_file(archive_path, zip::write::FileOptions::<()>::default())?;
    let mut input = fs::File::open(source_path)
        .with_context(|| format!("Failed to open input file: {}", source_path.display()))?;
    std::io::copy(&mut input, zip)
        .with_context(|| format!("Failed to add file to archive: {}", source_path.display()))?;
    Ok(())
}

fn path_to_posix_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn load_kicad_pro_json(kicad_pro_abs: &Path) -> Result<Value> {
    let content = fs::read_to_string(kicad_pro_abs)
        .with_context(|| format!("Failed to read {}", kicad_pro_abs.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", kicad_pro_abs.display()))
}

fn build_kicad_variable_resolver(
    project_dir: &Path,
    kicad_pro_json: &Value,
) -> KicadVariableResolver {
    let mut vars = BTreeMap::new();

    // KiCad user settings variables from kicad_common.json files.
    for path in discover_kicad_config_files(KICAD_COMMON_JSON_FILE) {
        for (key, value) in load_user_environment_vars_from_common_json(&path) {
            vars.insert(key, value);
        }
    }

    // Process environment overrides user settings.
    for (key, value) in env::vars() {
        vars.insert(key, value);
    }

    // Project text variables have highest precedence.
    if let Some(text_vars) = kicad_pro_json
        .get("text_variables")
        .and_then(|v| v.as_object())
    {
        for (key, value) in text_vars {
            if let Some(value) = value.as_str() {
                vars.insert(key.clone(), value.to_string());
            }
        }
    }

    // KIPRJMOD is special and always bound to current project directory.
    //
    // Use the on-disk path representation (including any Windows verbatim prefix)
    // and normalize it later when resolving expanded paths.
    vars.insert(
        "KIPRJMOD".to_string(),
        project_dir.to_string_lossy().into_owned(),
    );
    KicadVariableResolver { vars }
}

/// KiCad configuration roots for this machine, honouring `KICAD_CONFIG_HOME` and the platform
/// defaults. Both `kicad_common.json` and the global `fp-lib-table` live under these.
fn kicad_config_roots() -> BTreeSet<PathBuf> {
    let mut roots = BTreeSet::new();

    if let Ok(config_home) = env::var("KICAD_CONFIG_HOME")
        && !config_home.is_empty()
    {
        // KiCad treats this as an override for the platform configuration root, not an
        // additional lower-priority location.
        roots.insert(PathBuf::from(config_home));
        return roots;
    }

    // `dirs::config_dir` honors XDG_CONFIG_HOME on Linux and APPDATA on Windows.
    if let Some(config_dir) = dirs::config_dir() {
        roots.insert(config_dir.join("kicad"));
    }

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        roots.insert(home.join(".config/kicad"));
        roots.insert(home.join("Library/Preferences/kicad"));
    }

    if let Ok(app_data) = env::var("APPDATA")
        && !app_data.is_empty()
    {
        roots.insert(PathBuf::from(app_data).join("kicad"));
    }

    roots
}

/// Every `file_name` found directly in a KiCad configuration root or in one of its versioned
/// subdirectories, ordered by ascending KiCad version so later entries override earlier ones.
fn discover_kicad_config_files(file_name: &str) -> Vec<PathBuf> {
    let mut files = BTreeSet::new();
    for root in kicad_config_roots() {
        let top = root.join(file_name);
        if top.is_file() {
            files.insert(top);
        }

        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                    continue;
                }
                let candidate = entry.path().join(file_name);
                if candidate.is_file() {
                    files.insert(candidate);
                }
            }
        }
    }

    let mut files = files.into_iter().collect::<Vec<_>>();
    files.sort_by(|a, b| compare_kicad_common_paths(a, b));
    files
}

fn compare_kicad_common_paths(a: &Path, b: &Path) -> std::cmp::Ordering {
    let key_a = kicad_common_version_key(a);
    let key_b = kicad_common_version_key(b);
    key_a.cmp(&key_b)
}

fn kicad_common_version_key(path: &Path) -> (u8, Vec<u32>, String) {
    let parent_name = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let parts = parent_name
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>();

    match parts {
        Ok(parts) if !parts.is_empty() => (1, parts, parent_name),
        _ => (0, Vec::new(), parent_name),
    }
}

fn load_user_environment_vars_from_common_json(path: &Path) -> BTreeMap<String, String> {
    let Ok(content) = fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    let Ok(json) = serde_json::from_str::<Value>(&content) else {
        return BTreeMap::new();
    };
    let Some(vars) = json
        .get("environment")
        .and_then(|v| v.get("vars"))
        .and_then(|v| v.as_object())
    else {
        return BTreeMap::new();
    };

    vars.iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}

fn extract_root_uuid(kicad_pro_json: &Value) -> Result<String> {
    let sheets = kicad_pro_json
        .get("sheets")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("Missing or invalid 'sheets' array in .kicad_pro"))?;

    let mut root_uuid = None;
    for sheet in sheets {
        let Some(entry) = sheet.as_array() else {
            continue;
        };
        if entry.len() < 2 {
            continue;
        }
        let Some(uuid) = entry[0].as_str() else {
            continue;
        };
        let Some(name) = entry[1].as_str() else {
            continue;
        };
        if name == "Root" {
            if root_uuid.is_some() {
                bail!("Multiple 'Root' entries in .kicad_pro sheets array");
            }
            root_uuid = Some(uuid.to_string());
        }
    }

    root_uuid.ok_or_else(|| anyhow::anyhow!("No 'Root' sheet entry found in .kicad_pro"))
}

fn collect_kicad_refs_from_json(value: &Value) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    collect_refs_recursive(value, &mut refs);
    refs
}

fn collect_refs_recursive(value: &Value, refs: &mut BTreeSet<String>) {
    match value {
        Value::String(s)
            if extension_of_reference(s).is_some_and(|ext| is_relevant_kicad_extension(&ext)) =>
        {
            refs.insert(s.clone());
        }
        Value::Array(arr) => {
            for item in arr {
                collect_refs_recursive(item, refs);
            }
        }
        Value::Object(map) => {
            for value in map.values() {
                collect_refs_recursive(value, refs);
            }
        }
        _ => {}
    }
}

fn resolve_primary_pcb_from_pro(
    project_dir: &Path,
    project_name: &str,
    references: &BTreeSet<String>,
    variable_resolver: &KicadVariableResolver,
) -> Result<PathBuf> {
    let pcb_refs = references
        .iter()
        .filter(|r| extension_of_reference(r.as_str()).as_deref() == Some(KICAD_PCB_EXT))
        .collect::<Vec<_>>();

    if pcb_refs.len() > 1 {
        let refs = pcb_refs
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "Expected at most one .kicad_pcb reference in .kicad_pro, found {}: {}",
            pcb_refs.len(),
            refs
        );
    }

    let default_pcb = format!("{project_name}.{KICAD_PCB_EXT}");
    let reference = pcb_refs
        .first()
        .map(|s| s.as_str())
        .unwrap_or(default_pcb.as_str());
    resolve_reference_path(project_dir, project_dir, reference, variable_resolver)
}

fn resolve_root_schematic_from_pro(
    project_dir: &Path,
    project_name: &str,
    references: &BTreeSet<String>,
    variable_resolver: &KicadVariableResolver,
) -> Result<PathBuf> {
    let sch_refs = references
        .iter()
        .filter(|r| extension_of_reference(r.as_str()).as_deref() == Some(KICAD_SCH_EXT))
        .collect::<Vec<_>>();

    let default_root = format!("{project_name}.{KICAD_SCH_EXT}");
    let reference = if sch_refs.iter().any(|s| s.as_str() == default_root) {
        default_root.as_str()
    } else if sch_refs.len() == 1 {
        sch_refs[0].as_str()
    } else {
        default_root.as_str()
    };

    resolve_reference_path(project_dir, project_dir, reference, variable_resolver)
}

fn extract_first_schematic_uuid(schematic_abs: &Path) -> Result<Option<String>> {
    let content = fs::read_to_string(schematic_abs)
        .with_context(|| format!("Failed to read schematic {}", schematic_abs.display()))?;
    Ok(extract_first_schematic_uuid_from_text(&content))
}

fn extract_first_schematic_uuid_from_text(content: &str) -> Option<String> {
    let root = parse_sexpr(content).ok()?;
    let items = root.as_list()?;

    if items.first().and_then(|node| node.as_sym()) != Some("kicad_sch") {
        return None;
    }

    for node in &items[1..] {
        if let Some(uuid_items) = node.as_list()
            && uuid_items.first().and_then(|item| item.as_sym()) == Some("uuid")
            && let Some(uuid) = uuid_items.get(1).and_then(atom_or_string)
        {
            return Some(uuid.to_string());
        }
    }
    None
}

fn parse_library_table(path: &Path, table_tag: &str) -> Result<BTreeMap<String, String>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read library table {}", path.display()))?;
    let root = parse_sexpr(&content)
        .with_context(|| format!("Failed to parse library table {}", path.display()))?;

    let items = root
        .as_list()
        .ok_or_else(|| anyhow::anyhow!("Invalid library table root in {}", path.display()))?;

    if items.first().and_then(|item| item.as_sym()) != Some(table_tag) {
        bail!(
            "Expected '{}' root in {}, got {:?}",
            table_tag,
            path.display(),
            items.first().and_then(|item| item.as_sym())
        );
    }

    let mut map = BTreeMap::new();
    for node in &items[1..] {
        let Some(lib_items) = node.as_list() else {
            continue;
        };
        if lib_items.first().and_then(|item| item.as_sym()) != Some("lib") {
            continue;
        }

        let mut name = None::<String>;
        let mut uri = None::<String>;
        // KiCad's library-table grammar treats `(disabled)` as a flag with no arguments. A
        // disabled row is ignored for nickname lookup so a later table (global after project,
        // or a newer global config) can supply the same nickname.
        let mut disabled = false;
        for field in &lib_items[1..] {
            let Some(field_items) = field.as_list() else {
                continue;
            };
            match field_items.first().and_then(|item| item.as_sym()) {
                Some("name") => {
                    if let Some(value) = field_items.get(1).and_then(atom_or_string) {
                        name = Some(value.to_string());
                    }
                }
                Some("uri") => {
                    if let Some(value) = field_items.get(1).and_then(atom_or_string) {
                        uri = Some(value.to_string());
                    }
                }
                Some("disabled") => {
                    disabled = true;
                }
                _ => {}
            }
        }

        if disabled {
            continue;
        }

        if let (Some(name), Some(uri)) = (name, uri) {
            map.insert(name, uri);
        }
    }

    Ok(map)
}

fn discover_from_sexpr_text(content: &str) -> Result<SexprDiscovery> {
    let mut discovery = SexprDiscovery::default();
    let root = parse_sexpr(content)?;
    walk_sexpr(&root, &mut discovery);
    Ok(discovery)
}

fn walk_sexpr(node: &Sexpr, discovery: &mut SexprDiscovery) {
    let Some(items) = node.as_list() else {
        return;
    };

    if let Some(tag) = items.first().and_then(|item| item.as_sym()) {
        match tag {
            "lib_id" => {
                if let Some(identifier) = items.get(1).and_then(atom_or_string) {
                    discovery.symbol_ids.insert(identifier.to_string());
                }
            }
            "footprint" => {
                if let Some(identifier) = items.get(1).and_then(atom_or_string) {
                    discovery.footprint_ids.insert(identifier.to_string());
                }
            }
            "property" => {
                if items.get(1).and_then(atom_or_string) == Some("Sheetfile") {
                    if let Some(value) = items.get(2).and_then(atom_or_string) {
                        discovery.sheetfile_refs.insert(value.to_string());
                    }
                } else if items.get(1).and_then(atom_or_string) == Some("Footprint")
                    && let Some(identifier) = items.get(2).and_then(atom_or_string)
                {
                    discovery.footprint_ids.insert(identifier.to_string());
                }
            }
            "model" => {
                if let Some(model_path) = items.get(1).and_then(atom_or_string) {
                    discovery.model_refs.insert(model_path.to_string());
                }
            }
            _ => {}
        }
    }

    for child in items {
        walk_sexpr(child, discovery);
    }
}

fn resolve_model_path(
    project_dir: &Path,
    model_reference: &str,
    variable_resolver: &KicadVariableResolver,
) -> std::result::Result<PathBuf, String> {
    // Borrowed from `pcb export` (commit 7c1da9bb): model references may point outside the
    // project dir, so we allow external paths here and stage them into `models/...` inside the zip.
    resolve_file_reference(
        project_dir,
        project_dir,
        model_reference,
        variable_resolver,
        ResolveRefOptions {
            allow_external: true,
            allow_directory: false,
            kind: "model file",
        },
    )
}

fn model_archive_hint(model_reference: &str, resolved_path: &Path) -> String {
    artifact_archive_hint("models", model_reference, resolved_path)
}

fn artifact_archive_hint(prefix: &str, reference: &str, resolved_path: &Path) -> String {
    if let Some((var_name, remainder)) = split_leading_variable(reference)
        && !remainder.is_empty()
    {
        return format!(
            "{}/{}/{}",
            prefix,
            sanitize_archive_segment(&var_name),
            normalize_archive_path(&remainder)
        );
    }

    if !Path::new(reference).is_absolute() && !reference.contains("${") && !reference.contains("$(")
    {
        return format!("{}/project/{}", prefix, normalize_archive_path(reference));
    }

    format!(
        "{}/absolute/{}",
        prefix,
        normalize_archive_path(&path_to_portable_string(resolved_path))
    )
}

fn split_leading_variable(input: &str) -> Option<(String, String)> {
    let (open, close) = if input.starts_with("${") {
        ("${", '}')
    } else if input.starts_with("$(") {
        ("$(", ')')
    } else {
        return None;
    };

    let rest = &input[open.len()..];
    let end = rest.find(close)?;
    let var_name = &rest[..end];
    if var_name.is_empty() {
        return None;
    }
    let remainder = rest[end + 1..]
        .trim_start_matches('/')
        .trim_start_matches('\\')
        .to_string();
    Some((var_name.to_string(), remainder))
}

fn sanitize_archive_segment(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "var".to_string()
    } else {
        out
    }
}

fn normalize_archive_path(path: &str) -> String {
    let mut out = path.replace('\\', "/");
    while out.starts_with('/') {
        out.remove(0);
    }
    while out.contains("//") {
        out = out.replace("//", "/");
    }

    // Remove `..` segments to avoid path traversal in archives.
    let mut parts: Vec<&str> = Vec::new();
    for part in out.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop();
            continue;
        }
        parts.push(part);
    }
    parts.join("/")
}

fn path_to_portable_string(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            Component::Prefix(prefix) => {
                Some(prefix.as_os_str().to_string_lossy().replace(':', ""))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn ensure_unique_archive_path(used: &mut BTreeSet<String>, hint: &str) -> String {
    if used.insert(hint.to_string()) {
        return hint.to_string();
    }

    let (base, ext) = split_extension(hint);
    let mut idx = 2usize;
    loop {
        let candidate = if ext.is_empty() {
            format!("{base}_{idx}")
        } else {
            format!("{base}_{idx}.{ext}")
        };
        if used.insert(candidate.clone()) {
            return candidate;
        }
        idx += 1;
    }
}

fn split_extension(path: &str) -> (String, String) {
    let ext = Path::new(path)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    if ext.is_empty() {
        (path.to_string(), String::new())
    } else {
        let suffix = format!(".{ext}");
        let base = path.strip_suffix(&suffix).unwrap_or(path);
        (base.to_string(), ext.to_string())
    }
}

fn is_relevant_kicad_extension(ext: &str) -> bool {
    matches!(
        ext,
        KICAD_PRO_EXT | KICAD_PCB_EXT | KICAD_SCH_EXT | KICAD_SYM_EXT | KICAD_MOD_EXT
    )
}

fn atom_or_string(node: &Sexpr) -> Option<&str> {
    node.as_str().or_else(|| node.as_sym())
}

fn parse_library_identifier(identifier: &str) -> Option<(String, String)> {
    let (nickname, entry_name) = identifier.split_once(':')?;
    if nickname.is_empty() || entry_name.is_empty() {
        return None;
    }
    Some((nickname.to_string(), entry_name.to_string()))
}

fn resolve_symbol_library_uri(
    project_dir: &Path,
    uri: &str,
    variable_resolver: &KicadVariableResolver,
) -> std::result::Result<PathBuf, String> {
    let path = resolve_uri_path(
        project_dir,
        project_dir,
        uri,
        variable_resolver,
        false,
        false,
    )?;
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(KICAD_SYM_EXT) => Ok(path),
        Some("lib") => Err(format!(
            "Legacy .lib symbol libraries are not yet supported: {}",
            path.display()
        )),
        _ => Err(format!(
            "Symbol library URI does not point to .kicad_sym: {}",
            path.display()
        )),
    }
}

/// Resolves one footprint inside the library a table entry names.
///
/// `base_dir` is the directory a relative URI is taken against: the project directory for a
/// project table entry, the global table's directory for a global one. `allow_external` relaxes the
/// project-containment check, which global libraries never satisfy; the containment check against
/// the library directory itself still applies, so an entry name can never escape its library.
/// Resolve `path` for a containment comparison, falling back to the path itself.
///
/// Both sides of a `starts_with` containment check must be canonicalized the same way. Comparing a
/// canonicalized candidate against a raw directory silently fails wherever canonicalization rewrites
/// the prefix: on Windows it yields a `\\?\C:\...` verbatim path that never starts with `C:\...`, so
/// every project-local footprint library was judged outside the project and no footprint resolved. On
/// macOS `/var` resolving to `/private/var` has the same effect.
fn canonical_or_original(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn resolve_footprint_library_uri(
    project_dir: &Path,
    base_dir: &Path,
    uri: &str,
    footprint_name: &str,
    variable_resolver: &KicadVariableResolver,
    allow_external: bool,
) -> std::result::Result<PathBuf, String> {
    let footprint_path = Path::new(footprint_name);
    if footprint_path.components().count() != 1
        || !matches!(
            footprint_path.components().next(),
            Some(Component::Normal(_))
        )
    {
        return Err(format!(
            "Footprint entry name must not contain path components: {footprint_name:?}"
        ));
    }

    let base = resolve_uri_path(
        project_dir,
        base_dir,
        uri,
        variable_resolver,
        true,
        allow_external,
    )?;
    let candidate =
        if base.extension().and_then(|ext| ext.to_str()) == Some("pretty") || base.is_dir() {
            base.join(format!("{footprint_name}.{KICAD_MOD_EXT}"))
        } else if base.extension().and_then(|ext| ext.to_str()) == Some(KICAD_MOD_EXT) {
            base.clone()
        } else {
            return Err(format!(
                "Footprint library URI must point to a .pretty directory or .kicad_mod file: {}",
                uri
            ));
        };

    let metadata = fs::symlink_metadata(&candidate).map_err(|_| {
        format!(
            "Referenced footprint file '{}' not found from URI '{}'",
            candidate.display(),
            uri
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Symlinked footprint file is not supported: {}",
            candidate.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!(
            "Resolved footprint reference is not a file: {}",
            candidate.display()
        ));
    }

    let canonical = candidate
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize {}: {}", candidate.display(), e))?;
    if !allow_external && !canonical.starts_with(canonical_or_original(project_dir)) {
        return Err(format!(
            "External footprint file is outside project directory: {}",
            canonical.display()
        ));
    }
    if base.is_dir() && !canonical.starts_with(canonical_or_original(&base)) {
        return Err(format!(
            "Footprint file escapes its library directory: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

fn resolve_uri_path(
    project_dir: &Path,
    base_dir: &Path,
    uri: &str,
    variable_resolver: &KicadVariableResolver,
    allow_directory: bool,
    allow_external: bool,
) -> std::result::Result<PathBuf, String> {
    resolve_file_reference(
        project_dir,
        base_dir,
        uri,
        variable_resolver,
        ResolveRefOptions {
            allow_external,
            allow_directory,
            kind: "URI path",
        },
    )
}

fn resolve_reference_path(
    project_dir: &Path,
    base_dir: &Path,
    reference: &str,
    variable_resolver: &KicadVariableResolver,
) -> Result<PathBuf> {
    resolve_file_reference(
        project_dir,
        base_dir,
        reference,
        variable_resolver,
        ResolveRefOptions {
            allow_external: false,
            allow_directory: false,
            kind: "KiCad file",
        },
    )
    .map_err(|e| anyhow::anyhow!("Failed to resolve KiCad path reference '{reference}': {e}"))
}

#[derive(Debug, Clone, Copy)]
struct ResolveRefOptions {
    allow_external: bool,
    allow_directory: bool,
    kind: &'static str,
}

fn resolve_file_reference(
    project_dir: &Path,
    base_dir: &Path,
    reference: &str,
    variable_resolver: &KicadVariableResolver,
    options: ResolveRefOptions,
) -> std::result::Result<PathBuf, String> {
    let expanded_raw = variable_resolver.expand(reference)?;
    if expanded_raw.contains("://") {
        return Err(format!(
            "Unsupported non-file {} URI '{}'",
            options.kind, reference
        ));
    }

    let expanded_candidates = expanded_reference_candidates(&expanded_raw);

    let mut last_candidate: Option<PathBuf> = None;
    for expanded in &expanded_candidates {
        let ref_path = PathBuf::from(expanded.as_str());
        let candidate = if ref_path.is_absolute() {
            ref_path
        } else {
            base_dir.join(ref_path)
        };
        // Collapsed after the join, not before: a relative reference carries no verbatim prefix of its
        // own, and `join` is purely lexical, so `base_dir` — always canonicalized, hence verbatim on
        // Windows — is what contributes the prefix. Doing this on the expanded string alone would fix
        // `${KIPRJMOD}/../lib.pretty` while still failing the more common `../common/power.kicad_sch`
        // sheet reference. Absolute references are unaffected: `candidate` is then `ref_path` itself.
        let candidate = collapse_verbatim_path(&candidate);
        last_candidate = Some(candidate.clone());

        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "Symlinked {} is not supported: {}",
                options.kind,
                candidate.display()
            ));
        }
        if !(metadata.is_file() || options.allow_directory && metadata.is_dir()) {
            return Err(format!(
                "Resolved {} is not a file{}: {}",
                options.kind,
                if options.allow_directory {
                    " or directory"
                } else {
                    ""
                },
                candidate.display()
            ));
        }

        let canonical = candidate
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize {}: {}", candidate.display(), e))?;
        if !options.allow_external && !canonical.starts_with(canonical_or_original(project_dir)) {
            return Err(format!(
                "External referenced file is outside project directory: {}",
                canonical.display()
            ));
        }
        return Ok(canonical);
    }

    let candidate = last_candidate
        .unwrap_or_else(|| base_dir.join(PathBuf::from(expanded_candidates[0].as_str())));
    Err(format!(
        "Referenced {} not found: '{}' (resolved to {})",
        options.kind,
        reference,
        candidate.display()
    ))
}

#[cfg(not(windows))]
fn expanded_reference_candidates(expanded_raw: &str) -> Vec<String> {
    vec![normalize_expanded_reference_for_fs(expanded_raw)]
}

#[cfg(windows)]
fn expanded_reference_candidates(expanded_raw: &str) -> Vec<String> {
    let normalized = normalize_expanded_reference_for_fs(expanded_raw);
    if normalized == expanded_raw {
        vec![normalized]
    } else {
        // KiCad strings commonly use forward slashes even on Windows, but some
        // variable values expand to native paths. Try both forms.
        vec![normalized, expanded_raw.to_string()]
    }
}

fn normalize_expanded_reference_for_fs(expanded: &str) -> String {
    #[cfg(windows)]
    {
        // KiCad strings commonly use forward slashes even on Windows. Convert to a
        // Windows-friendly form after variable expansion, so `${KIPRJMOD}/...`
        // continues to work even when `KIPRJMOD` is a verbatim (`\\?\...`) path.
        // Any `..` this leaves behind is collapsed once the reference has been joined onto its base
        // directory, in `resolve_file_reference` — see `collapse_verbatim_path`.
        expanded.replace('/', "\\")
    }

    #[cfg(not(windows))]
    {
        expanded.to_string()
    }
}

/// [`collapse_verbatim_relative_components`] over a path, for the resolution chain.
///
/// A no-op on any path without a verbatim prefix, which is every path on a non-Windows host.
fn collapse_verbatim_path(path: &Path) -> PathBuf {
    match path.to_str() {
        // A non-UTF-8 path cannot contain a verbatim prefix this would act on without also being
        // lossy to rewrite, so it is passed through untouched.
        Some(text) => PathBuf::from(collapse_verbatim_relative_components(text)),
        None => path.to_path_buf(),
    }
}

/// Collapse `.` and `..` inside a Windows verbatim (`\\?\`) path.
///
/// Windows passes a verbatim path to the filesystem uninterpreted, so `..` inside one is a literal
/// component and `\\?\C:\project\..\lib.pretty` cannot be opened. Import canonicalizes the project
/// directory, which is verbatim on Windows, so without this no reference walking up a directory resolves.
///
/// Collapsing lexically can diverge from the filesystem across a junction, but never escapes the project:
/// the result is canonicalized and re-checked for containment. Non-verbatim paths are returned unchanged,
/// which makes this a no-op off Windows — where it stays compiled and tested so it cannot rot.
#[cfg_attr(not(windows), allow(dead_code))]
fn collapse_verbatim_relative_components(path: &str) -> String {
    const VERBATIM_PREFIX: &str = r"\\?\";

    let Some(rest) = path.strip_prefix(VERBATIM_PREFIX) else {
        return path.to_string();
    };
    if !rest.split('\\').any(|part| part == "." || part == "..") {
        return path.to_string();
    }

    // How many leading components name the volume and so must never be popped: one for a drive
    // (`C:`), three for a UNC share (`UNC\server\share`). Popping into either would produce a path
    // that names nothing.
    let root_components = if rest.starts_with("UNC\\") { 3 } else { 1 };

    let mut kept: Vec<&str> = Vec::new();
    for part in rest.split('\\') {
        match part {
            "" | "." => {}
            // A `..` with only the volume root left is dropped, matching how Windows clamps at the
            // root rather than erroring.
            ".." => {
                if kept.len() > root_components {
                    kept.pop();
                }
            }
            part => kept.push(part),
        }
    }

    // A path reduced to its root keeps a trailing separator. Without one, `\\?\C:` parses as a prefix
    // with no root, which is not absolute — `resolve_file_reference` would then join it onto the base
    // directory and report a failure against a nonsense path.
    let separator = if kept.len() == root_components {
        "\\"
    } else {
        ""
    };
    format!("{VERBATIM_PREFIX}{}{separator}", kept.join("\\"))
}

fn extension_of_reference(reference: &str) -> Option<String> {
    if reference.contains("://") {
        return None;
    }
    let path = Path::new(reference);
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

impl KicadVariableResolver {
    fn expand(&self, input: &str) -> std::result::Result<String, String> {
        let mut current = input.to_string();
        for _ in 0..16 {
            let mut changed = false;
            let expanded = self.expand_once(&current, &mut changed)?;
            if !changed {
                return Ok(expanded);
            }
            current = expanded;
        }

        Err(format!(
            "Variable expansion exceeded recursion limit for '{}'",
            input
        ))
    }

    fn expand_once(&self, input: &str, changed: &mut bool) -> std::result::Result<String, String> {
        let mut out = String::with_capacity(input.len());
        let mut cursor = 0usize;
        let bytes = input.as_bytes();

        while cursor < input.len() {
            let Some(dollar_offset) = input[cursor..].find('$') else {
                out.push_str(&input[cursor..]);
                break;
            };
            let dollar = cursor + dollar_offset;
            out.push_str(&input[cursor..dollar]);

            if dollar + 1 >= input.len() {
                out.push('$');
                break;
            }

            let next = bytes[dollar + 1];
            if next != b'{' && next != b'(' {
                out.push('$');
                cursor = dollar + 1;
                continue;
            }

            let close = if next == b'{' { '}' } else { ')' };
            let start = dollar + 2;
            let Some(end_offset) = input[start..].find(close) else {
                return Err(format!("Unterminated variable reference in '{}'", input));
            };
            let end = start + end_offset;

            let var_name = &input[start..end];
            if var_name.is_empty() {
                return Err(format!("Empty variable name in '{}'", input));
            }

            let value = self
                .lookup(var_name)
                .ok_or_else(|| format!("Unknown KiCad variable '{}' in '{}'", var_name, input))?;
            out.push_str(&value);
            *changed = true;
            cursor = end + 1;
        }

        Ok(out)
    }

    fn lookup(&self, name: &str) -> Option<String> {
        if let Some(value) = self.vars.get(name) {
            return Some(value.clone());
        }

        // KiCad legacy and versioned 3D model variables.
        if name == "KISYS3DMOD" || is_versioned_3dmodel_var(name) {
            return self.best_versioned_suffix("_3DMODEL_DIR");
        }

        None
    }

    fn best_versioned_suffix(&self, suffix: &str) -> Option<String> {
        self.vars
            .iter()
            .filter_map(|(key, value)| {
                if key.starts_with("KICAD") && key.ends_with(suffix) {
                    let mid = &key["KICAD".len()..key.len() - suffix.len()];
                    let major = mid.parse::<u32>().ok()?;
                    Some((major, value.clone()))
                } else {
                    None
                }
            })
            .max_by_key(|(major, _)| *major)
            .map(|(_, value)| value)
    }
}

fn is_versioned_3dmodel_var(name: &str) -> bool {
    if !name.starts_with("KICAD") || !name.ends_with("_3DMODEL_DIR") {
        return false;
    }
    let middle = &name["KICAD".len()..name.len() - "_3DMODEL_DIR".len()];
    !middle.is_empty() && middle.chars().all(|c| c.is_ascii_digit())
}

fn to_relative(project_dir: &Path, abs: &Path) -> PathBuf {
    abs.strip_prefix(project_dir).unwrap_or(abs).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::ZipArchive;

    #[test]
    fn discovers_root_schematic_from_kicad_pro_and_bundles_zip() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pcb-sch/test/kicad-bom");
        let pro = root.join("layout.kicad_pro");
        let mut project = discover_and_validate(&pro)?;
        assert_eq!(project.project_name, "layout");
        assert!(
            project
                .schematic_files_rel
                .iter()
                .any(|p| p == Path::new("layout.kicad_sch"))
        );

        // Preserve the project-name archive root exactly, including characters that are valid in
        // project filenames. This is established project-import behavior.
        project.project_name = "layout board".to_string();
        let dir = tempfile::tempdir()?;
        let zip_path = dir.path().join("out.zip");
        write_portable_zip(&project, &project.project_dir, &zip_path)?;

        let file = fs::File::open(&zip_path)?;
        let mut zip = ZipArchive::new(file)?;
        let mut names = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect::<Vec<_>>();
        names.sort();

        assert!(names.contains(&"layout board/layout.kicad_pro".to_string()));
        assert!(names.contains(&"layout board/layout.kicad_pcb".to_string()));
        assert!(names.contains(&"layout board/layout.kicad_sch".to_string()));
        assert!(names.contains(&MANIFEST_FILE_NAME.to_string()));

        Ok(())
    }
    /// `${KIPRJMOD}/../lib.pretty` is how a KiCad project names a library one directory up, and on
    /// Windows `KIPRJMOD` expands to a verbatim path the OS will not normalize, so the `..` has to be
    /// collapsed before the path reaches the filesystem.
    #[test]
    fn verbatim_paths_collapse_relative_components() {
        assert_eq!(
            collapse_verbatim_relative_components(r"\\?\C:\project\..\shared.pretty"),
            r"\\?\C:\shared.pretty"
        );
        assert_eq!(
            collapse_verbatim_relative_components(r"\\?\C:\a\b\..\.\c"),
            r"\\?\C:\a\c"
        );
        // Nothing to collapse, and non-verbatim paths Windows normalizes itself, are left alone —
        // including POSIX paths, where the whole operation must be a no-op.
        assert_eq!(
            collapse_verbatim_relative_components(r"\\?\C:\project\lib.pretty"),
            r"\\?\C:\project\lib.pretty"
        );
        assert_eq!(
            collapse_verbatim_relative_components(r"C:\project\..\lib.pretty"),
            r"C:\project\..\lib.pretty"
        );
        assert_eq!(
            collapse_verbatim_relative_components("/tmp/project/../lib.pretty"),
            "/tmp/project/../lib.pretty"
        );
        // A `..` with only the volume root left clamps instead of escaping it.
        assert_eq!(
            collapse_verbatim_relative_components(r"\\?\C:\..\..\lib.pretty"),
            r"\\?\C:\lib.pretty"
        );
        // A UNC share root is three components, not one; popping into it would name nothing.
        assert_eq!(
            collapse_verbatim_relative_components(r"\\?\UNC\server\share\project\..\lib.pretty"),
            r"\\?\UNC\server\share\lib.pretty"
        );
        assert_eq!(
            collapse_verbatim_relative_components(r"\\?\UNC\server\share\..\..\..\lib.pretty"),
            r"\\?\UNC\server\share\lib.pretty"
        );
    }

    /// The collapse has to happen after a relative reference is joined onto its base directory: the
    /// reference carries no verbatim prefix itself, and on Windows the canonicalized base is what
    /// contributes one. Resolving a real file through a `..` is what pins that ordering — on Windows
    /// this fails outright if the collapse is applied to the expanded string alone.
    #[test]
    fn a_relative_reference_resolves_through_a_parent_directory() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("project");
        let sheets = project.join("sheets");
        let common = project.join("common");
        fs::create_dir_all(&sheets)?;
        fs::create_dir_all(&common)?;
        let target = common.join("power.kicad_sch");
        fs::write(&target, "(kicad_sch)")?;
        let project = project.canonicalize()?;

        let resolver = build_kicad_variable_resolver(&project, &Value::Null);
        let resolved = resolve_reference_path(
            &project,
            &project.join("sheets").canonicalize()?,
            "../common/power.kicad_sch",
            &resolver,
        )?;

        assert_eq!(resolved, target.canonicalize()?);
        Ok(())
    }

    #[test]
    fn rejects_footprint_libraries_outside_project() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("project");
        let library = dir.path().join("External.pretty");
        fs::create_dir(&project)?;
        fs::create_dir(&library)?;
        fs::write(library.join("Thing.kicad_mod"), "(footprint \"Thing\")")?;
        let project = project.canonicalize()?;

        let resolver = build_kicad_variable_resolver(&project, &Value::Null);
        let error = resolve_footprint_library_uri(
            &project,
            &project,
            "${KIPRJMOD}/../External.pretty",
            "Thing",
            &resolver,
            false,
        )
        .unwrap_err();
        assert!(error.contains("outside project directory"));
        Ok(())
    }

    /// Writes a one-entry footprint library table plus the `.pretty` library it names, and returns
    /// the table path and the resolved footprint file.
    fn write_footprint_library(
        dir: &Path,
        table_name: &str,
        nickname: &str,
        marker: &str,
    ) -> Result<(PathBuf, PathBuf)> {
        write_footprint_library_entry(dir, table_name, nickname, marker, false)
    }

    fn write_footprint_library_entry(
        dir: &Path,
        table_name: &str,
        nickname: &str,
        marker: &str,
        disabled: bool,
    ) -> Result<(PathBuf, PathBuf)> {
        let library = dir.join(format!("{nickname}.pretty"));
        fs::create_dir_all(&library)?;
        let footprint = library.join("Thing.kicad_mod");
        fs::write(
            &footprint,
            format!("(footprint \"Thing\" (descr \"{marker}\"))"),
        )?;
        let table = dir.join(table_name);
        let disabled_flag = if disabled { " (disabled)" } else { "" };
        // A backslash opens an escape sequence inside an s-expression string, so an unescaped Windows
        // path silently loses its separators when the table is parsed back. KiCad escapes them when it
        // writes the table; the helper has to as well, or these tests exercise a corrupted URI.
        let uri = library.display().to_string().replace('\\', r"\\");
        fs::write(
            &table,
            format!(
                "(fp_lib_table (version 7) (lib (name \"{nickname}\") (type \"KiCad\") (uri \"{uri}\") (options \"\") (descr \"\"){disabled_flag}))"
            ),
        )?;
        Ok((table, footprint.canonicalize()?))
    }

    fn library_tables(
        footprints: BTreeMap<String, String>,
        global_table_paths: &[PathBuf],
    ) -> ProjectLibraryTables {
        ProjectLibraryTables {
            existing_tables: Vec::new(),
            symbols: BTreeMap::new(),
            footprints,
            global_footprints: global_footprint_table_from_paths(global_table_paths),
        }
    }

    fn footprint_assets(identifier: &str) -> SchematicAssets {
        SchematicAssets {
            footprint_ids: BTreeSet::from([identifier.to_string()]),
            ..SchematicAssets::default()
        }
    }

    /// A project table that registers nothing is the normal case for a design whose libraries are
    /// registered globally in KiCad, so the global table has to be consulted too.
    #[test]
    fn global_footprint_table_resolves_libraries_absent_from_the_project_table() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("project");
        let config = dir.path().join("kicad-config");
        fs::create_dir_all(&project)?;
        fs::create_dir_all(&config)?;
        let project = project.canonicalize()?;
        let (global_table, global_footprint) =
            write_footprint_library(&config, FP_LIB_TABLE_FILE, "vendor", "global")?;

        let resolver = build_kicad_variable_resolver(&project, &Value::Null);
        let tables = library_tables(BTreeMap::new(), &[global_table]);
        let mut abs_files = BTreeSet::new();
        let (resolved, project_footprint_ids) = resolve_project_library_assets(
            &project,
            &resolver,
            &tables,
            &footprint_assets("vendor:Thing"),
            &mut abs_files,
        );

        assert_eq!(resolved.get("vendor:Thing"), Some(&global_footprint));
        assert!(project_footprint_ids.is_empty());
        // The file is outside the project, so it is not staged or archived with the sources; the
        // geometry reaches the output by being copied into the generated component package.
        assert!(abs_files.is_empty());
        Ok(())
    }

    #[test]
    fn project_footprint_table_wins_over_the_global_table() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("project");
        let config = dir.path().join("kicad-config");
        fs::create_dir_all(&project)?;
        fs::create_dir_all(&config)?;
        let project = project.canonicalize()?;
        let (_, project_footprint) =
            write_footprint_library(&project, FP_LIB_TABLE_FILE, "vendor", "project")?;
        let (global_table, global_footprint) =
            write_footprint_library(&config, FP_LIB_TABLE_FILE, "vendor", "global")?;

        let resolver = build_kicad_variable_resolver(&project, &Value::Null);
        let tables = library_tables(
            parse_library_table(&project.join(FP_LIB_TABLE_FILE), "fp_lib_table")?,
            &[global_table],
        );
        let mut abs_files = BTreeSet::new();
        let (resolved, project_footprint_ids) = resolve_project_library_assets(
            &project,
            &resolver,
            &tables,
            &footprint_assets("vendor:Thing"),
            &mut abs_files,
        );

        assert_eq!(resolved.get("vendor:Thing"), Some(&project_footprint));
        assert!(project_footprint_ids.contains("vendor:Thing"));
        assert_ne!(resolved.get("vendor:Thing"), Some(&global_footprint));
        assert!(abs_files.contains(&project_footprint));
        Ok(())
    }

    #[test]
    fn missing_project_footprint_still_claims_the_identifier() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("project");
        fs::create_dir_all(&project)?;
        let project = project.canonicalize()?;
        write_footprint_library(&project, FP_LIB_TABLE_FILE, "vendor", "project")?;

        let resolver = build_kicad_variable_resolver(&project, &Value::Null);
        let tables = library_tables(
            parse_library_table(&project.join(FP_LIB_TABLE_FILE), "fp_lib_table")?,
            &[],
        );
        let mut abs_files = BTreeSet::new();
        let (resolved, project_footprint_ids) = resolve_project_library_assets(
            &project,
            &resolver,
            &tables,
            &footprint_assets("vendor:Missing"),
            &mut abs_files,
        );

        assert!(!resolved.contains_key("vendor:Missing"));
        assert!(project_footprint_ids.contains("vendor:Missing"));
        assert!(abs_files.is_empty());
        Ok(())
    }

    /// KiCad ignores a disabled project-table row and continues to the global table for that
    /// nickname. Import must do the same: keeping the disabled URI would bind the wrong library
    /// or leave the footprint unresolved when only the global entry is usable.
    #[test]
    fn disabled_project_footprint_entry_falls_through_to_the_global_table() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("project");
        let config = dir.path().join("kicad-config");
        fs::create_dir_all(&project)?;
        fs::create_dir_all(&config)?;
        let project = project.canonicalize()?;
        let (_, project_footprint) =
            write_footprint_library_entry(&project, FP_LIB_TABLE_FILE, "vendor", "project", true)?;
        let (global_table, global_footprint) =
            write_footprint_library(&config, FP_LIB_TABLE_FILE, "vendor", "global")?;

        let project_entries =
            parse_library_table(&project.join(FP_LIB_TABLE_FILE), "fp_lib_table")?;
        assert!(
            project_entries.is_empty(),
            "disabled project rows must not claim the nickname"
        );

        let resolver = build_kicad_variable_resolver(&project, &Value::Null);
        let tables = library_tables(project_entries, &[global_table]);
        let mut abs_files = BTreeSet::new();
        let (resolved, project_footprint_ids) = resolve_project_library_assets(
            &project,
            &resolver,
            &tables,
            &footprint_assets("vendor:Thing"),
            &mut abs_files,
        );

        assert_eq!(resolved.get("vendor:Thing"), Some(&global_footprint));
        assert!(project_footprint_ids.is_empty());
        assert_ne!(resolved.get("vendor:Thing"), Some(&project_footprint));
        assert!(abs_files.is_empty());
        Ok(())
    }

    #[test]
    fn parse_library_table_omits_disabled_entries() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let table = dir.path().join(FP_LIB_TABLE_FILE);
        fs::write(
            &table,
            r#"(fp_lib_table
  (version 7)
  (lib (name "alive") (type "KiCad") (uri "${KIPRJMOD}/alive.pretty") (options "") (descr ""))
  (lib (name "dead") (type "KiCad") (uri "${KIPRJMOD}/dead.pretty") (options "") (descr "") (disabled))
)"#,
        )?;

        let entries = parse_library_table(&table, "fp_lib_table")?;
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries.get("alive").map(String::as_str),
            Some("${KIPRJMOD}/alive.pretty")
        );
        assert!(!entries.contains_key("dead"));
        Ok(())
    }

    #[test]
    fn bundles_models_from_variable_paths() -> Result<()> {
        let dir = tempfile::tempdir()?;
        fs::create_dir_all(dir.path().join("3d"))?;
        fs::write(dir.path().join("3d").join("m.step"), "dummy")?;
        fs::write(
            dir.path().join("demo.kicad_pro"),
            r#"{
  "project_refs": ["demo.kicad_pcb", "demo.kicad_sch"],
  "text_variables": { "ANT3DMDL": "${KIPRJMOD}/3d" }
}"#,
        )?;
        fs::write(
            dir.path().join("demo.kicad_sch"),
            "(kicad_sch (uuid \"u\"))",
        )?;
        fs::write(
            dir.path().join("demo.kicad_pcb"),
            r#"(kicad_pcb (footprint "X" (model "${ANT3DMDL}/m.step")))"#,
        )?;
        fs::write(dir.path().join("demo.kicad_dru"), "(version 1)")?;

        let project = discover_and_validate(&dir.path().join("demo.kicad_pro"))?;
        let zip_path = dir.path().join("out.zip");
        write_portable_zip(&project, &project.project_dir, &zip_path)?;

        let file = fs::File::open(&zip_path)?;
        let mut zip = ZipArchive::new(file)?;
        let mut names = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect::<Vec<_>>();
        names.sort();

        assert!(names.contains(&"models/ANT3DMDL/m.step".to_string()));
        assert!(names.contains(&"demo/demo.kicad_dru".to_string()));
        Ok(())
    }
}
