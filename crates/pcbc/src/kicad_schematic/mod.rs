use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use pcb_layout::utils as layout_utils;
use pcb_sch::{ATTR_SCHEMATIC_NAME, AttributeValue, Schematic};
use serde::Serialize;

mod diagnostics;
pub use diagnostics::{has_unsuppressed_schematic_diagnostics, linked_schematic_diagnostics};
use serde_json::{Value, json};

use pcb_kicad_sch::{
    SchDocument, analysis::inspect_schematic, off_page_warnings, patch_page_source,
    reconcile::plan_reconciliation, sync_sheet_placements,
};

mod project;

pub use project::{KicadProject, read_project_file};
use project::{declared_root_schematics, project_schematic_path, schematic_project_path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SchematicApplyResult {
    pub project_file: PathBuf,
    pub root_schematic: PathBuf,
    pub schematic_files: Vec<PathBuf>,
    pub changed: bool,
    pub created: bool,
    pub warnings: Vec<String>,
}

/// Reconcile the linked KiCad schematic project with the evaluated Zener netlist.
///
/// Existing equivalent projects are not written unless their document requires
/// changes. Existing files are changed through UUID-addressed `PatchSet`
/// replacements, written atomically, then reloaded and analyzed to verify the
/// postcondition.
pub fn apply_linked_schematic(netlist: &Schematic) -> Result<Option<SchematicApplyResult>> {
    let Some(directory) = schematic_project_path(netlist)? else {
        return Ok(None);
    };
    let name = schematic_name(netlist)?;
    let files = layout_utils::resolve_kicad_files(&directory, &name)?;
    let result = if files.kicad_pro.is_file() {
        // KiCad pairs a root schematic only with the same-stem project.
        let project_file = files.rename(files.name())?.kicad_pro;
        // A project whose declared roots are all missing is created from scratch.
        if declared_root_schematics(&project_file)?
            .iter()
            .any(|root| root.is_file())
        {
            apply_existing(KicadProject::load(&project_file)?, netlist)?
        } else {
            initialize_project(project_file, &name, netlist)?
        }
    } else {
        initialize_project(files.kicad_pro, &name, netlist)?
    };
    layout_utils::write_footprint_library_table(&directory, netlist)?;
    Ok(Some(result))
}

fn apply_existing(mut project: KicadProject, netlist: &Schematic) -> Result<SchematicApplyResult> {
    let root_schematic = project
        .root_schematics
        .first()
        .cloned()
        .context("linked KiCad project has no root schematic")?;
    let root_file_name = root_schematic
        .file_name()
        .and_then(|name| name.to_str())
        .context("linked KiCad project has no UTF-8 root schematic filename")?;
    let plan = plan_reconciliation(Some(&project.document), netlist, root_file_name)?;

    // Semantic equality is the no-op boundary. Do not run a parsed KiCad file
    // through our serializer merely because its valid item ordering or
    // formatting differs from generated output.
    let desired = plan.apply(Some(&project.document))?;

    let mut writes = Vec::new();
    let existing_page_ids = project
        .document
        .pages
        .iter()
        .map(|page| page.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for page in &desired.pages {
        let file_name = page
            .file_name
            .as_deref()
            .with_context(|| format!("schematic page '{}' has no filename", page.id))?;
        let path = project_schematic_path(&project.directory, &project.directory, file_name)?;
        if existing_page_ids.contains(page.id.as_str()) {
            let source = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            if let Some(next) = patch_page_source(&source, page)? {
                writes.push(PendingWrite {
                    path,
                    source: Some(source),
                    next: Some(next),
                });
            }
        } else {
            if path.exists() {
                bail!(
                    "refusing to replace unrelated KiCad schematic {}",
                    path.display()
                );
            }
            let next = page.to_kicad_sch();
            writes.push(PendingWrite {
                path,
                source: None,
                next: Some(next),
            });
        }
    }
    let desired_paths = desired_file_paths(&project.directory, &desired)?;
    for path in &project.schematic_files {
        if !desired_paths.contains(path) {
            let source = fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            writes.push(PendingWrite {
                path: path.clone(),
                source: Some(source),
                next: None,
            });
        }
    }
    if sync_sheet_placements(&mut project.project, &desired)? {
        let source = fs::read_to_string(&project.project_file)
            .with_context(|| format!("failed to read {}", project.project_file.display()))?;
        let mut next = serde_json::to_string_pretty(&project.project)?;
        next.push('\n');
        writes.push(PendingWrite {
            path: project.project_file.clone(),
            source: Some(source),
            next: Some(next),
        });
    }
    let changed = !writes.is_empty();
    if changed {
        commit_and_verify(
            &writes,
            &project.project_file,
            netlist,
            "schematic apply failed",
        )?;
    }

    Ok(SchematicApplyResult {
        project_file: project.project_file,
        root_schematic,
        schematic_files: if changed {
            desired_paths
        } else {
            project.schematic_files
        },
        changed,
        created: false,
        warnings: off_page_warnings(&desired)?,
    })
}

struct PendingWrite {
    path: PathBuf,
    source: Option<String>,
    next: Option<String>,
}

/// Write every pending file, then verify the reloaded project against the
/// netlist; on any failure restore what was written (removing files that had
/// no prior source) in reverse order.
fn commit_and_verify(
    writes: &[PendingWrite],
    project_file: &Path,
    netlist: &Schematic,
    context: &str,
) -> Result<()> {
    let mut written = 0;
    let result = writes
        .iter()
        .try_for_each(|write| {
            match &write.next {
                Some(next) => write_atomically(&write.path, next)?,
                None => remove_file_if_present(&write.path)?,
            }
            written += 1;
            Ok(())
        })
        .and_then(|()| verify_project(project_file, netlist));
    let Err(error) = result else {
        return Ok(());
    };
    if let Err(rollback) = restore_sources(&writes[..written]) {
        return Err(error.context(format!("{context} and rollback also failed: {rollback:#}")));
    }
    Err(error.context(format!("{context}; restored original files")))
}

fn initialize_project(
    project_file: PathBuf,
    schematic_name: &str,
    netlist: &Schematic,
) -> Result<SchematicApplyResult> {
    let directory = project_file
        .parent()
        .context("schematic project path has no parent directory")?
        .to_path_buf();
    let root_schematic = project_file.with_extension("kicad_sch");
    if root_schematic.exists() {
        bail!(
            "refusing to replace existing KiCad schematic {}",
            root_schematic.display()
        );
    }
    let file_name = root_schematic
        .file_name()
        .and_then(|name| name.to_str())
        .context("generated schematic filename is not UTF-8")?;
    let plan = plan_reconciliation(None, netlist, file_name)?;
    let document = plan.apply(None)?;
    let schematic_files = desired_file_paths(&directory, &document)?;
    let mut unique_paths = std::collections::BTreeSet::new();
    for path in &schematic_files {
        if !unique_paths.insert(path) {
            bail!(
                "two schematic pages resolve to the same file {}; rename the conflicting module",
                path.display()
            );
        }
        if path.exists() {
            bail!(
                "refusing to replace existing KiCad schematic {}",
                path.display()
            );
        }
    }
    let original_project = project_file
        .exists()
        .then(|| {
            fs::read_to_string(&project_file)
                .with_context(|| format!("failed to read {}", project_file.display()))
        })
        .transpose()?;
    let project_source = project_with_root_schematic(
        original_project
            .as_deref()
            .unwrap_or("{\"meta\":{\"version\":1}}"),
        schematic_name,
        file_name,
    )?;
    let mut project: Value = serde_json::from_str(&project_source)?;
    sync_sheet_placements(&mut project, &document)?;
    let project_source = format!("{}\n", serde_json::to_string_pretty(&project)?);

    fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    // The project file first, so rollback (reverse order) restores it last.
    let mut writes = vec![PendingWrite {
        path: project_file.clone(),
        source: original_project,
        next: Some(project_source),
    }];
    writes.extend(
        document
            .pages
            .iter()
            .zip(&schematic_files)
            .map(|(page, path)| PendingWrite {
                path: path.clone(),
                source: None,
                next: Some(page.to_kicad_sch()),
            }),
    );
    commit_and_verify(
        &writes,
        &project_file,
        netlist,
        "failed to create verified KiCad schematic project",
    )?;

    Ok(SchematicApplyResult {
        project_file,
        root_schematic: root_schematic.clone(),
        schematic_files,
        changed: true,
        created: true,
        warnings: off_page_warnings(&document)?,
    })
}

fn schematic_name(netlist: &Schematic) -> Result<String> {
    let value = netlist
        .root_ref
        .as_ref()
        .and_then(|root| netlist.instances.get(root))
        .and_then(|root| root.attributes.get(ATTR_SCHEMATIC_NAME));
    match value {
        Some(AttributeValue::String(name)) => Ok(name.clone()),
        Some(_) => bail!("schematic_name must be a string"),
        None => bail!("Project() did not set schematic_name"),
    }
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
            "uuid": pcb_kicad_sch::root_page_id(),
        }]),
    );
    let mut source = serde_json::to_string_pretty(&project)?;
    source.push('\n');
    Ok(source)
}

fn verify_project(project_file: &Path, netlist: &Schematic) -> Result<()> {
    let reloaded = KicadProject::load(project_file)?;
    let analysis = inspect_schematic(&reloaded.document, netlist)?.analysis;
    if !analysis.is_equivalent() {
        bail!(
            "reloaded schematic is not netlist-equivalent: {:#?}",
            analysis.issues()
        );
    }
    Ok(())
}

fn restore_sources(writes: &[PendingWrite]) -> Result<()> {
    let mut failures = Vec::new();
    for write in writes.iter().rev() {
        let result = match &write.source {
            Some(source) => write_atomically(&write.path, source),
            None => remove_file_if_present(&write.path),
        };
        if let Err(error) = result {
            failures.push(format!("{}: {error:#}", write.path.display()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("failed to restore {}", failures.join("; "))
    }
}

fn desired_file_paths(directory: &Path, document: &SchDocument) -> Result<Vec<PathBuf>> {
    document
        .pages
        .iter()
        .map(|page| {
            let file_name = page
                .file_name
                .as_deref()
                .with_context(|| format!("schematic page '{}' has no filename", page.id))?;
            project_schematic_path(directory, directory, file_name)
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_apply_restores_deleted_files_and_earlier_writes() {
        for fail_write in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let child = directory.path().join("child.kicad_sch");
            let project = directory.path().join("demo.kicad_pro");
            let new_file = directory.path().join("new.kicad_sch");
            fs::write(&child, "original child\n").unwrap();
            fs::write(&project, "{}\n").unwrap();
            let mut writes = vec![
                PendingWrite {
                    path: child.clone(),
                    source: Some("original child\n".to_string()),
                    next: None,
                },
                PendingWrite {
                    path: new_file.clone(),
                    source: None,
                    next: Some("new child".to_string()),
                },
                PendingWrite {
                    path: project.clone(),
                    source: Some("{}\n".to_string()),
                    next: Some("invalid project JSON".to_string()),
                },
            ];
            if fail_write {
                writes.push(PendingWrite {
                    path: directory.path().join("missing/file.kicad_sch"),
                    source: None,
                    next: Some("cannot write".to_string()),
                });
            }
            let error = commit_and_verify(&writes, &project, &Schematic::default(), "test apply")
                .unwrap_err();
            assert!(error.to_string().contains("restored original files"));
            assert_eq!(fs::read_to_string(&child).unwrap(), "original child\n");
            assert_eq!(fs::read_to_string(&project).unwrap(), "{}\n");
            assert!(!new_file.exists());
        }
    }
}
