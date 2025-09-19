use anyhow::Result;
use clap::Args;
use pcb_fmt::RuffFormatter;
use pcb_ui::prelude::*;
use std::fs;
use std::path::PathBuf;

pub mod codemods;

/// Arguments for the `upgrade` command
#[derive(Args, Debug, Default, Clone)]
#[command(about = "Upgrade PCB projects from .zen files")]
pub struct UpgradeArgs {
    /// One or more .zen files or directories containing .zen files to upgrade.
    /// When omitted, all .zen files in the current directory tree are considered.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,
}

/// Execute the `upgrade` command
pub fn execute(args: UpgradeArgs) -> Result<()> {
    // Initialize ruff formatter once to format files after upgrades
    let formatter = RuffFormatter::default();
    // Determine target files - always recursive for directories
    let zen_paths = crate::file_walker::collect_zen_files(&args.paths, false)?;

    if zen_paths.is_empty() {
        let cwd = std::env::current_dir()?;
        anyhow::bail!(
            "No .zen source files found in {}",
            cwd.canonicalize().unwrap_or(cwd).display()
        );
    }

    // Initialize codemods sequence
    let codemods: Vec<Box<dyn codemods::Codemod>> = vec![Box::new(
        codemods::remove_directory_loads::RemoveDirectoryLoads,
    )];

    let mut has_errors = false;

    for path in zen_paths {
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());

        let mut spinner = Some(Spinner::builder(format!("{file_name}: Upgrading")).start());

        let original = fs::read_to_string(&path)?;
        let mut content = original.clone();
        let mut changed = false;

        let mut file_failed = false;
        for codemod in &codemods {
            match codemod.apply(&path, &content) {
                Ok(Some(updated)) => {
                    content = updated;
                    changed = true;
                }
                Ok(None) => {}
                Err(e) => {
                    if let Some(sp) = spinner.take() {
                        sp.error(format!("{file_name}: {e}"));
                    }
                    has_errors = true;
                    file_failed = true;
                    break;
                }
            }
        }

        if file_failed {
            continue;
        }

        if changed && content != original {
            if let Err(e) = fs::write(&path, content) {
                if let Some(sp) = spinner.take() {
                    sp.error(format!("{file_name}: Failed to write changes: {e}"));
                } else {
                    Spinner::builder(format!("{file_name}: Upgrading"))
                        .start()
                        .error(format!("{file_name}: Failed to write changes: {e}"));
                }
                has_errors = true;
                continue;
            }
            // Format file after successful write
            if let Err(e) = formatter.format_file(&path) {
                if let Some(sp) = spinner.take() {
                    sp.error(format!("{file_name}: Format failed: {e}"));
                } else {
                    Spinner::builder(format!("{file_name}: Upgrading"))
                        .start()
                        .error(format!("{file_name}: Format failed: {e}"));
                }
                has_errors = true;
                continue;
            }
            if let Some(sp) = spinner.take() {
                sp.finish();
            }
            eprintln!(
                "{} {}",
                pcb_ui::icons::success(),
                file_name.with_style(Style::Green).bold()
            );
        } else {
            if let Some(sp) = spinner.take() {
                sp.finish();
            }
            eprintln!(
                "{} {} (no changes)",
                pcb_ui::icons::success(),
                file_name.with_style(Style::Green).bold()
            );
        }
    }

    if has_errors {
        anyhow::bail!("Upgrade failed with errors");
    }

    Ok(())
}
