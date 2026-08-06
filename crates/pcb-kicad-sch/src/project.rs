use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use pcb_sch::{AttributeValue, Schematic};

use crate::{SchDocument, SchItem, SymbolLibrary, parse_kicad_sch_page};

/// A KiCad schematic project loaded from one project directory.
#[derive(Debug, Clone)]
pub struct KicadProject {
    pub directory: PathBuf,
    pub project_file: PathBuf,
    pub root_schematic: PathBuf,
    pub schematic_files: Vec<PathBuf>,
    pub document: SchDocument,
}

impl KicadProject {
    /// Load the `.kicad_pro` and the schematic hierarchy reachable from its root.
    ///
    /// The root schematic must have the same stem as the single project file.
    /// Child paths are resolved relative to the file containing each sheet.
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
        let root_schematic = project_file.with_extension("kicad_sch");
        if !root_schematic.is_file() {
            bail!(
                "KiCad project {} has no root schematic {}",
                project_file.display(),
                root_schematic.display()
            );
        }

        let (schematic_files, document) = load_schematic_hierarchy(&directory, &root_schematic)?;
        Ok(Self {
            directory,
            project_file,
            root_schematic,
            schematic_files,
            document,
        })
    }
}

fn load_schematic_hierarchy(
    directory: &Path,
    root_schematic: &Path,
) -> Result<(Vec<PathBuf>, SchDocument)> {
    let mut schematic_files = vec![root_schematic.to_path_buf()];
    let mut seen = BTreeSet::from([normalize_path(root_schematic)]);
    let mut pages = Vec::new();
    let mut library = SymbolLibrary::default();
    let mut index = 0;
    while index < schematic_files.len() {
        let path = schematic_files[index].clone();
        index += 1;
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let relative = path.strip_prefix(directory).unwrap_or(&path);
        let relative = relative.to_string_lossy().replace('\\', "/");
        let (page, page_library) = parse_kicad_sch_page(Some(&relative), &content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let parent = path.parent().unwrap_or(directory);
        for sheet in page.items.iter().filter_map(|item| match item {
            SchItem::Sheet(sheet) => Some(sheet),
            _ => None,
        }) {
            let child = normalize_path(&parent.join(&sheet.file_name));
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
        library.merge(page_library);
    }
    Ok((schematic_files, SchDocument { pages, library }))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
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

fn files_with_extension(directory: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let mut files = fs::read_dir(directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some(extension))
        .collect::<Vec<_>>();
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
    fn rejects_ambiguous_project_directories() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("one.kicad_pro"), "{}").unwrap();
        fs::write(directory.path().join("two.kicad_pro"), "{}").unwrap();

        let error = KicadProject::load(directory.path()).unwrap_err();

        assert!(error.to_string().contains("exactly one .kicad_pro"));
    }

    fn schematic(uuid: &str) -> String {
        format!(
            "(kicad_sch (version 20231120) (generator eeschema) (uuid {uuid}) (paper \"A4\") (lib_symbols) (sheet_instances (path \"/\" (page \"1\"))))"
        )
    }

    fn schematic_with_child(uuid: &str, child: &str) -> String {
        format!(
            "(kicad_sch (version 20260306) (generator eeschema) (uuid {uuid}) (paper \"A4\") (lib_symbols) (sheet (uuid sheet-1) (property \"Sheetfile\" \"{child}\" (at 0 0 0))) (sheet_instances (path \"/\" (page \"1\"))))"
        )
    }
}
