use clap::Args;
use std::process::Command;

#[derive(Args)]
pub struct SelfUpdateArgs {}

pub fn execute(_args: SelfUpdateArgs) -> anyhow::Result<()> {
    // Execute the pcb-update program
    let status = Command::new("pcb-update").status()?;

    // Forward the exit status
    if !status.success() {
        match status.code() {
            Some(code) => std::process::exit(code),
            None => anyhow::bail!("pcb-update terminated by signal"),
        }
    }

    Ok(())
}
