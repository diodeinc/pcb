use crate::common;

#[test]
fn board_path_and_legacy_calls_register_the_same_project() {
    for args in [
        r#"name="Test", path="hardware", layers=2"#,
        r#"name="Test", layout_path="hardware", layers=2"#,
        r#""Test", "hardware", None, 2"#,
    ] {
        for schematic in [false, true] {
            let source = format!(
                "load(\"@stdlib/board_config.zen\", \"Board\")\nBoard({args}, schematic={})",
                if schematic { "True" } else { "False" }
            );
            let result = common::eval_zen(vec![("test.zen".into(), source)]);
            assert!(result.is_success(), "{args}: {:?}", result.diagnostics);
            let output = result.output.unwrap();
            let tree = output.module_tree();
            let root = tree
                .values()
                .find(|module| module.path().is_root())
                .unwrap();
            let properties = root.properties();
            assert_eq!(
                properties
                    .get("layout_path")
                    .unwrap()
                    .to_value()
                    .unpack_str(),
                Some("package://test/hardware")
            );
            assert_eq!(
                properties
                    .get("layout_name")
                    .unwrap()
                    .to_value()
                    .unpack_str(),
                Some("Test")
            );
            assert!(properties.contains_key("board_config.Test"));
            if schematic {
                assert_eq!(
                    properties
                        .get("schematic_path")
                        .unwrap()
                        .to_value()
                        .unpack_str(),
                    Some("package://test/hardware")
                );
                assert_eq!(
                    properties
                        .get("schematic_name")
                        .unwrap()
                        .to_value()
                        .unpack_str(),
                    Some("Test")
                );
            } else {
                assert!(!properties.contains_key("schematic_path"));
                assert!(!properties.contains_key("schematic_name"));
            }
        }
    }
}

#[test]
fn board_rejects_empty_missing_or_ambiguous_paths() {
    for (args, message) in [
        (
            r#"name="Test", path="""#,
            "Board() requires a non-empty path",
        ),
        (
            r#"name="Test", layout_path="""#,
            "Board() requires a non-empty path",
        ),
        (r#""Test", """#, "Board() requires a non-empty path"),
        (r#"name="Test""#, "Board() requires a non-empty path"),
        (
            r#"name="Test", path="new", layout_path="old""#,
            "Board() accepts either path or layout_path, not both",
        ),
        (
            r#""Test", "new", layout_path="old""#,
            "Board() accepts either path or layout_path, not both",
        ),
    ] {
        let source = format!("load(\"@stdlib/board_config.zen\", \"Board\")\nBoard({args})");
        let result = common::eval_zen(vec![("test.zen".into(), source)]);
        assert!(!result.is_success(), "{args} should fail");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.to_string().contains(message)),
            "{:?}",
            result.diagnostics
        );
    }
}
