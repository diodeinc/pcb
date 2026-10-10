#![cfg(not(target_os = "windows"))]

use std::collections::BTreeSet;
use std::fs::File;

use httpmock::prelude::*;
use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;
use serde_json::Value;

const LED_MODULE_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio")

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

led_color = config(str, default = "red")
r_value = config(str, default = "330Ohm")
package = config(str, default = "0603")

VCC = io(Power)
GND = io(Ground)
CTRL = io(Gpio)

led_anode = Net("LED_ANODE")

Resistor(name = "R1", value = r_value, package = package, P1 = VCC, P2 = led_anode)
Led(name = "D1", color = led_color, package = package, A = led_anode, K = CTRL)
"#;

const TEST_BOARD_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio")

Layout(name="TestBoard", path="build/TestBoard")

LedModule = Module("modules/LedModule.zen")
Resistor = Module("@stdlib/generics/Resistor.zen")
Capacitor = Module("@stdlib/generics/Capacitor.zen")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
led_ctrl = Gpio("LED_CTRL")

Capacitor(name = "C1", value = "100nF", package = "0402", P1 = vcc_3v3, P2 = gnd)
Capacitor(name = "C2", value = "10uF", package = "0805", P1 = vcc_3v3, P2 = gnd)

LedModule(name = "LED1", led_color = "green", VCC = vcc_3v3, GND = gnd, CTRL = led_ctrl)

Resistor(name = "R1", value = "10kOhm", package = "0603", P1 = vcc_3v3, P2 = led_ctrl)
"#;

const BOM_INTENT_BOARD_ZEN: &str = r#"
Resistor = Module("@stdlib/generics/Resistor.zen")

vcc = Net("VCC")
gnd = Net("GND")

Resistor(name = "GENERIC", value = "1kOhm", package = "0603", P1 = vcc, P2 = gnd)
Resistor(
    name = "AUTHORED",
    value = "10kOhm",
    package = "0603",
    mpn = "AUTHORED-MPN",
    manufacturer = "Authored Manufacturer",
    P1 = vcc,
    P2 = gnd,
)
"#;

const PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.4"
name = "test_workspace"
"#;

const BOARD_PCB_TOML: &str = r#"
[board]
name = "TestBoard"
path = "TestBoard.zen"
"#;

const TB0001_BOARD_PCB_TOML: &str = r#"
[board]
name = "TB0001"
path = "TB0001.zen"
"#;

const TB0002_BOARD_PCB_TOML: &str = r#"
[board]
name = "TB0002"
path = "TB0002.zen"
"#;

const BOARD_WITH_DESCRIPTION_PCB_TOML: &str = r#"
[board]
name = "DescBoard"
path = "DescBoard.zen"
description = "A test board with a description"
"#;

const SIMPLE_COMPONENT: &str = r#"
value = config(str, default = "10kOhm")

P1 = io(Net)
P2 = io(Net)

Component(
    name = "R",
    prefix = "R",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": P1, "P2": P2},
    type = "resistor",
    datasheet = File("datasheet.txt"),
    properties = {
        "value": value,
    }
)
"#;

const TEST_KICAD_MOD: &str = r#"(footprint "test"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)
"#;

const SIMPLE_BOARD_ZEN: &str = r#"
SimpleComponent = Module("modules/component.zen")
Layout(name="TestBoard", path="build/TestBoard")
vcc_3v3 = Net("VCC_3V3")
gnd = Net("GND")
SimpleComponent(name = "foo", P1 = vcc_3v3, P2 = gnd)
"#;

/// Helper to build args for source-only publish (excludes all manufacturing artifacts)
fn source_only_args(board_zen: &str) -> Vec<&str> {
    vec![
        "publish",
        board_zen,
        "--no-push",
        "--exclude",
        "drc",
        "--exclude",
        "gerbers",
        "--exclude",
        "ipc2581",
        "--exclude",
        "vrml",
    ]
}

/// Find the staging directory for a board (uses git hash as version suffix)
fn find_staging_dir(sb: &Sandbox, board_name: &str) -> String {
    let releases_dir = sb.root_path().join("src/.pcb/releases");
    let staging_dir_name = std::fs::read_dir(&releases_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(&format!("{}-", board_name)) && !name.ends_with(".zip")
        })
        .map(|e| e.file_name().to_string_lossy().to_string())
        .expect("Staging directory not found");
    format!(".pcb/releases/{}", staging_dir_name)
}

#[test]
fn test_release_check_does_not_publish_or_modify_authored_sources() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/TestBoard.zen", "# No components or layout\n")
        .init_git()
        .commit("Initial commit")
        .sync();
    let temporary = sb.root_path().join("check-tmp");
    std::fs::create_dir(&temporary).unwrap();
    sb.env("TMPDIR", temporary.to_string_lossy());
    let head = sb.cmd("git", ["rev-parse", "HEAD"]).read().unwrap();
    let short_head = sb
        .cmd("git", ["rev-parse", "--short", "HEAD"])
        .read()
        .unwrap();
    for (source, severity) in [
        ("# Unsaved to Git\n", None),
        ("fail(\"release-check diagnostic\")\n", Some("error")),
        (
            "warn(\"release-check diagnostic\", kind=\"sch.mismatch\")\n",
            Some("warning"),
        ),
    ] {
        sb.write("boards/TestBoard.zen", source);
        let before = sb.cmd("git", ["diff", "HEAD"]).read().unwrap();
        let output = sb
            .run("pcbc", ["publish", "boards/TestBoard.zen", "--check"])
            .stdout_capture()
            .stderr_capture()
            .unchecked()
            .run()
            .unwrap();
        assert_eq!(output.status.success(), severity.is_none());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["schemaVersion"], 2);
        assert_eq!(report["version"], short_head);
        let build = if severity.is_none() {
            "passed"
        } else {
            "failed"
        };
        assert_eq!(
            report["stages"],
            serde_json::json!({ "build": build, "layout": "skipped" })
        );
        if let Some(severity) = severity {
            let diagnostic = &report["diagnostics"][0];
            assert_eq!(diagnostic["severity"], severity);
            assert!(
                diagnostic["body"]
                    .as_str()
                    .unwrap()
                    .contains("release-check diagnostic")
            );
        }
        assert!(!sb.root_path().join("src/.pcb/releases").exists());
        assert_eq!(std::fs::read_dir(&temporary).unwrap().count(), 0);
        assert_eq!(sb.cmd("git", ["diff", "HEAD"]).read().unwrap(), before);
        assert_eq!(sb.cmd("git", ["rev-parse", "HEAD"]).read().unwrap(), head);
        assert!(sb.cmd("git", ["tag"]).read().unwrap().is_empty());
    }

    // A schematic warning must also stop normal publishing, not just check mode.
    let output = sb
        .run("pcbc", ["publish", "boards/TestBoard.zen"])
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not equivalent"));
    let stage = stderr
        .lines()
        .find(|line| line.contains("Generating netlist from staged sources"))
        .unwrap();
    assert!(
        !stage.contains('✓'),
        "blocked stage claimed success: {stage}"
    );
}

#[test]
fn test_release_check_operational_failure_is_a_diagnostic() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write(
            "boards/TestBoard.zen",
            "Layout(name=\"TestBoard\", path=\"layout\")\n",
        )
        .write("boards/layout/one.kicad_pro", "{}")
        .write("boards/layout/two.kicad_pro", "{}")
        .init_git()
        .commit("Initial commit")
        .sync();
    let temporary = sb.root_path().join("check-tmp");
    std::fs::create_dir(&temporary).unwrap();
    sb.env("TMPDIR", temporary.to_string_lossy());

    let output = sb
        .run("pcbc", ["publish", "boards/TestBoard.zen", "--check"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["stages"]["layout"], "skipped");
    let diagnostic = &report["diagnostics"][0];
    assert_eq!(diagnostic["kind"], "release.preflight");
    assert_eq!(diagnostic["severity"], "error");
    assert!(diagnostic["body"].as_str().unwrap().contains(".kicad_pro"));
    assert_eq!(std::fs::read_dir(&temporary).unwrap().count(), 0);
}

#[test]
fn test_release_check_drc_exclusion_skips_layout() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write(
            "boards/TestBoard.zen",
            "Layout(name=\"TestBoard\", path=\"layout\")\n",
        )
        .write("boards/layout/layout.kicad_pro", "{}")
        .write("boards/layout/layout.kicad_pcb", "(kicad_pcb)")
        .init_git()
        .commit("Initial commit")
        .sync();
    let before = sb.cmd("git", ["diff", "HEAD"]).read().unwrap();
    let output = sb
        .run(
            "pcbc",
            [
                "publish",
                "boards/TestBoard.zen",
                "--check",
                "--exclude",
                "drc",
            ],
        )
        .stdout_capture()
        .stderr_capture()
        .run()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["stages"],
        serde_json::json!({ "build": "passed", "layout": "skipped" })
    );
    assert_eq!(sb.cmd("git", ["diff", "HEAD"]).read().unwrap(), before);
}

#[test]
fn test_release_check_reports_bumped_version() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/TestBoard.zen", "# No components or layout\n")
        .init_git()
        .commit("Initial commit")
        .tag("boards/v0.3.0")
        .sync();
    sb.cmd("git", ["push", "-q", "origin", "main", "--tags"])
        .run()
        .unwrap();
    // A local-only tag is pruned by a real publish, so the check ignores it.
    sb.tag("boards/v9.0.0");

    let output = sb
        .run(
            "pcbc",
            ["publish", "boards/TestBoard.zen", "--check", "--bump=minor"],
        )
        .stdout_capture()
        .stderr_capture()
        .run()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["version"], "v0.4.0");
    assert_eq!(
        sb.cmd("git", ["tag"]).read().unwrap(),
        "boards/v0.3.0\nboards/v9.0.0"
    );
}

#[test]
fn test_publish_checks_bom_offers_only_on_bump() {
    let server = MockServer::start();
    let bom_match = server.mock(|when, then| {
        when.method(POST).path("/api/boms/match");
        then.status(200).json_body(serde_json::json!({
            "results": (["GENERIC.R", "AUTHORED.R"].map(|path| serde_json::json!({
                "designEntry": { "path": path },
                "match": "MATCH_EXACT", "ranked": {}
            }))),
            "offers": {}
        }));
    });
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .env("DIODE_API_URL", server.base_url())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/TestBoard.zen", BOM_INTENT_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .sync();

    // Local preflight stays offline.
    let output = sb
        .run("pcbc", ["publish", "boards/TestBoard.zen", "--check"])
        .stdout_capture()
        .stderr_capture()
        .run()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|finding| !finding["kind"].as_str().unwrap_or("").starts_with("bom."))
    );
    bom_match.assert_calls(0);

    // A versioned publish checks offers, and late BOM warnings honor -S.
    let warning = "No supplier offers found for";
    for (flags, expect_warning) in [(vec![], true), (vec!["-S", "bom"], false)] {
        let mut args = vec![
            "publish",
            "boards/TestBoard.zen",
            "--bump=patch",
            "--no-push",
        ];
        args.extend(&flags);
        let output = sb
            .run("pcbc", args)
            .stdout_capture()
            .stderr_capture()
            .run()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(stderr.contains(warning), expect_warning, "{stderr}");
    }
    bom_match.assert_calls(2);
}

#[test]
fn test_publish_board_source_only() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen", "**/netlist.json"])
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .init_git()
        .commit("Initial commit")
        .sync();

    // Build after hydrating dependency manifests (required for release)
    sb.run("pcbc", ["build", "boards/TestBoard.zen"])
        .run()
        .expect("build failed");

    // Run source-only publish (no layout needed)
    sb.run("pcbc", source_only_args("boards/TestBoard.zen"))
        .run()
        .expect("Failed to run pcb publish command");

    let staging_dir = find_staging_dir(&sb, "TestBoard");
    assert_snapshot!("publish_source_only", sb.snapshot_dir(&staging_dir));
}

#[test]
fn test_publish_board_with_version() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen", "**/netlist.json"])
        .write(".gitignore", ".pcb")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", TB0001_BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TB0001.zen", TEST_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .sync();

    // Build after hydrating dependency manifests (required for release)
    sb.run("pcbc", ["build", "boards/TB0001.zen"])
        .run()
        .expect("build failed");

    // Put the existing version only on the remote, as in a clone whose tags
    // have not been fetched yet.
    sb.commit("Hydrate manifests").tag("boards/v1.2.3");
    sb.cmd("git", ["push", "origin", "main", "boards/v1.2.3"])
        .run()
        .expect("push existing release");
    sb.cmd("git", ["tag", "--delete", "boards/v1.2.3"])
        .run()
        .expect("delete local release tag");

    // Publish fetches the remote tag before computing the bump (creates v1.3.0).
    let mut args = source_only_args("boards/TB0001.zen");
    args.push("--bump=minor");
    sb.run("pcbc", &args)
        .run()
        .expect("Failed to run pcb publish command");

    // Staging directory uses version: .pcb/releases/{board_name}-v{version}
    let staging_dir = ".pcb/releases/TB0001-v1.3.0";

    // Check metadata for git version
    let metadata_file = File::open(
        sb.root_path()
            .join("src")
            .join(staging_dir)
            .join("metadata.json"),
    )
    .unwrap();
    let metadata_json: Value = serde_json::from_reader(metadata_file).unwrap();
    let git_version = metadata_json["release"]["git_version"].as_str().unwrap();
    assert_eq!(git_version, "v1.3.0");

    assert_snapshot!("publish_with_version", sb.snapshot_dir(staging_dir));
}

#[test]
fn test_publish_board_with_version_preserves_local_only_tags() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen", "**/netlist.json"])
        .write(".gitignore", ".pcb")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", TB0001_BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TB0001.zen", TEST_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .sync();

    sb.run("pcbc", ["build", "boards/TB0001.zen"])
        .run()
        .expect("build failed");
    sb.commit("Hydrate manifests").tag("boards/v1.2.3");
    sb.cmd("git", ["switch", "--detach"])
        .run()
        .expect("detach HEAD");

    let mut args = source_only_args("boards/TB0001.zen");
    args.push("--bump=patch");
    sb.run("pcbc", &args)
        .run()
        .expect("Failed to run pcb publish command");

    let tags = sb
        .cmd("git", ["tag", "--list", "boards/v*"])
        .read()
        .expect("list release tags");
    assert!(tags.lines().any(|tag| tag == "boards/v1.2.3"));
    assert!(tags.lines().any(|tag| tag == "boards/v1.2.4"));
}

#[test]
fn test_publish_preserves_authored_bom_intent() {
    let server = MockServer::start();
    let _bom_match = server.mock(|when, then| {
        when.method(POST).path("/api/boms/match");
        then.status(200).json_body(serde_json::json!({
            "results": [
                {
                    "designEntry": { "path": "GENERIC.R" },
                    "match": "MATCH_COMPATIBLE",
                    "ranked": { "US": [{ "offerId": "selection", "stockClass": "PLENTY" }] }
                },
                {
                    "designEntry": { "path": "AUTHORED.R" },
                    "match": "MATCH_COMPATIBLE",
                    "ranked": { "US": [{ "offerId": "selection", "stockClass": "PLENTY" }] }
                }
            ],
            "offers": {
                "selection": {
                    "id": "selection",
                    "geography": "US",
                    "sellerName": "Selected Seller",
                    "mpn": "SELECTED-MPN",
                    "manufacturer": "Selected Manufacturer",
                    "marketStock": 100
                }
            }
        }));
    });

    let mut sb = Sandbox::new();
    sb.cwd("src")
        .env("DIODE_API_URL", server.base_url())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/TestBoard.zen", BOM_INTENT_BOARD_ZEN)
        .init_git()
        .commit("Initial commit")
        .sync();

    sb.run("pcbc", ["publish", "boards/TestBoard.zen", "--no-push"])
        .run()
        .expect("Failed to run pcb publish command");

    let release_dir = sb
        .root_path()
        .join("src")
        .join(find_staging_dir(&sb, "TestBoard"));
    let netlist: pcb_sch::Schematic =
        serde_json::from_reader(File::open(release_dir.join("netlist.json")).unwrap()).unwrap();
    let component = |path| {
        netlist
            .instances
            .iter()
            .find_map(|(instance_ref, instance)| {
                (instance_ref.instance_path.join(".") == path).then_some(instance)
            })
            .unwrap()
    };
    assert_eq!(
        (
            component("GENERIC.R").mpn().as_deref(),
            component("GENERIC.R").manufacturer().as_deref()
        ),
        (None, None)
    );
    assert_eq!(
        (
            component("AUTHORED.R").mpn().as_deref(),
            component("AUTHORED.R").manufacturer().as_deref()
        ),
        (Some("AUTHORED-MPN"), Some("Authored Manufacturer"))
    );
}

#[test]
fn test_publish_board_full() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen", "**/netlist.json"])
        .ignore_globs([
            "layout/*",
            "3d/*",
            "manufacturing/*.xml",
            "manufacturing/*.html",
            "**/vendor/**",
            "**/build/**",
            "**/drc.json",
        ])
        .init_git()
        .commit("Initial commit")
        .sync();

    // Generate layout files first (full releases require layout)
    sb.run("pcbc", ["layout", "--no-open", "boards/TestBoard.zen"])
        .run()
        .expect("layout generation failed");

    // Run full publish (with all artifacts, suppress test board DRC issues)
    let output = sb
        .run(
            "pcbc",
            [
                "publish",
                "boards/TestBoard.zen",
                "-S",
                "layout",
                "--no-push",
            ],
        )
        .stderr_capture()
        .stdout_capture()
        .run()
        .expect("Failed to run pcb publish command");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let build_warning = stderr
        .find("io() 'GND' in module 'LED1' is not connected to any ports")
        .expect("staged build warning should be reported");
    let preflight_finished = stderr
        .find("Reviewing release preflight")
        .expect("preflight review should complete");
    assert!(build_warning < preflight_finished);
    assert!(!stderr.contains("Checking BOM offers"));

    let staging_dir = find_staging_dir(&sb, "TestBoard");
    let manufacturing = sb.default_cwd().join(&staging_dir).join("manufacturing");
    pcb_ipc2581_tools::ipc2581::Ipc2581::parse_file(manufacturing.join("ipc2581.xml")).unwrap();
    assert!(!manufacturing.join("ipc2581.html").exists());
    assert!(!manufacturing.join("cpl.csv").exists());
    let mut gerbers =
        zip::ZipArchive::new(File::open(manufacturing.join("gerbers.zip")).unwrap()).unwrap();
    let names = gerbers
        .file_names()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let job_name = names.iter().find(|name| name.ends_with(".gbrjob")).unwrap();
    let job: Value = serde_json::from_reader(gerbers.by_name(job_name).unwrap()).unwrap();
    let inventory = job["FilesAttributes"].as_array().unwrap();
    assert_eq!(
        inventory
            .iter()
            .map(|entry| entry["Path"].as_str().unwrap())
            .collect::<BTreeSet<_>>(),
        names
            .iter()
            .filter(|name| *name != job_name)
            .map(String::as_str)
            .collect()
    );
    for function in [
        "Copper,L1,Top",
        "Copper,L2,Bot",
        "AssemblyDrawing,Top",
        "AssemblyDrawing,Bot",
    ] {
        let entry = inventory
            .iter()
            .find(|entry| entry["FileFunction"] == function)
            .unwrap();
        let mut file = gerbers.by_name(entry["Path"].as_str().unwrap()).unwrap();
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut file, &mut contents).unwrap();
        assert!(contents.contains(&format!("%TF.FileFunction,{function}*%")));
        assert!(contents.contains("%TF.Part,Single*%"));
    }
    assert_snapshot!("publish_full", sb.snapshot_dir(&staging_dir));

    // Excluding the published IPC must not remove the Gerbers' input. Check
    // the final archive too, including repeated publishes into the same staging.
    for excluded in [vec!["ipc2581"], vec!["gerbers"], vec!["ipc2581", "gerbers"]] {
        let mut args = vec![
            "publish",
            "boards/TestBoard.zen",
            "-S",
            "layout",
            "--no-push",
            "--exclude",
            "vrml",
        ];
        for artifact in &excluded {
            args.extend(["--exclude", artifact]);
        }
        sb.run("pcbc", args)
            .run()
            .expect("publish with exclusions failed");
        let archive = zip::ZipArchive::new(
            File::open(sb.default_cwd().join(format!("{staging_dir}.zip"))).unwrap(),
        )
        .unwrap();
        let files = archive.file_names().collect::<BTreeSet<_>>();
        assert!(!files.contains("manufacturing/cpl.csv"));
        assert_eq!(
            files.contains("manufacturing/ipc2581.xml"),
            !excluded.contains(&"ipc2581")
        );
        assert_eq!(
            files.contains("manufacturing/gerbers.zip"),
            !excluded.contains(&"gerbers")
        );
        assert_eq!(
            files
                .iter()
                .filter(|name| name.starts_with("manufacturing/"))
                .count(),
            2 - excluded.len()
        );
    }
}

#[test]
fn test_publish_board_with_file() {
    let mut sb = Sandbox::new();
    const DATASHEET_CONTENTS: &str = "Simple component datasheet.";
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", TB0002_BOARD_PCB_TOML)
        .write("boards/modules/component.zen", SIMPLE_COMPONENT)
        .write("boards/modules/test.kicad_mod", TEST_KICAD_MOD)
        .write("boards/modules/datasheet.txt", DATASHEET_CONTENTS)
        .write("boards/modules/reference.pdf", "git-ignored")
        .write("boards/.gitignore", "*.pdf\n")
        .write("boards/TB0002.zen", SIMPLE_BOARD_ZEN)
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .init_git()
        .commit("Initial commit")
        .sync();

    // Build after hydrating dependency manifests (required for release)
    sb.run("pcbc", ["build", "boards/TB0002.zen"])
        .run()
        .expect("build failed");

    // Run source-only publish
    sb.run("pcbc", source_only_args("boards/TB0002.zen"))
        .run()
        .expect("Failed to run pcb publish command");

    let staging_dir = find_staging_dir(&sb, "TB0002");

    let datasheet_path = sb
        .root_path()
        .join("src")
        .join(&staging_dir)
        .join("src/boards/modules/datasheet.txt");
    let datasheet_contents = std::fs::read_to_string(&datasheet_path).unwrap();
    assert_eq!(datasheet_contents, DATASHEET_CONTENTS);
    assert!(!datasheet_path.with_file_name("reference.pdf").exists());

    assert_snapshot!("publish_with_file", sb.snapshot_dir(&staging_dir));
}

#[test]
fn test_publish_board_with_description() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_WITH_DESCRIPTION_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/DescBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen"])
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .init_git()
        .commit("Initial commit")
        .sync();

    // Build after hydrating dependency manifests (required for release)
    sb.run("pcbc", ["build", "boards/DescBoard.zen"])
        .run()
        .expect("build failed");

    // Run source-only publish
    sb.run("pcbc", source_only_args("boards/DescBoard.zen"))
        .run()
        .expect("Failed to run pcb publish command");

    let staging_dir = find_staging_dir(&sb, "DescBoard");
    assert_snapshot!("publish_with_description", sb.snapshot_dir(&staging_dir));
}

#[test]
fn test_publish_board_vendors_remote_deps_for_validation() {
    let mut sb = Sandbox::new();

    // A remote component that the published board depends on.
    sb.git_fixture("https://github.com/mycompany/components.git")
        .write("Resistor/pcb.toml", "[dependencies]\n")
        .write("Resistor/Resistor.zen", SIMPLE_COMPONENT)
        .write("Resistor/test.kicad_mod", TEST_KICAD_MOD)
        .write("Resistor/datasheet.txt", "Simple component datasheet.")
        .commit("Add remote component")
        .tag("Resistor/v1.0.0", false)
        .push_mirror();

    sb.cwd("src")
        .write("pcb.toml", "[workspace]\npcb-version = \"0.4\"\n")
        .write(
            "boards/pcb.toml",
            r#"[board]
name = "TestBoard"

[dependencies]
"github.com/mycompany/components/Resistor" = "1.0.0"
"#,
        )
        .write(
            "boards/TestBoard.zen",
            r#"
Resistor = Module("github.com/mycompany/components/Resistor/Resistor.zen")
Layout(name="TestBoard", path="build/TestBoard")
Resistor(name = "foo", P1 = Net("VCC_3V3"), P2 = Net("GND"))
"#,
        )
        .init_git()
        .commit("Initial commit")
        .sync();

    sb.run("pcbc", source_only_args("boards/TestBoard.zen"))
        .run()
        .expect("publish should succeed");

    // The hydrated remote dependency is vendored into the source bundle so it
    // validates offline without network or a populated package cache.
    let staging_dir = find_staging_dir(&sb, "TestBoard");
    let vendored_remote = sb
        .root_path()
        .join("src")
        .join(&staging_dir)
        .join("src/vendor/github.com/mycompany/components/Resistor/1.0.0/pcb.toml");

    assert!(
        vendored_remote.exists(),
        "publish should stage the board's remote dependency for offline validation"
    );
}

/// Test that `pcb publish` works when run from the board directory with a relative .zen path.
/// Regression test: previously, `pcb publish DM0002.zen` from `boards/DM0002/` would fail
/// because workspace discovery broke on the empty parent path.
#[test]
fn test_publish_board_from_board_dir() {
    let mut sb = Sandbox::new();
    sb.cwd("src")
        .write("pcb.toml", PCB_TOML)
        .write("boards/pcb.toml", BOARD_PCB_TOML)
        .write("boards/modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .hash_globs(["*.kicad_mod", "**/diodeinc/stdlib/*.zen", "**/netlist.json"])
        .ignore_globs(["layout/*", "**/vendor/**", "**/build/**"])
        .init_git()
        .commit("Initial commit")
        .sync();

    // Build after hydrating dependency manifests
    sb.run("pcbc", ["build", "boards/TestBoard.zen"])
        .run()
        .expect("build failed");

    // Run publish from the board directory with a relative path
    sb.cwd("src/boards")
        .run("pcbc", source_only_args("TestBoard.zen"))
        .run()
        .expect("publish from board dir with relative path should work");
}
