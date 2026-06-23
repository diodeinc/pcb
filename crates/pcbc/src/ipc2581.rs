use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;

use pcb_ipc2581_tools::{
    LayoutTarget, OutputFormat, RenderFormat, UnitFormat, ViewMode, commands, gerber, utils,
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
        #[arg(long)]
        offline: bool,
    },
    /// Edit IPC-2581 data
    Edit {
        #[command(subcommand)]
        command: EditCommands,
    },
    /// Create and inspect IPC-2581 board array data
    #[command(alias = "panel")]
    BoardArray {
        #[command(subcommand)]
        command: BoardArrayCommands,
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
    /// Export IPC-2581 outlines as a KiCad-importable DXF
    Outline {
        /// IPC-2581 XML file to export from
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Layout target to export
        #[arg(long, default_value = "board")]
        layout_target: LayoutTarget,
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
        /// Layout target to render
        #[arg(long, default_value = "layout")]
        layout_target: LayoutTarget,
        /// Flatten the layer into a single Gerber-style mask before rendering.
        #[arg(long)]
        flat: bool,
    },
    /// Export IPC-2581 fabrication layers as Gerber X2 files
    Gerber {
        /// IPC-2581 XML file to export from
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Layout target to export. Gerber supports board or board-array.
        #[arg(long, default_value = "board")]
        layout_target: GerberLayoutTarget,
        /// Output directory, or a .zip file for an archived Gerber package
        #[arg(short, long, value_hint = clap::ValueHint::AnyPath)]
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

#[derive(Subcommand)]
enum BoardArrayCommands {
    /// Create a rectangular board array. Generated array size must be 70-260 mm per side.
    Create {
        /// Input IPC-2581 XML file
        #[arg(value_hint = clap::ValueHint::FilePath)]
        input: PathBuf,
        /// Number of board columns. Must be between 1 and 10.
        #[arg(long)]
        columns: u32,
        /// Number of board rows. Must be between 1 and 10.
        #[arg(long)]
        rows: u32,
        /// Spacing between board columns, in millimeters. Must be 0 or between 5 and 20.
        #[arg(long)]
        column_spacing: f64,
        /// Spacing between board rows, in millimeters. Must be 0 or between 5 and 20.
        #[arg(long)]
        row_spacing: f64,
        /// Uniform edge rail width, in millimeters. Must be between 5 and 30.
        #[arg(long)]
        edge_rail_width: f64,
        /// Output IPC-2581 XML file
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: PathBuf,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum GerberLayoutTarget {
    Board,
    #[value(name = "board-array", alias = "panel")]
    BoardArray,
}

impl From<GerberLayoutTarget> for LayoutTarget {
    fn from(target: GerberLayoutTarget) -> Self {
        match target {
            GerberLayoutTarget::Board => Self::Board,
            GerberLayoutTarget::BoardArray => Self::BoardArray,
        }
    }
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
            offline,
        } => commands::bom::execute(&file, format, offline),
        Commands::Edit { command } => match command {
            EditCommands::Bom {
                file,
                rules,
                output,
                ..
            } => commands::bom_edit::execute(&file, &rules, output.as_deref()),
        },
        Commands::BoardArray { command } => match command {
            BoardArrayCommands::Create {
                input,
                columns,
                rows,
                column_spacing,
                row_spacing,
                edge_rail_width,
                output,
            } => commands::board_array::execute(
                &input,
                &output,
                &commands::board_array::BoardArrayCreateOptions {
                    columns,
                    rows,
                    column_spacing_mm: column_spacing,
                    row_spacing_mm: row_spacing,
                    edge_rail_width_mm: edge_rail_width,
                },
            ),
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
        Commands::Outline {
            file,
            layout_target,
            output,
        } => commands::outline::execute(
            &file,
            &commands::outline::OutlineOptions {
                output,
                layout_target,
            },
        ),
        Commands::Render {
            file,
            layer,
            output,
            format,
            layout_target,
            flat,
        } => commands::render::execute(
            &file,
            &commands::render::RenderOptions {
                layer,
                output,
                format,
                layout_target,
                flat,
            },
        ),
        Commands::Gerber {
            file,
            layout_target,
            output,
        } => {
            let set = gerber::execute_file_with_options(
                &file,
                &gerber::GerberExportOptions {
                    output: output.clone(),
                    layout_target: layout_target.into(),
                },
            )?;
            println!(
                "✓ IPC-2581 exported {} Gerber X2 file(s) to {}",
                set.files.len(),
                output.display()
            );
            Ok(())
        }
    }
}
