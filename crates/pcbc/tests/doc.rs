#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;
use std::process::Output;

const SIMPLE_RESISTOR_V1: &str = r#"
value = config(str, default = "1kOhm")

P1 = io(Net, direction = "input")
P2 = io(Net, direction = "output")
"#;

const SIMPLE_RESISTOR_V2: &str = r#"
value = config(str, default = "4.7kOhm")

P1 = io(Net, direction = "input")
P2 = io(Net, direction = "output")
"#;

const ALLOWED_CONFIG_MODULE: &str = r#"
package = config(
    str,
    allowed = ["0402", "0603"],
    default = "0603",
)
"#;

const REMOTE_PACKAGE_TOML: &str = r#"[workspace]
pcb-version = "0.4"
"#;

fn seed_remote_package(sb: &mut Sandbox, repository: &str) {
    let mut fixture = sb.git_fixture(repository);
    fixture
        .write("SimpleResistor/pcb.toml", REMOTE_PACKAGE_TOML)
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_V1)
        .commit("Add SimpleResistor v1")
        .tag("SimpleResistor/v1.0.0", false)
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_V2)
        .commit("Add SimpleResistor v2")
        .tag("SimpleResistor/v2.0.0", false)
        .push_mirror();
}

fn run_doc(sb: &mut Sandbox, package: &str) -> Output {
    sb.run("pcbc", ["doc", "--package", package])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("doc command failed")
}

#[test]
fn test_pcb_doc_remote_package_defaults_to_latest() {
    let mut sb = Sandbox::new();
    seed_remote_package(&mut sb, "https://github.com/mycompany/components.git");
    seed_remote_package(
        &mut sb,
        "https://code.diode.computer/mycompany/components.git",
    );

    let default_output = run_doc(&mut sb, "github.com/mycompany/components/SimpleResistor");
    let latest_output = run_doc(
        &mut sb,
        "github.com/mycompany/components/SimpleResistor@latest",
    );
    let pinned_output = run_doc(
        &mut sb,
        "github.com/mycompany/components/SimpleResistor@1.0.0",
    );
    let diodehub_output = run_doc(
        &mut sb,
        "code.diode.computer/mycompany/components/SimpleResistor@1.0.0",
    );

    for (label, output) in [
        ("default", &default_output),
        ("latest", &latest_output),
        ("pinned", &pinned_output),
        ("diodehub", &diodehub_output),
    ] {
        assert!(
            output.status.success(),
            "{label} command failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).trim().is_empty(),
            "{label} command wrote to stderr:\n{}",
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let default_stdout = String::from_utf8_lossy(&default_output.stdout);
    let latest_stdout = String::from_utf8_lossy(&latest_output.stdout);
    let pinned_stdout = String::from_utf8_lossy(&pinned_output.stdout);
    let diodehub_stdout = String::from_utf8_lossy(&diodehub_output.stdout);

    assert_eq!(
        default_stdout, latest_stdout,
        "default remote doc output should match @latest"
    );
    assert!(
        default_stdout.contains("| value | str | \"4.7kOhm\" |"),
        "default output should document the latest tag:\n{default_stdout}"
    );
    assert!(
        default_stdout.contains("| Name | Type | Direction |"),
        "default output should include the IO direction column:\n{default_stdout}"
    );
    assert!(
        default_stdout.contains("| P1 | Net | input |"),
        "default output should document the P1 direction:\n{default_stdout}"
    );
    assert!(
        default_stdout.contains("| P2 | Net | output |"),
        "default output should document the P2 direction:\n{default_stdout}"
    );
    assert!(
        !default_stdout.contains("| value | str | \"1kOhm\" |"),
        "default output should not document the older tag:\n{default_stdout}"
    );
    assert!(
        pinned_stdout.contains("| value | str | \"1kOhm\" |"),
        "explicit version should still resolve the older tag:\n{pinned_stdout}"
    );
    assert!(
        !pinned_stdout.contains("| value | str | \"4.7kOhm\" |"),
        "explicit version should not resolve the newer tag:\n{pinned_stdout}"
    );
    assert!(
        diodehub_stdout.contains("| value | str | \"1kOhm\" |"),
        "DiodeHub output should document the pinned package:\n{diodehub_stdout}"
    );
}

#[test]
fn test_pcb_doc_shows_allowed_values_for_config() {
    let mut sb = Sandbox::new().with_workspace();
    sb.write("Widget.zen", ALLOWED_CONFIG_MODULE);

    let output = run_doc(&mut sb, ".");

    assert!(
        output.status.success(),
        "doc command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).trim().is_empty(),
        "doc command wrote to stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = sb.sanitize_output(&String::from_utf8_lossy(&output.stdout));
    assert_snapshot!("doc_allowed_values", stdout);
}

// A workspace that declares both [workspace].repository and [workspace].path.
// Its canonical package URLs are `repository + "/" + path + "/" + relative_dir`,
// built by `build_workspace_base_url` during discovery (workspace.rs).
const NESTED_WORKSPACE_TOML: &str = r#"[workspace]
repository = "github.com/acme/monorepo"
path = "hardware/boards"
pcb-version = "0.4"
"#;

// A workspace root that is itself a package (has [dependencies]) and declares
// [workspace].path. Its canonical URL is `repository + "/" + path` (no
// relative dir), exercising the empty-relative branch of get_local_package_url.
const NESTED_ROOT_PACKAGE_TOML: &str = r#"[workspace]
repository = "github.com/acme/monorepo"
path = "hardware/boards"
pcb-version = "0.4"

[dependencies]
"#;

const MEMBER_PACKAGE_TOML: &str = "[dependencies]\n";

/// Assert both forms succeed, write nothing to stderr, and produce
/// byte-identical sanitized stdout (which includes the H1 package-URL header).
fn assert_both_forms_agree(
    sb: &mut Sandbox,
    local_spec: &str,
    url_spec: &str,
    expected_h1: &str,
    ctx: &str,
) {
    let local_output = run_doc(sb, local_spec);
    let url_output = run_doc(sb, url_spec);

    for (label, output) in [("local-path", &local_output), ("url-form", &url_output)] {
        assert!(
            output.status.success(),
            "{label} doc command failed ({ctx}):\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).trim().is_empty(),
            "{label} doc command wrote to stderr ({ctx}):\n{}",
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let local_stdout = sb.sanitize_output(&String::from_utf8_lossy(&local_output.stdout));
    let url_stdout = sb.sanitize_output(&String::from_utf8_lossy(&url_output.stdout));

    assert_eq!(
        local_stdout, url_stdout,
        "local-path and URL-form doc output disagree ({ctx})\n\
         --- local-path ---\n{local_stdout}\n--- url-form ---\n{url_stdout}"
    );
    assert!(
        local_stdout.contains(&format!("# {expected_h1}\n")),
        "local-path H1 header should be '{expected_h1}' ({ctx}):\n{local_stdout}"
    );
    assert!(
        url_stdout.contains(&format!("# {expected_h1}\n")),
        "url-form H1 header should be '{expected_h1}' ({ctx}):\n{url_stdout}"
    );
}

/// Regression test for `pcb doc --package <local-path>` ignoring `[workspace].path`
/// (doc.rs `get_local_package_url`). For a nested/monorepo workspace that sets
/// `[workspace].path`, the local-path form and the full-URL form must render the
/// same H1 header (`repository/path/relative_dir`) for the same on-disk package.
#[test]
fn test_pcb_doc_local_path_matches_url_form_with_workspace_path() {
    let mut sb = Sandbox::new();
    sb.write("hardware/boards/pcb.toml", NESTED_WORKSPACE_TOML);
    sb.write("hardware/boards/components/pcb.toml", MEMBER_PACKAGE_TOML);
    sb.write("hardware/boards/components/Widget.zen", SIMPLE_RESISTOR_V1);
    sb.cwd("hardware/boards");

    assert_both_forms_agree(
        &mut sb,
        "./components",
        "github.com/acme/monorepo/hardware/boards/components",
        "github.com/acme/monorepo/hardware/boards/components",
        "nested member package",
    );
}

/// The workspace root itself is a package and declares `[workspace].path`, so
/// its canonical URL is `repository/path` with no relative dir. This exercises
/// the empty-`relative_str` branch of `get_local_package_url`, which previously
/// returned the bare `repository()` and dropped the `path` segment.
#[test]
fn test_pcb_doc_workspace_root_package_url_includes_workspace_path() {
    let mut sb = Sandbox::new();
    sb.write("pcb.toml", NESTED_ROOT_PACKAGE_TOML);
    sb.write("Widget.zen", SIMPLE_RESISTOR_V1);

    assert_both_forms_agree(
        &mut sb,
        ".",
        "github.com/acme/monorepo/hardware/boards",
        "github.com/acme/monorepo/hardware/boards",
        "nested root package",
    );
}
