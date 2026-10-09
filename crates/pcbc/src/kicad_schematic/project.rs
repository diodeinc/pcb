use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use pcb_sch::{AttributeValue, Schematic};
use serde_json::Value;

use pcb_kicad_sch::{SchDocument, load_project, normalize_schematic_path};

/// A KiCad schematic project loaded from one project directory.
#[derive(Debug, Clone)]
pub struct KicadProject {
    pub directory: PathBuf,
    pub project_file: PathBuf,
    pub root_schematics: Vec<PathBuf>,
    pub schematic_files: Vec<PathBuf>,
    pub document: SchDocument,
    pub project: Value,
}

impl KicadProject {
    /// Load the `.kicad_pro` and the schematic hierarchy reachable from its root.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let requested = path.as_ref();
        let (directory, project_file) =
            if requested.extension().and_then(|ext| ext.to_str()) == Some("kicad_pro") {
                let directory = requested
                    .parent()
                    .context("KiCad project file has no parent directory")?
                    .to_path_buf();
                (directory, requested.to_path_buf())
            } else {
                let project_file = pcb_layout::utils::require_kicad_files(requested)?.kicad_pro;
                (requested.to_path_buf(), project_file)
            };
        let file_name = project_file
            .file_name()
            .and_then(|name| name.to_str())
            .context("KiCad project file name is not UTF-8")?;
        let loaded = load_project(file_name, |relative| {
            read_project_file(&directory, relative)
        })?;
        let absolute = |paths: Vec<String>| {
            paths
                .into_iter()
                .map(|path| normalize_schematic_path(&directory.join(path)))
                .collect()
        };
        Ok(Self {
            root_schematics: absolute(loaded.root_schematics),
            schematic_files: absolute(loaded.schematic_files),
            document: loaded.document,
            project: loaded.project,
            directory,
            project_file,
        })
    }
}

/// Absolute paths of the root schematics a project declares, which need not exist yet.
pub(crate) fn declared_root_schematics(project_file: &Path) -> Result<Vec<PathBuf>> {
    let directory = project_file
        .parent()
        .context("KiCad project file has no parent directory")?;
    let file_name = project_file
        .file_name()
        .and_then(|name| name.to_str())
        .context("KiCad project file name is not UTF-8")?;
    let project: Value = serde_json::from_str(&fs::read_to_string(project_file)?)
        .with_context(|| format!("failed to parse {}", project_file.display()))?;
    pcb_kicad_sch::project_root_schematics(file_name, &project)?
        .iter()
        .map(|root| project_schematic_path(directory, directory, root))
        .collect()
}

fn read_project_file(directory: &Path, relative: &str) -> Result<Option<String>> {
    let path = project_schematic_path(directory, directory, relative)?;
    match fs::read_to_string(&path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read {}", path.display())),
    }
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

/// Resolve the root module's `schematic_path` property, if present.
pub(crate) fn schematic_project_path(netlist: &Schematic) -> Result<Option<PathBuf>> {
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

#[cfg(test)]
mod tests {
    use std::fs;

    use pcb_kicad_sch::{SchItem, parse_kicad_sch_page, sync_sheet_placements};

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

        assert!(error.to_string().contains("Multiple .kicad_pro files"));
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

    #[test]
    fn loads_metadata_only_child_when_parent_source_no_longer_has_sheet() {
        let directory = tempfile::tempdir().unwrap();
        let parent = parse_kicad_sch_page(
            Some("demo.kicad_sch"),
            &schematic_with_child("root", "child.kicad_sch"),
        )
        .unwrap();
        let mut metadata = serde_json::json!({});
        sync_sheet_placements(
            &mut metadata,
            &SchDocument {
                pages: vec![parent],
                root_page_ids: vec!["root".into()],
                ..Default::default()
            },
        )
        .unwrap();
        fs::write(
            directory.path().join("demo.kicad_pro"),
            serde_json::to_string(&metadata).unwrap(),
        )
        .unwrap();
        fs::write(directory.path().join("demo.kicad_sch"), schematic("root")).unwrap();
        fs::write(directory.path().join("child.kicad_sch"), schematic("child")).unwrap();

        let project = KicadProject::load(directory.path()).unwrap();

        assert_eq!(
            project.document.pages.len(),
            2,
            "restored metadata relationship keeps its child loadable"
        );
        assert!(
            project
                .schematic_files
                .iter()
                .any(|path| path.ends_with("child.kicad_sch"))
        );
        assert!(project.document.pages[0].items.iter().any(
            |item| matches!(item, SchItem::Sheet(sheet) if sheet.id == "sheet-1" && !sheet.placed)
        ));
    }

    #[test]
    fn root_and_nested_parent_renames_preserve_metadata_only_children() {
        for nested in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let parent = parse_kicad_sch_page(
                Some("old.kicad_sch"),
                &schematic_with_child("parent", "child.kicad_sch"),
            )
            .unwrap();
            let mut metadata = serde_json::json!({
                "schematic": {"top_level_sheets": [{
                    "filename": if nested { "root.kicad_sch" } else { "renamed.kicad_sch" }
                }]}
            });
            sync_sheet_placements(
                &mut metadata,
                &SchDocument {
                    pages: vec![parent],
                    root_page_ids: vec!["parent".into()],
                    ..Default::default()
                },
            )
            .unwrap();
            fs::write(
                directory.path().join("demo.kicad_pro"),
                serde_json::to_string(&metadata).unwrap(),
            )
            .unwrap();
            fs::write(
                directory.path().join("renamed.kicad_sch"),
                schematic("parent"),
            )
            .unwrap();
            fs::write(directory.path().join("child.kicad_sch"), schematic("child")).unwrap();
            if nested {
                fs::write(
                    directory.path().join("root.kicad_sch"),
                    schematic_with_child("root", "renamed.kicad_sch"),
                )
                .unwrap();
            }
            let project = KicadProject::load(directory.path()).unwrap();
            assert!(project.document.pages.iter().any(|page| page.id == "child"));
            let parent = project
                .document
                .pages
                .iter()
                .find(|page| page.id == "parent")
                .unwrap();
            assert!(
                parent
                    .items
                    .iter()
                    .any(|item| matches!(item, SchItem::Sheet(sheet) if !sheet.placed))
            );
            assert_eq!(parent.file_name.as_deref(), Some("renamed.kicad_sch"));
        }
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
