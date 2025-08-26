use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;

use crate::lang::evaluator_ext::EvaluatorExt;

/// File system access primitives for Starlark.
///
/// Currently this exposes:
///  • File(path): resolves a file or directory path using the load resolver and returns the absolute path.
#[starlark_module]
pub(crate) fn file_globals(builder: &mut GlobalsBuilder) {
    /// Resolve a file or directory path using the load resolver and return the absolute path as a string.
    ///
    /// The path is resolved relative to the current file, just like load() statements.
    /// If the path cannot be resolved or doesn't exist, an error is raised.
    fn File<'v>(
        #[starlark(require = pos)] path: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Get the eval context to access the load resolver
        let eval_context = eval
            .eval_context()
            .ok_or_else(|| anyhow::anyhow!("No evaluation context available"))?;

        // Get the file provider
        let file_provider = eval_context
            .file_provider
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No file provider available"))?;

        // Get the load resolver
        let load_resolver = eval_context
            .get_load_resolver()
            .ok_or_else(|| anyhow::anyhow!("No load resolver available"))?;

        // Get the current file path
        let current_file = eval_context
            .get_source_path()
            .ok_or_else(|| anyhow::anyhow!("No source path available"))?;

        // Resolve the path using the load resolver
        let resolved_path = load_resolver
            .resolve_path(&path, current_file)
            .map_err(|e| anyhow::anyhow!("Failed to resolve file path '{}': {}", path, e))?;

        // Verify the path exists (either as a file or directory)
        if !file_provider.exists(&resolved_path) {
            return Err(anyhow::anyhow!(
                "Path not found: {}",
                resolved_path.display()
            ));
        }

        // Return the absolute path as a string
        Ok(eval
            .heap()
            .alloc_str(&resolved_path.to_string_lossy())
            .to_value())
    }
}
