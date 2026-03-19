#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;

const TEST_KICAD_MOD: &str = r#"(footprint "test"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)
"#;

const REMOTE_PACKAGE_ZEN: &str = r#"
value = config("value", str, default = "10kOhm")

P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "R",
    prefix = "R",
    footprint = File("test.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": P1, "P2": P2},
    properties = {"value": value},
)
"#;

const WORKSPACE_PCB_TOML: &str = r#"
[workspace]
pcb-version = "0.3"
members = ["boards/*"]
"#;

const BOARD_PCB_TOML: &str = r#"
[board]
name = "MainBoard"
path = "MainBoard.zen"
"#;

const BOARD_ZEN: &str = r#"
TPS54331 = Module("github.com/diodeinc/registry/reference/TPS54331/TPS54331.zen")

vcc = Net("VCC")
gnd = Net("GND")

TPS54331(name = "U1", P1 = vcc, P2 = gnd)
"#;

fn setup_registry_fixture(sb: &mut Sandbox) -> String {
    let mut fixture = sb.git_fixture("https://github.com/diodeinc/registry.git");
    fixture
        .write("reference/TPS54331/pcb.toml", "[dependencies]\n")
        .write("reference/TPS54331/TPS54331.zen", REMOTE_PACKAGE_ZEN)
        .write("reference/TPS54331/test.kicad_mod", TEST_KICAD_MOD)
        .write("reference/TPS54332/pcb.toml", "[dependencies]\n")
        .write("reference/TPS54332/TPS54332.zen", REMOTE_PACKAGE_ZEN)
        .write("reference/TPS54332/test.kicad_mod", TEST_KICAD_MOD)
        .commit("Add TPS54331 package")
        .tag("reference/TPS54331/v0.1.0", false)
        .push_mirror();
    fixture.rev_parse_head()
}

fn setup_workspace(sb: &mut Sandbox) {
    sb.write("pcb.toml", WORKSPACE_PCB_TOML)
        .write("boards/MainBoard/pcb.toml", BOARD_PCB_TOML)
        .write("boards/MainBoard/MainBoard.zen", BOARD_ZEN)
        .init_git()
        .commit("Initial workspace");

    sb.cmd(
        "git",
        [
            "remote",
            "set-url",
            "origin",
            "https://github.com/diodeinc/pcb-edit.git",
        ],
    )
    .run()
    .expect("set origin url");
}

#[test]
fn test_edit_creates_checkout() {
    let mut sb = Sandbox::new();
    let rev = setup_registry_fixture(&mut sb);
    setup_workspace(&mut sb);

    let output = sb.snapshot_run(
        "pcb",
        [
            "edit",
            "boards/MainBoard",
            "-p",
            "github.com/diodeinc/registry/reference/TPS54331",
        ],
    );
    assert!(
        output.contains("Exit Code: 0"),
        "expected edit to succeed:\n{output}"
    );

    let manifest =
        std::fs::read_to_string(sb.root_path().join("boards/MainBoard/pcb.toml")).unwrap();
    assert!(
        manifest.contains("[dependencies.\"github.com/diodeinc/registry/reference/TPS54331\"]")
    );
    assert!(manifest.contains("branch = \"diodeinc/pcb-edit/boards/MainBoard\""));
    assert!(manifest.contains(&format!("rev = \"{}\"", rev)));

    assert!(
        sb.root_path()
            .join("boards/MainBoard/.pcb/edit/github.com/diodeinc/registry")
            .exists(),
        "expected managed checkout to exist"
    );
    assert!(
        sb.root_path()
            .join("boards/MainBoard/.pcb/edit/github.com/diodeinc/registry/.git")
            .is_dir(),
        "expected managed checkout to be a normal git checkout"
    );
    assert!(
        sb.root_path()
            .join("boards/MainBoard/.pcb/edit/github.com/diodeinc/registry/.git/objects/info/alternates")
            .exists(),
        "expected managed checkout to borrow objects from the shared bare cache"
    );
}

#[test]
fn test_edit_supports_multiple_packages() {
    let mut sb = Sandbox::new();
    let rev = setup_registry_fixture(&mut sb);
    setup_workspace(&mut sb);

    let output = sb.snapshot_run(
        "pcb",
        [
            "edit",
            "boards/MainBoard",
            "-p",
            "github.com/diodeinc/registry/reference/TPS54331",
            "-p",
            "github.com/diodeinc/registry/reference/TPS54332",
        ],
    );
    assert!(
        output.contains("Exit Code: 0"),
        "expected edit to succeed:\n{output}"
    );

    let manifest =
        std::fs::read_to_string(sb.root_path().join("boards/MainBoard/pcb.toml")).unwrap();
    assert!(
        manifest.contains("[dependencies.\"github.com/diodeinc/registry/reference/TPS54331\"]")
    );
    assert!(
        manifest.contains("[dependencies.\"github.com/diodeinc/registry/reference/TPS54332\"]")
    );
    assert!(manifest.contains("branch = \"diodeinc/pcb-edit/boards/MainBoard\""));
    assert!(manifest.matches(&format!("rev = \"{}\"", rev)).count() >= 2);

    assert!(
        sb.root_path()
            .join("boards/MainBoard/.pcb/edit/github.com/diodeinc/registry")
            .exists(),
        "expected managed checkout to exist"
    );
}

#[test]
fn test_build_warns_for_dirty_edit_checkout() {
    let mut sb = Sandbox::new();
    setup_registry_fixture(&mut sb);
    setup_workspace(&mut sb);

    let edit_output = sb.snapshot_run(
        "pcb",
        [
            "edit",
            "boards/MainBoard",
            "-p",
            "github.com/diodeinc/registry/reference/TPS54331",
        ],
    );
    assert!(
        edit_output.contains("Exit Code: 0"),
        "expected edit to succeed:\n{edit_output}"
    );

    std::fs::write(
        sb.root_path().join(
            "boards/MainBoard/.pcb/edit/github.com/diodeinc/registry/reference/TPS54331/NOTES.txt",
        ),
        "dirty\n",
    )
    .unwrap();

    let output = sb.snapshot_run("pcb", ["build", "boards/MainBoard/MainBoard.zen"]);
    assert!(
        output.contains("Warning: Managed edit checkout has uncommitted changes"),
        "expected build warning:\n{output}"
    );
    assert!(
        output.contains("Exit Code: 0"),
        "expected build to succeed:\n{output}"
    );
}

#[test]
fn test_edit_uses_remote_rev_when_checkout_is_ahead_locally() {
    let mut sb = Sandbox::new();
    let remote_rev = setup_registry_fixture(&mut sb);
    setup_workspace(&mut sb);

    let first_edit = sb.snapshot_run(
        "pcb",
        [
            "edit",
            "boards/MainBoard",
            "-p",
            "github.com/diodeinc/registry/reference/TPS54331",
        ],
    );
    assert!(
        first_edit.contains("Exit Code: 0"),
        "expected first edit to succeed:\n{first_edit}"
    );

    let checkout = sb
        .root_path()
        .join("boards/MainBoard/.pcb/edit/github.com/diodeinc/registry");
    let checkout_str = checkout.to_str().unwrap();
    let branch = "diodeinc/pcb-edit/boards/MainBoard";

    sb.cmd("git", ["-C", checkout_str, "push", "-u", "origin", branch])
        .run()
        .expect("push edit branch");

    std::fs::write(checkout.join("reference/TPS54331/LOCAL.txt"), "local\n").unwrap();
    sb.cmd(
        "git",
        ["-C", checkout_str, "add", "reference/TPS54331/LOCAL.txt"],
    )
    .run()
    .expect("add local change");
    sb.cmd(
        "git",
        ["-C", checkout_str, "commit", "-m", "Local only change"],
    )
    .run()
    .expect("commit local change");

    let local_rev = sb
        .cmd("git", ["-C", checkout_str, "rev-parse", "HEAD"])
        .read()
        .expect("read local head");
    assert_ne!(local_rev.trim(), remote_rev);

    let second_edit = sb.snapshot_run(
        "pcb",
        [
            "edit",
            "boards/MainBoard",
            "-p",
            "github.com/diodeinc/registry/reference/TPS54332",
        ],
    );
    assert!(
        second_edit.contains("Exit Code: 0"),
        "expected second edit to succeed:\n{second_edit}"
    );

    let manifest =
        std::fs::read_to_string(sb.root_path().join("boards/MainBoard/pcb.toml")).unwrap();
    assert!(
        manifest.contains("[dependencies.\"github.com/diodeinc/registry/reference/TPS54332\"]")
    );
    assert!(manifest.contains(&format!("rev = \"{}\"", remote_rev)));
    assert!(
        !manifest.contains(&format!("rev = \"{}\"", local_rev.trim())),
        "expected new dependency to use remote branch tip, not local unpushed HEAD"
    );
}
