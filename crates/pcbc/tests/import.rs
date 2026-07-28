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
    #[cfg(not(target_os = "windows"))]
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
#[cfg(not(target_os = "windows"))]
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
                .map(|dir| {
                    dir.join(if cfg!(target_os = "windows") {
                        "kicad-cli.exe"
                    } else {
                        "kicad-cli"
                    })
                })
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
    assert!(import.status.success(), "import failed:\n{stderr}");

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

#[cfg(target_os = "windows")]
fn platform_kicad_cli() -> Option<std::path::PathBuf> {
    [
        r"C:\Program Files\KiCad\10.0\bin\kicad-cli.exe",
        r"C:\Program Files\KiCad\9.0\bin\kicad-cli.exe",
    ]
    .into_iter()
    .map(std::path::PathBuf::from)
    .find(|path| path.is_file())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
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

#[cfg(target_os = "windows")]
fn write_kicad_cli_wrapper(root: &std::path::Path) -> std::path::PathBuf {
    let wrapper = root.join("kicad-cli-wrapper.bat");
    fs::write(
        &wrapper,
        r#"@echo off
copy /Y "%PCB_TEST_MUTATED_SCHEMATIC%" "%PCB_TEST_ORIGINAL_SCHEMATIC%" >NUL
copy /Y "%PCB_TEST_MUTATED_PROJECT%" "%PCB_TEST_ORIGINAL_PROJECT%" >NUL
copy /Y "%PCB_TEST_MUTATED_PCB%" "%PCB_TEST_ORIGINAL_PCB%" >NUL
"%PCB_TEST_REAL_KICAD_CLI%" %*
exit /b %ERRORLEVEL%
"#,
    )
    .expect("write kicad-cli wrapper");
    wrapper
}

#[test]
#[cfg(not(target_os = "windows"))]
fn standalone_import_uses_existing_scaffolding_and_root_reports() {
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
    assert!(output.join(".git").is_dir(), "import did not run git init");
    assert!(output.join("README.md").is_file());
    assert!(output.join("pcb.toml").is_file());
    assert!(output.join("layout.zen").is_file());
    assert!(!output.join("layout").exists());
    assert!(!output.join("layout.kicad.archive.zip").exists());

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
#[cfg(not(target_os = "windows"))]
fn project_import_preserves_sources_and_existing_archive_behavior() {
    let mut sandbox = sandbox();
    sandbox.write("source/layout.kicad_sch", STANDALONE_FIXTURE);
    sandbox.write("source/layout.kicad_pro", PROJECT_FIXTURE);
    sandbox.write("source/layout.kicad_pcb", PCB_FIXTURE);
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
}

#[test]
#[cfg(not(target_os = "windows"))]
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
#[cfg(not(target_os = "windows"))]
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
#[cfg(not(target_os = "windows"))]
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
                .map(|port| {
                    let port = port.as_str().unwrap();
                    let refdes = ["J1", "J2", "J3"]
                        .into_iter()
                        .find(|refdes| port.contains(&format!(".{refdes}.")))
                        .unwrap_or_else(|| {
                            panic!("missing fixture refdes in generated port {port}")
                        });
                    let logical_name = port.rsplit('.').next().unwrap();
                    let pin = logical_name
                        .rsplit_once("__")
                        .map(|(_, pin)| pin)
                        .unwrap_or_else(|| panic!("missing physical-pin suffix in {port}"));
                    format!("{refdes}:{pin}")
                })
                .collect();
            partition.sort();
            partition
        })
        .collect()
}

#[test]
#[cfg(not(target_os = "windows"))]
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
}
