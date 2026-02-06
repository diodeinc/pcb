#[macro_use]
mod common;

use common::InMemoryFileProvider;
use pcb_sch::InstanceKind;
use pcb_zen_core::{CoreLoadResolver, EvalContext};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

#[test]
fn refdes_assignment_uses_natural_hier_name_sort() {
    // Regression test for natural sorting: `Resistor_2` should allocate before `Resistor_10`.
    //
    // This exercises the full Starlark eval -> schematic conversion pipeline, not just the
    // `pcb-sch` allocation helper.
    let mut decls = String::new();
    decls.push_str("vcc = Net(\"VCC\")\n\n");
    for i in 1..=20 {
        decls.push_str(&format!(
            "Component(name = \"Resistor_{i}\", footprint = \"TEST:0402\", pin_defs = {{\"V\": \"1\"}}, pins = {{\"V\": vcc}}, prefix = \"R\")\n",
        ));
    }

    let mut files = HashMap::new();
    files.insert("main.zen".to_string(), decls);

    let load_resolver = Arc::new(CoreLoadResolver::new(
        Arc::new(InMemoryFileProvider::new(files)),
        Default::default(),
    ));

    let ctx = EvalContext::new(load_resolver).set_source_path(PathBuf::from("/main.zen"));
    let result = ctx.eval();
    assert!(
        result.is_success(),
        "eval failed:\n{}",
        result
            .diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );

    let sch = result.output.unwrap().to_schematic().unwrap();

    let mut name_to_refdes: HashMap<String, String> = HashMap::new();
    for (inst_ref, inst) in &sch.instances {
        if inst.kind != InstanceKind::Component {
            continue;
        }
        let Some(refdes) = inst.reference_designator.as_ref() else {
            continue;
        };
        let Some(name) = inst_ref.instance_path.last() else {
            continue;
        };
        name_to_refdes.insert(name.clone(), refdes.clone());
    }

    for i in 1..=20 {
        let name = format!("Resistor_{i}");
        let expected = format!("R{i}");
        assert_eq!(
            name_to_refdes.get(&name),
            Some(&expected),
            "unexpected refdes for {name}"
        );
    }
}
