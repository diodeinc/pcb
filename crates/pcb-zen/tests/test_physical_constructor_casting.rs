mod common;

use common::TestProject;

#[test]
fn test_unit_constructor_casts_from_other_physical_value() {
    let env = TestProject::new();

    env.add_file(
        "cast_ok.zen",
        r#"
load("@stdlib/units.zen", "Voltage", "Resistance")

v = Voltage("3.3V")
r = Resistance(v)  # Cast/re-tag Voltage -> Resistance

if r.unit != "Ohm":
    fail("expected Ohm, got " + r.unit)

if abs(r.value - 3.3) > 1e-9:
    fail("expected r.value == 3.3, got " + str(r.value))

Component(
    name = "Test",
    footprint = "test",
    symbol = Symbol(definition=[("1", ["1"])]),
    pins = {"1": Net("N1")},
)
"#,
    );

    let result = env.eval("cast_ok.zen");
    assert!(
        result.output.is_some(),
        "expected module to eval, got diagnostics: {:?}",
        result.diagnostics
    );
}

#[test]
fn test_unit_constructor_rejects_mismatched_unit_string() {
    let env = TestProject::new();

    env.add_file(
        "cast_err.zen",
        r#"
load("@stdlib/units.zen", "Resistance")

# Explicit unit strings must match the constructor unit.
Resistance("3.3V")
"#,
    );

    let result = env.eval("cast_err.zen");
    assert!(
        result.output.is_none(),
        "expected eval failure, but module evaluated successfully"
    );

    let diag = format!("{:?}", result.diagnostics);
    assert!(
        diag.contains("Unit mismatch: expected Resistance, got Voltage"),
        "expected unit mismatch diagnostic, got: {diag}"
    );
}
