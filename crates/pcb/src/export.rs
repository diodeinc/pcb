use anyhow::{bail, Context, Result};
use clap::Args;
use pcb_sexpr::{parse as parse_sexpr, Sexpr};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::env;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use zip::ZipWriter;

const KICAD_PRO_EXT: &str = "kicad_pro";
const KICAD_PCB_EXT: &str = "kicad_pcb";
const KICAD_SCH_EXT: &str = "kicad_sch";
const KICAD_SYM_EXT: &str = "kicad_sym";
const KICAD_MOD_EXT: &str = "kicad_mod";
const SYM_LIB_TABLE_FILE: &str = "sym-lib-table";
const FP_LIB_TABLE_FILE: &str = "fp-lib-table";
const MANIFEST_FILE_NAME: &str = "export_manifest.json";

#[derive(Args, Debug, Clone)]
#[command(about = "Bundle KiCad project files into a zip archive")]
pub struct ExportArgs {
    /// Path to the KiCad .kicad_pro file
    #[arg(value_name = "KICAD_PRO", value_hint = clap::ValueHint::FilePath)]
    pub kicad_pro: PathBuf,

    /// Output path for archive ('.zip' is appended if missing)
    #[arg(short, long, value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub output: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct DiscoveredProject {
    project_dir: PathBuf,
    kicad_pro: PathBuf,            // relative to project_dir
    root_schematic: PathBuf,       // relative to project_dir
    primary_kicad_pcb: PathBuf,    // relative to project_dir
    files_to_bundle: Vec<PathBuf>, // relative to project_dir
    extra_files_to_bundle: Vec<BundleFile>,
    manifest_json: String,
}

#[derive(Debug, Clone)]
struct BundleFile {
    source_path: PathBuf,
    archive_relative_path: String, // archive-root relative path
    kind: KicadProjectFileKind,
}

#[derive(Debug, Clone, Serialize)]
struct KicadProject {
    project_dir: String,
    project_file: String,
    root_schematic: String,
    root_schematic_uuid: String,
    pcb_file: String,
    schematic_tree: SchematicNode,
    files: Vec<KicadProjectFile>,
    symbol_identifiers: Vec<LibraryIdentifier>,
    footprint_identifiers: Vec<LibraryIdentifier>,
    model_references: Vec<String>,
    resolved_artifacts: Vec<ResolvedArtifact>,
    unresolved_artifacts: Vec<UnresolvedArtifact>,
}

#[derive(Debug, Clone, Serialize)]
struct SchematicNode {
    path: String,
    children: Vec<SchematicNode>,
}

#[derive(Debug, Clone, Serialize)]
struct KicadProjectFile {
    path: String,
    kind: KicadProjectFileKind,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum KicadProjectFileKind {
    Project,
    Schematic,
    Pcb,
    Csv,
    SymbolLibrary,
    FootprintLibrary,
    Model,
    SymbolLibraryTable,
    FootprintLibraryTable,
}

#[derive(Debug, Clone, Serialize)]
struct LibraryIdentifier {
    identifier: String,
    library_nickname: String,
    entry_name: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum ResolvedArtifactKind {
    #[serde(rename = "symbol_library_file")]
    SymbolLibrary,
    #[serde(rename = "footprint_file")]
    Footprint,
    #[serde(rename = "model_file")]
    Model,
}

#[derive(Debug, Clone, Serialize)]
struct ResolvedArtifact {
    kind: ResolvedArtifactKind,
    identifier: String,
    source_uri: String,
    resolved_path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
enum UnresolvedArtifactKind {
    SymbolIdentifier,
    FootprintIdentifier,
    ModelReference,
}

#[derive(Debug, Clone, Serialize)]
struct UnresolvedArtifact {
    kind: UnresolvedArtifactKind,
    identifier: String,
    reason: String,
}

#[derive(Default)]
struct SexprDiscovery {
    kicad_file_refs: BTreeSet<String>,
    sheetfile_refs: BTreeSet<String>,
    symbol_ids: BTreeSet<String>,
    footprint_ids: BTreeSet<String>,
    model_refs: BTreeSet<String>,
}

struct KicadVariableResolver {
    vars: BTreeMap<String, String>,
}

pub fn execute(args: ExportArgs) -> Result<()> {
    let kicad_pro_path = args.kicad_pro.canonicalize().with_context(|| {
        format!(
            "Failed to resolve KiCad project file path: {}",
            args.kicad_pro.display()
        )
    })?;
    let discovered = discover_and_validate(&kicad_pro_path)?;
    let output_path = resolve_output_path(args.output, &discovered);

    bundle_project_files(&discovered, &output_path)?;

    println!(
        "Exported {} files from {} to {} (project: {}, pcb: {}, root schematic: {})",
        discovered.files_to_bundle.len() + discovered.extra_files_to_bundle.len(),
        discovered.project_dir.display(),
        output_path.display(),
        discovered.kicad_pro.display(),
        discovered.primary_kicad_pcb.display(),
        discovered.root_schematic.display(),
    );
    Ok(())
}

fn discover_and_validate(kicad_pro_path: &Path) -> Result<DiscoveredProject> {
    if !kicad_pro_path.exists() {
        bail!(
            "KiCad project file does not exist: {}",
            kicad_pro_path.display()
        );
    }
    if !kicad_pro_path.is_file() {
        bail!(
            "Expected a .kicad_pro file path, got: {}",
            kicad_pro_path.display()
        );
    }
    if kicad_pro_path.extension().and_then(|ext| ext.to_str()) != Some(KICAD_PRO_EXT) {
        bail!(
            "Expected a .kicad_pro file path, got: {}",
            kicad_pro_path.display()
        );
    }

    let kicad_pro_abs = kicad_pro_path
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", kicad_pro_path.display()))?;
    let project_dir = kicad_pro_abs.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "Failed to determine project directory from {}",
            kicad_pro_abs.display()
        )
    })?;
    let kicad_pro = to_relative(project_dir, &kicad_pro_abs)?;
    let kicad_pro_json = load_kicad_pro_json(&kicad_pro_abs)?;
    let variable_resolver = build_kicad_variable_resolver(project_dir, &kicad_pro_json);

    let root_uuid = extract_root_uuid(&kicad_pro_json)?;
    let kicad_refs = collect_kicad_refs_from_json(&kicad_pro_json);
    let project_name = kicad_pro_abs
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .ok_or_else(|| {
            anyhow::anyhow!("Failed to infer project name from {}", kicad_pro.display())
        })?;

    let primary_pcb_abs =
        resolve_primary_pcb_from_pro(project_dir, &project_name, &kicad_refs, &variable_resolver)?;
    let root_schematic_abs = resolve_root_schematic_from_pro(
        project_dir,
        &project_name,
        &kicad_refs,
        &variable_resolver,
    )?;

    let root_schematic_uuid =
        extract_first_schematic_uuid(&root_schematic_abs)?.ok_or_else(|| {
            anyhow::anyhow!(
                "Root schematic is missing top-level UUID: {}",
                root_schematic_abs.display()
            )
        })?;
    if root_schematic_uuid != root_uuid {
        bail!(
            "Root schematic UUID mismatch: .kicad_pro says '{}', but '{}' has '{}'",
            root_uuid,
            root_schematic_abs.display(),
            root_schematic_uuid
        );
    }

    let mut abs_files: BTreeSet<PathBuf> = BTreeSet::new();
    abs_files.insert(kicad_pro_abs.clone());
    abs_files.insert(primary_pcb_abs.clone());
    let sym_lib_table_abs = project_dir.join(SYM_LIB_TABLE_FILE);
    let fp_lib_table_abs = project_dir.join(FP_LIB_TABLE_FILE);

    if sym_lib_table_abs.exists() {
        abs_files.insert(sym_lib_table_abs.clone());
    }
    if fp_lib_table_abs.exists() {
        abs_files.insert(fp_lib_table_abs.clone());
    }

    for entry in fs::read_dir(project_dir)
        .with_context(|| format!("Failed to read project directory {}", project_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).with_context(|| {
            format!(
                "Failed to read metadata for potential CSV file {}",
                path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            bail!(
                "Symlinked top-level CSV file is not supported: {}",
                path.display()
            );
        }
        if !metadata.is_file() {
            continue;
        }
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"))
        {
            abs_files.insert(path.canonicalize().with_context(|| {
                format!(
                    "Failed to canonicalize top-level CSV file {}",
                    path.display()
                )
            })?);
        }
    }

    let sym_lib_table = if sym_lib_table_abs.exists() {
        parse_library_table(&sym_lib_table_abs, "sym_lib_table")?
    } else {
        BTreeMap::new()
    };
    let fp_lib_table = if fp_lib_table_abs.exists() {
        parse_library_table(&fp_lib_table_abs, "fp_lib_table")?
    } else {
        BTreeMap::new()
    };

    let mut symbol_ids: BTreeSet<String> = BTreeSet::new();
    let mut footprint_ids: BTreeSet<String> = BTreeSet::new();
    let mut model_refs: BTreeSet<String> = BTreeSet::new();

    // Include direct references from kicad_pro.
    for reference in &kicad_refs {
        if let Some(ext) = extension_of_reference(reference) {
            if !is_relevant_kicad_extension(&ext) {
                continue;
            }
            let resolved =
                resolve_reference_path(project_dir, project_dir, reference, &variable_resolver)?;
            abs_files.insert(resolved);
        }
    }

    // Parse PCB file references transitively (project-local only) from S-expression content.
    let pcb_content = fs::read_to_string(&primary_pcb_abs)
        .with_context(|| format!("Failed to read {}", primary_pcb_abs.display()))?;
    let pcb_discovery = discover_from_sexpr_text(&pcb_content)
        .with_context(|| format!("Failed to parse {}", primary_pcb_abs.display()))?;
    for reference in &pcb_discovery.kicad_file_refs {
        let resolved =
            resolve_reference_path(project_dir, project_dir, reference, &variable_resolver)?;
        abs_files.insert(resolved);
    }
    symbol_ids.extend(pcb_discovery.symbol_ids);
    footprint_ids.extend(pcb_discovery.footprint_ids);
    model_refs.extend(pcb_discovery.model_refs);

    // Traverse schematic hierarchy from resolved root schematic.
    let mut schematic_children: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    let mut visited_schematics: BTreeSet<PathBuf> = BTreeSet::new();
    let mut queue: VecDeque<PathBuf> = VecDeque::from([root_schematic_abs.clone()]);

    while let Some(current_abs) = queue.pop_front() {
        if !visited_schematics.insert(current_abs.clone()) {
            continue;
        }
        abs_files.insert(current_abs.clone());

        let content = fs::read_to_string(&current_abs)
            .with_context(|| format!("Failed to read schematic {}", current_abs.display()))?;
        let discovery = discover_from_sexpr_text(&content)
            .with_context(|| format!("Failed to parse schematic {}", current_abs.display()))?;

        // Include other direct refs from this schematic file.
        for reference in &discovery.kicad_file_refs {
            let resolved = resolve_reference_path(
                project_dir,
                current_abs.parent().unwrap_or(project_dir),
                reference,
                &variable_resolver,
            )?;
            abs_files.insert(resolved);
        }
        symbol_ids.extend(discovery.symbol_ids);
        footprint_ids.extend(discovery.footprint_ids);

        let current_rel = to_relative(project_dir, &current_abs)?;
        for sheet_ref in discovery.sheetfile_refs {
            let child_abs = resolve_reference_path(
                project_dir,
                current_abs.parent().unwrap_or(project_dir),
                &sheet_ref,
                &variable_resolver,
            )?;
            if child_abs.extension().and_then(|ext| ext.to_str()) != Some(KICAD_SCH_EXT) {
                bail!(
                    "Sheetfile reference must point to .kicad_sch, got '{}' in {}",
                    sheet_ref,
                    current_abs.display()
                );
            }
            let child_rel = to_relative(project_dir, &child_abs)?;
            schematic_children
                .entry(current_rel.clone())
                .or_default()
                .push(child_rel);
            queue.push_back(child_abs);
        }
    }

    for children in schematic_children.values_mut() {
        children.sort();
        children.dedup();
    }

    let symbol_identifiers = symbol_ids
        .iter()
        .filter_map(|identifier| parse_library_identifier(identifier))
        .map(|(library_nickname, entry_name)| LibraryIdentifier {
            identifier: format!("{library_nickname}:{entry_name}"),
            library_nickname,
            entry_name,
        })
        .collect::<Vec<_>>();
    let footprint_identifiers = footprint_ids
        .iter()
        .filter_map(|identifier| parse_library_identifier(identifier))
        .map(|(library_nickname, entry_name)| LibraryIdentifier {
            identifier: format!("{library_nickname}:{entry_name}"),
            library_nickname,
            entry_name,
        })
        .collect::<Vec<_>>();
    let model_references = model_refs.into_iter().collect::<Vec<_>>();
    let mut extra_bundle_map: BTreeMap<String, BundleFile> = BTreeMap::new();

    let mut resolved_artifacts = Vec::new();
    let mut unresolved_artifacts = Vec::new();

    for symbol in &symbol_identifiers {
        let Some(uri) = sym_lib_table.get(&symbol.library_nickname) else {
            unresolved_artifacts.push(UnresolvedArtifact {
                kind: UnresolvedArtifactKind::SymbolIdentifier,
                identifier: symbol.identifier.clone(),
                reason: format!(
                    "No symbol library table entry for nickname '{}'",
                    symbol.library_nickname
                ),
            });
            continue;
        };

        match resolve_symbol_library_uri(project_dir, uri, &variable_resolver) {
            Ok(path) => {
                insert_if_project_local(&mut abs_files, project_dir, &path);
                let archive_hint = symbol_archive_hint(uri, &path);
                let archive_relative_path = stage_extra_bundle_file(
                    &mut extra_bundle_map,
                    &archive_hint,
                    &path,
                    KicadProjectFileKind::SymbolLibrary,
                );
                resolved_artifacts.push(ResolvedArtifact {
                    kind: ResolvedArtifactKind::SymbolLibrary,
                    identifier: symbol.identifier.clone(),
                    source_uri: uri.clone(),
                    resolved_path: archive_relative_path,
                });
            }
            Err(reason) => {
                unresolved_artifacts.push(UnresolvedArtifact {
                    kind: UnresolvedArtifactKind::SymbolIdentifier,
                    identifier: symbol.identifier.clone(),
                    reason,
                });
            }
        }
    }

    for footprint in &footprint_identifiers {
        let Some(uri) = fp_lib_table.get(&footprint.library_nickname) else {
            unresolved_artifacts.push(UnresolvedArtifact {
                kind: UnresolvedArtifactKind::FootprintIdentifier,
                identifier: footprint.identifier.clone(),
                reason: format!(
                    "No footprint library table entry for nickname '{}'",
                    footprint.library_nickname
                ),
            });
            continue;
        };

        match resolve_footprint_library_uri(
            project_dir,
            uri,
            &footprint.entry_name,
            &variable_resolver,
        ) {
            Ok(path) => {
                insert_if_project_local(&mut abs_files, project_dir, &path);
                let archive_hint = footprint_archive_hint(uri, &path);
                let archive_relative_path = stage_extra_bundle_file(
                    &mut extra_bundle_map,
                    &archive_hint,
                    &path,
                    KicadProjectFileKind::FootprintLibrary,
                );
                resolved_artifacts.push(ResolvedArtifact {
                    kind: ResolvedArtifactKind::Footprint,
                    identifier: footprint.identifier.clone(),
                    source_uri: uri.clone(),
                    resolved_path: archive_relative_path,
                });
            }
            Err(reason) => {
                unresolved_artifacts.push(UnresolvedArtifact {
                    kind: UnresolvedArtifactKind::FootprintIdentifier,
                    identifier: footprint.identifier.clone(),
                    reason,
                });
            }
        }
    }

    // TODO: Instead of shipping external model files, embed resolved 3D assets into the PCB layout file.
    for model_ref in &model_references {
        match resolve_model_path(project_dir, model_ref, &variable_resolver) {
            Ok(resolved_path) => {
                let archive_hint = model_archive_hint(model_ref, &resolved_path);
                let archive_relative_path = stage_extra_bundle_file(
                    &mut extra_bundle_map,
                    &archive_hint,
                    &resolved_path,
                    KicadProjectFileKind::Model,
                );
                resolved_artifacts.push(ResolvedArtifact {
                    kind: ResolvedArtifactKind::Model,
                    identifier: model_ref.clone(),
                    source_uri: model_ref.clone(),
                    resolved_path: archive_relative_path,
                });
            }
            Err(reason) => {
                unresolved_artifacts.push(UnresolvedArtifact {
                    kind: UnresolvedArtifactKind::ModelReference,
                    identifier: model_ref.clone(),
                    reason,
                });
            }
        }
    }

    let root_schematic_rel = to_relative(project_dir, &root_schematic_abs)?;
    let schematic_tree = build_schematic_tree(
        &root_schematic_rel,
        &schematic_children,
        &mut BTreeSet::new(),
    )?;

    let mut files_to_bundle = abs_files
        .iter()
        .map(|abs| to_relative(project_dir, abs))
        .collect::<Result<Vec<_>>>()?;
    files_to_bundle.sort();
    files_to_bundle.dedup();

    let files = files_to_bundle
        .iter()
        .map(|path| {
            let kind = file_kind(path).ok_or_else(|| {
                anyhow::anyhow!("Unsupported file in bundle set: {}", path.display())
            })?;
            Ok(KicadProjectFile {
                path: path.to_string_lossy().replace('\\', "/"),
                kind,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let extra_files_to_bundle = extra_bundle_map.into_values().collect::<Vec<_>>();
    let mut files = files;
    for extra in &extra_files_to_bundle {
        files.push(KicadProjectFile {
            path: extra.archive_relative_path.clone(),
            kind: extra.kind.clone(),
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let manifest = KicadProject {
        project_dir: project_dir.display().to_string(),
        project_file: kicad_pro.to_string_lossy().replace('\\', "/"),
        root_schematic: root_schematic_rel.to_string_lossy().replace('\\', "/"),
        root_schematic_uuid: root_uuid,
        pcb_file: to_relative(project_dir, &primary_pcb_abs)?
            .to_string_lossy()
            .replace('\\', "/"),
        schematic_tree,
        files,
        symbol_identifiers,
        footprint_identifiers,
        model_references,
        resolved_artifacts,
        unresolved_artifacts,
    };
    let manifest_json =
        serde_json::to_string_pretty(&manifest).context("Failed to serialize project manifest")?;

    Ok(DiscoveredProject {
        project_dir: project_dir.to_path_buf(),
        kicad_pro,
        root_schematic: root_schematic_rel,
        primary_kicad_pcb: to_relative(project_dir, &primary_pcb_abs)?,
        files_to_bundle,
        extra_files_to_bundle,
        manifest_json,
    })
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
    for path in discover_kicad_common_json_files() {
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
    vars.insert("KIPRJMOD".to_string(), project_dir.display().to_string());
    KicadVariableResolver { vars }
}

fn discover_kicad_common_json_files() -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();

    if let Ok(config_home) = env::var("KICAD_CONFIG_HOME") {
        if !config_home.is_empty() {
            roots.insert(PathBuf::from(config_home));
        }
    }

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        roots.insert(home.join(".config/kicad"));
        roots.insert(home.join("Library/Preferences/kicad"));
    }

    if let Ok(app_data) = env::var("APPDATA") {
        if !app_data.is_empty() {
            roots.insert(PathBuf::from(app_data).join("kicad"));
        }
    }

    let mut files = BTreeSet::new();
    for root in roots {
        let top = root.join("kicad_common.json");
        if top.is_file() {
            files.insert(top);
        }

        if let Ok(entries) = fs::read_dir(&root) {
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                    continue;
                }
                let candidate = entry.path().join("kicad_common.json");
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
        Value::String(s) => {
            if extension_of_reference(s).is_some_and(|ext| is_relevant_kicad_extension(&ext)) {
                refs.insert(s.clone());
            }
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
        if let Some(uuid_items) = node.as_list() {
            if uuid_items.first().and_then(|item| item.as_sym()) == Some("uuid") {
                if let Some(uuid) = uuid_items.get(1).and_then(atom_or_string) {
                    return Some(uuid.to_string());
                }
            }
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
                _ => {}
            }
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
    if let Some(value) = atom_or_string(node) {
        if extension_of_reference(value).is_some_and(|ext| is_relevant_kicad_extension(&ext)) {
            discovery.kicad_file_refs.insert(value.to_string());
        }
    }

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
                } else if items.get(1).and_then(atom_or_string) == Some("Footprint") {
                    if let Some(identifier) = items.get(2).and_then(atom_or_string) {
                        discovery.footprint_ids.insert(identifier.to_string());
                    }
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

fn extension_of_reference(reference: &str) -> Option<String> {
    if reference.contains("://") {
        return None;
    }
    let path = Path::new(reference);
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
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
    let path = resolve_uri_path(project_dir, uri, variable_resolver)?;
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

fn resolve_footprint_library_uri(
    project_dir: &Path,
    uri: &str,
    footprint_name: &str,
    variable_resolver: &KicadVariableResolver,
) -> std::result::Result<PathBuf, String> {
    let base = resolve_uri_path(project_dir, uri, variable_resolver)?;
    let candidate =
        if base.extension().and_then(|ext| ext.to_str()) == Some("pretty") || base.is_dir() {
            base.join(format!("{footprint_name}.{KICAD_MOD_EXT}"))
        } else if base.extension().and_then(|ext| ext.to_str()) == Some(KICAD_MOD_EXT) {
            base
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

    candidate
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize {}: {}", candidate.display(), e))
}

fn resolve_uri_path(
    project_dir: &Path,
    uri: &str,
    variable_resolver: &KicadVariableResolver,
) -> std::result::Result<PathBuf, String> {
    let expanded = variable_resolver.expand(uri)?;
    if expanded.contains("://") {
        return Err(format!("Unsupported non-file URI '{}'", uri));
    }

    let path = PathBuf::from(&expanded);
    let candidate = if path.is_absolute() {
        path
    } else {
        project_dir.join(path)
    };
    let metadata = fs::symlink_metadata(&candidate).map_err(|_| {
        format!(
            "Referenced URI path not found: {} (from '{}')",
            candidate.display(),
            uri
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Symlinked URI paths are not supported: {}",
            candidate.display()
        ));
    }

    candidate
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize {}: {}", candidate.display(), e))
}

fn resolve_reference_path(
    project_dir: &Path,
    base_dir: &Path,
    reference: &str,
    variable_resolver: &KicadVariableResolver,
) -> Result<PathBuf> {
    let replaced = variable_resolver.expand(reference).map_err(|e| {
        anyhow::anyhow!(
            "Failed to resolve KiCad path reference '{}': {}",
            reference,
            e
        )
    })?;
    let ref_path = PathBuf::from(&replaced);
    let candidate = if ref_path.is_absolute() {
        ref_path
    } else {
        base_dir.join(ref_path)
    };

    let meta = fs::symlink_metadata(&candidate).with_context(|| {
        format!(
            "Referenced KiCad file not found: '{}' (resolved to {})",
            reference,
            candidate.display()
        )
    })?;
    if meta.file_type().is_symlink() {
        bail!(
            "Symlinked referenced files are not supported: {}",
            candidate.display()
        );
    }
    if !meta.is_file() {
        bail!("Referenced path is not a file: {}", candidate.display());
    }

    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize {}", candidate.display()))?;
    if !canonical.starts_with(project_dir) {
        bail!(
            "External referenced file is outside project directory: {}",
            canonical.display()
        );
    }
    Ok(canonical)
}

fn resolve_model_path(
    project_dir: &Path,
    model_reference: &str,
    variable_resolver: &KicadVariableResolver,
) -> std::result::Result<PathBuf, String> {
    let expanded = variable_resolver.expand(model_reference)?;
    if expanded.contains("://") {
        return Err(format!(
            "Unsupported non-file model URI '{}'",
            model_reference
        ));
    }

    let path = PathBuf::from(&expanded);
    let candidate = if path.is_absolute() {
        path
    } else {
        project_dir.join(path)
    };

    let metadata = fs::symlink_metadata(&candidate).map_err(|_| {
        format!(
            "Referenced model file not found: '{}' (resolved to {})",
            model_reference,
            candidate.display()
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Symlinked model file is not supported: {}",
            candidate.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!(
            "Resolved model reference is not a file: {}",
            candidate.display()
        ));
    }

    candidate
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize {}: {}", candidate.display(), e))
}

fn symbol_archive_hint(uri: &str, resolved_path: &Path) -> String {
    artifact_archive_hint("symbols", uri, resolved_path)
}

fn footprint_archive_hint(uri: &str, resolved_path: &Path) -> String {
    if let Some((var_name, remainder)) = split_leading_variable(uri) {
        let file_name = resolved_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "footprint.kicad_mod".to_string());

        let mut remainder = normalize_archive_path(&remainder);
        if remainder.is_empty() {
            remainder = file_name.clone();
        } else if !remainder.ends_with(".kicad_mod") {
            remainder = format!("{}/{}", remainder.trim_end_matches('/'), file_name);
        }

        return format!(
            "footprints/{}/{}",
            sanitize_archive_segment(&var_name),
            remainder
        );
    }

    if !Path::new(uri).is_absolute() && !uri.contains("${") && !uri.contains("$(") {
        let file_name = resolved_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "footprint.kicad_mod".to_string());

        let mut uri_path = normalize_archive_path(uri);
        if uri_path.is_empty() {
            uri_path = file_name.clone();
        } else if !uri_path.ends_with(".kicad_mod") {
            uri_path = format!("{}/{}", uri_path.trim_end_matches('/'), file_name);
        }

        return format!("footprints/project/{uri_path}");
    }

    format!(
        "footprints/absolute/{}",
        normalize_archive_path(&path_to_portable_string(resolved_path))
    )
}

fn model_archive_hint(model_reference: &str, resolved_path: &Path) -> String {
    artifact_archive_hint("models", model_reference, resolved_path)
}

fn artifact_archive_hint(prefix: &str, reference: &str, resolved_path: &Path) -> String {
    if let Some((var_name, remainder)) = split_leading_variable(reference) {
        if !remainder.is_empty() {
            return format!(
                "{}/{}/{}",
                prefix,
                sanitize_archive_segment(&var_name),
                normalize_archive_path(&remainder)
            );
        }
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

fn ensure_unique_archive_path(
    existing: &mut BTreeMap<String, BundleFile>,
    archive_hint: &str,
    source_path: &Path,
    kind: KicadProjectFileKind,
) -> String {
    if let Some(existing_file) = existing.get(archive_hint) {
        if existing_file.source_path == source_path && existing_file.kind == kind {
            return archive_hint.to_string();
        }
    } else {
        return archive_hint.to_string();
    }

    let (base, ext) = split_extension(archive_hint);
    let mut idx = 2usize;
    loop {
        let candidate = if ext.is_empty() {
            format!("{base}_{idx}")
        } else {
            format!("{base}_{idx}.{ext}")
        };
        match existing.get(&candidate) {
            Some(existing_file)
                if existing_file.source_path != source_path || existing_file.kind != kind =>
            {
                idx += 1
            }
            _ => return candidate,
        }
    }
}

fn stage_extra_bundle_file(
    existing: &mut BTreeMap<String, BundleFile>,
    archive_hint: &str,
    source_path: &Path,
    kind: KicadProjectFileKind,
) -> String {
    let archive_relative_path =
        ensure_unique_archive_path(existing, archive_hint, source_path, kind.clone());
    existing.insert(
        archive_relative_path.clone(),
        BundleFile {
            source_path: source_path.to_path_buf(),
            archive_relative_path: archive_relative_path.clone(),
            kind,
        },
    );
    archive_relative_path
}

fn insert_if_project_local(abs_files: &mut BTreeSet<PathBuf>, project_dir: &Path, path: &Path) {
    if path.starts_with(project_dir) {
        abs_files.insert(path.to_path_buf());
    }
}

fn split_extension(path: &str) -> (String, String) {
    let path_buf = PathBuf::from(path);
    let ext = path_buf
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_string();
    if ext.is_empty() {
        (path.to_string(), String::new())
    } else {
        (
            path.trim_end_matches(&format!(".{ext}")).to_string(),
            ext.to_string(),
        )
    }
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
    out
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
        let chars = input.as_bytes();
        let mut i = 0usize;

        while i < chars.len() {
            if chars[i] == b'$'
                && i + 1 < chars.len()
                && (chars[i + 1] == b'{' || chars[i + 1] == b'(')
            {
                let close = if chars[i + 1] == b'{' { b'}' } else { b')' };
                let start = i + 2;
                let mut end = start;
                while end < chars.len() && chars[end] != close {
                    end += 1;
                }
                if end >= chars.len() {
                    return Err(format!("Unterminated variable reference in '{}'", input));
                }

                let var_name = &input[start..end];
                if var_name.is_empty() {
                    return Err(format!("Empty variable name in '{}'", input));
                }

                let value = self.lookup(var_name).ok_or_else(|| {
                    format!("Unknown KiCad variable '{}' in '{}'", var_name, input)
                })?;
                out.push_str(&value);
                *changed = true;
                i = end + 1;
                continue;
            }

            out.push(chars[i] as char);
            i += 1;
        }

        Ok(out)
    }

    fn lookup(&self, name: &str) -> Option<String> {
        if let Some(value) = self.vars.get(name) {
            return Some(value.clone());
        }

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

fn build_schematic_tree(
    root: &Path,
    edges: &BTreeMap<PathBuf, Vec<PathBuf>>,
    stack: &mut BTreeSet<PathBuf>,
) -> Result<SchematicNode> {
    let root_path = root.to_path_buf();
    if !stack.insert(root_path.clone()) {
        bail!(
            "Cycle detected in schematic sheet references at {}",
            root.display()
        );
    }

    let children = edges
        .get(&root_path)
        .map(|children| {
            children
                .iter()
                .map(|child| build_schematic_tree(child, edges, stack))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();

    stack.remove(&root_path);
    Ok(SchematicNode {
        path: root.to_string_lossy().replace('\\', "/"),
        children,
    })
}

fn to_relative(project_dir: &Path, absolute: &Path) -> Result<PathBuf> {
    absolute
        .strip_prefix(project_dir)
        .map(Path::to_path_buf)
        .with_context(|| {
            format!(
                "Path '{}' is not inside project directory '{}'",
                absolute.display(),
                project_dir.display()
            )
        })
}

fn file_kind(path: &Path) -> Option<KicadProjectFileKind> {
    if path.file_name().and_then(|name| name.to_str()) == Some(SYM_LIB_TABLE_FILE) {
        return Some(KicadProjectFileKind::SymbolLibraryTable);
    }
    if path.file_name().and_then(|name| name.to_str()) == Some(FP_LIB_TABLE_FILE) {
        return Some(KicadProjectFileKind::FootprintLibraryTable);
    }

    match path.extension().and_then(|ext| ext.to_str()) {
        Some(KICAD_PRO_EXT) => Some(KicadProjectFileKind::Project),
        Some(KICAD_PCB_EXT) => Some(KicadProjectFileKind::Pcb),
        Some(KICAD_SCH_EXT) => Some(KicadProjectFileKind::Schematic),
        Some("csv") => Some(KicadProjectFileKind::Csv),
        Some(KICAD_SYM_EXT) => Some(KicadProjectFileKind::SymbolLibrary),
        Some(KICAD_MOD_EXT) => Some(KicadProjectFileKind::FootprintLibrary),
        _ => None,
    }
}

fn resolve_output_path(output: Option<PathBuf>, discovered: &DiscoveredProject) -> PathBuf {
    let project_name = discovered
        .kicad_pro
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kicad-project".to_string());

    match output {
        Some(output) => {
            if output.exists() && output.is_dir() {
                return output.join(format!("{project_name}.zip"));
            }
            if output.extension().is_some_and(|ext| ext == "zip") {
                output
            } else {
                PathBuf::from(format!("{}.zip", output.display()))
            }
        }
        None => discovered.project_dir.join(format!("{project_name}.zip")),
    }
}

fn bundle_project_files(discovered: &DiscoveredProject, output_path: &Path) -> Result<()> {
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create output directory for archive: {}",
                    parent.display()
                )
            })?;
        }
    }

    let output_file = fs::File::create(output_path)
        .with_context(|| format!("Failed to create archive: {}", output_path.display()))?;
    let mut zip = ZipWriter::new(BufWriter::new(output_file));
    let archive_project_root = archive_project_root(discovered);

    for relative in &discovered.files_to_bundle {
        let absolute = discovered.project_dir.join(relative);
        if !absolute.is_file() {
            bail!(
                "Discovered file is not a regular file: {}",
                absolute.display()
            );
        }
        let archive_path = format!(
            "{}/{}",
            archive_project_root,
            relative.to_string_lossy().replace('\\', "/")
        );
        zip.start_file(archive_path, zip::write::FileOptions::<()>::default())?;
        let mut input = fs::File::open(&absolute)
            .with_context(|| format!("Failed to open input file: {}", absolute.display()))?;
        std::io::copy(&mut input, &mut zip)
            .with_context(|| format!("Failed to add file to archive: {}", absolute.display()))?;
    }

    for extra in &discovered.extra_files_to_bundle {
        if !extra.source_path.is_file() {
            bail!(
                "Discovered model file is not a regular file: {}",
                extra.source_path.display()
            );
        }

        let archive_path = extra.archive_relative_path.replace('\\', "/");
        zip.start_file(archive_path, zip::write::FileOptions::<()>::default())?;
        let mut input = fs::File::open(&extra.source_path).with_context(|| {
            format!(
                "Failed to open input model file: {}",
                extra.source_path.display()
            )
        })?;
        std::io::copy(&mut input, &mut zip).with_context(|| {
            format!(
                "Failed to add model file to archive: {}",
                extra.source_path.display()
            )
        })?;
    }

    zip.start_file(MANIFEST_FILE_NAME, zip::write::FileOptions::<()>::default())?;
    zip.write_all(discovered.manifest_json.as_bytes())
        .context("Failed to write project manifest to archive")?;

    zip.finish()
        .with_context(|| format!("Failed to finalize archive: {}", output_path.display()))?;
    Ok(())
}

fn archive_project_root(discovered: &DiscoveredProject) -> String {
    discovered
        .kicad_pro
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "project".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use zip::ZipArchive;

    fn write_test_project(path: &Path) {
        fs::create_dir_all(path.join("libs")).unwrap();
        fs::create_dir_all(path.join("fp/project_footprints.pretty")).unwrap();
        fs::write(path.join("placements.csv"), "ref,x_mm,y_mm\nU1,1.0,2.0\n").unwrap();
        fs::write(
            path.join("demo.kicad_pro"),
            r#"{
  "sheets": [
    ["root-uuid", "Root"],
    ["child-uuid", "Child"]
  ],
  "project_refs": ["demo.kicad_pcb", "demo.kicad_sch"],
  "text_variables": {
    "PROJECT_3D": "${KIPRJMOD}/3d"
  }
}"#,
        )
        .unwrap();
        fs::write(
            path.join("demo.kicad_pcb"),
            r#"(kicad_pcb
  (footprint "project_footprints:Conn_A")
  (model "${PROJECT_3D}/Conn_A.step")
)"#,
        )
        .unwrap();
        fs::write(
            path.join("sym-lib-table"),
            r#"(sym_lib_table
  (version 7)
  (lib (name "project_symbols")(type "KiCad")(uri "${KIPRJMOD}/libs/project_symbols.kicad_sym")(options "")(descr ""))
)"#,
        )
        .unwrap();
        fs::write(
            path.join("fp-lib-table"),
            r#"(fp_lib_table
  (version 7)
  (lib (name "project_footprints")(type "KiCad")(uri "${KIPRJMOD}/fp/project_footprints.pretty")(options "")(descr ""))
)"#,
        )
        .unwrap();
        fs::write(
            path.join("demo.kicad_sch"),
            r#"(kicad_sch
  (version 20231120)
  (uuid "root-uuid")
  (symbol
    (lib_id "project_symbols:Demo")
    (property "Footprint" "project_footprints:Conn_A")
  )
  (sheet
    (property "Sheetfile" "child.kicad_sch")
  )
  (property "SymLib" "libs/project_symbols.kicad_sym")
)"#,
        )
        .unwrap();
        fs::write(
            path.join("child.kicad_sch"),
            r#"(kicad_sch
  (version 20231120)
  (uuid "child-uuid")
)"#,
        )
        .unwrap();
        fs::write(
            path.join("libs/project_symbols.kicad_sym"),
            "(kicad_symbol_lib)",
        )
        .unwrap();
        fs::write(
            path.join("fp/project_footprints.pretty/Conn_A.kicad_mod"),
            "(footprint \"Conn_A\")",
        )
        .unwrap();
        fs::create_dir_all(path.join("3d")).unwrap();
        fs::write(path.join("3d/Conn_A.step"), "ISO-10303-21;").unwrap();
    }

    #[test]
    fn discover_and_validate_builds_tree_and_manifest() {
        let dir = tempdir().unwrap();
        write_test_project(dir.path());
        let discovered = discover_and_validate(&dir.path().join("demo.kicad_pro")).unwrap();

        assert!(discovered
            .files_to_bundle
            .contains(&PathBuf::from("demo.kicad_pro")));
        assert!(discovered
            .files_to_bundle
            .contains(&PathBuf::from("demo.kicad_pcb")));
        assert!(discovered
            .files_to_bundle
            .contains(&PathBuf::from("demo.kicad_sch")));
        assert!(discovered
            .files_to_bundle
            .contains(&PathBuf::from("child.kicad_sch")));
        assert!(discovered
            .files_to_bundle
            .contains(&PathBuf::from("libs/project_symbols.kicad_sym")));
        assert!(discovered.files_to_bundle.contains(&PathBuf::from(
            "fp/project_footprints.pretty/Conn_A.kicad_mod"
        )));
        assert!(discovered
            .files_to_bundle
            .contains(&PathBuf::from("sym-lib-table")));
        assert!(discovered
            .files_to_bundle
            .contains(&PathBuf::from("fp-lib-table")));
        assert!(discovered
            .files_to_bundle
            .contains(&PathBuf::from("placements.csv")));
        assert!(discovered
            .manifest_json
            .contains("\"root_schematic_uuid\": \"root-uuid\""));
        assert!(discovered
            .manifest_json
            .contains("\"identifier\": \"project_symbols:Demo\""));
        assert!(discovered
            .manifest_json
            .contains("\"resolved_path\": \"symbols/KIPRJMOD/libs/project_symbols.kicad_sym\""));
        assert!(discovered
            .manifest_json
            .contains("\"identifier\": \"project_footprints:Conn_A\""));
        assert!(discovered.manifest_json.contains(
            "\"resolved_path\": \"footprints/KIPRJMOD/fp/project_footprints.pretty/Conn_A.kicad_mod\""
        ));
        assert!(discovered.manifest_json.contains("\"model_references\": ["));
        assert!(discovered
            .manifest_json
            .contains("${PROJECT_3D}/Conn_A.step"));
        assert!(discovered
            .manifest_json
            .contains("\"kind\": \"model_file\""));
        assert!(discovered
            .manifest_json
            .contains("\"resolved_path\": \"models/PROJECT_3D/Conn_A.step\""));
    }

    #[test]
    fn missing_sheetfile_is_rejected() {
        let dir = tempdir().unwrap();
        write_test_project(dir.path());
        fs::remove_file(dir.path().join("child.kicad_sch")).unwrap();

        let err = discover_and_validate(&dir.path().join("demo.kicad_pro")).unwrap_err();
        assert!(err.to_string().contains("Referenced KiCad file not found"));
    }

    #[test]
    fn root_uuid_mismatch_is_rejected() {
        let dir = tempdir().unwrap();
        write_test_project(dir.path());
        fs::write(
            dir.path().join("demo.kicad_sch"),
            r#"(kicad_sch
  (version 20231120)
  (uuid "not-root")
)"#,
        )
        .unwrap();

        let err = discover_and_validate(&dir.path().join("demo.kicad_pro")).unwrap_err();
        assert!(err.to_string().contains("Root schematic UUID mismatch"));
    }

    #[test]
    fn bundle_contains_manifest_json() {
        let dir = tempdir().unwrap();
        write_test_project(dir.path());
        let discovered = discover_and_validate(&dir.path().join("demo.kicad_pro")).unwrap();
        let output = resolve_output_path(None, &discovered);

        bundle_project_files(&discovered, &output).unwrap();

        let file = fs::File::open(&output).unwrap();
        let mut zip = ZipArchive::new(file).unwrap();
        let mut names = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect::<Vec<_>>();
        names.sort();

        assert!(names.contains(&"demo/demo.kicad_pro".to_string()));
        assert!(names.contains(&"demo/demo.kicad_pcb".to_string()));
        assert!(names.contains(&"demo/demo.kicad_sch".to_string()));
        assert!(names.contains(&"demo/child.kicad_sch".to_string()));
        assert!(names.contains(&"demo/libs/project_symbols.kicad_sym".to_string()));
        assert!(names.contains(&"demo/fp/project_footprints.pretty/Conn_A.kicad_mod".to_string()));
        assert!(names.contains(&"demo/sym-lib-table".to_string()));
        assert!(names.contains(&"demo/fp-lib-table".to_string()));
        assert!(names.contains(&"demo/placements.csv".to_string()));
        assert!(names.contains(&"symbols/KIPRJMOD/libs/project_symbols.kicad_sym".to_string()));
        assert!(names.contains(
            &"footprints/KIPRJMOD/fp/project_footprints.pretty/Conn_A.kicad_mod".to_string()
        ));
        assert!(names.contains(&"models/PROJECT_3D/Conn_A.step".to_string()));
        assert!(names.contains(&"export_manifest.json".to_string()));
    }
}
