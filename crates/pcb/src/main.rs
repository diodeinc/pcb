use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::process::Command;

mod bom;
mod build;
mod clean;
mod fmt;
mod layout;
mod lsp;
mod open;
mod upgrade;

#[derive(Parser)]
#[command(name = "pcb")]
#[command(about = "PCB tool with build and layout capabilities", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build PCB projects
    #[command(alias = "b")]
    Build(build::BuildArgs),

    /// Upgrade PCB projects
    #[command(alias = "u")]
    Upgrade(upgrade::UpgradeArgs),

    /// Generate Bill of Materials (BOM)
    Bom(bom::BomArgs),

    /// Layout PCB designs
    #[command(alias = "l")]
    Layout(layout::LayoutArgs),

    /// Clean PCB build artifacts
    Clean(clean::CleanArgs),

    /// Format .zen and .star files
    Fmt(fmt::FmtArgs),

    /// Language Server Protocol support
    Lsp(lsp::LspArgs),

    /// Open PCB layout files
    #[command(alias = "o")]
    Open(open::OpenArgs),

    /// External subcommands are forwarded to pcb-<command>
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

fn main() -> anyhow::Result<()> {
    // Initialize logger
    env_logger::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Build(args) => build::execute(args),
        Commands::Upgrade(args) => upgrade::execute(args),
        Commands::Bom(args) => bom::execute(args),
        Commands::Layout(args) => layout::execute(args),
        Commands::Clean(args) => clean::execute(args),
        Commands::Fmt(args) => fmt::execute(args),
        Commands::Lsp(args) => lsp::execute(args),
        Commands::Open(args) => open::execute(args),
        Commands::External(args) => {
            if args.is_empty() {
                anyhow::bail!("No external command specified");
            }

            // First argument is the subcommand name
            let command = args[0].to_string_lossy();
            let external_cmd = format!("pcb-{command}");

            // Try to find and execute the external command
            match Command::new(&external_cmd).args(&args[1..]).status() {
                Ok(status) => {
                    // Forward the exit status
                    if !status.success() {
                        match status.code() {
                            Some(code) => std::process::exit(code),
                            None => anyhow::bail!(
                                "External command '{}' terminated by signal",
                                external_cmd
                            ),
                        }
                    }
                    Ok(())
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        eprintln!("Error: Unknown command '{command}'");
                        eprintln!("No built-in command or external command '{external_cmd}' found");
                        std::process::exit(1);
                    } else {
                        anyhow::bail!(
                            "Failed to execute external command '{}': {}",
                            external_cmd,
                            e
                        )
                    }
                }
            }
        }
    }
}
