use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use pcb_sch::{AttributeValue, Schematic};
use serde_json::Value;
use uuid::Uuid;

use pcb_kicad_sch::{SchDocument, SchItem, normalize_schematic_path, parse_kicad_sch_page};

/// A KiCad schematic project loaded from one project directory.
#[derive(Debug, Clone)]
pub struct KicadProject {
    pub directory: PathBuf,
    pub project_file: PathBuf,
    pub root_schematics: Vec<PathBuf>,
    pub schematic_files: Vec<PathBuf>,
    pub document: SchDocument,
}

impl KicadProject {
    /// Load the `.kicad_pro` and the schematic hierarchy reachable from its root.
    ///
    /// KiCad 10 flat projects use `schematic.top_level_sheets`. Projects without
    /// that field use KiCad's legacy same-stem root-file rule.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let requested = path.as_ref();
        let directory = if requested.extension().and_then(|ext| ext.to_str()) == Some("kicad_pro") {
            requested
                .parent()
                .context("KiCad project file has no parent directory")?
                .to_path_buf()
        } else {
            requested.to_path_buf()
        };
        let mut project_files = files_with_extension(&directory, "kicad_pro")?;
        if requested.extension().and_then(|ext| ext.to_str()) == Some("kicad_pro") {
            project_files.retain(|candidate| candidate == requested);
        }
        if project_files.len() != 1 {
            bail!(
                "expected exactly one .kicad_pro in {}, found {}",
                directory.display(),
                project_files.len()
            );
        }
        let project_file = project_files.remove(0);
        let project_roots = project_root_schematics(&directory, &project_file)?;
        let root_schematics = project_roots.iter().map(|root| root.path.clone()).collect();
        let (schematic_files, document) = load_schematic_hierarchy(&directory, &project_roots)?;
        Ok(Self {
            directory,
            project_file,
            root_schematics,
            schematic_files,
            document,
        })
    }
}

struct ProjectRoot {
    path: PathBuf,
    id: Option<String>,
}

fn load_schematic_hierarchy(
    directory: &Path,
    roots: &[ProjectRoot],
) -> Result<(Vec<PathBuf>, SchDocument)> {
    let mut schematic_files = Vec::new();
    let mut root_by_path = std::collections::BTreeMap::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        let path = normalize_schematic_path(&root.path);
        if !seen.insert(path.clone()) {
            bail!(
                "top-level schematic {} is listed more than once",
                path.display()
            );
        }
        root_by_path.insert(path.clone(), root.id.as_deref());
        schematic_files.push(path);
    }
    let mut pages = Vec::new();
    let mut root_page_ids = Vec::new();
    let mut index = 0;
    while index < schematic_files.len() {
        let path = schematic_files[index].clone();
        index += 1;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let relative = path.strip_prefix(directory).unwrap_or(&path);
        let relative = relative.to_string_lossy().replace('\\', "/");
        let mut page = parse_kicad_sch_page(Some(&relative), &content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        if let Some(root_id) = root_by_path.get(&normalize_schematic_path(&path)) {
            if let Some(root_id) = root_id {
                page.id = (*root_id).to_string();
            }
            root_page_ids.push(page.id.clone());
        }
        let parent = path.parent().unwrap_or(directory);
        for sheet in page.items.iter().filter_map(|item| match item {
            SchItem::Sheet(sheet) => Some(sheet),
            _ => None,
        }) {
            let child = project_schematic_path(directory, parent, sheet.file_name())?;
            if !child.is_file() {
                bail!(
                    "sheet {} references missing schematic {}",
                    sheet.id,
                    child.display()
                );
            }
            if seen.insert(child.clone()) {
                schematic_files.push(child);
            }
        }
        pages.push(page);
    }
    Ok((
        schematic_files,
        SchDocument {
            pages,
            root_page_ids,
        },
    ))
}

fn project_root_schematics(directory: &Path, project_file: &Path) -> Result<Vec<ProjectRoot>> {
    let content = fs::read_to_string(project_file)
        .with_context(|| format!("failed to read {}", project_file.display()))?;
    let project: Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse {}", project_file.display()))?;
    let Some(top_levels) = project
        .get("schematic")
        .and_then(|schematic| schematic.get("top_level_sheets"))
    else {
        return legacy_project_root(project_file);
    };
    let top_levels = top_levels
        .as_array()
        .context("schematic.top_level_sheets must be an array")?;
    if top_levels.is_empty() {
        return legacy_project_root(project_file);
    }
    top_levels
        .iter()
        .enumerate()
        .map(|(index, sheet)| {
            let file_name = sheet
                .get("filename")
                .and_then(Value::as_str)
                .with_context(|| {
                    format!("schematic.top_level_sheets[{index}].filename must be a string")
                })?;
            let path = project_schematic_path(directory, directory, file_name)?;
            if !path.is_file() {
                bail!("top-level schematic {} does not exist", path.display());
            }
            let id = match sheet.get("uuid") {
                None => None,
                Some(value) => {
                    let value = value.as_str().with_context(|| {
                        format!("schematic.top_level_sheets[{index}].uuid must be a string")
                    })?;
                    let id = Uuid::parse_str(value).with_context(|| {
                        format!("schematic.top_level_sheets[{index}].uuid is invalid")
                    })?;
                    (!id.is_nil()).then(|| id.to_string())
                }
            };
            Ok(ProjectRoot { path, id })
        })
        .collect()
}

/// Resolve a schematic filename while keeping reads and writes inside the
/// linked KiCad project directory.
pub(crate) fn project_schematic_path(
    directory: &Path,
    parent: &Path,
    file_name: impl AsRef<Path>,
) -> Result<PathBuf> {
    let file_name = file_name.as_ref();
    if file_name.as_os_str().is_empty() || file_name.is_absolute() {
        bail!(
            "schematic path '{}' is not relative to project directory {}",
            file_name.display(),
            directory.display()
        );
    }

    let directory = normalize_schematic_path(directory);
    let path = normalize_schematic_path(&parent.join(file_name));
    if path == directory || !path.starts_with(&directory) {
        bail!(
            "schematic path '{}' escapes project directory {}",
            file_name.display(),
            directory.display()
        );
    }

    // Lexical containment rejects absolute paths and `..`. Canonicalizing the
    // closest existing ancestor also rejects a symlinked file or directory
    // that resolves outside the project while still allowing new files.
    if directory.exists() {
        let canonical_directory = fs::canonicalize(&directory)
            .with_context(|| format!("failed to resolve {}", directory.display()))?;
        let existing = path
            .ancestors()
            .find(|ancestor| ancestor.exists())
            .context("schematic path has no existing ancestor")?;
        let canonical_existing = fs::canonicalize(existing)
            .with_context(|| format!("failed to resolve {}", existing.display()))?;
        if !canonical_existing.starts_with(&canonical_directory) {
            bail!(
                "schematic path '{}' resolves outside project directory {}",
                file_name.display(),
                directory.display()
            );
        }
    }

    Ok(path)
}

fn legacy_project_root(project_file: &Path) -> Result<Vec<ProjectRoot>> {
    let root = project_file.with_extension("kicad_sch");
    if !root.is_file() {
        bail!(
            "KiCad project {} has no legacy root schematic {}",
            project_file.display(),
            root.display()
        );
    }
    Ok(vec![ProjectRoot {
        path: root,
        id: None,
    }])
}

/// Resolve the root module's `schematic_path` property, if present.
pub fn schematic_project_path(netlist: &Schematic) -> Result<Option<PathBuf>> {
    let Some(root) = netlist
        .root_ref
        .as_ref()
        .and_then(|root| netlist.instances.get(root))
    else {
        return Ok(None);
    };
    let Some(value) = root.attributes.get(pcb_sch::ATTR_SCHEMATIC_PATH) else {
        return Ok(None);
    };
    let AttributeValue::String(path) = value else {
        bail!("schematic_path must be a string");
    };
    netlist.resolve_package_uri(path).map(Some)
}

pub(crate) fn files_with_extension(directory: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .with_context(|| format!("failed to read an entry in {}", directory.display()))?
            .path();
        if path.extension().and_then(|ext| ext.to_str()) == Some(extension) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn loads_root_page_before_sibling_pages() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("demo.kicad_pro"), "{}").unwrap();
        fs::write(
            directory.path().join("demo.kicad_sch"),
            schematic_with_child("root", "child.kicad_sch"),
        )
        .unwrap();
        fs::write(directory.path().join("child.kicad_sch"), schematic("child")).unwrap();

        let project = KicadProject::load(directory.path()).unwrap();

        assert_eq!(project.document.pages.len(), 2);
        assert_eq!(project.document.pages[0].id, "root");
        assert_eq!(project.document.pages[1].id, "child");
    }

    #[test]
    fn empty_top_level_sheet_list_uses_legacy_root_rule() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("demo.kicad_pro"),
            r#"{"schematic":{"top_level_sheets":[]}}"#,
        )
        .unwrap();
        fs::write(directory.path().join("demo.kicad_sch"), schematic("root")).unwrap();

        let project = KicadProject::load(directory.path()).unwrap();

        assert_eq!(project.document.root_page_ids, ["root"]);
    }

    #[test]
    fn rejects_ambiguous_project_directories() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("one.kicad_pro"), "{}").unwrap();
        fs::write(directory.path().join("two.kicad_pro"), "{}").unwrap();

        let error = KicadProject::load(directory.path()).unwrap_err();

        assert!(error.to_string().contains("exactly one .kicad_pro"));
    }

    #[test]
    fn loads_all_kicad_10_top_level_sheets_in_project_order() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("demo.kicad_pro"),
            r#"{"schematic":{"top_level_sheets":[
                {"uuid":"11111111-1111-1111-1111-111111111111","name":"Main","filename":"main.kicad_sch"},
                {"uuid":"22222222-2222-2222-2222-222222222222","name":"Power","filename":"power.kicad_sch"}
            ]}}"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("main.kicad_sch"),
            schematic("file-main"),
        )
        .unwrap();
        fs::write(
            directory.path().join("power.kicad_sch"),
            schematic("file-power"),
        )
        .unwrap();

        let project = KicadProject::load(directory.path()).unwrap();

        assert_eq!(
            project.document.root_page_ids,
            [
                "11111111-1111-1111-1111-111111111111",
                "22222222-2222-2222-2222-222222222222"
            ]
        );
        assert_eq!(
            project
                .root_schematics
                .iter()
                .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
                .collect::<Vec<_>>(),
            ["main.kicad_sch", "power.kicad_sch"]
        );
    }

    #[test]
    fn rejects_nested_schematic_outside_project() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = workspace.path().join("project");
        fs::create_dir(&directory).unwrap();
        let outside = workspace.path().join("outside.kicad_sch");
        fs::write(directory.join("demo.kicad_pro"), "{}").unwrap();
        fs::write(
            directory.join("demo.kicad_sch"),
            schematic_with_child("root", "../outside.kicad_sch"),
        )
        .unwrap();
        fs::write(&outside, schematic("outside")).unwrap();

        let error = KicadProject::load(&directory).unwrap_err();

        assert!(error.to_string().contains("escapes project directory"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_schematic_symlink_outside_project() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let directory = workspace.path().join("project");
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("demo.kicad_pro"),
            r#"{"schematic":{"top_level_sheets":[
                {"filename":"linked.kicad_sch"}
            ]}}"#,
        )
        .unwrap();
        let outside = workspace.path().join("outside.kicad_sch");
        fs::write(&outside, schematic("outside")).unwrap();
        symlink(outside, directory.join("linked.kicad_sch")).unwrap();

        let error = KicadProject::load(&directory).unwrap_err();

        assert!(error.to_string().contains("resolves outside"));
    }

    fn schematic(uuid: &str) -> String {
        format!(
            "(kicad_sch (version 20260306) (generator eeschema) (uuid {uuid}) (paper \"A4\") (lib_symbols) (sheet_instances (path \"/\" (page \"1\"))))"
        )
    }

    fn schematic_with_child(uuid: &str, child: &str) -> String {
        format!(
            "(kicad_sch (version 20260306) (generator eeschema) (uuid {uuid}) (paper \"A4\") (lib_symbols) (sheet (uuid sheet-1) (property \"Sheetfile\" \"{child}\" (at 0 0 0))) (sheet_instances (path \"/\" (page \"1\"))))"
        )
    }
}
