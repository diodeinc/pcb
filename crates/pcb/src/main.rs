use clap::{Parser, Subcommand};
use env_logger::Env;
use std::ffi::OsString;
use std::process::Command;

mod bom;
mod build;
mod clean;
mod fmt;
mod info;
mod layout;
mod lsp;
mod open;
mod release;
mod sim;
mod tag;
mod test;
mod upgrade;
mod vendor;
mod workspace;

#[derive(Parser)]
#[command(name = "pcb")]
#[command(about = "PCB tool with build and layout capabilities", long_about = None)]
#[command(version)]
struct Cli {
    /// Enable debug logging
    #[arg(short = 'd', long = "debug", global = true, hide = true)]
    debug: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build PCB projects
    #[command(alias = "b")]
    Build(build::BuildArgs),

    /// Run tests in .zen files
    #[command(alias = "t")]
    Test(test::TestArgs),

    /// Upgrade PCB projects
    #[command(alias = "u")]
    Upgrade(upgrade::UpgradeArgs),

    /// Generate Bill of Materials (BOM)
    Bom(bom::BomArgs),

    /// Display workspace and board information
    Info(info::InfoArgs),

    /// Layout PCB designs
    #[command(alias = "l")]
    Layout(layout::LayoutArgs),

    /// Clean PCB build artifacts
    Clean(clean::CleanArgs),

    /// Format .zen files
    Fmt(fmt::FmtArgs),

    /// Language Server Protocol support
    Lsp(lsp::LspArgs),

    /// Open PCB layout files
    #[command(alias = "o")]
    Open(open::OpenArgs),

    /// Release PCB project versions
    #[command(alias = "r")]
    Release(release::ReleaseArgs),

    /// Create and manage PCB version tags
    Tag(tag::TagArgs),

    /// Vendor external dependencies
    Vendor(vendor::VendorArgs),

    /// Run SPICE simulations
    Sim(sim::SimArgs),

    /// External subcommands are forwarded to pcb-<command>
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logger with default level depending on --debug (overridden by RUST_LOG)
    let env = if cli.debug {
        Env::default().default_filter_or("debug")
    } else {
        Env::default().default_filter_or("error")
    };
    env_logger::Builder::from_env(env).init();

    match cli.command {
        Commands::Build(args) => build::execute(args),
        Commands::Test(args) => test::execute(args),
        Commands::Upgrade(args) => upgrade::execute(args),
        Commands::Bom(args) => bom::execute(args),
        Commands::Info(args) => info::execute(args),
        Commands::Layout(args) => layout::execute(args),
        Commands::Clean(args) => clean::execute(args),
        Commands::Fmt(args) => fmt::execute(args),
        Commands::Lsp(args) => lsp::execute(args),
        Commands::Open(args) => open::execute(args),
        Commands::Release(args) => release::execute(args),
        Commands::Tag(args) => tag::execute(args),
        Commands::Vendor(args) => vendor::execute(args),
        Commands::Sim(args) => sim::execute(args),
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
