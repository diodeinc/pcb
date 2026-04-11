#[macro_use]
mod common;

use pcb_zen_core::lang::error::CategorizedDiagnostic;
use starlark::errors::EvalSeverity;

fn binding_kinds(result: &pcb_zen_core::WithDiagnostics<pcb_zen_core::EvalOutput>) -> Vec<String> {
    result
        .diagnostics
        .iter()
        .filter(|diag| matches!(diag.severity, EvalSeverity::Warning))
        .filter_map(|diag| {
            diag.innermost()
                .downcast_error_ref::<CategorizedDiagnostic>()
                .map(|cat| cat.kind.clone())
        })
        .collect()
}

#[test]
fn rebinding_emits_warning() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            value = "A"
            value = "B"
        "#
        .to_string(),
    )]);

    assert!(result.is_success());
    let kinds = binding_kinds(&result);
    assert_eq!(kinds, vec!["binding.rebind"]);
}

#[test]
fn shadowing_does_not_emit_warning() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            value = "outer"

            def demo(arg = lambda value: value):
                value = "inner"
                return value
        "#
        .to_string(),
    )]);

    assert!(result.is_success());
    assert!(binding_kinds(&result).is_empty());
}

#[test]
fn function_local_rebinding_does_not_emit_warning() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            def demo():
                value = "A"
                value = "B"
                return value
        "#
        .to_string(),
    )]);

    assert!(result.is_success());
    assert!(binding_kinds(&result).is_empty());
}

#[test]
fn mutually_exclusive_top_level_bindings_do_not_warn() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            communication_type = "I2C"

            if communication_type == "I2C":
                COMMS = "i2c"
                I2C_ADDR_GPIO = "gpio"
                i2c_pullup = True

            elif communication_type == "SPI":
                COMMS = "spi"
        "#
        .to_string(),
    )]);

    assert!(result.is_success());
    assert!(binding_kinds(&result).is_empty());
}

#[test]
fn binding_after_mutually_exclusive_assignment_warns() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            mode = "a"

            if mode == "a":
                value = "A"
            else:
                value = "B"

            value = "C"
        "#
        .to_string(),
    )]);

    assert!(result.is_success());
    assert_eq!(binding_kinds(&result), vec!["binding.rebind"]);
}

#[test]
fn for_loop_bindings_do_not_emit_warning() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            for i in [1, 2]:
                value = i

            for i in [3, 4]:
                value = i
        "#
        .to_string(),
    )]);

    assert!(result.is_success());
    assert!(binding_kinds(&result).is_empty());
}
