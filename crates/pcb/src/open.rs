use anyhow::{Context, Result};
use clap::Args;
use pcb_layout::utils;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct OpenArgs {
    /// Path to .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Require that pcb.toml and pcb.sum are up-to-date. Fails if auto-deps would
    /// add dependencies or if the lockfile would be modified. Recommended for CI.
    #[arg(long)]
    pub locked: bool,
}

pub fn execute(args: OpenArgs) -> Result<()> {
    crate::file_walker::require_zen_file(&args.file)?;

    // Resolve dependencies before building
    let resolution_result = crate::resolve::resolve(Some(&args.file), args.offline, args.locked)?;

    let zen_path = &args.file;
    let file_name = zen_path.file_name().unwrap().to_string_lossy();

    // Evaluate the zen file
    let eval_result = pcb_zen::eval(zen_path, resolution_result);

    let output = eval_result
        .output_result()
        .map_err(|_| anyhow::anyhow!("Build failed for {}", file_name))?;

    let schematic = output.to_schematic()?;
    let layout_dir = utils::resolve_layout_dir(&schematic)?
        .ok_or_else(|| anyhow::anyhow!("No layout path defined in {}", file_name))?;

    let kicad_files = utils::require_kicad_files(&layout_dir)?;
    let layout_path = kicad_files.kicad_pcb();
    if !layout_path.exists() {
        anyhow::bail!(
            "Layout file not found: {}. Run 'pcb layout {}' to generate it.",
            layout_path.display(),
            zen_path.display()
        );
    }

    open::that(&layout_path)
        .with_context(|| format!("Failed to open file: {}", layout_path.display()))?;

    Ok(())
}
