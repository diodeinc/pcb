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
    /// Path to .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Setup file (e.g., voltage sources)
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    pub setup: Option<PathBuf>,

    /// Output file (use "-" for stdout)
    #[arg(short, long, default_value = "sim.cir", value_hint = clap::ValueHint::FilePath)]
    pub output: PathBuf,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Require that pcb.toml and pcb.sum are up-to-date. Fails if auto-deps would
    /// add dependencies or if the lockfile would be modified. Recommended for CI.
    #[arg(long)]
    pub locked: bool,
}

fn get_output_writer(path: &str) -> Result<Box<dyn Write>> {
    Ok(if path == "-" {
        Box::new(std::io::stdout()) // writes to stdout
    } else {
        Box::new(File::create(path)?)
    })
}

pub fn execute(args: SimArgs) -> Result<()> {
    crate::file_walker::require_zen_file(&args.file)?;

    let zen_path = &args.file;
    let mut out = get_output_writer(&args.output.to_string_lossy())?;

    // Resolve dependencies before building
    let (_workspace_info, resolution_result) =
        crate::resolve::resolve(zen_path.parent(), args.offline, args.locked)?;

    let Some(schematic) = build_zen(
        zen_path,
        create_diagnostics_passes(&[], &[]),
        false,
        &mut false.clone(),
        &mut false.clone(),
        resolution_result,
    ) else {
        anyhow::bail!("Build failed");
    };

    gen_sim(&schematic, &mut out)?;

    if let Some(setup_path) = args.setup {
        let mut setup = String::new();
        File::open(setup_path)?.read_to_string(&mut setup).unwrap();
        writeln!(out, "{setup}").unwrap();
    }

    Ok(())
}
