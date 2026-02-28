use anyhow::Result;
use clap::Args;
use colored::Colorize;
use pcb_sim::{gen_sim, has_sim_setup, run_ngspice_captured};
use pcb_ui::prelude::*;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

use crate::build::{build as build_zen, create_diagnostics_passes};
use crate::file_walker;

#[derive(Args, Debug)]
#[command(about = "Run SPICE simulations on .zen files with sim setup")]
pub struct SimArgs {
    /// .zen file or directory to simulate. Defaults to current directory.
    #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub path: Option<PathBuf>,

    /// Setup file (e.g., voltage sources). Only valid when simulating a single file.
    #[arg(long, value_hint = clap::ValueHint::FilePath)]
    pub setup: Option<PathBuf>,

    /// Write .cir to a file and exit (ngspice is not run). Only valid when simulating a single file.
    #[arg(short, long, value_hint = clap::ValueHint::FilePath, conflicts_with = "netlist")]
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

    /// Show full ngspice output on success
    #[arg(short, long)]
    pub verbose: bool,
}

fn simulate_one(
    zen_path: &std::path::Path,
    resolution_result: pcb_zen_core::resolution::ResolutionResult,
    args: &SimArgs,
) -> Result<bool> {
    let file_name = zen_path.file_name().unwrap().to_string_lossy().to_string();

    let Some(schematic) = build_zen(
        zen_path,
        create_diagnostics_passes(&[], &[]),
        false,
        &mut false.clone(),
        &mut false.clone(),
        resolution_result,
    ) else {
        anyhow::bail!("Build failed for {file_name}");
    };

    let has_inline_setup = has_sim_setup(&schematic);

    // Only simulate files that have inline sim setup (or an explicit --setup file)
    if !has_inline_setup && args.setup.is_none() {
        eprintln!("  {}", format!("{file_name}: No sim setup").dimmed(),);
        return Ok(false);
    }

    // Reject conflicting setup sources — inline setup typically ends with .end,
    // so appending an external --setup file would be silently ignored by ngspice.
    if has_inline_setup && args.setup.is_some() {
        anyhow::bail!(
            "{file_name}: Cannot use --setup with a file that already has inline sim setup"
        );
    }

    // Generate .cir into an in-memory buffer
    let mut buf: Vec<u8> = Vec::new();
    if let Err(e) = gen_sim(&schematic, &mut buf) {
        eprintln!(
            "{} {}: {e}",
            pcb_ui::icons::error(),
            file_name.as_str().with_style(Style::Red).bold(),
        );
        anyhow::bail!("Netlist generation failed for {file_name}");
    }

    if let Some(setup_path) = &args.setup {
        let mut setup = String::new();
        File::open(setup_path)?.read_to_string(&mut setup)?;
        writeln!(buf, "{setup}")?;
    }

    // --netlist: print to stdout and return (skip ngspice)
    if args.netlist {
        std::io::stdout().write_all(&buf)?;
        return Ok(true);
    }

    // --output: write .cir to file and return (skip ngspice)
    if let Some(output_path) = &args.output {
        File::create(output_path)?.write_all(&buf)?;
        return Ok(true);
    }

    // Write .cir next to the zen file so ngspice resolves relative paths correctly
    let zen_dir = zen_path.parent().unwrap_or(std::path::Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .suffix(".cir")
        .tempfile_in(zen_dir)?;
    tmp.write_all(&buf)?;
    tmp.flush()?;
    let cir_path = tmp.into_temp_path();

    let result = run_ngspice_captured(cir_path.as_ref(), zen_dir)?;

    if result.success {
        if args.verbose {
            eprint!("{}", result.output);
        }
        eprintln!(
            "{} {}: Simulation passed",
            pcb_ui::icons::success(),
            file_name.with_style(Style::Green).bold(),
        );
        Ok(true)
    } else {
        eprintln!(
            "{} {}: Simulation failed\n{}",
            pcb_ui::icons::error(),
            file_name.as_str().with_style(Style::Red).bold(),
            result.output.trim_end(),
        );
        anyhow::bail!("ngspice simulation failed for {file_name}");
    }
}

pub fn execute(args: SimArgs) -> Result<()> {
    // If a specific file is given, run it directly (preserves old single-file behaviour)
    if let Some(path) = &args.path
        && path.is_file()
    {
        file_walker::require_zen_file(path)?;
        let resolution_result = crate::resolve::resolve(Some(path), args.offline, args.locked)?;
        simulate_one(path, resolution_result, &args)?;
        return Ok(());
    }

    // Directory / workspace mode — behave like `pcb build`
    if args.setup.is_some() || args.output.is_some() || args.netlist {
        anyhow::bail!(
            "--setup, --output, and --netlist are only supported when simulating a single file"
        );
    }

    let resolution_result =
        crate::resolve::resolve(args.path.as_deref(), args.offline, args.locked)?;

    let zen_files = file_walker::collect_workspace_zen_files(
        args.path.as_deref(),
        &resolution_result.workspace_info,
    )?;

    let mut simulated = 0u32;
    let mut has_errors = false;

    for zen_path in &zen_files {
        match simulate_one(zen_path, resolution_result.clone(), &args) {
            Ok(ran) => {
                if ran {
                    simulated += 1;
                }
            }
            Err(e) => {
                eprintln!("{e}");
                has_errors = true;
            }
        }
    }

    if has_errors {
        anyhow::bail!("Simulation failed with errors");
    }

    if simulated == 0 {
        eprintln!("No files with simulation setup found");
    }

    Ok(())
}
