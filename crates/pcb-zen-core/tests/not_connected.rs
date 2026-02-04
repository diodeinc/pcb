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

#[test]
#[cfg(not(target_os = "windows"))]
fn not_connected_auto_names_are_stable_by_port() {
    // Two programs that only differ in where unrelated nets are created.
    // The NotConnected net connected to R1.P2 should get a stable, port-derived name.
    let a = r#"
NotConnected = builtin.net_type("NotConnected")

_dummy1 = NotConnected()
_dummy2 = NotConnected()

nc = NotConnected()

Component(
    name = "R1",
    prefix = "R",
    footprint = "TEST:0402",
    pin_defs = {"P2": "2"},
    pins = {"P2": nc},
)
"#;

    let b = r#"
NotConnected = builtin.net_type("NotConnected")

nc = NotConnected()

Component(
    name = "R1",
    prefix = "R",
    footprint = "TEST:0402",
    pin_defs = {"P2": "2"},
    pins = {"P2": nc},
)

_dummy1 = NotConnected()
_dummy2 = NotConnected()
"#;

    let mut files_a = std::collections::HashMap::new();
    files_a.insert("test.zen".to_string(), a.to_string());
    let res_a = eval_to_schematic(files_a, "test.zen");
    let sch_a = res_a.output.expect("expected schematic output");

    let mut files_b = std::collections::HashMap::new();
    files_b.insert("test.zen".to_string(), b.to_string());
    let res_b = eval_to_schematic(files_b, "test.zen");
    let sch_b = res_b.output.expect("expected schematic output");

    fn find_net_name(schematic: &pcb_sch::Schematic) -> String {
        let needle: [String; 2] = ["R1".to_string(), "P2".to_string()];
        schematic
            .nets
            .iter()
            .find_map(|(name, net)| {
                if net.kind != "NotConnected" {
                    return None;
                }
                let has_port = net
                    .ports
                    .iter()
                    .any(|p| p.instance_path.as_slice().ends_with(&needle));
                has_port.then(|| name.clone())
            })
            .unwrap_or_else(|| {
                panic!(
                    "failed to find NotConnected net for port R1.P2 (nets: {:?})",
                    schematic
                        .nets
                        .iter()
                        .map(|(n, net)| (n.clone(), net.kind.clone()))
                        .collect::<Vec<_>>()
                )
            })
    }

    let name_a = find_net_name(&sch_a);
    let name_b = find_net_name(&sch_b);

    assert_eq!(name_a, "NC_R1_P2");
    assert_eq!(name_b, "NC_R1_P2");
}
