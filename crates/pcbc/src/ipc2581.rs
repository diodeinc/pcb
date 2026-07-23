use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;

use pcb_ipc2581_tools::{
    LayoutTarget, OutputFormat, RenderFormat, UnitFormat, ViewMode, commands, manufacturing, utils,
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
    /// Generate component placement data (CPL)
    Cpl {
        /// IPC-2581 XML file to export from
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Output CSV file path. If omitted, writes CSV to stdout.
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: Option<PathBuf>,
        /// Component side to include
        #[arg(long, default_value = "both")]
        side: commands::cpl::CplSideFilter,
        /// Exclude BOM RefDes entries marked populate=false
        #[arg(long)]
        exclude_dnp: bool,
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
    /// Check exported Gerber geometry for manufacturability slivers
    Dfm {
        /// IPC-2581 XML file to check
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Layout target to check. Supports board or board-array.
        #[arg(long, default_value = "board")]
        layout_target: GerberLayoutTarget,
        /// Minimum feature and gap width in millimeters
        #[arg(long, default_value_t = 0.09)]
        min_width_mm: f64,
    },
    /// Export IPC-2581 fabrication layers as manufacturing files
    Gerber {
        /// IPC-2581 XML file to export from
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Layout target to export. Manufacturing export supports board or board-array.
        #[arg(long, default_value = "board")]
        layout_target: GerberLayoutTarget,
        /// Output directory, or a .zip file for an archived manufacturing package
        #[arg(short, long, value_hint = clap::ValueHint::AnyPath)]
        output: PathBuf,
        /// Write V-score relief debug SVGs to this directory.
        #[arg(long, hide = true, value_hint = clap::ValueHint::DirPath)]
        debug_reliefs: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum EditCommands {
    /// Apply manufacturer/MPN selections to BOM entries
    Bom {
        /// IPC-2581 XML file to hydrate
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// JSON file containing path-based manufacturer/MPN selections
        #[arg(
            short,
            long,
            value_hint = clap::ValueHint::FilePath,
            required_unless_present = "rules",
            conflicts_with = "rules"
        )]
        selections: Option<PathBuf>,
        /// Deprecated: JSON file containing BOM matching rules
        #[arg(
            short,
            long,
            value_hint = clap::ValueHint::FilePath,
            required_unless_present = "selections",
            conflicts_with = "selections"
        )]
        rules: Option<PathBuf>,
        /// Output IPC-2581 XML file
        #[arg(
            short,
            long,
            value_hint = clap::ValueHint::FilePath,
            required_unless_present = "rules"
        )]
        output: Option<PathBuf>,
        /// Deprecated output format option retained for rules clients
        #[arg(short = 'f', long, default_value = "text", hide = true)]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
enum BoardArrayCommands {
    /// Create a rectangular board array. Generated array size must be 70-297 mm per side.
    Create {
        /// Input IPC-2581 XML file
        #[arg(value_hint = clap::ValueHint::FilePath)]
        input: PathBuf,
        /// Choose the smallest fitting A-series board array automatically.
        #[arg(long)]
        auto: bool,
        /// Force auto board array generation to one A-series sheet size. Implies --auto.
        #[arg(long)]
        sheet: Option<commands::board_array_auto::AutoSheetSize>,
        /// Number of board columns. Must be between 1 and 10. Defaults to 1.
        #[arg(long)]
        columns: Option<u32>,
        /// Number of board rows. Must be between 1 and 10. Defaults to 1.
        #[arg(long)]
        rows: Option<u32>,
        /// Board margin in millimeters. Defaults to 5. Uses CSS shorthand: all | vertical horizontal | top horizontal bottom | top right bottom left.
        #[arg(long, num_args = 1..=4, value_name = "MARGIN")]
        board_margin: Vec<f64>,
        /// Edge rail in millimeters. Defaults to 5. Uses CSS shorthand: all | vertical horizontal | top horizontal bottom | top right bottom left.
        #[arg(long, num_args = 1..=4, value_name = "RAIL")]
        edge_rail: Vec<f64>,
        /// Output IPC-2581 XML file, or '-' for stdout
        #[arg(short, long, value_hint = clap::ValueHint::AnyPath)]
        output: PathBuf,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum GerberLayoutTarget {
    Board,
    #[value(name = "board-array", alias = "panel")]
    BoardArray,
}

impl From<GerberLayoutTarget> for pcb_ir::dialects::ipc::View {
    fn from(target: GerberLayoutTarget) -> Self {
        match target {
            GerberLayoutTarget::Board => Self::Board,
            GerberLayoutTarget::BoardArray => Self::ArrayFlattened,
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
        Commands::Cpl {
            file,
            output,
            side,
            exclude_dnp,
        } => commands::cpl::execute(
            &file,
            &commands::cpl::CplOptions {
                output,
                side,
                exclude_dnp,
            },
        ),
        Commands::Edit { command } => match command {
            EditCommands::Bom {
                file,
                selections,
                rules,
                output,
                ..
            } => {
                if let Some(rules) = rules {
                    commands::bom_edit::execute_rules(&file, &rules, output.as_deref())
                } else {
                    commands::bom_edit::execute_selections(
                        &file,
                        selections
                            .as_deref()
                            .expect("clap requires either --selections or --rules"),
                        output
                            .as_deref()
                            .expect("--output is required with --selections"),
                    )
                }
            }
        },
        Commands::BoardArray { command } => match command {
            BoardArrayCommands::Create {
                input,
                auto,
                sheet,
                columns,
                rows,
                board_margin,
                edge_rail,
                output,
            } => {
                if auto || sheet.is_some() {
                    if columns.is_some()
                        || rows.is_some()
                        || !board_margin.is_empty()
                        || !edge_rail.is_empty()
                    {
                        anyhow::bail!(
                            "--auto/--sheet cannot be combined with manual board array options"
                        );
                    }
                    commands::board_array::execute_auto(&input, &output, sheet)
                } else {
                    let board_margin_mm = if board_margin.is_empty() {
                        commands::board_array::BoardMarginMm::all(5.0)
                    } else {
                        commands::board_array::BoardMarginMm::from_css_shorthand(&board_margin)?
                    };
                    let edge_rail_mm = if edge_rail.is_empty() {
                        commands::board_array::BoardMarginMm::all(5.0)
                    } else {
                        commands::board_array::BoardMarginMm::from_css_shorthand_named(
                            "edge rail",
                            &edge_rail,
                        )?
                    };
                    commands::board_array::execute(
                        &input,
                        &output,
                        &commands::board_array::BoardArrayCreateOptions {
                            columns: columns.unwrap_or(1),
                            rows: rows.unwrap_or(1),
                            board_margin_mm,
                            edge_rail_mm,
                        },
                    )
                }
            }
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
        Commands::Dfm {
            file,
            layout_target,
            min_width_mm,
        } => commands::dfm::execute(&file, layout_target.into(), min_width_mm),
        Commands::Gerber {
            file,
            layout_target,
            output,
            debug_reliefs,
        } => {
            let package = manufacturing::execute_file_with_options(
                &file,
                &manufacturing::ManufacturingExportOptions {
                    output: output.clone(),
                    view: layout_target.into(),
                    relief_debug_dir: debug_reliefs,
                },
            )?;
            println!(
                "✓ IPC-2581 exported {} manufacturing file(s) to {}",
                package.files.len(),
                output.display()
            );
            Ok(())
        }
    }
}
