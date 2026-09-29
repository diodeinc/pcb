#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;
use serde_json::Value;
use std::path::Path;

const PACKAGE: &str = "github.com/mycompany/components/SimpleResistor";

fn seed_registry() -> Sandbox {
    let mut sb = Sandbox::new();
    sb.git_fixture("https://github.com/mycompany/components.git")
        .write("SimpleResistor/pcb.toml", "")
        .write("SimpleResistor/SimpleResistor.zen", "# v1\n")
        .commit("Add SimpleResistor v1")
        .tag("SimpleResistor/v1.0.0", false)
        .write("SimpleResistor/SimpleResistor.zen", "# v2\n")
        .commit("Add SimpleResistor v2")
        .tag("SimpleResistor/v2.0.0", false)
        .push_mirror();
    sb
}

fn seed_workspace() -> Sandbox {
    let mut sb = seed_registry();
    sb.write(
        "pcb.toml",
        "[workspace]\nrepository = \"github.com/acme/demo\"\npcb-version = \"0.4\"\n",
    )
    .write("lib/pcb.toml", "")
    .write(
        "board/pcb.toml",
        format!("[dependencies]\n\"{PACKAGE}\" = \"1.0.0\"\n"),
    )
    .write("docs/README.md", "");
    sb
}

fn list_json(sb: &mut Sandbox, package: &[&str]) -> Vec<Value> {
    let args = ["list", "-m", "-json"].iter().chain(package);
    let stdout = sb.run("pcbc", args).read().expect("pcb list failed");
    stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect()
}

fn source(module: &Value) -> String {
    let dir = Path::new(module["dir"].as_str().unwrap());
    std::fs::read_to_string(dir.join("SimpleResistor.zen")).unwrap()
}

fn paths(modules: &[Value]) -> Vec<&str> {
    modules
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect()
}

fn single(modules: Vec<Value>) -> Value {
    let [module] = <[Value; 1]>::try_from(modules).expect("expected one package");
    module
}

#[test]
fn list_json_lists_stdlib_workspace_and_dependencies() {
    let mut sb = seed_workspace();
    // Any directory in the workspace lists the whole workspace.
    sb.cwd("docs");
    let modules = list_json(&mut sb, &[]);

    assert_eq!(
        paths(&modules),
        [
            "@stdlib",
            "github.com/acme/demo/board",
            "github.com/acme/demo/lib",
            PACKAGE
        ]
    );
    let stdlib = Path::new(modules[0]["dir"].as_str().unwrap());
    assert!(stdlib.join("interfaces.zen").exists());
    assert!(modules[2].get("version").is_none());
    assert!(modules[2]["dir"].as_str().unwrap().ends_with("/lib"));
    assert_eq!(modules[3]["version"], "1.0.0");
    assert_eq!(source(&modules[3]), "# v1\n");
}

#[test]
fn list_json_resolves_workspace_before_registry() {
    let mut sb = seed_workspace();

    let dependency = single(list_json(&mut sb, &[PACKAGE]));
    assert_eq!(dependency["version"], "1.0.0");

    let lib = single(list_json(&mut sb, &["github.com/acme/demo/lib"]));
    assert!(lib.get("version").is_none());

    let latest = single(list_json(&mut sb, &[&format!("{PACKAGE}@latest")]));
    assert_eq!(latest["path"], PACKAGE);
    assert_eq!(latest["version"], "2.0.0");
    assert_eq!(source(&latest), "# v2\n");
}

#[test]
fn list_json_fetches_latest_outside_workspace() {
    let mut sb = seed_registry();

    let latest = single(list_json(&mut sb, &[PACKAGE]));
    assert_eq!(latest["version"], "2.0.0");
    assert_eq!(source(&latest), "# v2\n");

    let pinned = single(list_json(&mut sb, &[&format!("{PACKAGE}@1.0.0")]));
    assert_eq!(pinned["version"], "1.0.0");
    assert_eq!(source(&pinned), "# v1\n");
}
