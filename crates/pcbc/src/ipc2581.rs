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
    /// Generate the test-fixture interposer board for a board-array panel
    Interposer {
        /// Board-array IPC-2581 XML file (`board-array create` output)
        #[arg(value_hint = clap::ValueHint::FilePath)]
        input: PathBuf,
        /// Output `.kicad_pcb` path; a sibling `.kicad_pro` is written too
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: PathBuf,
        /// Also write the fixture map (tested boards + contact→land bindings) as JSON
        #[arg(long, value_hint = clap::ValueHint::FilePath)]
        fixture_map: Option<PathBuf>,
        /// Auto-route the interposer with FreeRouting (requires Java 25+)
        #[arg(long)]
        route: bool,
        /// Also export the finished board as manufacturing IPC-2581 XML
        #[arg(long, value_hint = clap::ValueHint::FilePath)]
        ipc: Option<PathBuf>,
    },
    /// List ICT fixture contacts (components with an `Ict` BOM characteristic)
    Ict {
        /// IPC-2581 XML file to export from
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Output CSV file path. If omitted, writes CSV to stdout.
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: Option<PathBuf>,
        /// Component side to include
        #[arg(long, default_value = "both")]
        side: commands::cpl::CplSideFilter,
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
    /// Tile assembly panels into a fabrication panel
    FabPanel {
        #[command(subcommand)]
        command: FabPanelCommands,
    },
    /// Export a filtered IPC-2581 function-mode view. Fabrication mode strips non-manufacturing data.
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
        /// What to export: the canonical board, or the file's root step with every repeat materialized.
        #[arg(long, default_value = "board-array")]
        layout_target: LayoutTarget,
        /// Also draw nested assembly-panel boundaries, not just the fabrication outlines.
        #[arg(long)]
        nested_outlines: bool,
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
        /// What to render: the canonical board, or the file's root step with every repeat materialized.
        #[arg(long, default_value = "board-array")]
        layout_target: LayoutTarget,
    },
    /// Check IPC-2581 geometry against a fabrication process design kit
    Dfm {
        #[command(subcommand)]
        command: DfmCommands,
    },
    /// Estimate panel bow and twist from the through-stack copper distribution
    Warp {
        /// IPC-2581 XML file to analyze
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Write a field report, with the copper and deflection maps, here
        #[arg(long, value_hint = clap::ValueHint::FilePath)]
        report: Option<PathBuf>,
    },
    /// Export IPC-2581 fabrication layers as manufacturing files
    Gerber {
        /// IPC-2581 XML file to export from
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// What to export: the canonical board, or the file's root step with every repeat materialized.
        #[arg(long, default_value = "board-array")]
        layout_target: LayoutTarget,
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
    /// Apply manufacturer, MPN, and supplier selections to BOM entries
    Bom {
        /// IPC-2581 XML file to hydrate
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// JSON file containing path-based manufacturer, MPN, and supplier selections
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
enum DfmCommands {
    /// Run PDK-derived DFM checks and emit a report
    Check {
        /// IPC-2581 XML file to check
        #[arg(value_hint = clap::ValueHint::FilePath)]
        file: PathBuf,
        /// Built-in PDK name or fabrication PDK TOML path (built-ins: standard)
        #[arg(long)]
        pdk: PathBuf,
        /// Waiver file of accepted finding ids (TOML)
        #[arg(long, value_hint = clap::ValueHint::FilePath)]
        waivers: Option<PathBuf>,
        /// What to check: the canonical board, or the file's root step with every repeat materialized.
        #[arg(long, default_value = "board-array")]
        layout_target: LayoutTarget,
        /// Output report path. Omit to write JSON to stdout.
        #[arg(short, long, value_hint = clap::ValueHint::FilePath)]
        output: Option<PathBuf>,
    },
}

#[derive(Args, Debug, Clone, Copy, Default)]
struct CopperBalanceArgs {
    /// Enable automatic copper balancing.
    #[arg(long, conflicts_with = "no_copper_balance")]
    copper_balance: bool,
    /// Disable automatic copper balancing.
    #[arg(long, conflicts_with = "copper_balance")]
    no_copper_balance: bool,
}

impl CopperBalanceArgs {
    fn resolve(self, default: bool) -> bool {
        if self.copper_balance {
            true
        } else if self.no_copper_balance {
            false
        } else {
            default
        }
    }
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
        #[command(flatten)]
        copper_balance: CopperBalanceArgs,
        /// Output IPC-2581 XML file, or '-' for stdout
        #[arg(short, long, value_hint = clap::ValueHint::AnyPath)]
        output: PathBuf,
    },
}

#[derive(Subcommand)]
enum FabPanelCommands {
    /// Create a fabrication panel with reserved process margins
    Create {
        /// Assembly panel IPC-2581 files. Repeat a path to request multiple copies.
        #[arg(
            required = true,
            num_args = 1..=32,
            value_name = "ASSEMBLY_PANEL",
            value_hint = clap::ValueHint::FilePath
        )]
        inputs: Vec<PathBuf>,
        /// Fabrication panel size in inches. Defaults to 18x24.
        #[arg(long, value_enum, value_name = "SIZE")]
        panel_size: Option<FabPanelSize>,
        /// Edge margin in millimeters. Defaults to 50.8 vertical and 25.4 horizontal. Uses CSS shorthand: all | vertical horizontal | top horizontal bottom | top right bottom left.
        #[arg(long, num_args = 1..=4, value_name = "MARGIN")]
        edge_margin: Vec<f64>,
        /// Gap between assembly panels in millimeters. Defaults to 7.62.
        #[arg(long, value_name = "GAP")]
        panel_gap: Option<f64>,
        /// Emit only the usable packing area and rebase it to the origin.
        #[arg(long)]
        emit_usable_area: bool,
        #[command(flatten)]
        copper_balance: CopperBalanceArgs,
        /// Output IPC-2581 XML file, or '-' for stdout
        #[arg(short, long, value_hint = clap::ValueHint::AnyPath)]
        output: PathBuf,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum FabPanelSize {
    #[value(name = "12x18")]
    Inches12x18,
    #[value(name = "16x18")]
    Inches16x18,
    #[value(name = "18x24")]
    Inches18x24,
    #[value(name = "21x24")]
    Inches21x24,
}

impl FabPanelSize {
    fn spec(self) -> commands::fab_panel::FabPanelSpec {
        match self {
            Self::Inches12x18 => commands::fab_panel::FabPanelSpec::INCHES_12_X_18,
            Self::Inches16x18 => commands::fab_panel::FabPanelSpec::INCHES_16_X_18,
            Self::Inches18x24 => commands::fab_panel::FabPanelSpec::INCHES_18_X_24,
            Self::Inches21x24 => commands::fab_panel::FabPanelSpec::INCHES_21_X_24,
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
        Commands::Interposer {
            input,
            output,
            fixture_map,
            route,
            ipc,
        } => {
            use anyhow::Context as _;
            let (pcb, pro) = pcb_interposer::generate(&input)?;
            std::fs::write(&output, pcb).with_context(|| format!("write {}", output.display()))?;
            let pro_path = output.with_extension("kicad_pro");
            std::fs::write(&pro_path, pro)
                .with_context(|| format!("write {}", pro_path.display()))?;
            println!(
                "✓ Wrote interposer {} and {}",
                output.display(),
                pro_path.display()
            );
            if let Some(map_path) = fixture_map {
                let map = pcb_interposer::fixture_map(&input)?;
                std::fs::write(&map_path, map)
                    .with_context(|| format!("write {}", map_path.display()))?;
                println!("✓ Wrote fixture map {}", map_path.display());
            }
            if route {
                let board_name = output
                    .file_stem()
                    .context("output path has no file name")?
                    .to_string_lossy()
                    .to_string();
                let route_args = crate::route::RouteArgs {
                    file: input,
                    engine: crate::route::RouteEngine::Freerouting,
                    no_open: true,
                    timeout: 20,
                    project_id: None,
                };
                // Stitch whatever board state routing left behind — a
                // partial result published before a routing error still
                // gets a coherent GND — then surface the routing error.
                let routed =
                    crate::freerouting::execute(&route_args, &output, &pro_path, &board_name);
                let vias = pcb_interposer::stitch::stitch(&output)?;
                println!("✓ Stitched {vias} GND vias");
                routed?;
            }
            if let Some(ipc_path) = ipc {
                pcb_kicad::KiCadCliBuilder::new()
                    .command("pcb")
                    .subcommand("export")
                    .subcommand("ipc2581")
                    .arg("--output")
                    .arg(ipc_path.to_string_lossy())
                    .arg("--bom-col-int-id")
                    .arg("Path")
                    .arg("--bom-col-mfg-pn")
                    .arg("Mpn")
                    .arg("--bom-col-mfg")
                    .arg("Manufacturer")
                    .arg(output.to_string_lossy())
                    .run()
                    .context("export interposer IPC-2581")?;
                println!("✓ Wrote IPC-2581 {}", ipc_path.display());
            }
            Ok(())
        }
        Commands::Ict { file, output, side } => {
            commands::ict::execute(&file, &commands::ict::IctOptions { output, side })
        }
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
                copper_balance,
                output,
            } => {
                let copper_balance = copper_balance.resolve(true);
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
                    commands::board_array::execute_auto(&input, &output, sheet, copper_balance)
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
                        copper_balance,
                    )
                }
            }
        },
        Commands::FabPanel { command } => match command {
            FabPanelCommands::Create {
                inputs,
                panel_size,
                edge_margin,
                panel_gap,
                emit_usable_area,
                copper_balance,
                output,
            } => {
                let mut spec = panel_size.map(FabPanelSize::spec).unwrap_or_default();
                if !edge_margin.is_empty() {
                    spec.edge_margin_mm = commands::EdgeInsetsMm::from_css_shorthand_named(
                        "edge margin",
                        &edge_margin,
                    )?;
                }
                if let Some(panel_gap) = panel_gap {
                    spec.panel_gap_mm = panel_gap;
                }
                spec.emit_usable_area = emit_usable_area;
                commands::fab_panel::execute(&inputs, &output, spec, copper_balance.resolve(false))
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
            nested_outlines,
            output,
        } => commands::outline::execute(
            &file,
            &commands::outline::OutlineOptions {
                output,
                layout_target,
                nested_outlines,
            },
        ),
        Commands::Render {
            file,
            layer,
            output,
            format,
            layout_target,
        } => commands::render::execute(
            &file,
            &commands::render::RenderOptions {
                layer,
                output,
                format,
                layout_target,
            },
        ),
        Commands::Dfm { command } => match command {
            DfmCommands::Check {
                file,
                pdk,
                waivers,
                layout_target,
                output,
            } => commands::dfm::execute_check(
                &file,
                &commands::dfm::CheckOptions {
                    pdk,
                    waivers,
                    output,
                    layout_target,
                },
            ),
        },
        Commands::Warp { file, report } => commands::warp::execute(&file, report.as_deref()),
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
                    view: layout_target.artwork_scope(),
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
