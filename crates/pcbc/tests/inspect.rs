use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn inspect(root: &Path, path: &str, format: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pcbc"))
        .args(["inspect", path, "--format", format])
        .current_dir(root)
        .env_clear()
        .env("HOME", root)
        .env("USERPROFILE", root)
        .output()
        .expect("run inspect")
}

fn library(symbols: &str) -> String {
    format!("(kicad_symbol_lib (version 20241209) {symbols})")
}

fn assert_failure(output: &Output, expected: &str) {
    assert!(!output.status.success(), "{expected}: {output:?}");
    assert!(output.stdout.is_empty(), "{expected}: {output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "{expected}: {output:?}"
    );
}

const BASE: &str = r#"(symbol "Z\"Base"
    (property "Value" "Base value")
    (property "ki_description" "Base; description")
    (property "ki_keywords" "power  buck\tconverter")
    (property "Footprint" "../footprints/part.kicad_mod")
    (property "Datasheet" "../docs/part.pdf")
    (property "Manufacturer_Name" "Acme")
    (property "Manufacturer_Part_Number" "BASE-42"))"#;
const CHILD: &str = r#"(symbol "AChild" (extends
    "Z\"Base")
    (property "Value" "Child value")
    (property "Description" "Child description")
    (property "Footprint" "")
    (property "Manufacturer_Part_Number" "CHILD-17"))"#;

#[test]
fn inspect_metadata_is_resolved_deterministic_and_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let source = library(&format!(
        "{BASE} {CHILD} (symbol \"Empty\")
         (symbol \"BGrandchild\" (extends \"AChild\")
             (property \"Datasheet\" \"\") (property \"ki_keywords\" \"\"))"
    ));
    fs::write(dir.path().join("parts.kicad_sym"), &source).unwrap();
    // An invalid workspace must not matter to local inspection.
    fs::write(dir.path().join("pcb.toml"), "not a manifest").unwrap();
    let output = inspect(dir.path(), "parts.kicad_sym", "json");
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        json!({
            "symbols": [
                {"name": "AChild", "metadata": {
                    "primary": {"value": "Child value", "description": "Child description",
                        "keywords": ["power", "buck", "converter"], "footprint": "", "datasheet": "../docs/part.pdf"},
                    "custom_properties": {"Manufacturer_Name": "Acme", "Manufacturer_Part_Number": "CHILD-17"}
                }},
                {"name": "BGrandchild", "metadata": {
                    "primary": {"value": "Child value", "description": "Child description",
                        "keywords": [], "footprint": "", "datasheet": ""},
                    "custom_properties": {"Manufacturer_Name": "Acme", "Manufacturer_Part_Number": "CHILD-17"}
                }},
                {"name": "Empty", "metadata": {"primary": {}, "custom_properties": {}}},
                {"name": "Z\"Base", "metadata": {
                    "primary": {"value": "Base value", "description": "Base; description",
                        "keywords": ["power", "buck", "converter"], "footprint": "../footprints/part.kicad_mod", "datasheet": "../docs/part.pdf"},
                    "custom_properties": {"Manufacturer_Name": "Acme", "Manufacturer_Part_Number": "BASE-42"}
                }}
            ]
        })
    );
    let repeated = inspect(dir.path(), "parts.kicad_sym", "json");
    assert!(repeated.status.success());
    assert_eq!(output.stdout, repeated.stdout);
    let human = inspect(dir.path(), "parts.kicad_sym", "human");
    assert!(human.status.success(), "{human:?}");
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.starts_with("AChild\n"));
    assert!(human.contains("  Manufacturer_Part_Number: CHILD-17\n"));
    assert_eq!(
        fs::read_to_string(dir.path().join("parts.kicad_sym")).unwrap(),
        source
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("pcb.toml")).unwrap(),
        "not a manifest"
    );
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
}

#[test]
fn inspect_split_library_resolves_across_files() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("parts.kicad_symdir")).unwrap();
    fs::write(
        dir.path().join("parts.kicad_symdir/a.kicad_sym"),
        library(CHILD),
    )
    .unwrap();
    fs::write(
        dir.path().join("parts.kicad_symdir/z.kicad_sym"),
        library(BASE),
    )
    .unwrap();
    fs::write(
        dir.path().join("flat.kicad_sym"),
        library(&format!("{BASE} {CHILD}")),
    )
    .unwrap();
    let split = inspect(dir.path(), "parts.kicad_symdir", "json");
    let flat = inspect(dir.path(), "flat.kicad_sym", "json");
    assert!(split.status.success(), "{split:?}");
    assert!(flat.status.success(), "{flat:?}");
    assert_eq!(split.stdout, flat.stdout);

    fs::write(
        dir.path().join("parts.kicad_symdir/duplicate.kicad_sym"),
        library(BASE),
    )
    .unwrap();
    let duplicate = inspect(dir.path(), "parts.kicad_symdir", "json");
    assert_failure(&duplicate, "duplicate symbol");
}

#[test]
fn inspect_errors_never_emit_partial_json() {
    let dir = tempfile::tempdir().unwrap();
    for (source, expected) in [
        (
            library("(symbol \"Valid\") (symbol \"Child\" (extends \"Missing\"))"),
            "extends missing parent \"Missing\"",
        ),
        (
            library("(symbol \"A\" (extends \"B\")) (symbol \"B\" (extends \"A\"))"),
            "inheritance cycle: A -> B -> A",
        ),
        (
            library("(symbol \"Self\" (extends \"Self\"))"),
            "inheritance cycle: Self -> Self",
        ),
        (library("(symbol \"A\") (symbol \"A\")"), "duplicate symbol"),
        (
            library("(symbol \"A\" (property \"Value\"))"),
            "property requires a name and value",
        ),
        (
            library("(symbol \"A\" (property \"Value\" \"1\") (property \"Value\" \"2\"))"),
            "duplicate property",
        ),
        (library("(symbol \"A\" (extends))"), "invalid extends"),
        (library("(symbol)"), "missing symbol name"),
        (
            "(kicad_symbol_lib (version 20241209) (symbol \"A\")".into(),
            "Invalid symbol library",
        ),
        (
            format!("{} trailing", library("(symbol \"A\")")),
            "Invalid symbol library",
        ),
        (
            format!("{} ; comment", library("(symbol \"A\")")),
            "KiCad does not support comments",
        ),
        ("(not_a_library)".into(), "Expected a kicad_symbol_lib root"),
    ] {
        fs::write(dir.path().join("bad.kicad_sym"), source).unwrap();
        let output = inspect(dir.path(), "bad.kicad_sym", "json");
        assert_failure(&output, expected);
        assert!(String::from_utf8_lossy(&output.stderr).contains("bad.kicad_sym"));
    }
    fs::write(dir.path().join("wrong.txt"), library("")).unwrap();
    fs::create_dir(dir.path().join("empty.kicad_symdir")).unwrap();
    fs::create_dir(dir.path().join("directory.kicad_sym")).unwrap();
    for (path, expected) in [
        ("missing.kicad_sym", "Cannot inspect"),
        ("wrong.txt", "Unsupported inspection path"),
        ("directory.kicad_sym", "Unsupported inspection path"),
        ("empty.kicad_symdir", "No .kicad_sym files"),
    ] {
        let output = inspect(dir.path(), path, "json");
        assert_failure(&output, expected);
    }
}
