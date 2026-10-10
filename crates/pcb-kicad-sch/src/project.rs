use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use uuid::Uuid;

use crate::{SchDocument, SchItem, parse_kicad_sch_page, restore_sheet_placements};

/// A KiCad project and every schematic page reachable from its roots. Paths
/// are POSIX and relative to the project directory.
#[derive(Debug, Clone)]
pub struct LoadedProject {
    pub project: Value,
    pub root_schematics: Vec<String>,
    pub schematic_files: Vec<String>,
    pub document: SchDocument,
}

/// Load a project through `read`, which returns `None` for a missing file.
///
/// KiCad 10 flat projects list their roots in `schematic.top_level_sheets`;
/// other projects use KiCad's legacy same-stem root file. Like KiCad, a missing
/// project file is an empty project.
pub fn load_project(
    project_file: &str,
    mut read: impl FnMut(&str) -> Result<Option<String>>,
) -> Result<LoadedProject> {
    let project: Value = serde_json::from_str(&read(project_file)?.unwrap_or_else(|| "{}".into()))
        .with_context(|| format!("failed to parse {project_file}"))?;
    let roots = project_roots(project_file, &project)?;

    let mut seen = BTreeSet::new();
    let mut queue = Vec::new();
    for (path, _) in &roots {
        if !seen.insert(path.clone()) {
            bail!("top-level schematic {path} is listed more than once");
        }
        let content =
            read(path)?.with_context(|| format!("top-level schematic {path} does not exist"))?;
        queue.push((path.clone(), content));
    }

    let mut pages = Vec::new();
    let mut root_page_ids = Vec::new();
    let mut index = 0;
    while index < queue.len() {
        let path = queue[index].0.clone();
        let content = std::mem::take(&mut queue[index].1);
        let mut page = parse_kicad_sch_page(Some(&path), &content)
            .with_context(|| format!("failed to parse {path}"))?;
        index += 1;
        if let Some((_, id)) = roots.iter().find(|(root, _)| *root == path) {
            if let Some(id) = id {
                page.id.clone_from(id);
            }
            root_page_ids.push(page.id.clone());
        }
        restore_sheet_placements(&mut page, &project)
            .with_context(|| format!("failed to restore sheet metadata for {path}"))?;
        let mut removed_sheets = BTreeSet::new();
        for sheet in page.items.iter().filter_map(|item| match item {
            SchItem::Sheet(sheet) => Some(sheet),
            _ => None,
        }) {
            let child = sheet_file(&path, sheet.file_name())?;
            if seen.contains(&child) {
                continue;
            }
            match read(&child)? {
                Some(content) => {
                    seen.insert(child.clone());
                    queue.push((child, content));
                }
                // Files are authoritative: a removed child invalidates retained
                // placement metadata. Reconciliation can recreate its content.
                None if !sheet.placed => {
                    removed_sheets.insert(sheet.id.clone());
                }
                None => bail!("sheet {} references missing schematic {child}", sheet.id),
            }
        }
        page.items.retain(
            |item| !matches!(item, SchItem::Sheet(sheet) if removed_sheets.contains(&sheet.id)),
        );
        pages.push(page);
    }

    let bus_aliases = project
        .pointer("/schematic/bus_aliases")
        .map(|aliases| serde_json::from_value(aliases.clone()))
        .transpose()
        .context("schematic.bus_aliases must map alias names to member lists")?
        .unwrap_or_default();
    Ok(LoadedProject {
        root_schematics: roots.into_iter().map(|(path, _)| path).collect(),
        schematic_files: queue.into_iter().map(|(path, _)| path).collect(),
        document: SchDocument {
            pages,
            root_page_ids,
            bus_aliases,
        },
        project,
    })
}

/// The root schematics a project declares, which need not exist yet.
pub fn project_root_schematics(project_file: &str, project: &Value) -> Result<Vec<String>> {
    Ok(project_roots(project_file, project)?
        .into_iter()
        .map(|(path, _)| path)
        .collect())
}

fn project_roots(project_file: &str, project: &Value) -> Result<Vec<(String, Option<String>)>> {
    let top_levels = project
        .pointer("/schematic/top_level_sheets")
        .map(|value| {
            value
                .as_array()
                .context("schematic.top_level_sheets must be an array")
        })
        .transpose()?
        .filter(|sheets| !sheets.is_empty());
    let Some(top_levels) = top_levels else {
        let root = Path::new(project_file).with_extension("kicad_sch");
        return Ok(vec![(sheet_file("", &root.to_string_lossy())?, None)]);
    };
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
            let id = sheet
                .get("uuid")
                .map(|value| {
                    let value = value.as_str().with_context(|| {
                        format!("schematic.top_level_sheets[{index}].uuid must be a string")
                    })?;
                    Uuid::parse_str(value).with_context(|| {
                        format!("schematic.top_level_sheets[{index}].uuid is invalid")
                    })
                })
                .transpose()?
                .filter(|id| !id.is_nil())
                .map(|id| id.to_string());
            Ok((sheet_file(project_file, file_name)?, id))
        })
        .collect()
}

/// The project-relative file a sheet in `parent` (project-relative) refers to,
/// resolved as KiCad does: Windows separators are accepted, `${KIPRJMOD}` is
/// the project directory, and the result must stay inside the project.
pub fn sheet_file(parent: &str, child: &str) -> Result<String> {
    let child = child.replace('\\', "/");
    let (base, child) = match child.strip_prefix("${KIPRJMOD}/") {
        Some(rooted) => ("", rooted),
        None => (
            parent.rsplit_once('/').map_or("", |(dir, _)| dir),
            child.as_str(),
        ),
    };
    let drive = child.split('/').next().is_some_and(
        |first| matches!(first.as_bytes(), [letter, b':'] if letter.is_ascii_alphabetic()),
    );
    if child.is_empty() || child.starts_with('/') || drive {
        bail!("schematic sheet path '{child}' must be relative");
    }
    let mut parts = Vec::new();
    for part in base.split('/').chain(child.split('/')) {
        match part {
            "" | "." => {}
            ".." => {
                parts
                    .pop()
                    .context("schematic sheet path escapes project directory")?;
            }
            part => parts.push(part),
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{load_project, sheet_file};
    use crate::connectivity::ConnectivityGraph;

    #[test]
    fn sheet_paths_resolve_like_kicad() {
        let page = |uuid: &str, children: &[&str]| {
            let sheets = children
                .iter()
                .enumerate()
                .map(|(index, child)| {
                    format!(
                        r#"(sheet (uuid s-{uuid}-{index}) (property "Sheetfile" "{child}" (at 0 0 0)))"#
                    )
                })
                .collect::<String>();
            format!(
                r#"(kicad_sch (version 20260306) (generator eeschema) (uuid {uuid}) (paper "A4") (lib_symbols) {sheets})"#
            )
        };
        let files = BTreeMap::from([
            ("demo.kicad_pro", "{}".to_string()),
            ("demo.kicad_sch", page("root", &[r"sub\\child.kicad_sch"])),
            (
                "sub/child.kicad_sch",
                page(
                    "child",
                    &[r"..\\leaf.kicad_sch", r"${KIPRJMOD}\\other.kicad_sch"],
                ),
            ),
            ("leaf.kicad_sch", page("leaf", &[])),
            ("other.kicad_sch", page("other", &[])),
        ]);
        let project = load_project("demo.kicad_pro", |path| Ok(files.get(path).cloned())).unwrap();

        assert_eq!(
            project.schematic_files,
            [
                "demo.kicad_sch",
                "sub/child.kicad_sch",
                "leaf.kicad_sch",
                "other.kicad_sch"
            ]
        );
        ConnectivityGraph::from_kicad(&project.document).unwrap();
        assert!(sheet_file("demo.kicad_sch", r"C:\other\x.kicad_sch").is_err());
        assert_eq!(
            sheet_file("demo.kicad_sch", "rev:/x.kicad_sch").unwrap(),
            "rev:/x.kicad_sch"
        );
    }
}
