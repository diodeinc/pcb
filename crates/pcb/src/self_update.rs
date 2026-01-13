use clap::{Args, Subcommand};
use std::process::Command;

#[derive(Args)]
pub struct SelfUpdateArgs {
    #[command(subcommand)]
    command: SelfUpdateCommands,
}

#[derive(Subcommand)]
enum SelfUpdateCommands {
    /// Update the pcb tool to the latest version
    Update,
}

pub fn execute(args: SelfUpdateArgs) -> anyhow::Result<()> {
    match args.command {
        SelfUpdateCommands::Update => {
            // Execute the pcb-update program
            let status = Command::new("pcb-update").status()?;

            // Forward the exit status
            if !status.success() {
                match status.code() {
                    Some(code) => std::process::exit(code),
                    None => anyhow::bail!("pcb-update terminated by signal"),
                }
            }

            // Print release notes from the newly installed version
            // We spawn `pcb doc --changelog` so we get the NEW binary's embedded changelog
            println!();
            let _ = Command::new("pcb")
                .args(["doc", "--changelog", "--latest"])
                .status();

            Ok(())
        }
    }
}
