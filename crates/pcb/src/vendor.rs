use anyhow::Result;
use clap::Args;
use pcb_ui::{Colorize, Style, StyledText};
use pcb_zen::{get_workspace_info, resolve_dependencies, vendor_deps};
use pcb_zen_core::DefaultFileProvider;
use std::path::PathBuf;

#[derive(Args)]
pub struct VendorArgs {
    /// Path to .zen file or directory to analyze for dependencies.
    /// If a directory, will search recursively for .zen files.
    /// When omitted, uses the current directory.
    #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub zen_path: Option<PathBuf>,

    /// Continue vendoring even if some designs have build errors
    #[arg(long = "ignore-errors")]
    pub ignore_errors: bool,

    /// Vendor all dependencies instead of just those in [workspace.vendor]
    #[arg(long = "all")]
    pub all: bool,
}

pub fn execute(args: VendorArgs) -> Result<()> {
    let zen_path = args
        .zen_path
        .unwrap_or_else(|| std::env::current_dir().unwrap())
        .canonicalize()?;
    let mut workspace_info = get_workspace_info(&DefaultFileProvider::new(), &zen_path)?;
    log::debug!("V2 workspace detected - using closure-based vendoring");

    if !args.all {
        println!(
            "{} `pcb build` automatically vendors [workspace.vendor] dependencies.",
            "Note:".yellow()
        );
    }

    // Vendoring always needs network access (offline=false) and allows modifications (locked=false)
    let resolution = resolve_dependencies(&mut workspace_info, false, false)?;

    // If --all, vendor everything with ["**"] pattern
    // Otherwise, pass empty patterns to use only [workspace.vendor] config
    let additional_patterns: Vec<String> = if args.all {
        vec!["**".to_string()]
    } else {
        vec![]
    };

    // Always prune for explicit vendor command
    let result = vendor_deps(
        &workspace_info,
        &resolution,
        &additional_patterns,
        None,
        true,
    )?;

    if result.package_count == 0 && result.asset_count == 0 {
        println!("{} Vendor directory is up to date", "✓".green().bold());
    } else {
        println!(
            "{} {}",
            "✓".green().bold(),
            format!(
                "Vendored {} packages and {} assets",
                result.package_count, result.asset_count
            )
            .bold()
        );
    }
    println!(
        "Vendor directory: {}",
        result
            .vendor_dir
            .display()
            .to_string()
            .with_style(Style::Cyan)
    );

    Ok(())
}
