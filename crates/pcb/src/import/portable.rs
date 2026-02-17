use super::{PortableExtraFile, PortableKicadProject};
use anyhow::{bail, Context, Result};
use pcb_sexpr::{parse as parse_sexpr, Sexpr};
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

const SYM_LIB_TABLE_FILE: &str = "sym-lib-table";
const FP_LIB_TABLE_FILE: &str = "fp-lib-table";

const MANIFEST_FILE_NAME: &str = "export_manifest.json";

#[derive(Default)]
struct SexprDiscovery {
    sheetfile_refs: BTreeSet<String>,
    symbol_ids: BTreeSet<String>,
    footprint_ids: BTreeSet<String>,
    model_refs: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub(super) struct KicadVariableResolver {
    vars: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
struct KicadProjectManifest {
    project_dir: String,
    project_file: String,
    root_schematic: String,
    pcb_file: String,
    schematic_files: Vec<String>,
    files: Vec<String>,
    bundled_models: Vec<String>,
}

pub(super) fn discover_and_validate(
    kicad_pro_abs: &Path,
) -> Result<(PortableKicadProject, KicadVariableResolver)> {
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
    if let Ok(root_uuid) = extract_root_uuid(&kicad_pro_json) {
        if let Some(root_sch_uuid) = extract_first_schematic_uuid(&root_schematic_abs)? {
            if root_sch_uuid != root_uuid {
                bail!(
                    "Root schematic UUID mismatch: .kicad_pro says '{}', but '{}' has '{}'",
                    root_uuid,
                    root_schematic_abs.display(),
                    root_sch_uuid
                );
            }
        }
    }

    let mut abs_files: BTreeSet<PathBuf> = BTreeSet::new();
    abs_files.insert(kicad_pro_abs.clone());
    abs_files.insert(primary_pcb_abs.clone());
    abs_files.insert(root_schematic_abs.clone());

    let sym_lib_table_abs = project_dir.join(SYM_LIB_TABLE_FILE);
    let fp_lib_table_abs = project_dir.join(FP_LIB_TABLE_FILE);
    if sym_lib_table_abs.exists() {
        abs_files.insert(sym_lib_table_abs.clone());
    }
    if fp_lib_table_abs.exists() {
        abs_files.insert(fp_lib_table_abs.clone());
    }

    // Library tables are optional; used for project-local resolution of symbols/footprints.
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

    // Parse primary PCB for additional project-local references.
    let pcb_content = fs::read_to_string(&primary_pcb_abs)
        .with_context(|| format!("Failed to read {}", primary_pcb_abs.display()))?;
    let pcb_discovery = discover_from_sexpr_text(&pcb_content)
        .with_context(|| format!("Failed to parse {}", primary_pcb_abs.display()))?;
    symbol_ids.extend(pcb_discovery.symbol_ids);
    footprint_ids.extend(pcb_discovery.footprint_ids);
    model_refs.extend(pcb_discovery.model_refs);

    // Traverse schematic hierarchy from resolved root schematic.
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

        symbol_ids.extend(discovery.symbol_ids);
        footprint_ids.extend(discovery.footprint_ids);
        model_refs.extend(discovery.model_refs);

        // Follow sheet hierarchy.
        for sheet_ref in discovery.sheetfile_refs {
            let base_dir = current_abs.parent().unwrap_or(project_dir);
            let child_abs =
                resolve_reference_path(project_dir, base_dir, &sheet_ref, &variable_resolver)?;
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

    // Resolve project-local symbol/footprint assets through library tables.
    for identifier in &symbol_ids {
        let Some((library_nickname, _entry_name)) = parse_library_identifier(identifier) else {
            continue;
        };
        let Some(uri) = sym_lib_table.get(&library_nickname) else {
            continue;
        };
        if let Ok(path) = resolve_symbol_library_uri(project_dir, uri, &variable_resolver) {
            abs_files.insert(path);
        }
    }

    for identifier in &footprint_ids {
        let Some((library_nickname, entry_name)) = parse_library_identifier(identifier) else {
            continue;
        };
        let Some(uri) = fp_lib_table.get(&library_nickname) else {
            continue;
        };
        if let Ok(path) =
            resolve_footprint_library_uri(project_dir, uri, &entry_name, &variable_resolver)
        {
            abs_files.insert(path);
        }
    }

    let mut extra_files_to_bundle: Vec<PortableExtraFile> = Vec::new();
    let mut used_extra_paths: BTreeSet<String> = BTreeSet::new();
    for model_ref in &model_refs {
        let Ok(resolved_abs) = resolve_model_path(project_dir, model_ref, &variable_resolver)
        else {
            continue;
        };

        let archive_hint = model_archive_hint(model_ref, &resolved_abs);
        let archive_path = ensure_unique_archive_path(&mut used_extra_paths, &archive_hint);

        extra_files_to_bundle.push(PortableExtraFile {
            source_path: resolved_abs,
            archive_relative_path: archive_path,
        });
    }

    let mut schematic_files_rel: Vec<PathBuf> = visited_schematics
        .iter()
        .map(|abs| to_relative(project_dir, abs))
        .collect();
    schematic_files_rel.sort();
    schematic_files_rel.dedup();

    let root_schematic_rel = to_relative(project_dir, &root_schematic_abs);
    let primary_kicad_pcb_rel = to_relative(project_dir, &primary_pcb_abs);

    let mut files_to_bundle_rel: Vec<PathBuf> = abs_files
        .iter()
        .map(|abs| to_relative(project_dir, abs))
        .collect();
    files_to_bundle_rel.sort();
    files_to_bundle_rel.dedup();

    // Emit a small manifest into the archive for reproducibility/debugging.
    let manifest = KicadProjectManifest {
        project_dir: project_dir.display().to_string(),
        project_file: path_to_posix_string(&kicad_pro_rel),
        root_schematic: path_to_posix_string(&root_schematic_rel),
        pcb_file: path_to_posix_string(&primary_kicad_pcb_rel),
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

    Ok((
        PortableKicadProject {
            project_dir: project_dir.to_path_buf(),
            project_name,
            kicad_pro_rel,
            root_schematic_rel,
            primary_kicad_pcb_rel,
            schematic_files_rel,
            files_to_bundle_rel,
            extra_files_to_bundle,
            manifest_json,
        },
        variable_resolver,
    ))
}

pub(super) fn write_portable_zip(project: &PortableKicadProject, output_zip: &Path) -> Result<()> {
    if let Some(parent) = output_zip.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create output directory for archive: {}",
                    parent.display()
                )
            })?;
        }
    }

    let output_file = fs::File::create(output_zip)
        .with_context(|| format!("Failed to create archive: {}", output_zip.display()))?;
    let mut zip = ZipWriter::new(BufWriter::new(output_file));

    for relative in &project.files_to_bundle_rel {
        let absolute = project.project_dir.join(relative);
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
    //
    // Use the on-disk path representation (including any Windows verbatim prefix)
    // and normalize it later when resolving expanded paths.
    vars.insert(
        "KIPRJMOD".to_string(),
        project_dir.to_string_lossy().into_owned(),
    );
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
            kind: "model file",
        },
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
    resolve_file_reference(
        project_dir,
        project_dir,
        uri,
        variable_resolver,
        ResolveRefOptions {
            allow_external: false,
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
            kind: "KiCad file",
        },
    )
    .map_err(|e| anyhow::anyhow!("Failed to resolve KiCad path reference '{reference}': {e}"))
}

#[derive(Debug, Clone, Copy)]
struct ResolveRefOptions {
    allow_external: bool,
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
        if !metadata.is_file() {
            return Err(format!(
                "Resolved {} is not a file: {}",
                options.kind,
                candidate.display()
            ));
        }

        let canonical = candidate
            .canonicalize()
            .map_err(|e| format!("Failed to canonicalize {}: {}", candidate.display(), e))?;
        if !options.allow_external && !canonical.starts_with(project_dir) {
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
        expanded.replace('/', "\\")
    }

    #[cfg(not(windows))]
    {
        expanded.to_string()
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

impl KicadVariableResolver {
    /// Create a resolver with no variables (useful for testing or fallback).
    #[allow(dead_code)]
    pub(super) fn empty() -> Self {
        Self {
            vars: BTreeMap::new(),
        }
    }

    /// Expand variables in `input`, keeping the original text for any
    /// variable that cannot be resolved (with a debug log).
    pub(super) fn expand_best_effort(&self, input: &str) -> String {
        match self.expand(input) {
            Ok(v) => v,
            Err(e) => {
                log::debug!("Variable expansion skipped: {}", e);
                input.to_string()
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use zip::ZipArchive;

    #[test]
    fn discovers_root_schematic_from_kicad_pro_and_bundles_zip() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pcb-sch/test/kicad-bom");
        let pro = root.join("layout.kicad_pro");
        let (project, _resolver) = discover_and_validate(&pro)?;
        assert_eq!(project.project_name, "layout");
        assert!(project
            .schematic_files_rel
            .iter()
            .any(|p| p == Path::new("layout.kicad_sch")));

        let dir = tempfile::tempdir()?;
        let zip_path = dir.path().join("out.zip");
        write_portable_zip(&project, &zip_path)?;

        let file = fs::File::open(&zip_path)?;
        let mut zip = ZipArchive::new(file)?;
        let mut names = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect::<Vec<_>>();
        names.sort();

        assert!(names.contains(&"layout/layout.kicad_pro".to_string()));
        assert!(names.contains(&"layout/layout.kicad_pcb".to_string()));
        assert!(names.contains(&"layout/layout.kicad_sch".to_string()));
        assert!(names.contains(&MANIFEST_FILE_NAME.to_string()));

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

        let (project, _resolver) = discover_and_validate(&dir.path().join("demo.kicad_pro"))?;
        let zip_path = dir.path().join("out.zip");
        write_portable_zip(&project, &zip_path)?;

        let file = fs::File::open(&zip_path)?;
        let mut zip = ZipArchive::new(file)?;
        let mut names = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect::<Vec<_>>();
        names.sort();

        assert!(names.contains(&"models/ANT3DMDL/m.step".to_string()));
        Ok(())
    }

    #[test]
    fn expand_best_effort_resolves_known_variables() {
        let mut vars = BTreeMap::new();
        vars.insert("KIPRJMOD".to_string(), "/my/project".to_string());
        vars.insert("MY_VAR".to_string(), "hello".to_string());
        let resolver = KicadVariableResolver { vars };

        assert_eq!(
            resolver.expand_best_effort("${KIPRJMOD}/lib/foo.step"),
            "/my/project/lib/foo.step"
        );
        assert_eq!(resolver.expand_best_effort("${MY_VAR}"), "hello");
        assert_eq!(resolver.expand_best_effort("no variables"), "no variables");
    }

    #[test]
    fn expand_best_effort_returns_original_on_unknown_variable() {
        let resolver = KicadVariableResolver::empty();

        assert_eq!(
            resolver.expand_best_effort("${UNKNOWN_VAR}/path"),
            "${UNKNOWN_VAR}/path"
        );
        assert_eq!(
            resolver.expand_best_effort("$(ALSO_UNKNOWN)/x"),
            "$(ALSO_UNKNOWN)/x"
        );
    }
}

fn to_relative(project_dir: &Path, abs: &Path) -> PathBuf {
    abs.strip_prefix(project_dir).unwrap_or(abs).to_path_buf()
}
