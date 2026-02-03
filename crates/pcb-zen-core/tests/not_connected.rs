use std::sync::Arc;

use pcb_zen_core::{CoreLoadResolver, DiagnosticsPass, EvalContext, NoopRemoteFetcher, SortPass};

mod common;
use common::InMemoryFileProvider;

fn eval_to_schematic(
    files: std::collections::HashMap<String, String>,
    main: &str,
) -> pcb_zen_core::WithDiagnostics<pcb_sch::Schematic> {
    let load_resolver = Arc::new(CoreLoadResolver::new(
        Arc::new(InMemoryFileProvider::new(files)),
        Arc::new(NoopRemoteFetcher::default()),
        std::path::PathBuf::from("/"),
        true,
        None,
    ));

    let ctx = EvalContext::new(load_resolver).set_source_path(std::path::PathBuf::from(main));
    let eval = ctx.eval();
    assert!(eval.is_success(), "eval failed: {:?}", eval.diagnostics);
    let eval_output = eval.output.expect("expected EvalOutput on success");
    eval_output.to_schematic_with_diagnostics()
}

#[test]
#[cfg(not(target_os = "windows"))]
fn not_connected_warns_on_multiple_ports() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
NotConnected = builtin.net_type("NotConnected")
nc = NotConnected("NC_PIN")

Component(
    name = "R1",
    prefix = "R",
    footprint = "TEST:0402",
    pin_defs = {"P2": "2"},
    pins = {"P2": nc},
)

Component(
    name = "R2",
    prefix = "R",
    footprint = "TEST:0402",
    pin_defs = {"P2": "2"},
    pins = {"P2": nc},
)
"#
        .to_string(),
    );

    let mut result = eval_to_schematic(files, "test.zen");
    SortPass.apply(&mut result.diagnostics);
    let warnings = result.diagnostics.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.body.contains("NotConnected net connects to 2 ports")),
        "expected multi-port NotConnected warning, got: {:?}",
        warnings
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.body.contains("R1.P2") && w.body.contains("R2.P2")),
        "expected warning to mention ports, got: {:?}",
        warnings
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn not_connected_does_not_warn_on_single_port_multiple_pads() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
NotConnected = builtin.net_type("NotConnected")
nc = NotConnected("NC_PIN")

Component(
    name = "U1",
    prefix = "U",
    footprint = "TEST:0402",
    symbol = Symbol(
        definition = [
            ("GND", ["5", "17"]),
        ]
    ),
    pins = {"GND": nc},
)
"#
        .to_string(),
    );

    let mut result = eval_to_schematic(files, "test.zen");
    SortPass.apply(&mut result.diagnostics);
    let warnings = result.diagnostics.warnings();
    assert!(
        warnings
            .iter()
            .all(|w| !w.body.contains("NotConnected net connects to")),
        "did not expect multi-port NotConnected warning, got: {:?}",
        warnings
    );
}
