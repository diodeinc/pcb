use clap::Args;
use colored::Colorize;

#[derive(Args)]
pub struct UpdateArgs {}

pub fn execute(_args: UpdateArgs) -> anyhow::Result<()> {
    eprintln!("{}", "Error: 'pcb update' is reserved.".red().bold());
    eprintln!();
    eprintln!("To update the pcb tool itself, use:");
    eprintln!("  {}", "pcb self update".yellow().bold());

    std::process::exit(1);
}
