use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use env_logger::Env;

mod commands;
mod utils;

#[derive(Parser)]
#[command(name = "pcb-ipc2581")]
#[command(about = "IPC-2581 parser and inspection tool", long_about = None)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show high-level board summary
    Info {
        /// IPC-2581 XML file to inspect
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,

        /// Output format
        #[arg(short, long, default_value = "text")]
        format: OutputFormat,

        /// Unit preference for dimensions
        #[arg(short, long, default_value = "mm")]
        units: UnitFormat,
    },

    /// Generate Bill of Materials (BOM)
    Bom {
        /// IPC-2581 XML file to inspect
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,

        /// Output format
        #[arg(short, long, default_value = "text")]
        format: OutputFormat,
    },

    /// Edit IPC-2581 data
    Edit {
        #[command(subcommand)]
        command: EditCommands,
    },
}

#[derive(Subcommand)]
enum EditCommands {
    /// Add manufacturer/MPN alternatives to BOM entries
    Bom {
        /// IPC-2581 XML file to enrich
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,

        /// JSON file with BOM alternatives
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        rules: PathBuf,

        /// Output file (default: overwrite input)
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: Option<PathBuf>,

        /// Output format for progress/errors
        #[arg(short = 'f', long, default_value = "text")]
        format: OutputFormat,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum OutputFormat {
    Text,
    Json,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum UnitFormat {
    Mm,
    Mil,
    Inch,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize color handling (respects NO_COLOR)
    utils::color::init_color();

    // Initialize logger with default level (overridden by RUST_LOG)
    let env = Env::default().default_filter_or("warn");
    env_logger::Builder::from_env(env).init();

    // Skip auto-update check in CI environments
    if std::env::var("CI").is_err() {
        check_and_update();
    }

    match cli.command {
        Commands::Info {
            file,
            format,
            units,
        } => commands::info::execute(&file, format, units),

        Commands::Bom { file, format } => commands::bom::execute(&file, format),

        Commands::Edit { command } => match command {
            EditCommands::Bom {
                file,
                rules,
                output,
                ..
            } => commands::bom_edit::execute(&file, &rules, output.as_deref()),
        },
    }
}

fn check_and_update() {
    let mut updater = axoupdater::AxoUpdater::new_for("pcb-ipc2581");
    if let Ok(updater) = updater.load_receipt() {
        if let Ok(true) = updater.is_update_needed_sync() {
            eprintln!(
                "{}",
                "A new version of pcb-ipc2581 is available!".blue().bold()
            );
            eprintln!("Run {} to update.", "pcb-ipc2581 update".yellow().bold());
        }
    }
}
