#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
const STANDALONE_FIXTURE: &str = include_str!("../../pcb-sch/test/kicad-bom/layout.kicad_sch");
const PROJECT_FIXTURE: &str = include_str!("../../pcb-sch/test/kicad-bom/layout.kicad_pro");
const PCB_FIXTURE: &str = include_str!("../../pcb-sch/test/kicad-bom/layout.kicad_pcb");
const PRL_FIXTURE: &str = include_str!("../../pcb-sch/test/kicad-bom/layout.kicad_prl");

/// Import prints the path to each diagnostics file, which is what an agent consuming this tool reads.
fn printed_path(stderr: &str, prefix: &str) -> std::path::PathBuf {
    let line = stderr
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("missing {prefix:?} in stderr:\n{stderr}"));
    std::path::PathBuf::from(line[prefix.len()..].trim())
}

// `pcbc` has no library target, so these duplicate the prefixes `flow.rs` prints. Rewording one
// there makes `printed_path` find nothing and every test that reads a diagnostics file panics, so
// the divergence cannot pass silently.
const EXTRACTION_REPORT_PREFIX: &str = "Wrote import extraction report to ";
const VALIDATION_DIAGNOSTICS_PREFIX: &str = "Wrote import validation diagnostics to ";

fn extraction_report(stderr: &str) -> std::path::PathBuf {
    printed_path(stderr, EXTRACTION_REPORT_PREFIX)
}

fn validation_diagnostics(stderr: &str) -> std::path::PathBuf {
    printed_path(stderr, VALIDATION_DIAGNOSTICS_PREFIX)
}

/// Sorted output-relative paths of every regular file under `root`.
fn all_files(root: &std::path::Path) -> Vec<String> {
    fn visit(root: &std::path::Path, current: &std::path::Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(current).expect("read output directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                visit(root, &path, out);
            } else {
                out.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort();
    files
}

/// A fresh import writes the design and the manifest that makes it buildable — nothing else. In
/// particular no diagnostics, no `git init`, no `README.md`, and no template board `.zen`.
///
/// Only the git-visible set is asserted. Import generates in place, so validating the result builds
/// it in the output directory and materializes `.pcb/stdlib` there — the same `.pcb/` any `pcb build`
/// in that directory produces, which is why import writes the `.gitignore` that hides it.
#[test]
fn fresh_import_writes_only_the_minimal_file_set() {
    let mut sandbox = Sandbox::new();
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
    assert_eq!(
        tracked_files(&output).keys().cloned().collect::<Vec<_>>(),
        vec![
            ".gitignore".to_string(),
            "components/ERJ-2RKF1003X/ERJ-2RKF1003X.kicad_sym".to_string(),
            "components/ERJ-2RKF1003X/ERJ-2RKF1003X.zen".to_string(),
            "layout.zen".to_string(),
            "pcb.toml".to_string(),
        ]
    );
    assert!(!output.join(".git").exists(), "import ran git init");
    assert!(!output.join("README.md").exists());
    // Everything import puts outside the tracked set is confined to the gitignored build directory.
    assert!(
        all_files(&output)
            .iter()
            .all(|relative| relative.starts_with(".pcb/")
                || tracked_files(&output).contains_key(relative)),
        "import wrote an untracked file outside .pcb/"
    );
    // The .gitignore is what keeps the staged build's `.pcb/` output out of `git status`.
    assert!(
        fs::read_to_string(output.join(".gitignore"))
            .expect("read .gitignore")
            .contains(".pcb/")
    );

    // Both diagnostics live under the output repository's gitignored `.pcb/`, so they are findable
    // next to the board they describe without churning `git status` or accumulating in system temp.
    // Canonicalized: import prints resolved paths, and on macOS the sandbox root reaches the same
    // directory through the `/var` -> `/private/var` symlink.
    let build_dir = output
        .join(".pcb")
        .canonicalize()
        .expect("canonicalize .pcb");
    for diagnostics in [extraction_report(&stderr), validation_diagnostics(&stderr)] {
        assert!(
            diagnostics.starts_with(&build_dir),
            "a diagnostics file escaped the gitignored build directory: {}",
            diagnostics.display()
        );
        let bytes = fs::read(&diagnostics)
            .unwrap_or_else(|error| panic!("read {}: {error}", diagnostics.display()));
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap_or_else(|error| panic!("parse {}: {error}", diagnostics.display()));
    }
    assert!(
        stderr.contains("Wrote imported board to /") && stderr.contains("out/layout.zen"),
        "the printed board path is wrong:\n{stderr}"
    );
}
#[test]
fn project_import_does_not_modify_source_files() {
    let mut sandbox = Sandbox::new();
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
    assert_eq!(
        after.keys().collect::<Vec<_>>(),
        before.keys().collect::<Vec<_>>()
    );
    for (name, contents) in before {
        assert_eq!(
            after.get(&name),
            Some(&contents),
            "source file changed: {name:?}"
        );
    }
}

#[test]
fn standalone_schematic_imports_and_builds_without_synthetic_layout() {
    let mut sandbox = Sandbox::new();
    sandbox.write("layout.kicad_sch", STANDALONE_FIXTURE);

    let import = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("run pcbc import");
    let import_stderr = String::from_utf8_lossy(&import.stderr).into_owned();
    assert!(import.status.success(), "import failed:\n{import_stderr}");

    let output = sandbox.root_path().join("out");
    let board_path = output.join("layout.zen");
    let board = fs::read_to_string(&board_path).expect("read generated board");
    let compact_board: String = board.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(compact_board.contains("layout_path=\"layout\""));
    assert!(compact_board.contains("layers=4"));
    for refdes in ["R1", "R2", "R3"] {
        assert!(board.contains(&format!("# pcb:sch {refdes}")), "{refdes}");
    }

    assert!(!output.join("layout").exists());
    assert!(!output.join("layout.kicad_pro").exists());
    assert!(!output.join("layout.kicad_pcb").exists());

    // The source archive is a copy of an input the user already has, so it is opt-in.
    assert!(!output.join("layout.kicad.archive.zip").exists());

    sandbox.cwd("out");
    let build = sandbox
        .run("pcbc", ["build", "layout.zen", "--netlist"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("run generated build");
    assert!(
        build.status.success(),
        "generated build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(!build.stdout.is_empty(), "generated netlist is empty");
    assert!(
        String::from_utf8_lossy(&build.stderr).contains("[imported_incomplete]"),
        "expected explicit incomplete-sourcing warnings:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(
        &fs::read(extraction_report(&import_stderr)).expect("read import report"),
    )
    .expect("parse import report");
    assert!(
        report["generated"]["sourcing_by_refdes"]
            .as_object()
            .unwrap()
            .values()
            .any(|status| status == "incomplete")
    );
    let netlist: serde_json::Value =
        serde_json::from_slice(&build.stdout).expect("parse generated netlist JSON");
    let components = netlist["instances"]
        .as_object()
        .unwrap()
        .values()
        .filter(|instance| instance["kind"] == "Component")
        .collect::<Vec<_>>();
    assert_eq!(components.len(), 3);
}

/// Output-relative path -> bytes for every tracked file, i.e. everything outside the untracked
/// `.pcb/` output a build materializes.
fn tracked_files(root: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    all_files(root)
        .into_iter()
        .filter(|relative| !relative.starts_with(".pcb/"))
        .map(|relative| {
            let bytes = fs::read(root.join(&relative)).expect("read tracked file");
            (relative, bytes)
        })
        .collect()
}

/// Migration iteration: fix the KiCad side, import again. Nothing that already exists is rewritten,
/// what was kept is reported, and genuinely new output still lands.
#[test]
fn default_reimport_after_a_source_edit_keeps_existing_design_files() {
    let mut sandbox = Sandbox::new();
    sandbox.write("layout.kicad_sch", STANDALONE_FIXTURE);
    let first = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("initial import");
    let first_stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(
        first.status.success(),
        "initial import failed:\n{first_stderr}"
    );

    let output = sandbox.root_path().join("out");
    // Nothing was kept and nothing was ambiguous on a fresh import, so neither field is serialized
    // at all — an agent reading the report sees an absent key, not a null.
    let first_report: serde_json::Value = serde_json::from_slice(
        &fs::read(extraction_report(&first_stderr)).expect("read import report"),
    )
    .expect("parse import report");
    assert!(
        first_report["generated"]
            .get("kept_existing_files")
            .is_none(),
        "fresh import reported kept files: {}",
        first_report["generated"]
    );
    assert!(
        first_report["generated"]
            .get("registry_ambiguous_compatible_entrypoints_by_refdes")
            .is_none(),
        "fresh import reported registry ambiguity: {}",
        first_report["generated"]
    );

    // Authored files at the root, inside a generated package root, and inside `layout/` — the three
    // places a re-import could plausibly write over something it did not create.
    fs::write(output.join("authored.txt"), "authored\n").expect("write authored file");
    fs::create_dir_all(output.join("modules/authored")).expect("authored module dir");
    fs::write(
        output.join("modules/authored/Keep.zen"),
        "AUTHORED = True\n",
    )
    .expect("write authored module");
    fs::create_dir_all(output.join("layout")).expect("authored layout dir");
    fs::write(output.join("layout/keep.txt"), "authored layout\n").expect("write authored layout");
    let before = tracked_files(&output);

    // A symbol-only source edit: the component's `.kicad_sym` regenerates differently while its
    // `.zen` does not. The collision is inside a package, so the whole package is kept.
    sandbox.write(
        "layout.kicad_sch",
        STANDALONE_FIXTURE.replace("(width 0.254)", "(width 0.4)"),
    );

    let rerun = sandbox
        .run(
            "pcbc",
            ["import", "layout.kicad_sch", "out", "--archive-sources"],
        )
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("default re-import");
    let stderr = String::from_utf8_lossy(&rerun.stderr).into_owned();
    assert!(
        rerun.status.success(),
        "default re-import failed:\n{stderr}"
    );

    assert!(
        stderr.contains("Kept existing file(s)")
            && stderr.contains("components/ERJ-2RKF1003X/ERJ-2RKF1003X.kicad_sym"),
        "the kept symbol was not reported:\n{stderr}"
    );
    // The stderr warning has a machine-readable counterpart: an agent that reads the report can tell
    // which files are stale with respect to the KiCad source.
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(extraction_report(&stderr)).expect("read import report"))
            .expect("parse import report");
    let kept = report["generated"]["kept_existing_files"]
        .as_array()
        .unwrap_or_else(|| panic!("kept_existing_files missing: {}", report["generated"]))
        .iter()
        .map(|value| value.as_str().expect("kept path").to_string())
        .collect::<Vec<_>>();
    assert!(
        kept.contains(&"components/ERJ-2RKF1003X/ERJ-2RKF1003X.kicad_sym".to_string()),
        "the kept symbol is not in the report: {kept:?}"
    );
    let after = tracked_files(&output);
    for (relative, bytes) in &before {
        assert_eq!(
            after.get(relative),
            Some(bytes),
            "existing file was rewritten: {relative}"
        );
    }
    // The archive was not requested on the first run, so it is genuinely new output.
    assert!(
        output.join("layout.kicad.archive.zip").is_file(),
        "new output was not added: {:?}",
        all_files(&output)
    );
}
#[test]
fn standalone_schematic_with_unavailable_footprints_imports_and_builds() {
    let mut sandbox = Sandbox::new();
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
    assert!(
        stderr.contains("unresolved footprint definition"),
        "expected unresolved-footprint warning:\n{stderr}"
    );

    let output = sandbox.root_path().join("out");
    let component = fs::read_to_string(first_component_zen(&output)).unwrap();
    assert!(component.contains(&format!("footprint=\"{unresolved_fpid}\"")));
    assert!(component.contains(&format!(
        "\"__imported_unresolved_footprint\": \"{unresolved_fpid}\""
    )));
    let report: serde_json::Value =
        serde_json::from_slice(&fs::read(extraction_report(&stderr)).unwrap()).unwrap();
    let imported_components = report["extraction"]["netlist_components"]
        .as_object()
        .unwrap();
    assert_eq!(imported_components.len(), 3);
    assert!(imported_components.values().all(|component| {
        component["layout"]["unresolved_footprint"]["source_id"] == unresolved_fpid
    }));

    sandbox.cwd("out");
    let build = sandbox
        .run("pcbc", ["build", "layout.zen", "--netlist"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("build imported standalone schematic");
    assert!(
        build.status.success(),
        "generated build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let netlist: serde_json::Value = serde_json::from_slice(&build.stdout).unwrap();
    assert_eq!(
        netlist["instances"]
            .as_object()
            .unwrap()
            .values()
            .filter(|instance| instance["kind"] == "Component")
            .count(),
        3
    );
}

fn first_component_zen(output: &std::path::Path) -> std::path::PathBuf {
    let components = output.join("components");
    let relative = all_files(&components)
        .into_iter()
        .find(|path| path.ends_with(".zen"))
        .expect("generated component .zen");
    components.join(relative)
}

#[test]
fn reimport_reuses_compatible_zener_collision_and_rejects_incompatible_collision() {
    let mut sandbox = Sandbox::new();
    sandbox.write(
        "layout.kicad_sch",
        STANDALONE_FIXTURE.replace("(in_bom yes)", "(in_bom no)"),
    );
    let first = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("initial import");
    assert!(first.status.success());

    let output = sandbox.root_path().join("out");
    let component_zen = first_component_zen(&output);
    let mut compatible = fs::read_to_string(&component_zen).unwrap();
    compatible.push_str("\n# authored compatible change\n");
    fs::write(&component_zen, &compatible).unwrap();

    let reuse = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("compatible collision import");
    assert!(
        reuse.status.success(),
        "compatible collision was rejected:\n{}",
        String::from_utf8_lossy(&reuse.stderr)
    );
    assert!(String::from_utf8_lossy(&reuse.stderr).contains("Validated 1 reused Zener entrypoint"));
    assert_eq!(fs::read_to_string(&component_zen).unwrap(), compatible);

    let incompatible = compatible.replace("P2 = io(Net)", "P2 = io(Net)\nBROKEN = missing_name()");
    fs::write(&component_zen, &incompatible).unwrap();
    fs::write(output.join("authored-sentinel.txt"), "keep\n").unwrap();
    let rejected = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("incompatible collision import");
    assert!(!rejected.status.success());
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("not interface-compatible"),
        "unexpected rejection:\n{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert_eq!(fs::read_to_string(&component_zen).unwrap(), incompatible);
    assert_eq!(
        fs::read_to_string(output.join("authored-sentinel.txt")).unwrap(),
        "keep\n"
    );
}
/// Connectivity validation is the one failure import refuses to ship a board over, so it has to leave a
/// structured record: an agent driving import reads the report at a fixed path, and before this it found
/// either nothing or a previous run's report describing a board that no longer existed.
#[test]
fn a_connectivity_failure_is_recorded_in_the_extraction_report() {
    let mut sandbox = Sandbox::new();
    sandbox.write("layout.kicad_sch", STANDALONE_FIXTURE);
    let first = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("initial import");
    assert!(
        first.status.success(),
        "initial import failed:\n{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let report_path = extraction_report(&String::from_utf8_lossy(&first.stderr));
    let succeeded: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    assert!(
        succeeded["generated"].get("validation_failure").is_none(),
        "a successful import must not record a validation failure"
    );

    // Drop physical pin 2 from the kept component's `pin_defs` and `pins` while leaving its `io`
    // declared, so the package still loads and still satisfies the board's interface, and fails on
    // connectivity — the check this test is about — rather than on evaluation.
    let output = sandbox.root_path().join("out");
    let component_zen = first_component_zen(&output);
    let original = fs::read_to_string(&component_zen).unwrap();
    let dropped = original
        .lines()
        .filter(|line| {
            let line = line.trim();
            line != "\"2\": \"2\"," && line != "\"2\": P2,"
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        original.lines().count() - dropped.lines().count(),
        2,
        "fixture did not drop physical pin 2 from both pin_defs and pins:\n{original}"
    );
    fs::write(&component_zen, &dropped).unwrap();

    let failed = sandbox
        .run("pcbc", ["import", "layout.kicad_sch", "out"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("re-import over a component missing a physical pin");
    let stderr = String::from_utf8_lossy(&failed.stderr).into_owned();
    assert!(
        !failed.status.success(),
        "import should have failed:\n{stderr}"
    );

    let report: serde_json::Value = serde_json::from_slice(&fs::read(&report_path).unwrap())
        .expect("report written on failure");
    let recorded = report["generated"]["validation_failure"]
        .as_str()
        .unwrap_or_else(|| panic!("no validation_failure recorded; report:\n{report:#}"));
    // The record has to name the component, which is what an agent needs in order to act on it.
    assert!(
        recorded.contains("physical pins"),
        "unexpected recorded failure: {recorded}"
    );
    // The kept edit survives the failed run: import still never overwrites.
    assert_eq!(fs::read_to_string(&component_zen).unwrap(), dropped);
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
fn standalone_import_preserves_duplicate_display_name_pin_partitions() {
    let mut sandbox = Sandbox::new();
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

    let source = source_physical_partitions(&report);
    let generated = generated_physical_partitions(&netlist);
    assert_eq!(source, generated);
    assert_eq!(
        generated,
        BTreeSet::from([
            vec!["J1:1".to_string(), "J2:1".to_string()],
            vec!["J1:2".to_string(), "J3:1".to_string()],
            vec!["J2:2".to_string()],
            vec!["J3:2".to_string()],
        ])
    );
}
