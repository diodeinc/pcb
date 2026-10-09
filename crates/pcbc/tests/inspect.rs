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

const BASE: &str = r#"(symbol "ZBase"
    (property "Value" "Base value")
    (property "ki_description" "Base; \"quoted\" description")
    (property "ki_keywords" "power  buck\tconverter")
    (property "Footprint" "../footprints/part.kicad_mod")
    (property "Datasheet" "../docs/part.pdf")
    (property "Manufacturer_Name" "Acme")
    (property private "Manufacturer_Part_Number" "BASE-42"))"#;
const CHILD: &str = r#"(symbol "AChild" (extends
    "ZBase")
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
                {"name": "ZBase", "metadata": {
                    "primary": {"value": "Base value", "description": "Base; \"quoted\" description",
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

    // KiCad keeps the last definition of a repeated name.
    fs::write(
        dir.path().join("parts.kicad_symdir/duplicate.kicad_sym"),
        library(&BASE.replace("Base value", "earlier")),
    )
    .unwrap();
    let duplicate = inspect(dir.path(), "parts.kicad_symdir", "json");
    assert!(duplicate.status.success(), "{duplicate:?}");
    assert_eq!(duplicate.stdout, flat.stdout);
}

#[test]
fn inspect_rejects_what_kicad_cannot_load_without_partial_json() {
    let dir = tempfile::tempdir().unwrap();
    for (source, expected) in [
        (
            library("(symbol \"Valid\") (symbol \"Child\" (extends \"Missing\"))"),
            "[symbol.extends] Child: extends \"Missing\", which this library does not define",
        ),
        (
            library("(symbol \"A\" (property \"Value\"))"),
            "`property` is missing text",
        ),
        (
            library("(symbol \"A\" (property \"\" \"v\"))"),
            "`property` takes a name, not ``",
        ),
        (
            library("(symbol \"A\" (property \"Value\" 1))"),
            "`property` takes text, not `1`",
        ),
        (
            library("(symbol \"A\" (property \"Value\" \"v\" (at 0 0)))"),
            "`at` is missing a number",
        ),
        (
            library("(symbol \"A\" (extends))"),
            "`extends` is missing text",
        ),
        (
            library("(symbol)"),
            ":1:38: [symbol.parse] a symbol is missing text",
        ),
        (
            library("(symbol \"A\" (offset 0))"),
            "`offset` is not something KiCad accepts in a symbol",
        ),
        (
            library("(symbol \"A\" (symbol \"A_1_1\" (offset 0)))"),
            "`offset` is not something KiCad accepts in a unit symbol",
        ),
        (
            library("(symbol \"A\" (symbol \"B_1_1\"))"),
            "[symbol.unit.naming]",
        ),
        (
            library(
                "(symbol \"A\" (symbol \"A_1_1\" (pin passive line (at 0 0 0) (length 2.54) (name \"A\") (number 1)))))",
            ),
            "`number` takes text, not `1`",
        ),
        (
            library(
                "(symbol \"A\" (symbol \"A_1_1\" (pin passive line (at 0 0 0) (length 2.54) (name \"A\" (bogus 1)) (number \"1\")))))",
            ),
            "`bogus` is not something KiCad accepts in `name`",
        ),
        (
            library("(symbol \"A\" (in_bom maybe))"),
            "`in_bom` takes `yes` or `no`, not `maybe`",
        ),
        (
            library("(symbol \"A<B\")"),
            "`<` cannot be part of a symbol name",
        ),
        (
            library("(offset 0) (symbol \"A\")"),
            "`offset` is not something KiCad accepts in a symbol library",
        ),
        (
            library("(symbol \"A\") (embedded_fonts no)"),
            "`embedded_fonts` is not something KiCad accepts in a symbol library",
        ),
        (
            library("(generator) (symbol \"A\")"),
            "`generator` is missing text",
        ),
        (
            library("(generator_version (nested))"),
            "`generator_version` takes a value, not `(…)`",
        ),
        (
            library("(host eeschema \"5.99\")"),
            "`5.99` is not something KiCad accepts in `host`",
        ),
        (
            library("(version 20241209)"),
            "`version` is not something KiCad accepts in a symbol library",
        ),
        (
            "(kicad_symbol_lib (version 20991231) (symbol \"A\"))".into(),
            "format version 20991231 is newer than KiCad 10 reads",
        ),
        (
            "(kicad_symbol_lib (generator \"x\") (version 20241209))".into(),
            "library does not open with `(kicad_symbol_lib (version …)`",
        ),
        (
            "(kicad_symbol_lib (version 20241209) (symbol \"A\")".into(),
            "file does not parse: unclosed `(`",
        ),
        (
            library("(symbol \"A\") ; comment\n"),
            "`;` starts a comment here, but KiCad reads it as text",
        ),
        ("(not_a_library)".into(), "library does not open with"),
    ] {
        fs::write(dir.path().join("bad.kicad_sym"), source).unwrap();
        let output = inspect(dir.path(), "bad.kicad_sym", "json");
        assert_failure(&output, expected);
        assert!(String::from_utf8_lossy(&output.stderr).contains("bad.kicad_sym:1:"));
    }
    fs::create_dir(dir.path().join("empty.kicad_symdir")).unwrap();
    for (path, expected) in [
        ("missing.kicad_sym", "Cannot inspect"),
        ("empty.kicad_symdir", "No symbol library sources"),
    ] {
        let output = inspect(dir.path(), path, "json");
        assert_failure(&output, expected);
    }
}

#[test]
fn inspect_accepts_what_kicad_loads() {
    let dir = tempfile::tempdir().unwrap();
    let old = "(kicad_symbol_lib (version 20200101) (host eeschema \"5.99\")";
    let new = "(kicad_symbol_lib (version 20241209) (generator kicad) (generator_version 10.0) (host eeschema)";
    for (head, symbols) in [
        // -0 is a formatting warning, missing properties are lint, and `hide`
        // is valid legacy syntax. None should block metadata inspection.
        (old, "(symbol \"A\" (pin_names (offset -0) hide))"),
        (new, "(symbol \"A\" (pin_names (offset -0) hide))"),
        // Repeated names, properties, a cycle and an empty parent all load.
        (
            new,
            "(symbol \"A\" (property \"Value\" \"1\") (property \"Value\" \"2\")) (symbol \"A\")",
        ),
        (new, "(symbol \"A\" (extends \"A\"))"),
        (new, "(symbol \"A\" (extends \"\"))"),
        // Duplicate and empty pin numbers are a build's concern, not KiCad's.
        (
            new,
            "(symbol \"A\" (symbol \"A_1_1\" (pin passive line (at 0 0 0) (length 2.54) (name \"X\") (number \"\")) (pin passive line (at 0 2.54 0) (length 2.54) (name \"Y\") (number \"\"))))",
        ),
    ] {
        let source = format!("{head} {symbols})");
        fs::write(dir.path().join("valid.kicad_sym"), &source).unwrap();
        let output = inspect(dir.path(), "valid.kicad_sym", "json");
        assert!(output.status.success(), "{source}: {output:?}");
        assert!(output.stderr.is_empty(), "{source}: {output:?}");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            json!({"symbols": [{"name": "A", "metadata": {"primary": {}, "custom_properties": {}}}]}),
            "{source}"
        );
    }
    // What follows the root list is not read, like KiCad.
    let source = format!("{new} (symbol \"A\")) (symbol \"B\")");
    fs::write(dir.path().join("valid.kicad_sym"), &source).unwrap();
    let output = inspect(dir.path(), "valid.kicad_sym", "json");
    assert!(output.status.success(), "{output:?}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("\"B\""));
}
