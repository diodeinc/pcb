use std::ffi::OsString;
use std::process::Command;

use clap::{Parser, Subcommand};
use colored::Colorize;
use env_logger::Env;

#[derive(Parser)]
#[command(name = "pcb-ipc2581")]
#[command(about = "IPC-2581 parser and generator", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize logger with default level depending on --debug (overridden by RUST_LOG)
    let env = Env::default().default_filter_or("error");
    env_logger::Builder::from_env(env).init();

    // Skip auto-update check in CI environments or when running the update command
    if std::env::var("CI").is_err() && !is_update_command(&cli.command) {
        check_and_update();
    }

    match cli.command {
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

fn is_update_command(command: &Commands) -> bool {
    matches!(
        command,
        Commands::External(args) if args.first().map(|s| s.to_string_lossy() == "update").unwrap_or(false)
    )
}

fn check_and_update() {
    let mut updater = axoupdater::AxoUpdater::new_for("pcb-ipc2581");
    if let Ok(updater) = updater.load_receipt() {
        if let Ok(true) = updater.is_update_needed_sync() {
            eprintln!(
                "{}",
                "A new version of pcb-ipc2581 is available!".blue().bold()
            );
            eprintln!("Run {} to update.", "pcb ipc2581 update".yellow().bold());
        }
    }
}
