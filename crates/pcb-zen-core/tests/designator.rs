use std::sync::Arc;

use pcb_zen_core::{CoreLoadResolver, DiagnosticsPass, EvalContext, SortPass};

mod common;
use common::InMemoryFileProvider;

fn eval_to_schematic(
    files: std::collections::HashMap<String, String>,
    main: &str,
) -> pcb_zen_core::WithDiagnostics<pcb_sch::Schematic> {
    let load_resolver = Arc::new(CoreLoadResolver::new(
        Arc::new(InMemoryFileProvider::new(files)),
        Default::default(),
    ));

    let ctx = EvalContext::new(load_resolver).set_source_path(std::path::PathBuf::from(main));
    let eval = ctx.eval();
    assert!(eval.is_success(), "eval failed: {:?}", eval.diagnostics);
    let eval_output = eval.output.expect("expected EvalOutput on success");
    eval_output.to_schematic_with_diagnostics()
}

#[test]
#[cfg(not(target_os = "windows"))]
fn duplicate_manual_designators_are_errors() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
Component(
    name = "r_a",
    designator = "R1",
    prefix = "R",
    footprint = "TEST:0402",
    pin_defs = {"P1": "1"},
    pins = {"P1": Net("N1")},
)

Component(
    name = "r_b",
    designator = "R1",
    prefix = "R",
    footprint = "TEST:0402",
    pin_defs = {"P1": "1"},
    pins = {"P1": Net("N2")},
)
"#
        .to_string(),
    );

    let mut result = eval_to_schematic(files, "test.zen");
    SortPass.apply(&mut result.diagnostics);
    let errors = result.diagnostics.errors();

    assert!(
        errors.iter().any(|e| e
            .body
            .contains("Reference designator 'R1' is assigned more than once")),
        "expected duplicate designator error, got: {:?}",
        errors
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn explicit_none_designator_is_treated_as_unset() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
Component(
    name = "r_a",
    designator = None,
    prefix = "R",
    footprint = "TEST:0402",
    pin_defs = {"P1": "1"},
    pins = {"P1": Net("N1")},
)
"#
        .to_string(),
    );

    let result = eval_to_schematic(files, "test.zen");
    assert!(
        result.diagnostics.errors().is_empty(),
        "unexpected errors: {:?}",
        result.diagnostics.errors()
    );

    let refdes = result
        .output
        .as_ref()
        .expect("expected schematic output")
        .instances
        .values()
        .find_map(|inst| inst.reference_designator.as_deref());
    assert_eq!(refdes, Some("R1"));
}
