#[macro_use]
mod common;

use crate::common::eval_zen;

fn warning_bodies(
    result: &pcb_zen_core::WithDiagnostics<pcb_zen_core::lang::eval::EvalOutput>,
) -> Vec<String> {
    result
        .diagnostics
        .warnings()
        .into_iter()
        .map(|diag| diag.body)
        .collect()
}

snapshot_eval!(nonexistent_file, {
    "test.zen" => r#"
        # This load should fail and the error should point to this line
        load("nonexistent.zen", "foo")
    "#
});

snapshot_eval!(file_with_syntax_error, {
    "broken.zen" => r#"
        # This file has a syntax error
        def broken_function(
            # Missing closing parenthesis
    "#,
    "test.zen" => r#"
        # Loading a file with syntax errors should show error at this load statement
        load("broken.zen", "broken_function")
    "#
});

snapshot_eval!(nested_load_errors, {
    "level3.zen" => r#"
        # This file has an actual error
        undefined_variable + 1
    "#,
    "level2.zen" => r#"
        # This loads a file with an error
        load("level3.zen", "something")
    "#,
    "level1.zen" => r#"
        # This loads a file that loads a file with an error
        load("level2.zen", "something")
    "#,
    "test.zen" => r#"
        # Top level load - error should propagate up with proper spans
        load("level1.zen", "something")
    "#
});

snapshot_eval!(cyclic_load_error, {
    "a.zen" => r#"
        # This creates a cycle: a -> b -> a
        load("b.zen", "b_func")

        def a_func():
            return "a"
    "#,
    "b.zen" => r#"
        # This completes the cycle
        load("a.zen", "a_func")

        def b_func():
            return "b"
    "#
});

snapshot_eval!(module_loader_attrs, {
    "Module.zen" => r#"
        TestExport = "test"
    "#,
    "top.zen" => r#"
        MyModule = Module("Module.zen")

        check(MyModule.TestExport == "test", "TestExport should be 'test'")
    "#
});

#[test]
fn stdlib_units_range_alias_warns_on_constructor() {
    let result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            load("@stdlib/units.zen", "VoltageRange")
            VoltageRange("3.3V")
        "#
        .to_string(),
    )]);

    let warning_bodies = warning_bodies(&result);

    assert!(
        warning_bodies
            .iter()
            .any(|body| body == "VoltageRange is deprecated. Use Voltage instead."),
        "expected deprecation warning, got: {warning_bodies:?}"
    );
}

#[test]
fn stdlib_units_range_alias_does_not_warn_on_load() {
    let result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            load("@stdlib/units.zen", "VoltageRange")
        "#
        .to_string(),
    )]);

    let warning_bodies = warning_bodies(&result);

    assert!(
        !warning_bodies
            .iter()
            .any(|body| body.contains("VoltageRange is deprecated")),
        "did not expect deprecation warning, got: {warning_bodies:?}"
    );
}

#[test]
fn stdlib_units_primary_type_does_not_warn_on_constructor() {
    let result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            load("@stdlib/units.zen", "Voltage")
            Voltage("3.3V")
        "#
        .to_string(),
    )]);

    let warning_bodies = warning_bodies(&result);

    assert!(
        !warning_bodies
            .iter()
            .any(|body| body.contains("Voltage is deprecated")),
        "did not expect deprecation warning, got: {warning_bodies:?}"
    );
}
