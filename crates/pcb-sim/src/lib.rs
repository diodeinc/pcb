pub mod ngspice;
pub use ngspice::{SimulationResult, check_ngspice_installed, run_ngspice, run_ngspice_captured};

use anyhow::Result;
use itertools::Itertools;
use pcb_sch::{AttributeValue, InstanceRef, Schematic};
use pcb_zen_core::attrs;
use std::collections::HashSet;
use std::io::Write;

/// Check if a schematic has inline simulation setup.
pub fn has_sim_setup(schematic: &Schematic) -> bool {
    schematic
        .root()
        .and_then(|root| root.attributes.get(attrs::SIM_SETUP))
        .and_then(|v| v.string())
        .is_some()
}

/// Format an instance path as `Foo.Bar.Baz` from its hierarchical components.
fn instance_path_str(iref: &InstanceRef) -> String {
    iref.instance_path.join(".")
}

// Generate .cir from a zen file
pub fn gen_sim(schematic: &Schematic, out: &mut impl Write) -> Result<()> {
    // Start with an empty line
    writeln!(out).unwrap();

    let mut included_libs = HashSet::new();

    let components: Vec<_> = schematic
        .instances
        .iter()
        .filter(|(_, i)| i.kind == pcb_sch::InstanceKind::Component)
        .sorted_by_key(|(_, i)| i.reference_designator.as_ref().unwrap())
        .collect();

    // Fail if any components are missing a spice model — the netlist would be incomplete
    let missing: Vec<String> = components
        .iter()
        .filter(|(_, i)| !i.attributes.contains_key(attrs::MODEL_DEF))
        .map(|(iref, i)| {
            let refdes = i.reference_designator.as_deref().unwrap_or("??");
            let path = instance_path_str(iref);
            format!("{refdes} [{path}]")
        })
        .collect();
    if !missing.is_empty() {
        let lines = missing
            .iter()
            .map(|m| format!("  - {m}"))
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!("Components missing a SpiceModel:\n{lines}");
    }

    // Generate the .cir file
    for (_, comp_inst) in &components {
        let model_def = comp_inst
            .attributes
            .get(attrs::MODEL_DEF)
            .unwrap()
            .string()
            .unwrap();
        if included_libs.insert(model_def) {
            write!(out, "{model_def}").unwrap();
        }
        assert!(comp_inst.attributes.contains_key(attrs::MODEL_NAME));
        let model_name = comp_inst
            .attributes
            .get(attrs::MODEL_NAME)
            .unwrap()
            .string()
            .unwrap();
        let comp_name = comp_inst.reference_designator.as_ref().unwrap();
        let arg_str = comp_inst
            .attributes
            .get(attrs::MODEL_ARGS)
            .unwrap()
            .string()
            .unwrap();
        if let AttributeValue::Array(net_arr) = comp_inst.attributes.get(attrs::MODEL_NETS).unwrap()
        {
            let nets = net_arr.iter().map(|s| s.string().unwrap()).join(" ");
            writeln!(out, "X{comp_name} {nets} {model_name} {arg_str}").unwrap();
        } else {
            unreachable!("bad spice model");
        }
    }

    // Emit inline sim setup if present on the root instance
    if let Some(root) = schematic.root()
        && let Some(setup) = root.attributes.get(attrs::SIM_SETUP)
        && let Some(text) = setup.string()
    {
        writeln!(out)?;
        write!(out, "{text}")?;
        if !text.ends_with('\n') {
            writeln!(out)?;
        }
    }

    Ok(())
}
