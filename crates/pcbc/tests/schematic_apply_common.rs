use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use pcb_zen_core::{
    DefaultFileProvider, EvalContext,
    resolution::{FrozenPackage, FrozenPackageIdentity, FrozenResolutionMap, ResolutionResult},
    workspace::{WorkspaceInfo, WorkspacePackage},
};

const PACKAGE_URL: &str = "github.com/diodeinc/pcb-kicad-sch-analysis-fixture";

pub fn test_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../pcb-kicad-sch/test-data")
}

pub fn compile_fixture(project: &str, entrypoint: &str) -> pcb_sch::Schematic {
    compile_zener_project(&test_data_dir().join(project), entrypoint)
}

fn compile_zener_project(project_fixture: &Path, entrypoint: &str) -> pcb_sch::Schematic {
    let workspace = tempfile::tempdir().expect("create analysis fixture workspace");
    let workspace_root = workspace
        .path()
        .canonicalize()
        .expect("canonical workspace");
    copy_project_tree(project_fixture, &workspace_root);
    let source = workspace_root.join(entrypoint);
    materialize_test_stdlib(&workspace_root);

    let result = EvalContext::new(
        Arc::new(DefaultFileProvider::new()),
        test_resolution(&workspace_root),
    )
    .set_source_path(source)
    .eval();
    let eval_output = result
        .output
        .unwrap_or_else(|| panic!("evaluate analysis fixture: {:#?}", result.diagnostics));
    let schematic = eval_output.to_schematic_with_diagnostics();
    schematic
        .output
        .unwrap_or_else(|| panic!("compile analysis fixture: {:#?}", schematic.diagnostics))
}

fn copy_project_tree(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source).expect("read Zener fixture project") {
        let entry = entry.expect("read Zener fixture entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            fs::create_dir_all(&destination_path).expect("create Zener fixture directory");
            copy_project_tree(&source_path, &destination_path);
        } else {
            fs::copy(&source_path, &destination_path).expect("copy Zener fixture file");
        }
    }
}

fn materialize_test_stdlib(workspace: &Path) {
    let target = pcb_zen_core::workspace_stdlib_root(workspace);
    for (relative, contents) in pcb_zen_core::stdlib::files_for_tests() {
        let path = target.join(relative);
        fs::create_dir_all(path.parent().unwrap()).expect("create stdlib directory");
        fs::write(path, contents).expect("write stdlib source");
    }

    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/std");
    for relative in [
        "pcb.toml",
        "kicad-symbols/Device.kicad_symdir/R_Small.kicad_sym",
        "kicad-symbols/power.kicad_symdir/GND.kicad_sym",
        "kicad-symbols/power.kicad_symdir/VCC.kicad_sym",
        "kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod",
        "kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod",
        "generics/spice/Resistor.lib",
    ] {
        let destination = target.join(relative);
        fs::create_dir_all(destination.parent().unwrap()).expect("create stdlib asset directory");
        fs::copy(source.join(relative), destination).expect("copy stdlib asset");
    }
}

fn test_resolution(workspace: &Path) -> ResolutionResult {
    let workspace_info = WorkspaceInfo {
        root: workspace.to_path_buf(),
        cache_dir: PathBuf::new(),
        config: None,
        packages: BTreeMap::from([(
            PACKAGE_URL.to_string(),
            WorkspacePackage {
                rel_path: PathBuf::new(),
                config: Default::default(),
                version: None,
                published_at: None,
                preferred: false,
                dirty: false,
                entrypoints: Vec::new(),
                symbol_files: Vec::new(),
            },
        )]),
        errors: Vec::new(),
    };
    ResolutionResult::frozen(
        workspace_info,
        BTreeMap::from([(
            PACKAGE_URL.to_string(),
            FrozenResolutionMap {
                selected_remote: BTreeMap::new(),
                packages: BTreeMap::from([
                    (
                        workspace.to_path_buf(),
                        FrozenPackage {
                            identity: FrozenPackageIdentity::Workspace(PACKAGE_URL.to_string()),
                            deps: BTreeMap::new(),
                            parts: Vec::new(),
                        },
                    ),
                    (
                        pcb_zen_core::workspace_stdlib_root(workspace),
                        FrozenPackage {
                            identity: FrozenPackageIdentity::Stdlib,
                            deps: BTreeMap::new(),
                            parts: Vec::new(),
                        },
                    ),
                ]),
            },
        )]),
        HashMap::new(),
    )
}
