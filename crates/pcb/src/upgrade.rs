use clap::Args;
use colored::Colorize;

#[derive(Args)]
pub struct UpgradeArgs {
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub args: Vec<String>,
}

pub fn execute(_args: UpgradeArgs) -> anyhow::Result<()> {
    eprintln!("{}", "Error: 'pcb upgrade' is reserved.".red().bold());
    eprintln!();
    eprintln!("To migrate PCB projects, use:");
    eprintln!("  {}", "pcb migrate".yellow().bold());

    std::process::exit(1);
}
