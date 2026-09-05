use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{SchDocument, SchItem, SchPage};

#[derive(Serialize, Deserialize)]
struct Entry {
    // Legacy entries are matched by filename until the next metadata sync.
    #[serde(default)]
    parent_id: Option<String>,
    parent_file: String,
    /// Native sheet syntax avoids persisting parser spans or Rust model details.
    sheet: String,
}

fn entries(project: &Value) -> Result<Option<Vec<Entry>>> {
    let Some(value) = project.get("diode").and_then(|v| v.get("schematic_sheets")) else {
        return Ok(None);
    };
    serde_json::from_value(value.clone())
        .context("diode.schematic_sheets must be an array of valid sheet relationships")
        .map(Some)
}

fn normalized(base: &str, child: &str) -> Result<PathBuf> {
    let child = Path::new(child);
    if child.as_os_str().is_empty() || child.is_absolute() {
        bail!(
            "schematic sheet path '{child}' must be relative",
            child = child.display()
        );
    }
    let mut out = PathBuf::new();
    if let Some(parent) = Path::new(base).parent() {
        out.push(parent);
    }
    out.push(child);
    let mut clean = PathBuf::new();
    for component in out.components() {
        match component {
            Component::Normal(value) => clean.push(value),
            Component::CurDir => {}
            Component::ParentDir => {
                if !clean.pop() {
                    bail!("schematic sheet path escapes project directory");
                }
            }
            _ => bail!("schematic sheet path escapes project directory"),
        }
    }
    Ok(clean)
}

/// Restore logical relationships retained in project metadata for one parent page.
pub fn restore_sheet_placements(page: &mut SchPage, project: &Value) -> Result<()> {
    let Some(parent_file) = page.file_name.as_deref() else {
        return Ok(());
    };
    let Some(entries) = entries(project)? else {
        return Ok(());
    };
    let mut existing_ids = page
        .items
        .iter()
        .filter_map(SchItem::id)
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>();
    for entry in entries.into_iter().filter(|e| match &e.parent_id {
        Some(id) => id == &page.id,
        None => e.parent_file == parent_file,
    }) {
        let mut sheet = crate::kicad::parse_sheet_source(&entry.sheet)?;
        normalized(parent_file, sheet.file_name())?;
        if !existing_ids.insert(sheet.id.clone()) {
            continue;
        }
        sheet.placed = false;
        page.items.push(SchItem::Sheet(Box::new(sheet)));
    }
    Ok(())
}

/// Replace Diode's relationship metadata from the complete logical document.
pub fn sync_sheet_placements(project: &mut Value, document: &SchDocument) -> Result<bool> {
    let mut next = Vec::new();
    for page in &document.pages {
        let parent_file = page
            .file_name
            .clone()
            .with_context(|| format!("schematic page '{}' has no filename", page.id))?;
        for item in &page.items {
            if let SchItem::Sheet(sheet) = item {
                next.push(Entry {
                    parent_id: Some(page.id.clone()),
                    parent_file: parent_file.clone(),
                    sheet: pcb_sexpr::formatter::format_tree(
                        &crate::kicad::sheet_to_sexpr(sheet),
                        pcb_sexpr::formatter::FormatMode::Normal,
                    ),
                });
            }
        }
    }
    next.sort_by(|a, b| (&a.parent_file, &a.sheet).cmp(&(&b.parent_file, &b.sheet)));
    let current = entries(project)?;
    if next.is_empty() && current.is_none() {
        return Ok(false);
    }
    let value = serde_json::to_value(next)?;
    if project.pointer("/diode/schematic_sheets") == Some(&value) {
        return Ok(false);
    }
    let root = project
        .as_object_mut()
        .context("KiCad project root must be an object")?;
    let diode = root
        .entry("diode")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .context("KiCad project diode section must be an object")?;
    diode.insert("schematic_sheets".into(), value);
    Ok(true)
}
