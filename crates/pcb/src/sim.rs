use anyhow::Result;
use clap::Args;
use pcb_sim::gen_sim;
use pcb_ui::prelude::*;
use pcb_zen::EvalSeverity;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;

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
    let file_name = zen_path.file_name().unwrap().to_string_lossy();

    // Show spinner while building
    let spinner = Spinner::builder(format!("{file_name}: Building")).start();

    // Evaluate the design
    let eval_result = pcb_zen::run(&zen_path, false);

    let mut out = get_output_writer(&args.output.to_string_lossy())?;

    // Check if we have diagnostics to print
    if !eval_result.diagnostics.is_empty() {
        // Finish spinner before printing diagnostics
        spinner.finish();

        // Now print diagnostics
        let mut file_has_errors = false;
        for diag in eval_result.diagnostics.iter() {
            pcb_zen::render_diagnostic(diag);
            eprintln!();

            if matches!(diag.severity, EvalSeverity::Error) {
                file_has_errors = true;
            }
        }

        if file_has_errors {
            println!(
                "{} {}: Build failed",
                pcb_ui::icons::error(),
                file_name.with_style(Style::Red).bold()
            );
            anyhow::bail!("Build failed with errors");
        }
    } else if let Some(schematic) = &eval_result.output {
        spinner.finish();
        // Print success with component count
        let component_count = schematic
            .instances
            .values()
            .filter(|i| i.kind == pcb_sch::InstanceKind::Component)
            .count();
        eprintln!(
            "{} {} ({} components)",
            pcb_ui::icons::success(),
            file_name.with_style(Style::Green).bold(),
            component_count
        );

        gen_sim(schematic, &mut out)?;

        if let Some(setup_path) = args.setup {
            let mut setup = String::new();
            File::open(setup_path)?.read_to_string(&mut setup).unwrap();
            writeln!(out, "{setup}").unwrap();
        }
    } else {
        spinner.error(format!("{file_name}: No output generated"));
        anyhow::bail!("Build failed with errors");
    }

    Ok(())
}
