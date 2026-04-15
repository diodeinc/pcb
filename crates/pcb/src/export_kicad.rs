use anyhow::{Context, Result, bail};
use clap::Args;
use pcb_layout::process_layout;
use pcb_ui::prelude::*;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::build::{build, create_diagnostics_passes};

#[derive(Args, Debug, Clone)]
#[command(about = "Export a .zen design as a standalone KiCad project")]
pub struct ExportKicadArgs {
    /// Path to .zen file
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Output directory for the exported KiCad project
    #[arg(short = 'o', long = "output", value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub output: PathBuf,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,

    /// Require that pcb.toml is up-to-date and verify pcb.sum if it exists.
    #[arg(long)]
    pub locked: bool,
}

pub fn execute(args: ExportKicadArgs) -> Result<()> {
    crate::file_walker::require_zen_file(&args.file)?;

    let locked = args.locked || std::env::var("CI").is_ok();
    let resolution_result = crate::resolve::resolve(Some(&args.file), args.offline, locked)?;
    let model_dirs = resolution_result.kicad_model_dirs();

    let zen_path = &args.file;
    let file_name = zen_path.file_name().unwrap().to_string_lossy().to_string();

    let Some(schematic) = build(
        zen_path,
        Default::default(),
        create_diagnostics_passes(&[], &[]),
        false,
        &mut false,
        &mut false,
        resolution_result,
    ) else {
        bail!("Build failed");
    };

    let spinner = Spinner::builder(format!("{file_name}: Exporting KiCad project")).start();
    let mut diagnostics = pcb_zen_core::Diagnostics::default();
    let result = process_layout(
        &schematic,
        &model_dirs,
        true,  // use_temp_dir: always export to a fresh temp dir, ignoring any layout_path on the design
        false, // check_mode
        &mut diagnostics,
    )?;
    spinner.finish();

    let Some(layout_result) = result else {
        bail!("Export failed: process_layout produced no result");
    };

    copy_dir_contents(&layout_result.layout_dir, &args.output).with_context(|| {
        format!(
            "Failed to copy exported project to {}",
            args.output.display()
        )
    })?;

    // process_layout(use_temp_dir=true) leaks its working directory via TempDir::keep().
    // We just copied everything we need out, so remove the source to avoid piling up
    // orphaned directories under $TMPDIR across CLI invocations and test runs.
    let _ = std::fs::remove_dir_all(&layout_result.layout_dir);

    crate::drc::render_diagnostics(&mut diagnostics, &[]);
    if diagnostics.error_count() > 0 {
        bail!("Export failed with errors");
    }

    println!(
        "{} {} ({})",
        pcb_ui::icons::success(),
        file_name.with_style(Style::Green).bold(),
        args.output.display()
    );
    Ok(())
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)
        .with_context(|| format!("Failed to create {}", dst.display()))?;
    for entry in WalkDir::new(src) {
        let entry = entry?;
        let rel = entry
            .path()
            .strip_prefix(src)
            .expect("walkdir entry is under src");
        if rel.as_os_str().is_empty() {
            continue;
        }
        let dest_path = dst.join(rel);
        let ft = entry.file_type();
        if ft.is_dir() {
            std::fs::create_dir_all(&dest_path)
                .with_context(|| format!("Failed to create {}", dest_path.display()))?;
        } else if ft.is_file() {
            if let Some(parent) = dest_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create {}", parent.display()))?;
            }
            std::fs::copy(entry.path(), &dest_path).with_context(|| {
                format!(
                    "Failed to copy {} -> {}",
                    entry.path().display(),
                    dest_path.display()
                )
            })?;
        }
    }
    Ok(())
}
