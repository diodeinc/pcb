use super::*;
use anyhow::Result;
use std::ffi::OsStr;
use std::path::Path;

pub(super) fn discover_and_select(
    paths: &ImportPaths,
    _args: &ImportArgs,
) -> Result<ImportSelection> {
    let portable = portable::discover_and_validate(&paths.kicad_pro_abs)?;
    let board_name = portable.project_name.clone();

    let selected = SelectedKicadFiles {
        kicad_pro: portable.kicad_pro_rel.clone(),
        kicad_sch: portable.root_schematic_rel.clone(),
        kicad_pcb: portable.primary_kicad_pcb_rel.clone(),
    };

    let files = build_discovered_files(&portable);

    Ok(ImportSelection {
        board_name,
        board_name_source: BoardNameSource::KicadProArgument,
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
            kicad_pro_abs: pro,
        };
        let args = ImportArgs {
            kicad_pro: paths.kicad_pro_abs.clone(),
            output_dir: root.clone(),
            force: true,
        };
        let selection = discover_and_select(&paths, &args)?;
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
            PathBuf::from("layout.kicad_pcb")
        );
        assert!(selection
            .files
            .kicad_sch
            .iter()
            .any(|p| p == Path::new("layout.kicad_sch")));

        Ok(())
    }
}
