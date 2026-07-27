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
/// Import never reaches the network, and this is the only place in the module that resolves a
/// workspace — so that is a property of the code rather than a convention two call sites happen to
/// honour. Offline resolution fails on an uncached dependency instead of fetching it, which is what
/// makes an import reproducible: converting a KiCad design must depend only on the schematic, the
/// machine's KiCad libraries, and whatever is already on disk. It also means a `pcb import` behaves
/// the same on a laptop with no connectivity as on a build machine.
///
/// The registry side is offline for the same reason and by the same means: exact-MPN lookup reads only
/// index files already under `~/.pcb/registry/indexes`, and a substitution candidate must already be
/// present in `~/.pcb/cache` or the board's `vendor/`. Nothing here downloads a package, an index, or
/// registry metadata, and nothing adds a dependency to make that happen later.
pub(super) fn offline_eval_state(board_dir: &Path) -> Result<crate::build::BuildEvalState> {
    let resolution = crate::resolve::resolve(Some(board_dir), /* offline */ true)
        .with_context(|| format!("Failed to resolve workspace {}", board_dir.display()))?;
    Ok(crate::build::BuildEvalState::new(resolution))
}
