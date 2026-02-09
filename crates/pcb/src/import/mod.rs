//! KiCad import flow.

mod discover;
mod extract;
mod flow;
mod generate;
mod hierarchy;
mod materialize;
mod paths;
mod report;
mod semantic;
mod types;
mod validate;

pub use types::ImportArgs;

use anyhow::Result;

// Re-export internal types so submodules can `use super::*;`.
pub(super) use types::*;

pub fn execute(args: ImportArgs) -> Result<()> {
    flow::execute(args)
}
