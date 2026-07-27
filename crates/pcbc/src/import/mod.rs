//! KiCad import flow.

mod discover;
mod extract;
mod flow;
mod footprint_identity;
mod generate;
mod hierarchy;
mod materialize;
mod output;
mod paths;
mod portable;
mod registry_lookup;
mod registry_reuse;
mod report;
mod reuse_validate;
mod semantic;
mod types;
mod validate;

pub use types::ImportArgs;

use anyhow::{Context, Result};
use std::path::Path;

// Re-export internal types so submodules can `use super::*;`.
pub(super) use types::*;

pub fn execute(args: ImportArgs) -> Result<()> {
    flow::execute(args)
}

/// Build a Zener evaluation state for `board_dir`, resolved **offline**.
///
/// The only place the module resolves a workspace, so import staying off the network is a property of
/// the code rather than a convention every call site has to remember. Offline resolution fails on an
/// uncached dependency instead of fetching it, which is what makes a conversion depend only on the
/// schematic, the machine's KiCad libraries, and what is already on disk.
pub(super) fn offline_eval_state(board_dir: &Path) -> Result<crate::build::BuildEvalState> {
    let resolution = crate::resolve::resolve(Some(board_dir), /* offline */ true)
        .with_context(|| format!("Failed to resolve workspace {}", board_dir.display()))?;
    Ok(crate::build::BuildEvalState::new(resolution))
}
