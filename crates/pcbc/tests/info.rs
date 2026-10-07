#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.4"
"#;

const WORKSPACE_PCB_TOML_WITH_PREFERRED: &str = r#"
[workspace]
pcb-version = "0.4"
preferred = ["boards/test-board"]
"#;

const TEST_BOARD_PCB_TOML: &str = r#"
[board]
name = "TestBoard"
path = "test_board.zen"
description = "Main test board for validation"
"#;

const MAIN_BOARD_PCB_TOML: &str = r#"
[board]
name = "MainBoard"
path = "main_board.zen"
"#;

const BROKEN_BOARD_PCB_TOML: &str = r#"
[board]
name = "BrokenBoard"
path = "broken.zen"
"#;

const CUSTOM_BOARD_PCB_TOML: &str = r#"
[board]
name = "CustomBoard"
path = "custom.zen"
description = "Special custom board with unique features"
"#;

const TEST_BOARD_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
test_signal = Gpio("TEST_SIGNAL")
internal_net = Net("INTERNAL")
"#;

#[test]
fn test_pcb_info_empty_workspace() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .snapshot_run("pcbc", ["info"]);
    assert_snapshot!("empty_workspace", output);
}

#[test]
fn test_pcb_info_exits_cleanly_when_output_pipe_closes() {
    let mut sandbox = Sandbox::new();
    let board_manifest = format!(
        "[board]\nname = \"LargeBoard\"\npath = \"board.zen\"\ndescription = \"{}\"\n",
        "x".repeat(2 * 1024 * 1024)
    );
    sandbox
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/large/pcb.toml", board_manifest);

    let command = format!(
        "\"{}\" info | head -n 1 >/dev/null",
        env!("CARGO_BIN_EXE_pcbc")
    );
    let output = sandbox
        .cmd("bash", ["-o", "pipefail", "-c", &command])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("run pcb info through head");

    assert!(
        output.status.success(),
        "pcb info failed after its output pipe closed: {output:?}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("panicked"),
        "pcb info panicked after its output pipe closed: {output:?}"
    );
}

#[test]
fn test_pcb_info_multiple_boards() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/test-board/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/test-board/test_board.zen", TEST_BOARD_ZEN)
        .write("boards/main-board/pcb.toml", MAIN_BOARD_PCB_TOML)
        .write("boards/main-board/main_board.zen", TEST_BOARD_ZEN)
        .write("boards/broken-board/pcb.toml", BROKEN_BOARD_PCB_TOML)
        .write("special/custom-board/pcb.toml", CUSTOM_BOARD_PCB_TOML)
        .write("special/custom-board/custom.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcbc", ["info"]);
    assert_snapshot!("multiple_boards", output);
}

#[test]
fn test_pcb_info_json_includes_preferred() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML_WITH_PREFERRED)
        .write("boards/test-board/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/test-board/test_board.zen", TEST_BOARD_ZEN)
        .write("boards/main-board/pcb.toml", MAIN_BOARD_PCB_TOML)
        .write("boards/main-board/main_board.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcbc", ["info", "-f", "json"]);
    assert_snapshot!("json_format_with_preferred", output);
}

#[test]
fn test_pcb_info_json_includes_external_dependency_closure() {
    let mut sandbox = Sandbox::new();

    sandbox
        .git_fixture("https://github.com/vendor/components.git")
        .write(
            "Thing/pcb.toml",
            r#"
[dependencies]
"github.com/vendor/components/Leaf" = "1.0.0"
"#,
        )
        .write("Thing/Thing.zen", "P1 = io(Net)\n")
        .write("Leaf/pcb.toml", "")
        .write("Leaf/Leaf.zen", "P1 = io(Net)\n")
        .commit("Add component packages")
        .tag("Thing/v1.0.0", true)
        .tag("Leaf/v1.0.0", true)
        .push_mirror();

    sandbox.write(
        "pcb.toml",
        r#"
[workspace]
pcb-version = "0.4"

[dependencies]
"github.com/vendor/components/Thing" = "1.0.0"

[dependencies.indirect]
"github.com/vendor/components/Thing@1" = "1.0.0"
"github.com/vendor/components/Leaf@1" = "1.0.0"
"#,
    );

    let json_output = sandbox.snapshot_run("pcbc", ["info", "-f", "json"]);
    assert_snapshot!("json_with_external_dependencies", json_output);

    let human_output = sandbox.snapshot_run("pcbc", ["info"]);
    assert_snapshot!("human_with_external_dependencies", human_output);

    sandbox
        .run("pcbc", ["vendor", "--all"])
        .stdout_capture()
        .stderr_capture()
        .run()
        .expect("vendor fixture dependencies");
    let vendored = inspect(&sandbox);
    assert!(vendored.get("errors").is_none(), "{vendored}");
    let deps = vendored["external_dependencies"].as_object().unwrap();
    assert_eq!(deps.len(), 2);
    assert!(deps.values().all(|pkg| pkg["source"] == "vendor"));
}

#[test]
fn test_pcb_info_json_includes_sum_free_external_dependency_closure() {
    let mut sandbox = Sandbox::new();

    sandbox
        .git_fixture("https://github.com/vendor/components.git")
        .write(
            "Thing/pcb.toml",
            r#"
[dependencies]
"github.com/vendor/components/Leaf" = "1.0.0"
"#,
        )
        .write("Thing/Thing.zen", "P1 = io(Net)\n")
        .write("Leaf/pcb.toml", "")
        .write("Leaf/Leaf.zen", "P1 = io(Net)\n")
        .commit("Add component packages")
        .tag("Thing/v1.0.0", true)
        .tag("Leaf/v1.0.0", true)
        .push_mirror();

    let output = sandbox
        .write(
            "pcb.toml",
            r#"
[workspace]
pcb-version = "0.4"
"#,
        )
        .write(
            "boards/Board/pcb.toml",
            r#"
[board]
name = "Board"
path = "Board.zen"

[dependencies]
"github.com/vendor/components/Thing" = "1.0.0"

[dependencies.indirect]
"github.com/vendor/components/Thing@1" = "1.0.0"
"github.com/vendor/components/Leaf@1" = "1.0.0"
"#,
        )
        .write("boards/Board/Board.zen", "p1 = Net(\"P1\")\n")
        .run("pcbc", ["info", "-f", "json", "boards/Board"])
        .stdout_capture()
        .stderr_capture()
        .run()
        .expect("run pcb info");

    assert!(output.status.success(), "pcb info failed: {output:?}");

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse pcb info JSON");
    let deps = json["external_dependencies"]
        .as_object()
        .expect("external_dependencies is an object");

    assert!(deps.contains_key("github.com/vendor/components/Thing@1.0.0"));
    assert!(deps.contains_key("github.com/vendor/components/Leaf@1.0.0"));
}

#[test]
fn test_pcb_info_json_includes_published_at() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/test-board/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/test-board/test_board.zen", TEST_BOARD_ZEN)
        .init_git()
        .commit("initial publishable board");

    sandbox.env("GIT_COMMITTER_DATE", "2024-06-01T12:00:00+00:00");
    sandbox
        .cmd(
            "git",
            [
                "tag",
                "-a",
                "boards/test-board/v0.1.0",
                "-m",
                "Release 0.1.0",
            ],
        )
        .run()
        .expect("create first annotated tag");

    sandbox.env("GIT_COMMITTER_DATE", "2024-01-02T03:04:05+00:00");
    sandbox
        .cmd(
            "git",
            [
                "tag",
                "-a",
                "boards/test-board/v0.2.0",
                "-m",
                "Release 0.2.0",
            ],
        )
        .run()
        .expect("create second annotated tag");

    let expected_published_at = "2024-01-02T03:04:05Z";

    let output = sandbox.snapshot_run("pcbc", ["info", "-f", "json"]);
    let json = output
        .split("--- STDOUT ---\n")
        .nth(1)
        .and_then(|stdout| stdout.split("\n--- STDERR ---").next())
        .expect("extract JSON output");
    let parsed: serde_json::Value = serde_json::from_str(json).expect("parse JSON output");
    let pkg = &parsed["packages"]["boards/test-board"];

    assert_eq!(pkg["version"], "0.2.0");
    assert_eq!(pkg["published_at"], expected_published_at);

    let normalized = output.replace(expected_published_at, "<PUBLISHED_AT>");
    assert_snapshot!("json_format_with_published_at", normalized);
}

#[test]
fn test_pcb_info_json_versions_ignore_unmerged_tags_and_flag_changes() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/clean/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/clean/test_board.zen", TEST_BOARD_ZEN)
        .write("boards/changed/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("boards/changed/test_board.zen", TEST_BOARD_ZEN)
        .init_git()
        .commit("initial boards")
        .tag("boards/clean/v0.1.0");
    let git = |sandbox: &Sandbox, args: &[&str]| {
        sandbox.cmd("git", args).run().expect("run git");
    };
    git(
        &sandbox,
        &["tag", "-a", "boards/changed/v0.1.0", "-m", "Release 0.1.0"],
    );

    // A newer version published from a branch that never merged.
    git(&sandbox, &["checkout", "-b", "side"]);
    sandbox
        .write("boards/clean/notes.txt", "side")
        .commit("side work")
        .tag("boards/clean/v0.2.0");
    git(&sandbox, &["checkout", "main"]);

    sandbox
        .write("boards/changed/notes.txt", "after publish")
        .commit("change a published board");

    let output = sandbox.snapshot_run("pcbc", ["info", "-f", "json"]);
    let json = output
        .split("--- STDOUT ---\n")
        .nth(1)
        .and_then(|stdout| stdout.split("\n--- STDERR ---").next())
        .expect("extract JSON output");
    let parsed: serde_json::Value = serde_json::from_str(json).expect("parse JSON output");
    let packages = &parsed["packages"];

    assert_eq!(packages["boards/clean"]["version"], "0.1.0");
    assert!(packages["boards/clean"].get("dirty").is_none());
    assert_eq!(packages["boards/changed"]["version"], "0.1.0");
    assert_eq!(packages["boards/changed"]["dirty"], true);
}

#[test]
fn test_pcb_info_with_path() {
    let output = Sandbox::new()
        .write("subdir/pcb.toml", WORKSPACE_PCB_TOML)
        .write("subdir/boards/test-board/pcb.toml", TEST_BOARD_PCB_TOML)
        .write("subdir/boards/test-board/test_board.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcbc", ["info", "subdir"]);
    assert_snapshot!("with_path", output);
}

// Board config without explicit path - should discover the single .zen file
const BOARD_NO_PATH_PCB_TOML: &str = r#"
[board]
name = "DiscoveredBoard"
description = "Board with auto-discovered zen file"
"#;

#[test]
fn test_pcb_info_zen_discovery_json() {
    // Test JSON output includes discovered path
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/discovered/pcb.toml", BOARD_NO_PATH_PCB_TOML)
        .write("boards/discovered/discovered.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcbc", ["info", "-f", "json"]);
    assert_snapshot!("zen_discovery_json", output);
}

// Board with multiple .zen files - discovery should fail
const BOARD_MULTI_ZEN_PCB_TOML: &str = r#"
[board]
name = "AmbiguousBoard"
description = "Board with multiple zen files"
"#;

#[test]
fn test_pcb_info_multiple_zen_files() {
    // When multiple .zen files exist, discovery should fail gracefully
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/ambiguous/pcb.toml", BOARD_MULTI_ZEN_PCB_TOML)
        .write("boards/ambiguous/board1.zen", TEST_BOARD_ZEN)
        .write("boards/ambiguous/board2.zen", TEST_BOARD_ZEN)
        .snapshot_run("pcbc", ["info"]);
    assert_snapshot!("multiple_zen_files", output);
}

fn inspect(sandbox: &Sandbox) -> serde_json::Value {
    let output = sandbox
        .cmd(env!("CARGO_BIN_EXE_pcbc"), ["info", "-f", "json"])
        .stdout_capture()
        .stderr_capture()
        .run()
        .expect("inspection succeeds even when the workspace is broken");
    serde_json::from_slice(&output.stdout).expect("inspection emits valid JSON")
}

#[test]
fn test_pcb_info_invalid_root_does_not_invent_workspace_members() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", "[workspace\n")
        .write("boards/good/pcb.toml", TEST_BOARD_PCB_TOML);

    let manifest = sandbox.root_path().join("pcb.toml").canonicalize().unwrap();
    sandbox.cwd("boards/good");
    let output = sandbox
        .run("pcbc", ["info", "-f", "json"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(info.get("root").is_none());
    assert!(info.get("config").is_none());
    assert_eq!(info["packages"], serde_json::json!({}));
    let errors = info["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["path"], manifest.to_str().unwrap());
    assert!(errors[0]["error"].as_str().unwrap().contains("pcb.toml"));
}

#[test]
fn test_pcb_info_partial_output() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write(
            "pcb.toml",
            format!(
                "{WORKSPACE_PCB_TOML}\n[patch]\n\"example.com/broken\" = {{ path = \"fork/broken\" }}\n"
            ),
        )
        .write("boards/good/pcb.toml", format!(
            "{TEST_BOARD_PCB_TOML}\n[dependencies]\n\"example.com/broken\" = \"1.0.0\"\n[dependencies.indirect]\n\"example.com/broken@1\" = \"1.0.0\"\n"
        ))
        .write("boards/good/test_board.zen", TEST_BOARD_ZEN)
        .write("boards/bad/pcb.toml", "[board\n")
        .write("fork/broken/pcb.toml", "[dependencies\n")
        .write("vendor/example.com/broken/1.0.0/pcb.toml", "")
        .write("vendor/example.com/stale/1.0.0/pcb.toml", "# keep\n")
        .write("boards/good/broken.kicad_sym", [0xff]);

    sandbox.cwd("boards/bad");
    let info = inspect(&sandbox);
    sandbox.cwd(".");
    assert_eq!(inspect(&sandbox), info);
    assert_eq!(
        info["root"],
        sandbox
            .root_path()
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(info.get("external_dependencies").is_none());
    assert_eq!(info["packages"].as_object().unwrap().len(), 1);
    assert_eq!(
        info["packages"]["boards/good"]["entrypoints"],
        serde_json::json!(["test_board.zen"])
    );
    let errors = info["errors"].as_array().unwrap();
    for suffix in [
        "boards/bad/pcb.toml",
        "fork/broken/pcb.toml",
        "broken.kicad_sym",
    ] {
        assert!(
            errors
                .iter()
                .any(|error| error["path"].as_str().unwrap().ends_with(suffix))
        );
    }

    sandbox.write("boards/bad/pcb.toml", "");
    let output = sandbox
        .run("pcbc", ["build", "boards/good/test_board.zen"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid pcb.toml"));

    let output = sandbox
        .run("pcbc", ["vendor", "--all"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid pcb.toml"));
    assert_eq!(
        std::fs::read_to_string(
            sandbox
                .root_path()
                .join("vendor/example.com/stale/1.0.0/pcb.toml")
        )
        .unwrap(),
        "# keep\n"
    );
}

#[test]
fn test_pcb_info_resolution_failure_preserves_local_files() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .write(
            "boards/main/pcb.toml",
            format!(
                "{TEST_BOARD_PCB_TOML}\n[dependencies]\n\"example.com/missing/pkg\" = \"1.0.0\"\n"
            ),
        )
        .write("boards/main/test_board.zen", TEST_BOARD_ZEN);

    let info = inspect(&sandbox);
    assert!(!info["errors"].as_array().unwrap().is_empty());
    assert_eq!(
        info["packages"]["boards/main"]["entrypoints"],
        serde_json::json!(["test_board.zen"])
    );
    assert!(info.get("external_dependencies").is_none());
}

#[test]
fn test_pcb_info_source_patch_takes_precedence_over_vendor() {
    let mut sandbox = Sandbox::new();
    let patch_path = sandbox
        .root_path()
        .canonicalize()
        .unwrap()
        .join("vendor/parts/1.0.0");
    sandbox
        .write(
            "pcb.toml",
            format!(
                "{WORKSPACE_PCB_TOML}\n[patch]\n\"example.com/parts\" = {{ path = {patch_path:?} }}\n"
            ),
        )
        .write("vendor/parts/1.0.0/pcb.toml", format!("[board]\nname = \"Patched\"\npath = {:?}\n", patch_path.join("part.zen")))
        .write("vendor/parts/1.0.0/part.zen", "");

    let info = inspect(&sandbox);
    assert!(info.get("errors").is_none(), "{info}");
    let patch = &info["packages"]["example.com/parts"];
    assert_eq!(patch["source"], "patch");
    assert_eq!(patch["board_entrypoint"], "vendor/parts/1.0.0/part.zen");
}

fn inventory(sandbox: &Sandbox) -> serde_json::Value {
    let output = sandbox
        .cmd(
            env!("CARGO_BIN_EXE_pcbc"),
            ["info", "--format", "json", "--inventory"],
        )
        .stdout_capture()
        .stderr_capture()
        .run()
        .expect("inventory succeeds");
    serde_json::from_slice(&output.stdout).expect("inventory emits one JSON document")
}

#[test]
fn test_inventory_pins_selected_release_not_checkout_contents() {
    use serde_json::json;
    let mut sandbox = Sandbox::new();
    sandbox
        .write(
            "pcb.toml",
            "[workspace]\nrepository = \"example.com/registry\"\npreferred = [\"parts/a\"]\n",
        )
        .write(
            "parts/a/pcb.toml",
            "[dependencies]\n\"example.invalid/missing\" = \"1.0.0\"\n",
        )
        .write("parts/a/broken.kicad_sym", "not a symbol library")
        .write("parts/legacy/pcb.toml", "")
        .write("parts/lightweight/pcb.toml", "")
        .write("parts/unpublished/pcb.toml", "")
        .init_git()
        .commit("published snapshot")
        .tag("parts/a/v0.9.0")
        .tag("parts/lightweight/v0.4.0");
    let release_commit = sandbox.cmd("git", ["rev-parse", "HEAD"]).read().unwrap();
    let content_hash = "h1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    // No v prefix in the selected tag. Ignore manifest and other-package hashes.
    sandbox.cmd("git", ["tag", "-a", "parts/a/0.10.0", "-m", &format!(
        "example.com/registry/parts/a v0.10.0/pcb.toml h1:manifest\nexample.com/registry/parts/other v0.10.0 h1:other\nexample.com/registry/parts/a v0.10.0 {content_hash}"
    )]).run().unwrap();
    sandbox
        .cmd(
            "git",
            ["tag", "-a", "parts/legacy/v0.2.0", "-m", "Legacy release"],
        )
        .run()
        .unwrap();
    sandbox
        .cmd("git", ["checkout", "-b", "side"])
        .run()
        .unwrap();
    sandbox
        .write("side.txt", "side branch")
        .commit("unmerged")
        .tag("parts/a/v9.0.0");
    sandbox.cmd("git", ["checkout", "main"]).run().unwrap();
    sandbox
        .write("parts/a/new.zen", "unpublished bytes")
        .commit("unpublished package edits");
    sandbox.write("parts/a/dirty.zen", "dirty bytes");
    let expected = json!({"packages": [
        {"path": "parts/a", "preferred": true, "release": {"version": "0.10.0", "commit": release_commit, "contentHash": content_hash}},
        {"path": "parts/legacy", "preferred": false, "release": {"version": "0.2.0", "commit": release_commit, "contentHash": null}},
        {"path": "parts/lightweight", "preferred": false, "release": {"version": "0.4.0", "commit": release_commit, "contentHash": null}},
        {"path": "parts/unpublished", "preferred": false, "release": null}
    ]});
    assert_eq!(inventory(&sandbox), expected);
    assert!(
        !sandbox.root_path().join(".pcb").exists(),
        "inventory must not materialize dependencies"
    );
    assert!(!sandbox.root_path().join("pcb.sum").exists());
}

#[test]
fn test_inventory_physical_membership_and_repository_relative_paths() {
    use serde_json::json;
    let mut sandbox = Sandbox::new();
    sandbox
        .write("registry/pcb.toml", "[workspace]\nrepository = \"example.com/repo\"\npath = \"registry\"\nexclude = [\"excluded\"]\n[patch]\n\"example.com/outside\" = { path = \"../outside\" }\n\"example.com/fork\" = { path = \"fork/example.com/fork/1.0.0\" }\n")
        .write("registry/parent/pcb.toml", "")
        .write("registry/parent/child/pcb.toml", "")
        .write("registry/excluded/pcb.toml", "invalid [")
        .write("registry/fork/example.com/fork/1.0.0/pcb.toml", "")
        .write("outside/pcb.toml", "")
        .init_git()
        .commit("nested workspace")
        .tag("registry/parent/child/v1.2.0");
    let commit = sandbox.cmd("git", ["rev-parse", "HEAD"]).read().unwrap();
    sandbox.cwd("registry");
    assert_eq!(
        inventory(&sandbox),
        json!({"packages": [
            {"path": "registry/parent", "preferred": false, "release": null},
            {"path": "registry/parent/child", "preferred": false, "release": {"version": "1.2.0", "commit": commit, "contentHash": null}}
        ]})
    );
    sandbox.cwd("outside");
    assert_eq!(
        inventory(&sandbox),
        json!({"packages": [
            {"path": "outside", "preferred": false, "release": null}
        ]})
    );
}

#[test]
fn test_inventory_unpublished_root_and_failures_emit_no_document() {
    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", WORKSPACE_PCB_TOML)
        .init_git()
        .commit("no tags");
    assert_eq!(
        inventory(&sandbox),
        serde_json::json!({"packages": [
            {"path": "", "preferred": false, "release": null}
        ]})
    );
    let fails = |sandbox: &Sandbox, args: &[&str], diagnostic: &str| {
        let output = sandbox
            .cmd(env!("CARGO_BIN_EXE_pcbc"), args)
            .stdout_capture()
            .stderr_capture()
            .unchecked()
            .run()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            output.stdout.is_empty(),
            "partial inventory: {:?}",
            output.stdout
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{:?}",
            output
        );
    };
    let args = ["info", "--inventory", "--format", "json"];
    sandbox
        .write("good/pcb.toml", "")
        .write("broken/pcb.toml", "[broken");
    fails(&sandbox, &args, "broken/pcb.toml");
    sandbox.write("broken/pcb.toml", "");
    let head = sandbox.cmd("git", ["rev-parse", "HEAD"]).read().unwrap();
    sandbox.write(".git/shallow", format!("{head}\n"));
    fails(&sandbox, &args, "shallow");
    std::fs::remove_file(sandbox.root_path().join(".git/shallow")).unwrap();
    sandbox.write(".git/refs/tags/broken", "not an object id\n");
    fails(&sandbox, &args, "Cannot list local tags");
}
