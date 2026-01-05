use anyhow::Result;
use clap::Args;
use colored::Colorize;
use pcb_ui::{Style, StyledText};
use pcb_zen::fork::{fork_package, ForkOptions};

#[derive(Args)]
pub struct ForkArgs {
    /// Fully-qualified module URL (e.g., github.com/diodeinc/registry/modules/UsbPdController)
    #[arg(value_name = "URL")]
    pub url: String,

    /// Specific version to fork (default: latest tagged version)
    #[arg(long)]
    pub version: Option<String>,

    /// Force overwrite if fork directory already exists
    #[arg(long)]
    pub force: bool,
}

pub fn execute(args: ForkArgs) -> Result<()> {
    println!("{} {}", "Forking".cyan().bold(), args.url.bold());
    println!("  {} Discovering versions...", "→".dimmed());

    let result = fork_package(ForkOptions {
        url: args.url,
        version: args.version,
        force: args.force,
    })?;

    // Success message
    println!();
    println!("{} Forked successfully!", "✓".green().bold());
    println!();
    println!(
        "  {} {}",
        "Fork location:".dimmed(),
        result
            .fork_dir
            .display()
            .to_string()
            .with_style(Style::Cyan)
    );
    println!(
        "  {} [patch].\"{}\" = {{ path = \"{}\" }}",
        "Patch entry:".dimmed(),
        result.module_url,
        result.patch_path
    );
    println!();
    println!(
        "{}",
        "You can now edit files in the fork directory. Changes will be used by 'pcb build'."
            .dimmed()
    );

    Ok(())
}
