use anyhow::Result;
use pcb_zen_core::{config::find_workspace_root, DefaultFileProvider};
use std::path::PathBuf;
use std::sync::Arc;

mod alias_expansion;
mod escape_paths;
mod manifest;
mod path_correction;
mod zen_paths;

pub fn migrate_to_v2(paths: &[PathBuf]) -> Result<()> {
    let start = if paths.is_empty() {
        std::env::current_dir()?
    } else {
        paths[0].clone()
    };

    let file_provider = Arc::new(DefaultFileProvider::new());

    // Phase 1: Find workspace root (reuse existing function)
    eprintln!("Phase 1: Detecting workspace root");
    let workspace_root = find_workspace_root(&*file_provider, &start);
    eprintln!("  Workspace root: {}", workspace_root.display());

    // Phase 2: Convert pcb.toml to V2
    eprintln!("\nPhase 2: Converting pcb.toml files to V2");
    let (repository, workspace_path) = manifest::convert_workspace_to_v2(&workspace_root)?;

    // Phase 3: Convert workspace-relative paths in .zen files
    eprintln!("\nPhase 3: Converting workspace-relative paths in .zen files");
    zen_paths::convert_workspace_paths(&workspace_root)?;

    // Phase 4: Convert cross-package relative paths to URLs
    eprintln!("\nPhase 4: Converting cross-package paths to URLs");
    escape_paths::convert_escape_paths(&workspace_root, &repository, workspace_path.as_deref())?;

    // Phase 5: Expand hardcoded aliases (@registry -> github.com/diodeinc/registry)
    eprintln!("\nPhase 5: Expanding hardcoded aliases");
    alias_expansion::expand_aliases(&workspace_root)?;

    // Phase 6: Correct stale registry paths
    eprintln!("\nPhase 6: Correcting stale registry paths");
    path_correction::correct_paths(&workspace_root)?;

    eprintln!("\n✓ Migration to V2 complete");
    eprintln!("  Review changes with: git diff");
    eprintln!("  Run build to verify: pcb build");

    Ok(())
}
