use anyhow::Result;
use clap::Args;
use itertools::Itertools;
use pcb_sch::{AttributeValue, Schematic};
use pcb_ui::prelude::*;
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

// Generate .cir from a zen file
pub fn gen_sim(schematic: &Schematic, out: &mut impl Write) -> Result<()> {
    // Start with an empty line
    writeln!(out).unwrap();

    let mut included_libs = HashSet::new();

    // Generate the .cir file
    for comp_inst in schematic
        .instances
        .values()
        .filter(|i| i.kind == pcb_sch::InstanceKind::Component)
    {
        if !comp_inst.attributes.contains_key("model_def") {
            continue;
        }
        let model_def = comp_inst
            .attributes
            .get("model_def")
            .unwrap()
            .string()
            .unwrap();
        if included_libs.insert(model_def) {
            write!(out, "{}", model_def).unwrap();
        }
        assert!(comp_inst.attributes.contains_key("model_name"));
        let model_name = comp_inst
            .attributes
            .get("model_name")
            .unwrap()
            .string()
            .unwrap();
        let comp_name = comp_inst.reference_designator.as_ref().unwrap();
        let arg_str = comp_inst
            .attributes
            .get("model_args")
            .unwrap()
            .string()
            .unwrap();
        if let AttributeValue::Array(net_arr) = comp_inst.attributes.get("model_nets").unwrap() {
            let nets = net_arr.iter().map(|s| s.string().unwrap()).join(" ");
            writeln!(out, "X{} {} {} {}", comp_name, nets, model_name, arg_str).unwrap();
        } else {
            unreachable!("bad spice model");
        }
    }
    Ok(())
}
