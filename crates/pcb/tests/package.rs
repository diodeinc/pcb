#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;
use serde_json::Value;
use std::fs::File;
use tar::Archive;
use zstd::stream::read::Decoder;

const WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
members = ["modules/*"]
"#;

const CHILD_PCB_TOML: &str = "";

const CHILD_ZEN: &str = r#"
P1 = io("P1", Net)
P2 = io("P2", Net)
"#;

const PARENT_PCB_TOML: &str = r#"
[dependencies]
"modules/Child" = "0.1.0"
"#;

const PARENT_ZEN: &str = r#"
Child = Module("../Child/Child.zen")

P1 = io("P1", Net)
P2 = io("P2", Net)

Child(name = "U1", P1 = P1, P2 = P2)
"#;

fn write_module_workspace(sb: &mut Sandbox) {
    sb.cwd("src")
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("modules/Child/pcb.toml", CHILD_PCB_TOML)
        .write("modules/Child/Child.zen", CHILD_ZEN)
        .write("modules/Parent/pcb.toml", PARENT_PCB_TOML)
        .write("modules/Parent/Parent.zen", PARENT_ZEN)
        .hash_globs(["**/diodeinc/stdlib/*.zen"])
        .init_git()
        .commit("Initial commit");
}

#[test]
fn test_package_module_bundle() {
    let mut sb = Sandbox::new();
    write_module_workspace(&mut sb);

    let result = sb
        .run(
            "pcb",
            ["package", "-o", "out/package.tar.zst", "modules/Parent"],
        )
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("package command failed");
    assert!(
        result.status.success(),
        "expected success:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr),
    );

    let archive_path = sb.root_path().join("src/out/package.tar.zst");
    let decoder = Decoder::new(File::open(&archive_path).unwrap()).unwrap();
    let mut archive = Archive::new(decoder);
    let mut entries = archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    entries.sort();

    assert!(entries.iter().any(|path| path == "metadata.json"));
    assert!(
        entries
            .iter()
            .any(|path| path == "src/modules/Parent/Parent.zen")
    );
    assert!(
        entries
            .iter()
            .any(|path| path == "src/modules/Child/Child.zen")
    );
}

#[test]
fn test_package_module_bundle_json_output() {
    let mut sb = Sandbox::new();
    write_module_workspace(&mut sb);

    let result = sb
        .run(
            "pcb",
            [
                "package",
                "-f",
                "json",
                "-o",
                "out/package.tar.zst",
                "modules/Parent",
            ],
        )
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("package command failed");

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success(),
        "expected success:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stderr.trim().is_empty(), "expected empty stderr:\n{stderr}");

    let json: Value = serde_json::from_slice(&result.stdout).expect("stdout should be valid json");
    assert_eq!(json["mode"], "bundle");
    assert_eq!(json["package_url"], "modules/Parent");
    assert_eq!(json["bundle_stem"], "modules--Parent");
    assert_eq!(json["target_name"], "Parent");
    assert_eq!(json["output_path"], "out/package.tar.zst");
    assert!(json["output_size_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn test_package_hash_only_json_output() {
    let mut sb = Sandbox::new();
    write_module_workspace(&mut sb);

    let result = sb
        .run(
            "pcb",
            ["package", "--hash-only", "-f", "json", "modules/Parent"],
        )
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("package command failed");

    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.success(),
        "expected success:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(stderr.trim().is_empty(), "expected empty stderr:\n{stderr}");

    let json: Value = serde_json::from_slice(&result.stdout).expect("stdout should be valid json");
    assert_eq!(json["mode"], "hash_only");
    assert_eq!(json["package_url"], "modules/Parent");
    assert_eq!(
        json["content_hash"],
        "h1:+9bG3s5EuNonXq+fkqSepe1+T09uRSaCYTQISn7IMaQ="
    );
    assert_eq!(
        json["manifest_hash"],
        "h1:WrxlP6P2pTtcWH9rx38OfDi0myKaG6IF8Bdwxj7YO+A="
    );
    assert!(json["output_path"].is_null());
    assert!(
        !sb.root_path().join("src/.pcb/packages").exists(),
        "hash-only mode should not stage a bundle"
    );
}
