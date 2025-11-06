use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

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

    /// Export a filtered view of an IPC-2581 file for a specific mode
    View {
        /// Input IPC-2581 XML file
        #[arg(value_hint = clap::ValueHint::FilePath)]
        input: PathBuf,

        /// Target function mode for the view
        #[arg(short, long)]
        mode: ViewMode,

        /// Output file path
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: PathBuf,
    },

    /// External subcommands are forwarded to pcb-ipc2581-<command>
    #[command(external_subcommand)]
    External(Vec<OsString>),
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

#[derive(ValueEnum, Debug, Clone, Copy)]
enum ViewMode {
    Bom,
    Assembly,
    Fabrication,
    Stackup,
    Test,
    Stencil,
    Dfx,
}

impl ViewMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Bom => "BOM",
            Self::Assembly => "ASSEMBLY",
            Self::Fabrication => "FABRICATION",
            Self::Stackup => "STACKUP",
            Self::Test => "TEST",
            Self::Stencil => "STENCIL",
            Self::Dfx => "DFX",
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    utils::color::init_color();
    env_logger::Builder::from_env(Env::default().default_filter_or("warn")).init();

    if std::env::var("CI").is_err() && !is_update_command(&cli.command) {
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

        Commands::View {
            input,
            mode,
            output,
        } => commands::view::execute(&input, mode, &output),

        Commands::External(args) => {
            let Some(cmd) = args.first() else {
                anyhow::bail!("No external command specified");
            };

            let cmd_name = cmd.to_string_lossy();
            let external_cmd = format!("pcb-ipc2581-{cmd_name}");

            match Command::new(&external_cmd).args(&args[1..]).status() {
                Ok(status) if status.success() => Ok(()),
                Ok(status) => match status.code() {
                    Some(code) => std::process::exit(code),
                    None => anyhow::bail!("Command '{external_cmd}' terminated by signal"),
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    eprintln!("Error: Unknown command '{cmd_name}'");
                    eprintln!("No built-in or external command '{external_cmd}' found");
                    std::process::exit(1);
                }
                Err(e) => anyhow::bail!("Failed to execute '{external_cmd}': {e}"),
            }
        }
    }
}

fn is_update_command(command: &Commands) -> bool {
    matches!(
        command,
        Commands::External(args) if args.first().is_some_and(|s| s.to_string_lossy() == "update")
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
            eprintln!("Run {} to update.", "pcb-ipc2581 update".yellow().bold());
        }
    }
}
