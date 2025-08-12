use anyhow::Result;
use std::path::Path;

pub mod remove_directory_loads;

pub trait Codemod {
    fn name(&self) -> &'static str;
    fn apply(&self, path: &Path, content: &str) -> Result<Option<String>>;
}
