use pcb_test_utils::sandbox::Sandbox;
use pcb_zen_core::config::pcb_version_from_cargo;
use std::fs;

const LEGACY_REGISTRY: &str = "github.com/diodeinc/registry";
const CANONICAL_REGISTRY: &str = "code.diode.computer/diode/registry";

#[test]
fn migrate_bumps_workspace_pcb_version_to_current_lane() {
    let target = pcb_version_from_cargo();
    let previous = if target == "0.3" { "0.2" } else { "0.3" };
    let mut sandbox = Sandbox::new();
    sandbox.write(
        "pcb.toml",
        format!(
            r#"# workspace comment
[workspace]
name = "demo"
pcb-version = "{previous}" # old lane

[dependencies]
"#
        ),
    );

    run_migrate(&mut sandbox);

    let content = fs::read_to_string(sandbox.root_path().join("pcb.toml")).unwrap();
    assert!(content.contains("# workspace comment"));
    assert!(content.contains(&format!("pcb-version = \"{target}\" # old lane")));
}

#[test]
fn migrate_leaves_current_workspace_manifest_unchanged() {
    let target = pcb_version_from_cargo();
    let original = format!("[workspace]\npcb-version = \"{target}\"\nname = \"demo\"\n");
    let mut sandbox = Sandbox::new();
    sandbox.write("pcb.toml", &original);

    run_migrate(&mut sandbox);

    let content = fs::read_to_string(sandbox.root_path().join("pcb.toml")).unwrap();
    assert_eq!(content, original);
}

#[test]
fn migrate_removes_deprecated_workspace_members() {
    let target = pcb_version_from_cargo();
    let original = format!(
        r#"[workspace]
pcb-version = "{target}"
members = ["boards/*"]
name = "demo"
"#
    );
    let mut sandbox = Sandbox::new();
    sandbox.write("pcb.toml", original);

    run_migrate(&mut sandbox);

    let content = fs::read_to_string(sandbox.root_path().join("pcb.toml")).unwrap();
    assert!(content.contains(&format!("pcb-version = \"{target}\"")));
    assert!(content.contains("name = \"demo\""));
    assert!(!content.contains("members"));
}

#[test]
fn migrate_rewrites_registry_references_in_manifests_and_sources() {
    let target = pcb_version_from_cargo();
    let root_manifest = format!(
        r#"# keep root formatting
[workspace]
pcb-version = "{target}"
repository = '{LEGACY_REGISTRY}' # repository
vendor = ["{LEGACY_REGISTRY}/**"]

[dependencies]
"{LEGACY_REGISTRY}/components/Foo" = "1.0"

[patch]
"github.com/example/Fork" = {{ path = "fork/Fork" }}
"#
    );
    let package_manifest = format!(
        r#"[board]
name = "Main"
path = "Main.zen"

[dependencies]
"{LEGACY_REGISTRY}/components/Bar" = "1.0" # direct
"#
    );
    let source = format!(
        r#"load('{LEGACY_REGISTRY}/components/Foo/Foo.zen', "Foo")
Bar = Module("{LEGACY_REGISTRY}/components/Bar/Bar.zen")
"#
    );
    let vendored = format!("Thing = Module(\"{LEGACY_REGISTRY}/components/Old/Old.zen\")\n");
    let patched = format!("Thing = Module(\"{LEGACY_REGISTRY}/components/Old/Old.zen\")\n");
    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", &root_manifest)
        .write("boards/Main/pcb.toml", &package_manifest)
        .write("boards/Main/Main.zen", &source)
        .write("vendor/old.zen", &vendored)
        .write(
            "fork/Fork/pcb.toml",
            "[board]\nname = \"Fork\"\npath = \"Fork.zen\"\n",
        )
        .write("fork/Fork/Fork.zen", &patched);

    run_migrate(&mut sandbox);

    let migrated_root = fs::read_to_string(sandbox.root_path().join("pcb.toml")).unwrap();
    let migrated_package =
        fs::read_to_string(sandbox.root_path().join("boards/Main/pcb.toml")).unwrap();
    let migrated_source =
        fs::read_to_string(sandbox.root_path().join("boards/Main/Main.zen")).unwrap();
    assert!(migrated_root.contains("# keep root formatting"));
    assert!(migrated_root.contains(&format!(
        "repository = \"{CANONICAL_REGISTRY}\" # repository"
    )));
    assert!(migrated_root.contains(&format!("vendor = [\"{CANONICAL_REGISTRY}/**\"]")));
    assert!(!migrated_package.contains(LEGACY_REGISTRY));
    assert!(migrated_package.contains(&format!(
        "\"{CANONICAL_REGISTRY}/components/Bar\" = \"1.0\" # direct"
    )));
    assert!(!migrated_source.contains(LEGACY_REGISTRY));
    assert!(migrated_source.contains(CANONICAL_REGISTRY));
    assert_eq!(
        fs::read_to_string(sandbox.root_path().join("vendor/old.zen")).unwrap(),
        vendored
    );
    assert_eq!(
        fs::read_to_string(sandbox.root_path().join("fork/Fork/Fork.zen")).unwrap(),
        patched
    );

    let first_run = [migrated_root, migrated_package, migrated_source];
    run_migrate(&mut sandbox);
    assert_eq!(
        first_run,
        [
            fs::read_to_string(sandbox.root_path().join("pcb.toml")).unwrap(),
            fs::read_to_string(sandbox.root_path().join("boards/Main/pcb.toml")).unwrap(),
            fs::read_to_string(sandbox.root_path().join("boards/Main/Main.zen")).unwrap(),
        ]
    );
}

fn run_migrate(sandbox: &mut Sandbox) {
    let output = sandbox
        .run("pcbc", ["migrate"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert!(
        output.status.success(),
        "pcbc migrate failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
