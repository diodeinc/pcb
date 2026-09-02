#![allow(dead_code)]

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use pcb_kicad_sch::{
    SchDocument, SchItem,
    analysis::{ConnectivityAnalysis, analyze_connectivity},
    connectivity::ConnectivityGraph,
    normalize_schematic_path, parse_kicad_sch_page,
};

pub mod kicad_builder;
use pcb_zen_core::{
    DefaultFileProvider, EvalContext,
    resolution::{FrozenPackage, FrozenPackageIdentity, FrozenResolutionMap, ResolutionResult},
    workspace::{WorkspaceInfo, WorkspacePackage},
};

const PACKAGE_URL: &str = "github.com/diodeinc/pcb-kicad-sch-analysis-fixture";

/// The pcb-kicad-sch fixture tree, workspace-relative so sibling test crates
/// can share this module via `#[path]`.
pub fn test_data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../pcb-kicad-sch/test-data")
}

/// A compiled Zener project paired with a parsed KiCad project.
///
/// The project path is relative to this crate's `test-data` directory. Tests can
/// analyze the pair as-is or make focused edits to the parsed KiCad document.
pub struct AnalysisFixture {
    netlist: pcb_sch::Schematic,
    project: TestProject,
}

impl AnalysisFixture {
    pub fn load(project: &str, zener_entrypoint: &str, kicad_project: &str) -> Self {
        let test_data = test_data_dir();
        let project = test_data.join(project);
        let netlist = compile_zener_project(&project, zener_entrypoint);
        let project = TestProject::load(project.join(kicad_project));
        Self { netlist, project }
    }

    pub fn analyze(&self) -> ConnectivityAnalysis {
        analyze_connectivity(&self.zener_connectivity(), &self.kicad_connectivity())
    }

    pub fn zener_connectivity(&self) -> ConnectivityGraph {
        ConnectivityGraph::from_zener(&self.netlist).expect("reduce Zener analysis fixture")
    }

    pub fn kicad_connectivity(&self) -> ConnectivityGraph {
        ConnectivityGraph::from_kicad(&self.project.document)
            .expect("reduce KiCad analysis fixture")
    }

    pub fn kicad_document(&self) -> &SchDocument {
        &self.project.document
    }

    pub fn netlist(&self) -> &pcb_sch::Schematic {
        &self.netlist
    }

    /// Remove a semantic KiCad item by UUID and return it.
    ///
    /// This is the smallest useful fixture-editing API: tests start with a real
    /// KiCad project and describe only the deliberate divergence under test.
    pub fn remove_kicad_item(&mut self, id: &str) -> SchItem {
        for page in &mut self.project.document.pages {
            if let Some(index) = page.items.iter().position(|item| item_id(item) == Some(id)) {
                return page.items.remove(index);
            }
        }
        panic!("KiCad fixture item {id:?} not found");
    }
}

pub struct TestProject {
    pub schematic_files: Vec<PathBuf>,
    pub document: SchDocument,
}

impl TestProject {
    pub fn load(path: impl AsRef<Path>) -> Self {
        let requested = path.as_ref();
        let directory = if requested.extension().and_then(|ext| ext.to_str()) == Some("kicad_pro") {
            requested.parent().unwrap()
        } else {
            requested
        };
        let project_file = if requested.extension().and_then(|ext| ext.to_str())
            == Some("kicad_pro")
        {
            requested.to_path_buf()
        } else {
            let mut projects = fs::read_dir(directory)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("kicad_pro"))
                .collect::<Vec<_>>();
            projects.sort();
            assert_eq!(projects.len(), 1, "fixture must contain one KiCad project");
            projects.remove(0)
        };
        let project: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&project_file).unwrap()).unwrap();
        let roots = project
            .pointer("/schematic/top_level_sheets")
            .and_then(serde_json::Value::as_array)
            .filter(|roots| !roots.is_empty())
            .map(|roots| {
                roots
                    .iter()
                    .map(|root| {
                        let path = directory.join(root["filename"].as_str().unwrap());
                        let id = root["uuid"].as_str().map(str::to_owned);
                        (normalize_schematic_path(&path), id)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![(project_file.with_extension("kicad_sch"), None)]);
        let root_ids = roots
            .iter()
            .map(|(path, id)| (path.clone(), id.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut schematic_files = roots
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>();
        let mut seen = schematic_files
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let mut pages = Vec::new();
        let mut root_page_ids = Vec::new();
        let mut index = 0;
        while index < schematic_files.len() {
            let path = schematic_files[index].clone();
            index += 1;
            let relative = path.strip_prefix(directory).unwrap_or(&path);
            let mut page = parse_kicad_sch_page(
                Some(&relative.to_string_lossy().replace('\\', "/")),
                &fs::read_to_string(&path).unwrap(),
            )
            .unwrap();
            if let Some(root_id) = root_ids.get(&path) {
                if let Some(root_id) = root_id {
                    page.id = root_id.clone();
                }
                root_page_ids.push(page.id.clone());
            }
            for sheet in page.items.iter().filter_map(|item| match item {
                SchItem::Sheet(sheet) => Some(sheet),
                _ => None,
            }) {
                let child = normalize_schematic_path(
                    &path.parent().unwrap_or(directory).join(sheet.file_name()),
                );
                if seen.insert(child.clone()) {
                    schematic_files.push(child);
                }
            }
            pages.push(page);
        }
        Self {
            schematic_files,
            document: SchDocument {
                pages,
                root_page_ids,
            },
        }
    }
}

/// World position of one pin of a managed symbol, found by its Zener path.
pub fn pin_point(document: &SchDocument, path: &str, number: &str) -> pcb_kicad_sch::Point {
    for page in &document.pages {
        let Some(symbol) = page.items.iter().find_map(|item| match item {
            SchItem::Symbol(symbol) if symbol.field_value("Path") == Some(path) => Some(symbol),
            _ => None,
        }) else {
            continue;
        };
        return page.library.definitions[&symbol.lib_id]
            .placed_pins(symbol)
            .unwrap()
            .into_iter()
            .find(|pin| pin.number == number)
            .unwrap_or_else(|| panic!("missing {path} pin {number}"))
            .point;
    }
    panic!("missing managed symbol {path}")
}

pub fn compile_fixture(project: &str, entrypoint: &str) -> pcb_sch::Schematic {
    let test_data = test_data_dir();
    compile_zener_project(&test_data.join(project), entrypoint)
}

fn item_id(item: &SchItem) -> Option<&str> {
    match item {
        SchItem::Symbol(item) => Some(&item.id),
        SchItem::Wire(item) => Some(&item.id),
        SchItem::Junction(item) => Some(&item.id),
        SchItem::NoConnect(item) => Some(&item.id),
        SchItem::Label(item) => Some(&item.id),
        SchItem::Sheet(item) => Some(&item.id),
        SchItem::Unsupported(_) => None,
    }
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
        "kicad-symbols/Device.kicad_symdir/C_Small.kicad_sym",
        "kicad-symbols/Device.kicad_symdir/R_Small.kicad_sym",
        "kicad-symbols/power.kicad_symdir/GND.kicad_sym",
        "kicad-symbols/power.kicad_symdir/VCC.kicad_sym",
        "kicad-footprints/Capacitor_SMD.pretty/C_0402_1005Metric.kicad_mod",
        "kicad-footprints/Capacitor_SMD.pretty/C_0603_1608Metric.kicad_mod",
        "kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod",
        "kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod",
        "generics/spice/Capacitor.lib",
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
