mod common;

use common::TestProject;

fn assert_eval_fails(env: &TestProject, rel_path: &str, expected_substring: &str) {
    let result = env.eval_module(rel_path);
    assert!(
        result.output.is_none(),
        "expected eval failure, but module evaluated successfully"
    );
    let diag = format!("{:?}", result.diagnostics);
    assert!(
        diag.contains(expected_substring),
        "expected diagnostics to contain '{expected_substring}', got: {diag}"
    );
}

#[test]
fn test_negative_tolerance_rejected_numeric() {
    let env = TestProject::new();
    env.add_file(
        "neg_tol_numeric.zen",
        r#"
load("@stdlib:v0.3.20/units.zen", "Voltage")

Voltage("3.3V").with_tolerance(-0.05)
"#,
    );
    assert_eval_fails(&env, "neg_tol_numeric.zen", "Invalid tolerance value");
}

#[test]
fn test_negative_tolerance_rejected_percent_string() {
    let env = TestProject::new();
    env.add_file(
        "neg_tol_string.zen",
        r#"
load("@stdlib:v0.3.20/units.zen", "Voltage")

Voltage("3.3V").with_tolerance("-5%")
"#,
    );
    assert_eval_fails(&env, "neg_tol_string.zen", "Invalid tolerance value");
}

#[test]
fn test_negative_tolerance_rejected_in_constructor_kwarg() {
    let env = TestProject::new();
    env.add_file(
        "neg_tol_ctor.zen",
        r#"
load("@stdlib:v0.3.20/units.zen", "Voltage")

Voltage("3.3V", tolerance=-0.05)
"#,
    );
    assert_eval_fails(&env, "neg_tol_ctor.zen", "Invalid tolerance value");
}

#[test]
fn test_negative_tolerance_rejected_in_parsed_string() {
    let env = TestProject::new();
    env.add_file(
        "neg_tol_parse.zen",
        r#"
load("@stdlib:v0.3.20/units.zen", "Voltage")

Voltage("3.3V -5%")
"#,
    );
    // This comes through the string parser and is surfaced as a parse error.
    assert_eval_fails(&env, "neg_tol_parse.zen", "Tolerance must be non-negative");
}
