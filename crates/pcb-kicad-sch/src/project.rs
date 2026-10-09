use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use uuid::Uuid;

use crate::sheet_placements::normalized;
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
/// other projects use KiCad's legacy same-stem root file.
pub fn load_project(
    project_file: &str,
    mut read: impl FnMut(&str) -> Result<Option<String>>,
) -> Result<LoadedProject> {
    let project: Value = serde_json::from_str(
        &read(project_file)?.with_context(|| format!("{project_file} does not exist"))?,
    )
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
            let child = posix(&normalized(&path, sheet.file_name())?);
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
        return Ok(vec![(posix(&root), None)]);
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
            Ok((posix(&normalized(project_file, file_name)?), id))
        })
        .collect()
}

fn posix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
