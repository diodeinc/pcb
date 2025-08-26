use anyhow::Result;
use clap::Args;
use pcb_sim::gen_sim;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::build::{build as build_zen, create_diagnostics_passes};

#[derive(Args, Debug)]
#[command(about = "generate spice .cir file for simulation")]
pub struct SimArgs {
    // Path to the .zen file describing the design that we will simulate
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::AnyPath)]
    pub path: PathBuf,

    // setup file (e.g., setup voltage)
    #[arg(
        long = "setup",
        value_name = "FILE",
        value_hint = clap::ValueHint::FilePath,
    )]
    pub setup: Option<PathBuf>,

    // Output file
    #[arg(
        short = 'o',
        long = "output",
        value_name = "FILE",
        value_hint = clap::ValueHint::FilePath,
        default_value = "sim.cir",
    )]
    pub output: PathBuf,
}

fn get_output_writer(path: &str) -> Result<Box<dyn Write>> {
    Ok(if path == "-" {
        Box::new(std::io::stdout()) // writes to stdout
    } else {
        Box::new(File::create(path)?)
    })
}

pub fn execute(args: SimArgs) -> Result<()> {
    let zen_path = args.path;

    let mut out = get_output_writer(&args.output.to_string_lossy())?;

    // Reuse the shared build flow from build.rs
    let mut has_errors = false;
    let passes = create_diagnostics_passes(&[]);
    let Some(schematic) = build_zen(&zen_path, false, passes, &mut has_errors) else {
        if has_errors {
            anyhow::bail!("Build failed with errors");
        } else {
            anyhow::bail!("No output generated");
        }
    };

    gen_sim(&schematic, &mut out)?;

    if let Some(setup_path) = args.setup {
        let mut setup = String::new();
        File::open(setup_path)?.read_to_string(&mut setup).unwrap();
        writeln!(out, "{setup}").unwrap();
    }

    Ok(())
}
