#[macro_use]
mod common;

use common::InMemoryFileProvider;
use pcb_sch::InstanceKind;
use pcb_zen_core::EvalContext;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn refdes_assignment_uses_natural_hier_name_sort() {
    let mut decls = String::new();
    decls.push_str("vcc = Net(\"VCC\")\n\n");
    for i in 1..=20 {
        decls.push_str(&format!(
            "Component(name = \"Resistor_{i}\", footprint = \"TEST:0402\", pin_defs = {{\"V\": \"1\"}}, pins = {{\"V\": vcc}}, prefix = \"R\")\n",
        ));
    }

    let mut files = HashMap::new();
    files.insert("main.zen".to_string(), decls);

    let file_provider: Arc<dyn pcb_zen_core::FileProvider> =
        Arc::new(InMemoryFileProvider::new(files));
    let resolution = pcb_zen_core::resolution::ResolutionResult::empty();

    let ctx =
        EvalContext::new(file_provider, resolution).set_source_path(PathBuf::from("/main.zen"));
    let result = ctx.eval();
    assert!(result.is_success(), "eval failed: {:?}", result.diagnostics);

    let eval_output = result.output.unwrap();
    let sch_result = eval_output.to_schematic_with_diagnostics();
    assert!(
        sch_result.output.is_some(),
        "schematic conversion failed: {:?}",
        sch_result.diagnostics
    );

    let sch = sch_result.output.unwrap();
    let refdes_map: HashMap<String, String> = sch
        .instances
        .iter()
        .filter(|(_, inst)| matches!(inst.kind, InstanceKind::Component))
        .filter_map(|(iref, inst)| {
            let hier_name = iref
                .instance_path
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(".");
            inst.reference_designator
                .as_ref()
                .map(|rd| (hier_name, rd.clone()))
        })
        .collect();

    // Natural sort order: 1,2,...,9,10,...,20 => R1–R20
    for i in 1..=20 {
        let hier = format!("root.Resistor_{i}");
        let expected_refdes = format!("R{i}");
        let actual = refdes_map.get(&hier).unwrap_or_else(|| {
            // Try without "root." prefix
            let short = format!("Resistor_{i}");
            refdes_map.get(&short).unwrap_or_else(|| {
                panic!("missing refdes for {hier} or {short}, map: {refdes_map:?}")
            })
        });
        assert_eq!(
            actual, &expected_refdes,
            "expected {hier} -> {expected_refdes}, got {actual}"
        );
    }
}
