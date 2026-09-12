#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;

const STANDALONE_FIXTURE: &str = include_str!("../../pcb-sch/test/kicad-bom/layout.kicad_sch");
const PROJECT_FIXTURE: &str = include_str!("../../pcb-sch/test/kicad-bom/layout.kicad_pro");
const PCB_FIXTURE: &str = include_str!("../../pcb-sch/test/kicad-bom/layout.kicad_pcb");
const PRL_FIXTURE: &str = include_str!("../../pcb-sch/test/kicad-bom/layout.kicad_prl");
const EXTRACTION_REPORT_PREFIX: &str = "Wrote import extraction report to ";
const VALIDATION_DIAGNOSTICS_PREFIX: &str = "Wrote import validation diagnostics to ";

fn sandbox() -> Sandbox {
    let mut sandbox = Sandbox::new();
    let mut paths = Vec::new();
    paths.extend(["/usr/bin", "/bin"].map(std::path::PathBuf::from));
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    sandbox.env(
        "PATH",
        std::env::join_paths(paths)
            .expect("construct test PATH")
            .to_string_lossy(),
    );
    sandbox
}

fn printed_path(stderr: &str, prefix: &str) -> std::path::PathBuf {
    let line = stderr
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("missing {prefix:?} in stderr:\n{stderr}"));
    std::path::PathBuf::from(line[prefix.len()..].trim())
}

fn extraction_report(stderr: &str) -> std::path::PathBuf {
    printed_path(stderr, EXTRACTION_REPORT_PREFIX)
}

fn validation_diagnostics(stderr: &str) -> std::path::PathBuf {
    printed_path(stderr, VALIDATION_DIAGNOSTICS_PREFIX)
}

#[test]
fn import_requires_output_directory() {
    let import = sandbox()
        .run("pcbc", ["import", "layout.kicad_sch"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("run pcbc import");

    assert!(!import.status.success());
    let stderr = String::from_utf8_lossy(&import.stderr);
    assert!(
        stderr.contains("<OUTPUT_DIR>"),
        "unexpected stderr:\n{stderr}"
    );
}

#[test]
fn validation_extraction_and_materialization_share_one_source_snapshot() {
    let mut sandbox = sandbox();
    sandbox.write("source/layout.kicad_sch", STANDALONE_FIXTURE);
    sandbox.write("source/layout.kicad_pro", PROJECT_FIXTURE);
    sandbox.write("source/layout.kicad_pcb", PCB_FIXTURE);
    sandbox.write("mutated-schematic", "not a KiCad schematic\n");
    sandbox.write("mutated-project", "not a KiCad project\n");
    sandbox.write("mutated-pcb", "not a KiCad PCB\n");

    let real_kicad_cli = std::env::var_os("KICAD_CLI")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join("kicad-cli"))
                .find(|path| path.is_file())
        })
        .or_else(platform_kicad_cli);
    assert!(
        real_kicad_cli.as_ref().is_some_and(|path| path.is_file()),
        "could not locate the real kicad-cli"
    );
    let real_kicad_cli = real_kicad_cli.unwrap();

    let wrapper = write_kicad_cli_wrapper(sandbox.root_path());

    let source_root = sandbox.root_path().join("source");
    let source_schematic = source_root.join("layout.kicad_sch");
    let source_project = source_root.join("layout.kicad_pro");
    let source_pcb = source_root.join("layout.kicad_pcb");
    let initial_schematic = fs::read(&source_schematic).unwrap();
    let initial_project = fs::read(&source_project).unwrap();
    let initial_pcb = fs::read(&source_pcb).unwrap();
    let mutated_schematic = sandbox.root_path().join("mutated-schematic");
    let mutated_project = sandbox.root_path().join("mutated-project");
    let mutated_pcb = sandbox.root_path().join("mutated-pcb");
    sandbox
        .env("KICAD_CLI", wrapper.to_string_lossy())
        .env("PCB_TEST_REAL_KICAD_CLI", real_kicad_cli.to_string_lossy())
        .env(
            "PCB_TEST_MUTATED_SCHEMATIC",
            mutated_schematic.to_string_lossy(),
        )
        .env(
            "PCB_TEST_MUTATED_PROJECT",
            mutated_project.to_string_lossy(),
        )
        .env("PCB_TEST_MUTATED_PCB", mutated_pcb.to_string_lossy())
        .env(
            "PCB_TEST_ORIGINAL_SCHEMATIC",
            source_schematic.to_string_lossy(),
        )
        .env(
            "PCB_TEST_ORIGINAL_PROJECT",
            source_project.to_string_lossy(),
        )
        .env("PCB_TEST_ORIGINAL_PCB", source_pcb.to_string_lossy());

    let import = sandbox
        .run("pcbc", ["import", "source/layout.kicad_pro", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("run pcbc import");
    let stderr = String::from_utf8_lossy(&import.stderr);

    assert!(import.status.success(), "import failed:\n{stderr}");
    assert_eq!(
        fs::read(&source_schematic).unwrap(),
        fs::read(&mutated_schematic).unwrap()
    );
    assert_eq!(
        fs::read(&source_project).unwrap(),
        fs::read(&mutated_project).unwrap()
    );
    assert_eq!(
        fs::read(&source_pcb).unwrap(),
        fs::read(&mutated_pcb).unwrap()
    );

    let output = sandbox.root_path().join("out");
    assert_eq!(
        fs::read(output.join("layout/layout.kicad_pro")).unwrap(),
        initial_project
    );
    assert_ne!(
        fs::read(output.join("layout/layout.kicad_pcb")).unwrap(),
        fs::read(&mutated_pcb).unwrap()
    );

    let archive = output.join("layout.kicad.archive.zip");
    assert_eq!(
        read_zip_entry(&archive, "layout/layout.kicad_sch"),
        initial_schematic
    );
    assert_eq!(
        read_zip_entry(&archive, "layout/layout.kicad_pro"),
        initial_project
    );
    assert_eq!(
        read_zip_entry(&archive, "layout/layout.kicad_pcb"),
        initial_pcb
    );
}

fn read_zip_entry(archive_path: &std::path::Path, entry_name: &str) -> Vec<u8> {
    let file = fs::File::open(archive_path).expect("open KiCad archive");
    let mut archive = zip::ZipArchive::new(file).expect("read KiCad archive");
    let mut entry = archive.by_name(entry_name).expect("find archived source");
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).expect("read archived source");
    bytes
}

#[cfg(target_os = "linux")]
fn platform_kicad_cli() -> Option<std::path::PathBuf> {
    None
}

#[cfg(target_os = "macos")]
fn platform_kicad_cli() -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from("/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli");
    path.is_file().then_some(path)
}

fn write_kicad_cli_wrapper(root: &std::path::Path) -> std::path::PathBuf {
    let wrapper = root.join("kicad-cli-wrapper");
    fs::write(
        &wrapper,
        r#"#!/bin/sh
cp "$PCB_TEST_MUTATED_SCHEMATIC" "$PCB_TEST_ORIGINAL_SCHEMATIC"
cp "$PCB_TEST_MUTATED_PROJECT" "$PCB_TEST_ORIGINAL_PROJECT"
cp "$PCB_TEST_MUTATED_PCB" "$PCB_TEST_ORIGINAL_PCB"
exec "$PCB_TEST_REAL_KICAD_CLI" "$@"
"#,
    )
    .expect("write kicad-cli wrapper");
    let mut permissions = fs::metadata(&wrapper).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
    fs::set_permissions(&wrapper, permissions).expect("make kicad-cli wrapper executable");
    wrapper
}

#[test]
fn standalone_import_links_schematic_without_creating_a_pcb_or_archive() {
    let mut sandbox = sandbox();
    sandbox.write("layout.kicad_sch", STANDALONE_FIXTURE);

    let import = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("run pcbc import");
    let stderr = String::from_utf8_lossy(&import.stderr).into_owned();
    assert!(import.status.success(), "import failed:\n{stderr}");

    let output = sandbox.root_path().join("out");
    assert!(output.join("layout.zen").is_file());
    assert!(output.join("layout/layout.kicad_pro").is_file());
    assert!(output.join("layout/layout.kicad_sch").is_file());
    assert!(!output.join("layout/layout.kicad_pcb").exists());
    assert!(!output.join("layout.kicad.archive.zip").exists());
    assert_preserved_schematic(
        &output.join("layout/layout.kicad_sch"),
        STANDALONE_FIXTURE,
        false,
    );
    assert_repeated_schematic_apply(&mut sandbox, "out/layout.zen", &["-S", "bom"]);

    assert_eq!(
        extraction_report(&stderr).canonicalize().unwrap(),
        output
            .join(".kicad.import.extraction.json")
            .canonicalize()
            .unwrap()
    );
    assert_eq!(
        validation_diagnostics(&stderr).canonicalize().unwrap(),
        output
            .join(".kicad.validation.diagnostics.json")
            .canonicalize()
            .unwrap()
    );
    for path in [extraction_report(&stderr), validation_diagnostics(&stderr)] {
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(path).expect("read diagnostics")).expect("parse JSON");
        assert!(value.is_object());
    }

    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(extraction_report(&stderr)).expect("read report"))
            .expect("parse report JSON");
    assert_eq!(
        report["generated"]["validation_diagnostics_json"],
        ".kicad.validation.diagnostics.json"
    );
    assert_eq!(
        report["generated"]["import_extraction_json"],
        ".kicad.import.extraction.json"
    );
}

#[test]
fn project_import_preserves_sources_and_existing_archive_behavior() {
    let mut sandbox = sandbox();
    // Keep this electrical/layout check independent of the fixture's incomplete sourcing.
    let schematic = STANDALONE_FIXTURE.replace("(in_bom yes)", "(in_bom no)");
    // Source parity is advisory: preserve stale PCB metadata/nets rather than correcting them.
    let pcb = PCB_FIXTURE
        .replace("(attr smd)", "(attr smd exclude_from_bom)")
        .replace("(attr smd dnp)", "(attr smd dnp exclude_from_bom)")
        .replace(
            "(property \"Datasheet\" \"~\"",
            "(property \"Datasheet\" \"stale datasheet\"",
        )
        .replace("unconnected-(R1-Pad1)", "STALE_PCB_NET");
    sandbox.write("source/layout.kicad_sch", &schematic);
    sandbox.write("source/layout.kicad_pro", PROJECT_FIXTURE);
    sandbox.write("source/layout.kicad_pcb", &pcb);
    sandbox.write("source/layout.kicad_prl", PRL_FIXTURE);
    let source = sandbox.root_path().join("source");
    let before = fs::read_dir(&source)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_os_string(),
                fs::read(path).unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();

    let import = sandbox
        .run("pcbc", ["import", "source/layout.kicad_pro", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("run project import");
    assert!(
        import.status.success(),
        "project import failed:\n{}",
        String::from_utf8_lossy(&import.stderr)
    );
    let stderr = String::from_utf8_lossy(&import.stderr);
    assert!(stderr.contains("schematic/PCB parity mismatches; these do not block import"));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(extraction_report(&stderr)).unwrap()).unwrap();
    assert_eq!(report["validation"]["schematic_parity_ok"], false);
    assert!(
        report["validation"]["schematic_parity_violations"]
            .as_u64()
            .unwrap()
            > 0
    );
    let diagnostics = fs::read_to_string(validation_diagnostics(&stderr)).unwrap();
    assert!(diagnostics.contains("stale datasheet"));
    assert!(diagnostics.contains("STALE_PCB_NET"));

    let after = fs::read_dir(&source)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_os_string(),
                fs::read(path).unwrap(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(before, after);

    let output = sandbox.root_path().join("out");
    assert!(output.join("layout.kicad.archive.zip").is_file());
    assert!(output.join("layout/layout.kicad_pro").is_file());
    assert!(output.join("layout/layout.kicad_pcb").is_file());
    assert_preserved_schematic(&output.join("layout/layout.kicad_sch"), &schematic, false);
    let pcb_before_apply = fs::read(output.join("layout/layout.kicad_pcb")).unwrap();
    let mut source_pcb = pcb_sexpr::parse(&pcb).unwrap();
    let mut imported_pcb =
        pcb_sexpr::parse(std::str::from_utf8(&pcb_before_apply).unwrap()).unwrap();
    for board in [&mut source_pcb, &mut imported_pcb] {
        for item in board.as_list_mut().unwrap() {
            let Some(footprint) = item.as_list_mut() else {
                continue;
            };
            if footprint.first().and_then(pcb_sexpr::Sexpr::as_sym) == Some("footprint") {
                footprint.retain(|item| {
                    let Some(items) = item.as_list() else {
                        return true;
                    };
                    !(items.first().and_then(pcb_sexpr::Sexpr::as_sym) == Some("path")
                        || (items.first().and_then(pcb_sexpr::Sexpr::as_sym) == Some("property")
                            && items.get(1).and_then(pcb_sexpr::Sexpr::as_str) == Some("Path")))
                });
            }
        }
    }
    assert_eq!(
        imported_pcb, source_pcb,
        "Only footprint identity bindings may change"
    );
    assert_repeated_schematic_apply(&mut sandbox, "out/layout.zen", &[]);
    assert_eq!(
        pcb_before_apply,
        fs::read(output.join("layout/layout.kicad_pcb")).unwrap()
    );
}

fn assert_preserved_schematic(path: &std::path::Path, original: &str, applied: bool) {
    use pcb_kicad_sch::{SchDocument, SchItem, SymbolSlotKey};
    let mut imported = SchDocument::from_kicad_sch(&fs::read_to_string(path).unwrap()).unwrap();
    let mut source = SchDocument::from_kicad_sch(original).unwrap();
    let library = source.pages[0].library.clone();
    // Only managed symbol identity may differ. This checks the whole parsed page, including
    // library definitions, wires, graphics, no-connects, fields, hierarchy, and unit placement.
    for (imported, original) in imported.pages[0]
        .items
        .iter_mut()
        .zip(&mut source.pages[0].items)
    {
        match (imported, original) {
            (SchItem::Symbol(imported), SchItem::Symbol(original)) => {
                if let Some(path) = imported.field_value("Path") {
                    assert_eq!(
                        imported.id,
                        SymbolSlotKey::new(path, imported.unit).unwrap().symbol_id()
                    );
                    imported.id.clone_from(&original.id);
                    imported.fields.remove("Path");
                    if let Some(path) = original.fields.get("Path") {
                        imported.fields.insert("Path".into(), path.clone());
                    }
                }
                imported
                    .fields
                    .retain(|name, _| !name.starts_with("pcb:net"));
                if applied {
                    // KiCad may store all units' pin UUIDs on each unit. Apply keeps only the
                    // selected unit's records, without changing its physical pin identity.
                    let pins = library.definitions[original.library_key()]
                        .placed_pins(original)
                        .unwrap()
                        .into_iter()
                        .map(|pin| pin.number)
                        .collect::<BTreeSet<_>>();
                    original.pins.retain(|pin| pins.contains(&pin.number));
                    original.pins.sort_by(|a, b| a.number.cmp(&b.number));
                    imported.pins.sort_by(|a, b| a.number.cmp(&b.number));
                }
            }
            (SchItem::Label(imported), SchItem::Label(_)) => {
                imported.fields.remove("pcb:net");
            }
            _ => {}
        }
    }
    assert_eq!(imported, source);
}

fn assert_repeated_schematic_apply(sandbox: &mut Sandbox, board: &str, extra: &[&str]) {
    let build = sandbox
        .run(
            "pcbc",
            ["build", board, "--offline"]
                .into_iter()
                .chain(extra.iter().copied()),
        )
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(!String::from_utf8_lossy(&build.stderr).contains("Linked KiCad schematic:"));
    for iteration in 0..2 {
        let apply = sandbox
            .run(
                "pcbc",
                [
                    "apply",
                    "schematic",
                    board,
                    "--offline",
                    "--no-open",
                    "-f",
                    "json",
                ]
                .into_iter()
                .chain(extra.iter().copied()),
            )
            .stdout_capture()
            .stderr_capture()
            .unchecked()
            .run()
            .unwrap();
        assert!(
            apply.status.success(),
            "apply failed: {}",
            String::from_utf8_lossy(&apply.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&apply.stdout).unwrap();
        if iteration == 1 {
            assert_eq!(result["changed"], false, "{result}");
        }
    }
}

#[test]
fn reimport_refuses_without_force_and_force_regenerates() {
    let mut sandbox = sandbox();
    sandbox.write("layout.kicad_sch", STANDALONE_FIXTURE);
    let first = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("initial import");
    assert!(first.status.success());

    let output = sandbox.root_path().join("out");
    let component = output.join("components/ERJ-2RKF1003X/ERJ-2RKF1003X.zen");
    fs::write(&component, "authored change\n").expect("modify generated component");

    let refused = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("refused reimport");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("Use --force"));
    assert_eq!(fs::read_to_string(&component).unwrap(), "authored change\n");

    let forced = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out", "--force"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("forced reimport");
    assert!(
        forced.status.success(),
        "forced reimport failed:\n{}",
        String::from_utf8_lossy(&forced.stderr)
    );
    assert_ne!(fs::read_to_string(component).unwrap(), "authored change\n");
}

#[test]
fn missing_footprint_assignment_preserves_unset_marker_and_still_imports() {
    let mut sandbox = sandbox();
    sandbox.write(
        "layout.kicad_sch",
        STANDALONE_FIXTURE.replacen("Resistor_SMD:R_0402_1005Metric", "~", 1),
    );

    let import = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("run pcbc import");
    let stderr = String::from_utf8_lossy(&import.stderr).into_owned();
    assert!(import.status.success(), "import failed:\n{stderr}");
    assert!(stderr.contains("<missing footprint>"));

    let generated_component = fs::read_to_string(
        sandbox
            .root_path()
            .join("out/components/ERJ-2RKF1003X/ERJ-2RKF1003X.zen"),
    )
    .expect("read generated component without a footprint assignment");
    assert!(generated_component.contains("footprint=\"~\""));
}

#[test]
fn unavailable_footprints_emit_a_short_warning_and_stay_in_the_report() {
    let mut sandbox = sandbox();
    let unresolved_fpid = "UnavailableLibrary:R_0402_1005Metric";
    sandbox.write(
        "layout.kicad_sch",
        STANDALONE_FIXTURE.replace("Resistor_SMD:R_0402_1005Metric", unresolved_fpid),
    );

    let import = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("run pcbc import");
    let stderr = String::from_utf8_lossy(&import.stderr).into_owned();
    assert!(import.status.success(), "import failed:\n{stderr}");
    assert!(stderr.contains(unresolved_fpid));
    assert!(!stderr.contains("Looked in:"));

    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(extraction_report(&stderr)).unwrap()).unwrap();
    let imported_components = report["extraction"]["netlist_components"]
        .as_object()
        .unwrap();
    assert_eq!(imported_components.len(), 3);
    assert!(imported_components.values().all(|component| {
        component["layout"]["unresolved_footprint"]["source_id"] == unresolved_fpid
    }));
}

fn duplicate_pin_connectivity_fixture() -> String {
    let mut schematic = STANDALONE_FIXTURE.to_string();
    schematic = schematic.replace(
        "(property \"Reference\" \"R\"",
        "(property \"Reference\" \"J\"",
    );
    for index in 1..=3 {
        schematic = schematic.replace(&format!("\"R{index}\""), &format!("\"J{index}\""));
    }
    schematic = schematic.replace("(name \"~\"", "(name \"D+\"");
    schematic = schematic.replace("(in_bom yes)", "(in_bom no)");

    let segments = [
        ((101.6, 104.14), (95.25, 104.14)),
        ((95.25, 104.14), (95.25, 114.3)),
        ((95.25, 114.3), (101.6, 114.3)),
        ((101.6, 111.76), (92.71, 111.76)),
        ((92.71, 111.76), (92.71, 124.46)),
        ((92.71, 124.46), (101.6, 124.46)),
    ];
    let mut wires = String::new();
    for (index, (start, end)) in segments.into_iter().enumerate() {
        wires.push_str(&format!(
            "\t(wire\n\t\t(pts\n\t\t\t(xy {} {}) (xy {} {})\n\t\t)\n\t\t(stroke (width 0) (type default))\n\t\t(uuid \"00000000-0000-4000-8000-{:012}\")\n\t)\n",
            start.0,
            start.1,
            end.0,
            end.1,
            index + 1
        ));
    }
    // J3 pin 2 is intentionally open; J2 pin 2 remains floating and unmarked. The marker
    // differs in floating point but occupies the same KiCad integer coordinate as the pin.
    wires.push_str(
        "\t(no_connect (at 101.60001 132.08) (uuid \"00000000-0000-4000-8000-000000000007\"))\n",
    );
    schematic.replacen(
        "\t(sheet_instances",
        &format!("{wires}\t(sheet_instances"),
        1,
    )
}

fn source_physical_partitions(report: &serde_json::Value) -> BTreeSet<Vec<String>> {
    let extraction = &report["extraction"];
    let anchor_to_refdes: BTreeMap<&str, &str> = extraction["netlist_components"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(anchor, component)| {
            (
                anchor.as_str(),
                component["netlist"]["refdes"].as_str().unwrap(),
            )
        })
        .collect();

    extraction["netlist_nets"]
        .as_object()
        .unwrap()
        .values()
        .map(|net| {
            let mut partition: Vec<String> = net["ports"]
                .as_array()
                .unwrap()
                .iter()
                .map(|port| {
                    let anchor = port["component"].as_str().unwrap();
                    let pin = port["pin"].as_str().unwrap();
                    format!("{}:{pin}", anchor_to_refdes[anchor])
                })
                .collect();
            partition.sort();
            partition
        })
        .collect()
}

fn generated_physical_partitions(netlist: &serde_json::Value) -> BTreeSet<Vec<String>> {
    netlist["nets"]
        .as_object()
        .unwrap()
        .values()
        .map(|net| {
            let mut partition: Vec<String> = net["ports"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|port| {
                    let port = port.as_str().unwrap();
                    // The wrapper instance retains the source refdes; logical pin names do not
                    // identify physical pads (e.g. LM358's V+ is pad 8).
                    let refdes = port.rsplit('.').nth(2).unwrap();
                    netlist["instances"][port]["attributes"]["pads"]["Array"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(move |pad| format!("{refdes}:{}", pad["String"].as_str().unwrap()))
                })
                .collect();
            partition.sort();
            partition
        })
        .collect()
}

#[test]
fn standalone_import_preserves_duplicate_display_name_pin_partitions() {
    let mut sandbox = sandbox();
    sandbox.write("layout.kicad_sch", duplicate_pin_connectivity_fixture());

    let import = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("run pcbc import");
    let stderr = String::from_utf8_lossy(&import.stderr).into_owned();
    assert!(import.status.success(), "import failed:\n{stderr}");

    let output = sandbox.root_path().join("out");
    let report: serde_json::Value = serde_json::from_slice(
        &fs::read(extraction_report(&stderr)).expect("read extraction report"),
    )
    .expect("parse extraction report");

    let component_zen =
        fs::read_to_string(output.join("components/ERJ-2RKF1003X/ERJ-2RKF1003X.zen"))
            .expect("read generated component module");
    assert!(component_zen.contains("pin_defs"));
    assert!(component_zen.contains("\"D+__1\": \"1\""));
    assert!(component_zen.contains("\"D+__2\": \"2\""));

    sandbox.cwd("out");
    let build = sandbox
        .run("pcbc", ["build", "layout.zen", "--netlist"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("build imported Zener");
    assert!(
        build.status.success(),
        "generated build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let netlist: serde_json::Value =
        serde_json::from_slice(&build.stdout).expect("parse generated netlist JSON");

    assert_eq!(
        source_physical_partitions(&report),
        generated_physical_partitions(&netlist)
    );
    assert_preserved_schematic(
        &output.join("layout/layout.kicad_sch"),
        &duplicate_pin_connectivity_fixture(),
        false,
    );
    assert_repeated_schematic_apply(&mut sandbox, "layout.zen", &[]);
}

#[test]
fn stacked_no_connect_import_preserves_drawing_and_distinct_physical_pads() {
    let mut sandbox = sandbox();
    let source = STANDALONE_FIXTURE
        .replace("(at 0 -3.81 90)", "(at 0 3.81 270) (hide yes)")
        .replace("(name \"~\"", "(name \"NC\"")
        .replacen("\t(sheet_instances", concat!(
            "\t(no_connect (at 101.60001 124.46) (uuid \"00000000-0000-4000-8000-000000000007\"))\n",
            "\t(sheet_instances"), 1);
    sandbox.write("layout.kicad_sch", &source);
    let import = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    let stderr = String::from_utf8_lossy(&import.stderr);
    assert!(import.status.success(), "{stderr}");
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(extraction_report(&stderr)).unwrap()).unwrap();
    let source_partitions = source_physical_partitions(&report);
    assert_eq!(
        source_partitions,
        BTreeSet::from([
            vec!["R1:1".into(), "R1:2".into()],
            vec!["R2:1".into(), "R2:2".into()],
            vec!["R3:1".into(), "R3:2".into()],
        ]),
        "raw extracted source must retain KiCad's geometric groups"
    );
    let build = sandbox
        .run(
            "pcbc",
            [
                "build",
                "out/layout.zen",
                "--offline",
                "--netlist",
                "-S",
                "bom",
            ],
        )
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let netlist: serde_json::Value = serde_json::from_slice(&build.stdout).unwrap();
    assert_eq!(
        generated_physical_partitions(&netlist),
        BTreeSet::from([
            vec!["R1:1".into(), "R1:2".into()],
            vec!["R2:1".into(), "R2:2".into()],
            vec!["R3:1".into()],
            vec!["R3:2".into()],
        ])
    );
    assert_eq!(
        netlist["nets"]
            .as_object()
            .unwrap()
            .values()
            .filter(|net| net["kind"] == "NotConnected")
            .count(),
        2
    );
    let schematic_path = sandbox.root_path().join("out/layout/layout.kicad_sch");
    assert_preserved_schematic(&schematic_path, &source, false);
    let mut first_apply = None;
    for _ in 0..2 {
        let apply = sandbox
            .run(
                "pcbc",
                [
                    "apply",
                    "schematic",
                    "out/layout.zen",
                    "--offline",
                    "--no-open",
                    "-f",
                    "json",
                    "-S",
                    "bom",
                ],
            )
            .stdout_capture()
            .stderr_capture()
            .unchecked()
            .run()
            .unwrap();
        assert!(
            apply.status.success(),
            "{}",
            String::from_utf8_lossy(&apply.stderr)
        );
        let bytes = fs::read(&schematic_path).unwrap();
        if let Some(first) = &first_apply {
            assert!(first == &bytes, "second apply changed schematic bytes");
            let result: serde_json::Value = serde_json::from_slice(&apply.stdout).unwrap();
            assert_eq!(result["changed"], false);
        }
        first_apply = Some(bytes);
    }
}

#[test]
fn hierarchy_and_no_connect_markers_survive_import_and_apply() {
    let mut sandbox = sandbox();
    let root =
        include_str!("../../pcb-kicad-sch/test-data/kicad-10/issue24201/issue24201.kicad_sch");
    // Sourcing is intentionally absent in this upstream electrical test, not part of this check.
    // Native saves may keep only a cached alias, distinct from the library identity.
    let child = include_str!("../../pcb-kicad-sch/test-data/kicad-10/issue24201/aSheet.kicad_sch")
        .replace("(in_bom yes)", "(in_bom no)")
        .replace("(symbol \"R_", "(symbol \"R_cached_")
        .replace("(symbol \"Device:R\"", "(symbol \"R_cached\"")
        .replace(
            "(lib_id \"Device:R\")",
            "(lib_id \"Device:R\") (lib_name \"R_cached\")",
        );
    sandbox.write("source/issue24201.kicad_sch", root);
    sandbox.write("source/aSheet.kicad_sch", &child);
    let import = sandbox
        .run("pcbc", ["import", "source/issue24201.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert!(
        import.status.success(),
        "{}",
        String::from_utf8_lossy(&import.stderr)
    );
    let output = sandbox.root_path().join("out/layout");
    assert_preserved_schematic(&output.join("issue24201.kicad_sch"), root, false);
    assert_preserved_schematic(&output.join("aSheet.kicad_sch"), &child, false);
    let project = pcbc::kicad_schematic::KicadProject::load(&output).unwrap();
    let paths = project
        .document
        .pages
        .iter()
        .flat_map(|page| &page.items)
        .filter_map(|item| match item {
            pcb_kicad_sch::SchItem::Symbol(symbol) => symbol.field_value("Path"),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(paths, BTreeSet::from(["A.R1.R", "A.R3.R"]));
    assert_repeated_schematic_apply(&mut sandbox, "out/issue24201.zen", &[]);
    assert_preserved_schematic(&output.join("issue24201.kicad_sch"), root, true);
    assert_preserved_schematic(&output.join("aSheet.kicad_sch"), &child, true);
}

#[test]
fn multiunit_import_preserves_physical_pins_and_original_net_label_text() {
    let mut sandbox = sandbox();
    sandbox.write(
        "source/multiunit.kicad_sch",
        include_str!("fixtures/import/multiunit_pinmap_split.kicad_sch")
            .replace("(in_bom yes)", "(in_bom no)"),
    );
    pcb_kicad::KiCadCliBuilder::new()
        .command("sch")
        .subcommand("upgrade")
        .args(["--force", "source/multiunit.kicad_sch"])
        .current_dir(sandbox.root_path().to_string_lossy())
        .run()
        .unwrap();
    let original =
        fs::read_to_string(sandbox.root_path().join("source/multiunit.kicad_sch")).unwrap();
    let import = sandbox
        .run("pcbc", ["import", "source/multiunit.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert!(
        import.status.success(),
        "{}",
        String::from_utf8_lossy(&import.stderr)
    );
    let output = sandbox.root_path().join("out/layout/multiunit.kicad_sch");
    assert_preserved_schematic(&output, &original, false);
    let document =
        pcb_kicad_sch::SchDocument::from_kicad_sch(&fs::read_to_string(&output).unwrap()).unwrap();
    let units = document.pages[0]
        .items
        .iter()
        .filter_map(|item| match item {
            pcb_kicad_sch::SchItem::Symbol(symbol) if symbol.reference() == Some("U1") => {
                Some(symbol.unit)
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(units, BTreeSet::from([1, 2, 3]));
    assert!(document.pages[0].items.iter().any(|item| matches!(item, pcb_kicad_sch::SchItem::Label(label) if label.text == "in" && label.fields["pcb:net"].value == "/in")));
    let build = sandbox
        .run(
            "pcbc",
            ["build", "out/multiunit.zen", "--offline", "--netlist"],
        )
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let netlist: serde_json::Value = serde_json::from_slice(&build.stdout).unwrap();
    let report: serde_json::Value = serde_json::from_slice(
        &fs::read(
            sandbox
                .root_path()
                .join("out/.kicad.import.extraction.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let partitions = generated_physical_partitions(&netlist);
    assert_eq!(source_physical_partitions(&report), partitions);
    assert_eq!(partitions.len(), 9);
    assert_repeated_schematic_apply(&mut sandbox, "out/multiunit.zen", &[]);
    assert_preserved_schematic(&output, &original, true);
}
