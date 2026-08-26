use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use colored::Colorize;
use pcbc::kicad_schematic::{SchematicApplyResult, apply_linked_schematic};
use serde::Serialize;
use std::path::{Path, PathBuf};

use crate::{
    config_input::CONFIG_ARG_HELP,
    layout::{self, LayoutArgs, LayoutCommandResult, LayoutOutputFormat},
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

    #[command(flatten)]
    shared: SharedApplyArgs,

    /// Run KiCad DRC after applying the layout
    #[arg(long)]
    check: bool,
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

    #[command(flatten)]
    shared: SharedApplyArgs,
}

/// Options shared by every `pcb apply` form.
#[derive(Args, Debug, Clone)]
struct SharedApplyArgs {
    #[arg(long = "config", value_name = "KEY=VALUE", help = CONFIG_ARG_HELP)]
    config: Vec<String>,

    /// Skip opening KiCad after applying
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

impl SharedApplyArgs {
    /// The layout invocation equivalent to these apply options. Unrelated
    /// layout flags keep their defaults via struct update.
    fn layout_args(&self, file: PathBuf, check: bool) -> LayoutArgs {
        LayoutArgs {
            file,
            config: self.config.clone(),
            no_open: true,
            offline: self.offline,
            check,
            suppress: self.suppress.clone(),
            format: self.format,
            ..LayoutArgs::default()
        }
    }
}

pub fn execute(args: ApplyArgs) -> Result<()> {
    match args.command {
        Some(ApplyCommand::Layout(args)) => layout::execute(args),
        Some(ApplyCommand::Schematic(args)) => {
            let layout_args = args.shared.layout_args(args.file.clone(), false);
            let design = layout::prepare_design_for_apply(&layout_args)?;
            let result = apply_linked_schematic(&design.schematic)?;
            print_schematic_result(result.as_ref(), args.shared.format, &design.file_name)?;
            if !args.shared.no_open
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
            let layout_args = args.shared.layout_args(file, args.check);
            if crate::sandbox_uri::parse_sandbox_file_arg(&layout_args.file)?.is_some() {
                anyhow::bail!(
                    "pcb apply cannot update a remote schematic; use `pcb apply layout` for a remote sandbox"
                );
            }
            let design = layout::prepare_design_for_apply(&layout_args)?;
            let schematic = apply_linked_schematic(&design.schematic)?;
            let layout = layout::apply_prepared(&layout_args, design)?;
            let project_to_open = if args.shared.no_open || args.check {
                None
            } else {
                complete_project_file(schematic.as_ref(), &layout)?
            };
            print_complete_result(
                args.shared.format,
                &layout_args.file,
                schematic.as_ref(),
                &layout,
            )?;
            layout::run_drc_check(&layout_args, &layout)?;
            if let Some(project_file) = project_to_open {
                open_path(&project_file, "project")?;
            }
            Ok(())
        }
    }
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
