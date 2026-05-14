use clap::{Args, Subcommand};
use std::path::PathBuf;

use pcb_ipc2581_tools::{
    OutputFormat, RenderFormat, UnitFormat, ViewMode, commands, gerber, utils,
};

#[derive(Args)]
pub struct Ipc2581Args {
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
        #[arg(short, long, default_value = "text")]
        format: OutputFormat,
        #[arg(short, long, default_value = "mm")]
        units: UnitFormat,
    },
    /// Generate Bill of Materials (BOM)
    Bom {
        /// IPC-2581 XML file to inspect
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        #[arg(short, long, default_value = "text")]
        format: OutputFormat,
        /// Run in offline mode without fetching part availability
        #[cfg(feature = "api")]
        #[arg(long)]
        offline: bool,
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
        #[arg(short, long)]
        mode: ViewMode,
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: PathBuf,
    },
    /// Export board summary and stackup to HTML
    Html {
        /// IPC-2581 XML file to export
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Output HTML file path
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: Option<PathBuf>,
        /// Unit format for dimensions
        #[arg(short, long, default_value = "mm")]
        units: UnitFormat,
    },
    /// Export the board outline as a KiCad-importable DXF
    Outline {
        /// IPC-2581 XML file to export from
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Output DXF file path
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: PathBuf,
    },
    /// Render processed geometry for a single IPC-2581 layer
    Render {
        /// IPC-2581 XML file to render from
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Layer name to render, for example TOP or BOTTOM
        #[arg(short, long)]
        layer: String,
        /// Output file path. If omitted, auto renders to the terminal when possible.
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: Option<PathBuf>,
        /// Render format. Auto infers SVG/PNG from the output extension or uses terminal graphics.
        #[arg(short, long, default_value = "auto")]
        format: RenderFormat,
        /// Flatten the layer into a single Gerber-style mask before rendering.
        #[arg(long)]
        flat: bool,
    },
    /// Export IPC-2581 fabrication layers as Gerber X2 files
    Gerber {
        /// IPC-2581 XML file to export from
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Output directory for generated Gerber files
        #[arg(short, long, value_hint = clap::ValueHint::DirPath)]
        output: PathBuf,
    },
}

#[derive(Subcommand)]
enum EditCommands {
    /// Add manufacturer/MPN alternatives to BOM entries
    Bom {
        /// IPC-2581 XML file to enrich
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        rules: PathBuf,
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: Option<PathBuf>,
        #[arg(short = 'f', long, default_value = "text")]
        format: OutputFormat,
    },
}

pub fn execute(args: Ipc2581Args) -> anyhow::Result<()> {
    utils::color::init_color();

    match args.command {
        Commands::Info {
            file,
            format,
            units,
        } => commands::info::execute(&file, format, units),
        Commands::Bom {
            file,
            format,
            #[cfg(feature = "api")]
            offline,
        } => commands::bom::execute(&file, format, {
            #[cfg(feature = "api")]
            {
                offline
            }
            #[cfg(not(feature = "api"))]
            {
                true
            }
        }),
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
        Commands::Html {
            file,
            output,
            units,
        } => commands::html_export::execute(&file, output.as_deref(), units),
        Commands::Outline { file, output } => {
            commands::outline::execute(&file, &commands::outline::OutlineOptions { output })
        }
        Commands::Render {
            file,
            layer,
            output,
            format,
            flat,
        } => commands::render::execute(
            &file,
            &commands::render::RenderOptions {
                layer,
                output,
                format,
                flat,
            },
        ),
        Commands::Gerber { file, output } => {
            let set = gerber::execute_file(&file, &output)?;
            println!(
                "✓ IPC-2581 exported {} Gerber X2 file(s) to {}",
                set.files.len(),
                output.display()
            );
            Ok(())
        }
    }
}
