use super::*;
use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::path::Path;

pub(super) fn discover_and_select(paths: &ImportPaths) -> Result<ImportSelection> {
    let portable = portable::discover_and_validate(&paths.kicad_input_abs)?;
    let board_name = portable.project_name.clone();
    // Validated here, at the point the name is first known: it is interpolated into `pcb.toml` and
    // used as the generated `.zen` file name, so a name the manifest cannot hold must fail before
    // any output is written. Rename the input file rather than have the import rename the board.
    crate::new::validate_derived_board_name(&board_name).with_context(|| {
        format!(
            "cannot derive a board name from {}",
            paths.kicad_input_abs.display()
        )
    })?;
    let source_kind = portable.source_kind;

    let selected = SelectedKicadFiles {
        kicad_pro: portable.kicad_pro_rel.clone(),
        kicad_sch: portable.root_schematic_rel.clone(),
        kicad_pcb: portable.primary_kicad_pcb_rel.clone(),
    };

    let files = build_discovered_files(&portable);
    let board_name_source = match source_kind {
        ImportSourceKind::Schematic => BoardNameSource::KicadSchArgument,
        ImportSourceKind::Project => BoardNameSource::KicadProArgument,
    };

    Ok(ImportSelection {
        board_name,
        board_name_source,
        files,
        selected,
        portable,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KicadFileKind {
    KicadPro,
    KicadSch,
    KicadPcb,
    KicadSym,
    KicadMod,
    KicadPrl,
    KicadDru,
    FpLibTable,
    SymLibTable,
}

fn classify_kicad_file(rel_path: &Path) -> Option<KicadFileKind> {
    let file_name = rel_path.file_name().and_then(|s| s.to_str()).unwrap_or("");

    if file_name == "fp-lib-table" {
        return Some(KicadFileKind::FpLibTable);
    }
    if file_name == "sym-lib-table" {
        return Some(KicadFileKind::SymLibTable);
    }

    match rel_path.extension().and_then(OsStr::to_str) {
        Some("kicad_pro") => Some(KicadFileKind::KicadPro),
        Some("kicad_sch") => Some(KicadFileKind::KicadSch),
        Some("kicad_pcb") => Some(KicadFileKind::KicadPcb),
        Some("kicad_sym") => Some(KicadFileKind::KicadSym),
        Some("kicad_mod") => Some(KicadFileKind::KicadMod),
        Some("kicad_prl") => Some(KicadFileKind::KicadPrl),
        Some("kicad_dru") => Some(KicadFileKind::KicadDru),
        _ => None,
    }
}

fn sort_discovered_files(files: &mut KicadDiscoveredFiles) {
    files.kicad_pro.sort();
    files.kicad_sch.sort();
    files.kicad_pcb.sort();
    files.kicad_sym.sort();
    files.kicad_mod.sort();
    files.kicad_prl.sort();
    files.kicad_dru.sort();
    files.fp_lib_table.sort();
    files.sym_lib_table.sort();
}

fn build_discovered_files(portable: &PortableKicadProject) -> KicadDiscoveredFiles {
    let mut out = KicadDiscoveredFiles::default();

    for rel in &portable.files_to_bundle_rel {
        let Some(kind) = classify_kicad_file(rel) else {
            continue;
        };
        match kind {
            KicadFileKind::KicadPro => out.kicad_pro.push(rel.clone()),
            KicadFileKind::KicadSch => out.kicad_sch.push(rel.clone()),
            KicadFileKind::KicadPcb => out.kicad_pcb.push(rel.clone()),
            KicadFileKind::KicadSym => out.kicad_sym.push(rel.clone()),
            KicadFileKind::KicadMod => out.kicad_mod.push(rel.clone()),
            KicadFileKind::KicadPrl => out.kicad_prl.push(rel.clone()),
            KicadFileKind::KicadDru => out.kicad_dru.push(rel.clone()),
            KicadFileKind::FpLibTable => out.fp_lib_table.push(rel.clone()),
            KicadFileKind::SymLibTable => out.sym_lib_table.push(rel.clone()),
        }
    }

    // Ensure all reachable schematics are always included in the list used by extraction.
    out.kicad_sch = portable.schematic_files_rel.clone();
    sort_discovered_files(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::path::PathBuf;

    #[test]
    fn discovers_kicad_files_and_infers_name() -> Result<()> {
        // Use an existing KiCad project fixture already in the repo.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pcb-sch/test/kicad-bom");
        let pro = root.join("layout.kicad_pro");

        let paths = ImportPaths {
            workspace_root: root.clone(),
            kicad_project_root: root.clone(),
            kicad_input_abs: pro,
        };
        let selection = discover_and_select(&paths)?;
        assert_eq!(selection.board_name, "layout");
        assert!(matches!(
            selection.board_name_source,
            BoardNameSource::KicadProArgument
        ));
        assert_eq!(
            selection.selected.kicad_sch,
            PathBuf::from("layout.kicad_sch")
        );
        assert_eq!(
            selection.selected.kicad_pcb,
            Some(PathBuf::from("layout.kicad_pcb"))
        );
        assert!(
            selection
                .files
                .kicad_sch
                .iter()
                .any(|p| p == Path::new("layout.kicad_sch"))
        );

        Ok(())
    }

    /// Copy the standalone-schematic fixture under `stem`, then run discovery against it.
    fn discover_schematic_named(stem: &str) -> Result<ImportSelection> {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../pcb-sch/test/kicad-bom/layout.kicad_sch");
        let temp = tempfile::tempdir()?;
        let root = temp.path().to_path_buf();
        let schematic = root.join(format!("{stem}.kicad_sch"));
        std::fs::copy(&fixture, &schematic)?;

        discover_and_select(&ImportPaths {
            workspace_root: root.clone(),
            kicad_project_root: root,
            kicad_input_abs: schematic,
        })
    }

    /// Unix only: Windows filesystems cannot hold a name containing a double quote or a newline, so the
    /// input this rejects cannot be constructed there. The validator itself is platform-independent and
    /// covered by `crate::new`'s own tests.
    #[cfg(unix)]
    #[test]
    fn rejects_board_names_that_would_break_the_manifest() {
        for (stem, expected) in [
            ("say \"hi\"", "contains a double quote"),
            ("line\nbreak", "contains a newline"),
        ] {
            let Err(error) = discover_schematic_named(stem) else {
                panic!("expected discovery to reject the derived board name {stem:?}");
            };
            let rendered = format!("{error:#}");
            assert!(
                rendered.contains(expected),
                "missing {expected:?} in {rendered:?}"
            );
            // The error must name the input file the name came from.
            assert!(
                rendered.contains("cannot derive a board name from")
                    && rendered.contains(".kicad_sch"),
                "error does not name the input file: {rendered:?}"
            );
        }
    }

    #[test]
    fn accepts_board_names_pcb_new_would_reject() -> Result<()> {
        let selection = discover_schematic_named("My Board (rev B)")?;
        assert_eq!(selection.board_name, "My Board (rev B)");
        Ok(())
    }

    #[test]
    fn discovers_standalone_schematic_without_synthetic_layout_files() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../pcb-sch/test/kicad-bom");
        let schematic = root.join("layout.kicad_sch");
        let paths = ImportPaths {
            workspace_root: root.clone(),
            kicad_project_root: root.clone(),
            kicad_input_abs: schematic,
        };
        let selection = discover_and_select(&paths)?;
        assert_eq!(selection.board_name, "layout");
        assert_eq!(selection.portable.source_kind, ImportSourceKind::Schematic);
        assert!(matches!(
            selection.board_name_source,
            BoardNameSource::KicadSchArgument
        ));
        assert_eq!(selection.selected.kicad_pro, None);
        assert_eq!(selection.selected.kicad_pcb, None);
        assert_eq!(
            selection.selected.kicad_sch,
            PathBuf::from("layout.kicad_sch")
        );
        assert!(selection.files.kicad_pro.is_empty());
        assert!(selection.files.kicad_pcb.is_empty());
        assert_eq!(
            selection.files.kicad_sch,
            vec![PathBuf::from("layout.kicad_sch")]
        );
        Ok(())
    }
}
