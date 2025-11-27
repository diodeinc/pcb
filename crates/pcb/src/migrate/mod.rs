use anyhow::Result;
use clap::Args;
use pcb_ui::prelude::*;
use pcb_zen_core::{config::find_workspace_root, DefaultFileProvider};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

pub mod codemods;

use crate::file_walker;
pub use codemods::MigrateContext;
use codemods::{
    alias_expansion::AliasExpansion, escape_paths::EscapePaths, manifest_v2,
    path_correction::PathCorrection, remove_directory_loads::RemoveDirectoryLoads,
    workspace_paths::WorkspacePaths, Codemod,
};

/// Arguments for the `migrate` command
#[derive(Args, Debug, Default, Clone)]
#[command(about = "Migrate PCB projects to V2 format")]
pub struct MigrateArgs {
    /// One or more .zen files or directories containing .zen files to migrate.
    /// When omitted, all .zen files in the current directory tree are considered.
    #[arg(value_name = "PATHS", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,
}

/// Execute the `migrate` command
pub fn execute(args: MigrateArgs) -> Result<()> {
    let start = if args.paths.is_empty() {
        std::env::current_dir()?
    } else {
        args.paths[0].clone()
    };

    let file_provider = Arc::new(DefaultFileProvider::new());

    // Step 1: Find workspace root
    eprintln!("Step 1: Detecting workspace root");
    let workspace_root = find_workspace_root(&*file_provider, &start);
    eprintln!("  Workspace root: {}", workspace_root.display());

    // Step 2: Detect git repository info
    eprintln!("\nStep 2: Detecting git repository");
    let repository = manifest_v2::detect_repository(&workspace_root)?;
    let repo_subpath = manifest_v2::detect_repo_subpath(&workspace_root)?;

    // Step 3: Convert pcb.toml files to V2 (must happen BEFORE .zen file discovery)
    eprintln!("\nStep 3: Converting pcb.toml files to V2");
    manifest_v2::convert_workspace_to_v2(&workspace_root, &repository, repo_subpath.as_deref())?;

    // Step 4: Discover .zen files (now that workspace is V2)
    eprintln!("\nStep 4: Discovering .zen files");
    let zen_files = file_walker::collect_zen_files(&[&workspace_root], false)?;
    eprintln!("  Found {} .zen files", zen_files.len());

    if zen_files.is_empty() {
        eprintln!("  No .zen files found, skipping codemods");
    } else {
        // Build context for codemods
        let ctx = MigrateContext {
            workspace_root: workspace_root.clone(),
            repository,
            repo_subpath,
        };

        // Step 5: Run all codemods on .zen files
        eprintln!("\nStep 5: Running codemods on .zen files");
        let codemods: Vec<Box<dyn Codemod>> = vec![
            Box::new(RemoveDirectoryLoads),
            Box::new(WorkspacePaths),
            Box::new(EscapePaths),
            Box::new(AliasExpansion),
            Box::new(PathCorrection),
        ];
        run_codemods(&ctx, &zen_files, &codemods)?;
    }

    eprintln!("\n✓ Migration complete");
    eprintln!("  Review changes with: git diff");
    eprintln!("  Run build to verify: pcb build");

    Ok(())
}

/// Run codemods on a list of .zen files
fn run_codemods(
    ctx: &MigrateContext,
    zen_paths: &[PathBuf],
    codemods: &[Box<dyn Codemod>],
) -> Result<()> {
    let mut has_errors = false;

    for path in zen_paths {
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());

        let mut spinner = Some(Spinner::builder(format!("{file_name}: Migrating")).start());

        let original = fs::read_to_string(path)?;
        let mut content = original.clone();
        let mut changed = false;

        let mut file_failed = false;
        for codemod in codemods {
            match codemod.apply(ctx, path, &content) {
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
            if let Err(e) = fs::write(path, content) {
                if let Some(sp) = spinner.take() {
                    sp.error(format!("{file_name}: Failed to write changes: {e}"));
                } else {
                    Spinner::builder(format!("{file_name}: Migrating"))
                        .start()
                        .error(format!("{file_name}: Failed to write changes: {e}"));
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
        } else if let Some(sp) = spinner.take() {
            sp.finish();
        }
    }

    if has_errors {
        anyhow::bail!("Migrate failed with errors");
    }

    Ok(())
}
