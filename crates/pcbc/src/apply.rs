use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use pcb_kicad_sch::apply::{SchematicApplyResult, apply_linked_schematic};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::{
    config_input::CONFIG_ARG_HELP,
    layout::{self, LayoutArgs, LayoutCommandResult, LayoutOutputFormat, PreparedDesign},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CompleteApplyResult<'a> {
    source_file: &'a Path,
    schematic: Option<&'a SchematicApplyResult>,
    layout: &'a LayoutCommandResult,
}

#[derive(Args, Debug)]
#[command(about = "Apply a Zener design to its linked KiCad project")]
#[command(args_conflicts_with_subcommands = true)]
pub struct ApplyArgs {
    #[command(subcommand)]
    command: Option<ApplyCommand>,

    /// Path to the .zen file when applying the complete project
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    file: Option<PathBuf>,

    #[arg(long = "config", value_name = "KEY=VALUE", help = CONFIG_ARG_HELP)]
    config: Vec<String>,

    /// Skip opening KiCad after applying the project
    #[arg(long)]
    no_open: bool,

    /// Disable network access
    #[arg(long)]
    offline: bool,

    /// Run KiCad DRC after applying the layout
    #[arg(long)]
    check: bool,

    /// Suppress diagnostics by kind or severity
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    suppress: Vec<String>,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value_t = LayoutOutputFormat::Human)]
    format: LayoutOutputFormat,
}

#[derive(Subcommand, Debug)]
enum ApplyCommand {
    /// Apply only the linked KiCad schematic
    Schematic(SchematicArgs),
    /// Apply only the linked KiCad layout
    Layout(LayoutArgs),
}

#[derive(Args, Debug)]
struct SchematicArgs {
    /// Path to a .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    file: PathBuf,

    #[arg(long = "config", value_name = "KEY=VALUE", help = CONFIG_ARG_HELP)]
    config: Vec<String>,

    /// Skip opening KiCad after applying the schematic
    #[arg(long)]
    no_open: bool,

    /// Disable network access
    #[arg(long)]
    offline: bool,

    /// Suppress diagnostics by kind or severity
    #[arg(short = 'S', long = "suppress", value_name = "KIND")]
    suppress: Vec<String>,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value_t = LayoutOutputFormat::Human)]
    format: LayoutOutputFormat,
}

pub fn execute(args: ApplyArgs) -> Result<()> {
    match args.command {
        Some(ApplyCommand::Layout(args)) => layout::execute(args),
        Some(ApplyCommand::Schematic(args)) => {
            let layout_args = schematic_layout_args(&args);
            let design = layout::prepare_design_for_apply(&layout_args)?;
            let result = apply_schematic(&design)?;
            print_schematic_result(result.as_ref(), args.format, &design.file_name)?;
            if !args.no_open
                && let Some(result) = &result
            {
                open_path(&result.root_schematic, "schematic")?;
            }
            Ok(())
        }
        None => {
            let file = args
                .file
                .context("pcb apply requires FILE, `schematic FILE`, or `layout FILE`")?;
            let layout_args = LayoutArgs {
                file,
                config: args.config,
                no_open: true,
                offline: args.offline,
                check: args.check,
                suppress: args.suppress,
                no_sync: false,
                format: args.format,
            };
            if crate::sandbox_uri::parse_sandbox_file_arg(&layout_args.file)?.is_some() {
                anyhow::bail!(
                    "pcb apply cannot update a remote schematic; use `pcb apply layout` for a remote sandbox"
                );
            }
            let design = layout::prepare_design_for_apply(&layout_args)?;
            let schematic = apply_schematic(&design)?;
            let layout = layout::apply_prepared(&layout_args, design)?;
            let project_to_open = if args.no_open || args.check {
                None
            } else {
                complete_project_file(schematic.as_ref(), &layout)?
            };
            print_complete_result(args.format, &layout_args.file, schematic.as_ref(), &layout)?;
            if let Some(project_file) = project_to_open {
                open_path(&project_file, "project")?;
            }
            Ok(())
        }
    }
}

fn schematic_layout_args(args: &SchematicArgs) -> LayoutArgs {
    LayoutArgs {
        file: args.file.clone(),
        config: args.config.clone(),
        no_open: true,
        offline: args.offline,
        check: false,
        suppress: args.suppress.clone(),
        no_sync: false,
        format: args.format,
    }
}

fn apply_schematic(design: &PreparedDesign) -> Result<Option<SchematicApplyResult>> {
    apply_linked_schematic(&design.schematic)
}

fn print_schematic_result(
    result: Option<&SchematicApplyResult>,
    format: LayoutOutputFormat,
    file_name: &str,
) -> Result<()> {
    match format {
        LayoutOutputFormat::Json => println!("{}", serde_json::to_string_pretty(&result)?),
        LayoutOutputFormat::Human => {
            if let Some(result) = result {
                let action = if result.created {
                    "created"
                } else if result.changed {
                    "updated"
                } else {
                    "unchanged"
                };
                println!(
                    "{} {} schematic {} ({})",
                    pcb_ui::icons::success(),
                    file_name.green().bold(),
                    action,
                    result.root_schematic.display()
                );
            }
        }
    }
    Ok(())
}

fn print_complete_result(
    format: LayoutOutputFormat,
    source_file: &Path,
    schematic: Option<&SchematicApplyResult>,
    layout: &LayoutCommandResult,
) -> Result<()> {
    match format {
        LayoutOutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&CompleteApplyResult {
                source_file,
                schematic,
                layout,
            })?
        ),
        LayoutOutputFormat::Human => {
            let file_name = source_file
                .file_name()
                .unwrap_or(source_file.as_os_str())
                .to_string_lossy();
            print_schematic_result(schematic, format, &file_name)?;
            layout::print_layout_result(layout, format)?;
        }
    }
    Ok(())
}

fn complete_project_file(
    schematic: Option<&SchematicApplyResult>,
    layout: &LayoutCommandResult,
) -> Result<Option<PathBuf>> {
    let schematic_project = schematic.map(|result| result.project_file.clone());
    let layout_project = layout
        .pcb_file
        .as_ref()
        .map(|path| path.with_extension("kicad_pro"));
    match (schematic_project, layout_project) {
        (Some(schematic), Some(layout)) if schematic != layout => anyhow::bail!(
            "linked schematic and layout use different KiCad projects: {} and {}",
            schematic.display(),
            layout.display()
        ),
        (Some(project), _) | (_, Some(project)) => Ok(Some(project)),
        (None, None) => Ok(None),
    }
}

fn open_path(path: &Path, kind: &str) -> Result<()> {
    open::that(path).with_context(|| format!("Failed to open KiCad {kind} {}", path.display()))
}
