use anyhow::{Context, Result};
use clap::Args;
use pcb_layout::utils;
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct OpenArgs {
    /// Path to .zen/.kicad_pcb file or diode:// sandbox URI
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Require that pcb.toml is up-to-date and verify pcb.sum if it exists.
    /// Does not write pcb.toml or pcb.sum. Recommended for CI.
    #[arg(long)]
    pub locked: bool,
}

pub fn execute(args: OpenArgs) -> Result<()> {
    if let Some(uri) = crate::sandbox_uri::parse_sandbox_file_arg(&args.file)? {
        crate::sandbox_uri::require_remote_openable_file(&uri)?;
        return crate::remote_sandbox::execute_open(uri, args);
    }

    if crate::sandbox_uri::is_kicad_pcb_path(&args.file) {
        return open_pcb_file(&args.file);
    }

    crate::file_walker::require_zen_file(&args.file)?;

    // Resolve dependencies before building
    let resolution_result = crate::resolve::resolve(Some(&args.file), args.offline, args.locked)?;

    let zen_path = &args.file;
    let file_name = zen_path.file_name().unwrap().to_string_lossy();

    // Evaluate the zen file
    let eval_result = pcb_zen::eval(zen_path, resolution_result, Default::default());

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

    open_pcb_file(&layout_path)?;

    Ok(())
}

fn open_pcb_file(path: &Path) -> Result<()> {
    pcb_kicad::open_pcbnew(path).with_context(|| {
        format!(
            "Failed to open file in KiCad PCB Editor: {}",
            path.display()
        )
    })?;
    Ok(())
}
