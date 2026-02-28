use anyhow::Result;
use clap::Args;
use pcb_sim::{gen_sim, run_ngspice};
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use tempfile::NamedTempFile;

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

    /// Write .cir to a specific file (ngspice still runs on it)
    #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
    pub output: Option<PathBuf>,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Require that pcb.toml and pcb.sum are up-to-date. Fails if auto-deps would
    /// add dependencies or if the lockfile would be modified. Recommended for CI.
    #[arg(long)]
    pub locked: bool,

    /// Print the .cir netlist to stdout (skip running ngspice)
    #[arg(long = "netlist")]
    pub netlist: bool,
}

pub fn execute(args: SimArgs) -> Result<()> {
    crate::file_walker::require_zen_file(&args.file)?;

    let zen_path = &args.file;

    // Resolve dependencies before building
    let resolution_result = crate::resolve::resolve(Some(zen_path), args.offline, args.locked)?;

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

    // Generate .cir into an in-memory buffer
    let mut buf: Vec<u8> = Vec::new();
    gen_sim(&schematic, &mut buf)?;

    if let Some(setup_path) = &args.setup {
        let mut setup = String::new();
        File::open(setup_path)?.read_to_string(&mut setup)?;
        writeln!(buf, "{setup}")?;
    }

    // --netlist: print to stdout and return
    if args.netlist {
        std::io::stdout().write_all(&buf)?;
        return Ok(());
    }

    // Write .cir to the requested output file or a tempfile, then run ngspice
    if let Some(output_path) = &args.output {
        File::create(output_path)?.write_all(&buf)?;
        run_ngspice(output_path)?;
    } else {
        let mut tmp = NamedTempFile::with_suffix(".cir")?;
        tmp.write_all(&buf)?;
        tmp.flush()?;
        run_ngspice(tmp.path())?;
    }

    Ok(())
}
