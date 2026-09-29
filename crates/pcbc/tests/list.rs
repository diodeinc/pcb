#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;
use serde_json::Value;
use std::path::Path;

const PACKAGE: &str = "github.com/mycompany/components/SimpleResistor";

fn seed_workspace() -> Sandbox {
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
    sb.write(
        "pcb.toml",
        format!(
            "[workspace]\npcb-version = \"0.4\"\n\n[dependencies]\n\"{PACKAGE}\" = \"1.0.0\"\n"
        ),
    );
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

#[test]
fn list_json_lists_stdlib_and_dependencies() {
    let mut sb = seed_workspace();
    let modules = list_json(&mut sb, &[]);

    let paths: Vec<_> = modules
        .iter()
        .map(|m| m["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["@stdlib", PACKAGE]);
    let stdlib = Path::new(modules[0]["dir"].as_str().unwrap());
    assert!(stdlib.join("interfaces.zen").exists());
    assert_eq!(modules[1]["version"], "1.0.0");
    assert_eq!(source(&modules[1]), "# v1\n");
}

#[test]
fn list_json_resolves_dependency_before_registry() {
    let mut sb = seed_workspace();

    let [dependency] = &list_json(&mut sb, &[PACKAGE])[..] else {
        panic!("expected one package");
    };
    assert_eq!(dependency["version"], "1.0.0");

    let latest = format!("{PACKAGE}@latest");
    let [latest] = &list_json(&mut sb, &[&latest])[..] else {
        panic!("expected one package");
    };
    assert_eq!(latest["path"], PACKAGE);
    assert_eq!(latest["version"], "2.0.0");
    assert_eq!(source(latest), "# v2\n");
}
