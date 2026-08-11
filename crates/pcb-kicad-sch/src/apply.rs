use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use pcb_sch::{ATTR_SCHEMATIC_NAME, AttributeValue, KICAD_PROJECT_BASENAME, Schematic};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    KicadProject, SchDocument, analysis::analyze_schematic, compose::reconcile_document,
    patch::patch_page_source, schematic_project_path,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchematicApplyResult {
    pub project_file: PathBuf,
    pub root_schematic: PathBuf,
    pub schematic_files: Vec<PathBuf>,
    pub changed: bool,
    pub created: bool,
}

/// Reconcile the linked KiCad schematic project with the evaluated Zener netlist.
///
/// Existing equivalent projects are not written unless their document requires
/// changes. Existing files are changed through UUID-addressed `PatchSet`
/// replacements, written atomically, then reloaded and analyzed to verify the
/// postcondition.
pub fn apply_linked_schematic(netlist: &Schematic) -> Result<Option<SchematicApplyResult>> {
    let Some(path) = schematic_project_path(netlist)? else {
        return Ok(None);
    };
    crate::component_slots::validate_symbol_library_versions(netlist)?;
    match project_state(&path)? {
        ProjectState::Complete(project) => apply_existing(project, netlist).map(Some),
        ProjectState::Uninitialized(project_file) => {
            initialize_project(project_file, false, netlist).map(Some)
        }
        ProjectState::Missing(project_file) => {
            initialize_project(project_file, true, netlist).map(Some)
        }
    }
}

enum ProjectState {
    Complete(KicadProject),
    Uninitialized(PathBuf),
    Missing(PathBuf),
}

fn project_state(path: &Path) -> Result<ProjectState> {
    let explicit_project =
        path.extension().and_then(|extension| extension.to_str()) == Some("kicad_pro");
    let project_file = if explicit_project {
        path.to_path_buf()
    } else if !path.exists() {
        path.join(format!("{KICAD_PROJECT_BASENAME}.kicad_pro"))
    } else {
        if !path.is_dir() {
            bail!("schematic_path is not a directory: {}", path.display());
        }
        let mut projects = files_with_extension(path, "kicad_pro")?;
        match projects.len() {
            0 => path.join(format!("{KICAD_PROJECT_BASENAME}.kicad_pro")),
            1 => projects.remove(0),
            count => bail!(
                "expected at most one .kicad_pro in {}, found {count}",
                path.display()
            ),
        }
    };
    if !project_file.exists() {
        return Ok(ProjectState::Missing(project_file));
    }
    match KicadProject::load(&project_file) {
        Ok(project) => Ok(ProjectState::Complete(project)),
        Err(_error) if project_has_single_missing_root(&project_file)? => {
            Ok(ProjectState::Uninitialized(project_file))
        }
        Err(error) => Err(error),
    }
}

fn apply_existing(project: KicadProject, netlist: &Schematic) -> Result<SchematicApplyResult> {
    let root_schematic = project
        .root_schematics
        .first()
        .cloned()
        .context("linked KiCad project has no root schematic")?;
    let root_file_name = root_schematic
        .file_name()
        .and_then(|name| name.to_str())
        .context("linked KiCad project has no UTF-8 root schematic filename")?;
    let desired = reconcile_document(Some(&project.document), netlist, root_file_name)?;
    verify_document(&desired, netlist, "planned schematic")?;

    let mut writes = Vec::new();
    for page in &desired.pages {
        let file_name = page
            .file_name
            .as_deref()
            .with_context(|| format!("schematic page '{}' has no filename", page.id))?;
        let path = project.directory.join(file_name);
        let source = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if let Some(next) = patch_page_source(&source, page)? {
            writes.push(PendingWrite { path, source, next });
        }
    }
    if writes.is_empty() {
        return Ok(SchematicApplyResult {
            project_file: project.project_file,
            root_schematic,
            schematic_files: project.schematic_files,
            changed: false,
            created: false,
        });
    }

    for (index, write) in writes.iter().enumerate() {
        if let Err(error) = write_atomically(&write.path, &write.next) {
            if let Err(rollback) = restore_sources(&writes[..index]) {
                return Err(error.context(format!(
                    "schematic write failed and rollback also failed: {rollback:#}"
                )));
            }
            return Err(error.context("schematic write failed; restored previously written files"));
        }
    }
    if let Err(error) = verify_project(&project.project_file, netlist) {
        if let Err(rollback) = restore_sources(&writes) {
            return Err(error.context(format!(
                "schematic verification failed and rollback also failed: {rollback:#}"
            )));
        }
        return Err(error.context("schematic verification failed; restored original files"));
    }

    Ok(SchematicApplyResult {
        project_file: project.project_file,
        root_schematic,
        schematic_files: project.schematic_files,
        changed: true,
        created: false,
    })
}

struct PendingWrite {
    path: PathBuf,
    source: String,
    next: String,
}

fn initialize_project(
    project_file: PathBuf,
    create_project_file: bool,
    netlist: &Schematic,
) -> Result<SchematicApplyResult> {
    let directory = project_file
        .parent()
        .context("schematic project path has no parent directory")?
        .to_path_buf();
    let schematic_name = schematic_name(netlist, &project_file)?;
    let root_schematic = directory.join(format!("{schematic_name}.kicad_sch"));
    if root_schematic.exists() {
        bail!(
            "refusing to replace existing KiCad schematic {}",
            root_schematic.display()
        );
    }
    fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    let file_name = root_schematic
        .file_name()
        .and_then(|name| name.to_str())
        .context("generated schematic filename is not UTF-8")?;
    let document = reconcile_document(None, netlist, file_name)?;
    verify_document(&document, netlist, "generated schematic")?;
    let schematic_source = document.to_kicad_sch()?;
    let original_project = if create_project_file {
        None
    } else {
        Some(
            fs::read_to_string(&project_file)
                .with_context(|| format!("failed to read {}", project_file.display()))?,
        )
    };
    let project_source = project_with_root_schematic(
        original_project
            .as_deref()
            .unwrap_or("{\"meta\":{\"version\":1}}"),
        &schematic_name,
        file_name,
    )?;

    write_atomically(&project_file, &project_source)?;
    let result = write_atomically(&root_schematic, &schematic_source)
        .and_then(|_| verify_project(&project_file, netlist));
    if let Err(error) = result {
        if let Err(rollback) =
            rollback_initialization(&project_file, original_project.as_deref(), &root_schematic)
        {
            return Err(error.context(format!(
                "failed to create verified KiCad schematic project and rollback also failed: {rollback:#}"
            )));
        }
        return Err(error.context(
            "failed to create verified KiCad schematic project; restored original files",
        ));
    }

    Ok(SchematicApplyResult {
        project_file,
        root_schematic: root_schematic.clone(),
        schematic_files: vec![root_schematic],
        changed: true,
        created: true,
    })
}

fn schematic_name(netlist: &Schematic, project_file: &Path) -> Result<String> {
    let value = netlist
        .root_ref
        .as_ref()
        .and_then(|root| netlist.instances.get(root))
        .and_then(|root| root.attributes.get(ATTR_SCHEMATIC_NAME));
    let name = match value {
        Some(AttributeValue::String(name)) => name.clone(),
        Some(_) => bail!("schematic_name must be a string"),
        None => project_file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("KiCad project filename has no UTF-8 stem")?
            .to_string(),
    };
    if name.trim().is_empty()
        || name != name.trim()
        || name.contains(['/', '\\'])
        || matches!(name.as_str(), "." | "..")
    {
        bail!("schematic_name must be a non-empty file basename, got '{name}'");
    }
    Ok(name)
}

fn project_with_root_schematic(source: &str, name: &str, file_name: &str) -> Result<String> {
    let mut project: Value =
        serde_json::from_str(source).context("failed to parse KiCad project")?;
    let project = project
        .as_object_mut()
        .context("KiCad project root must be an object")?;
    let schematic = project
        .entry("schematic")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("KiCad project schematic section must be an object")?;
    schematic.insert(
        "top_level_sheets".to_string(),
        json!([{
            "filename": file_name,
            "name": name,
            "uuid": crate::root_page_id(),
        }]),
    );
    let mut source = serde_json::to_string_pretty(&project)?;
    source.push('\n');
    Ok(source)
}

fn project_has_single_missing_root(project_file: &Path) -> Result<bool> {
    let source = fs::read_to_string(project_file)
        .with_context(|| format!("failed to read {}", project_file.display()))?;
    let project: Value = serde_json::from_str(&source)
        .with_context(|| format!("failed to parse {}", project_file.display()))?;
    let top_levels = project
        .get("schematic")
        .and_then(|schematic| schematic.get("top_level_sheets"));
    let root = match top_levels {
        None => project_file.with_extension("kicad_sch"),
        Some(Value::Array(roots)) if roots.is_empty() => project_file.with_extension("kicad_sch"),
        Some(Value::Array(roots)) if roots.len() == 1 => {
            let file_name = roots[0].get("filename").and_then(Value::as_str);
            let Some(file_name) = file_name else {
                return Ok(false);
            };
            project_file
                .parent()
                .context("KiCad project file has no parent directory")?
                .join(file_name)
        }
        _ => return Ok(false),
    };
    Ok(!root.is_file())
}

fn files_with_extension(directory: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
    {
        let path = entry
            .with_context(|| format!("failed to read an entry in {}", directory.display()))?
            .path();
        if path.extension().and_then(|found| found.to_str()) == Some(extension) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn verify_document(document: &SchDocument, netlist: &Schematic, description: &str) -> Result<()> {
    let analysis = analyze_schematic(document, netlist)?;
    if !analysis.is_equivalent() {
        bail!(
            "{description} is not netlist-equivalent: {:#?}",
            analysis.issues()
        );
    }
    Ok(())
}

fn verify_project(project_file: &Path, netlist: &Schematic) -> Result<()> {
    let reloaded = KicadProject::load(project_file)?;
    verify_document(&reloaded.document, netlist, "reloaded schematic")
}

fn restore_sources(writes: &[PendingWrite]) -> Result<()> {
    let mut failures = Vec::new();
    for write in writes {
        if let Err(error) = write_atomically(&write.path, &write.source) {
            failures.push(format!("{}: {error:#}", write.path.display()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("failed to restore {}", failures.join("; "))
    }
}

fn rollback_initialization(
    project_file: &Path,
    original_project: Option<&str>,
    root_schematic: &Path,
) -> Result<()> {
    let mut failures = Vec::new();
    if let Err(error) = remove_file_if_present(root_schematic) {
        failures.push(format!("{}: {error:#}", root_schematic.display()));
    }
    let project_result = match original_project {
        Some(original) => write_atomically(project_file, original),
        None => remove_file_if_present(project_file),
    };
    if let Err(error) = project_result {
        failures.push(format!("{}: {error:#}", project_file.display()));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("failed to restore {}", failures.join("; "))
    }
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn write_atomically(path: &Path, content: &str) -> Result<()> {
    AtomicFile::new(path, OverwriteBehavior::AllowOverwrite)
        .write(|file| file.write_all(content.as_bytes()))
        .with_context(|| format!("failed to write {} atomically", path.display()))
}
