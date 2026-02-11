use starlark::environment::GlobalsBuilder;
use starlark::errors::EvalSeverity;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;

use crate::lang::evaluator_ext::EvaluatorExt;
use crate::load_spec::LoadSpec;
use crate::Diagnostic;

/// File system access primitives for Starlark.
///
/// Currently this exposes:
///  • File(path): resolves a file or directory path using the load resolver and returns the absolute path.
///  • Path(path, allow_not_exist=false): resolves any LoadSpec and returns the absolute path, with optional non-existence tolerance.
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

        // Get the current file path
        let current_file = eval_context
            .get_source_path()
            .ok_or_else(|| anyhow::anyhow!("No source path available"))?;

        // Resolve the path using the load resolver
        let resolved_path = eval_context
            .get_config()
            .resolve_path(&path, current_file)
            .map_err(|e| anyhow::anyhow!("Failed to resolve file path '{}': {}", path, e))?;

        // Return the absolute path as a string
        Ok(eval
            .heap()
            .alloc_str(&resolved_path.to_string_lossy())
            .to_value())
    }

    /// Resolve a file or directory path using the load resolver and return the absolute path as a string.
    ///
    /// The path is resolved relative to the current file, just like load() statements.
    ///
    /// Args:
    ///   path: The path string to resolve (can be any LoadSpec format)
    ///   allow_not_exist: Optional boolean (default: false). If true, allows non-existent paths.
    ///                     Can only be used with Path LoadSpecs, not Package/GitHub/GitLab specs.
    fn Path<'v>(
        #[starlark(require = pos)] path: String,
        #[starlark(require = named, default = false)] allow_not_exist: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Get the eval context to access the load resolver
        let eval_context = eval
            .eval_context()
            .ok_or_else(|| anyhow::anyhow!("No evaluation context available"))?;

        // Get the current file path
        let current_file = eval_context
            .get_source_path()
            .ok_or_else(|| anyhow::anyhow!("No source path available"))?;

        // Parse the path string into a LoadSpec using the standard parser
        let mut load_spec = LoadSpec::parse(&path)
            .ok_or_else(|| anyhow::anyhow!("Invalid load path spec: {}", path))?;

        // Handle allow_not_exist parameter
        if allow_not_exist {
            // If allow_not_exist is true, the LoadSpec must be a Path type
            if let LoadSpec::Path {
                allow_not_exist: spec_allow_not_exist,
                ..
            } = &mut load_spec
            {
                *spec_allow_not_exist = true;
            } else {
                anyhow::bail!("allow_not_exist can only be used with path");
            }
        }

        // Use the load resolver to resolve the LoadSpec to an absolute path
        let resolved_path = eval_context
            .get_config()
            .resolve_spec(&load_spec, current_file)
            .map_err(|e| anyhow::anyhow!("Failed to resolve path '{}': {}", path, e))?;

        // If resolved_path doesn't exist, emit a warning diagnostic
        if !eval_context.file_provider().exists(&resolved_path) {
            let call_stack = eval.call_stack();

            // Start with the innermost diagnostic (deepest frame - always has location)
            let deepest_frame = &call_stack.frames[call_stack.frames.len() - 1];
            let location = deepest_frame.location.as_ref().unwrap();
            let mut diagnostic = Diagnostic {
                path: location.file.filename().to_string(),
                span: Some(location.resolve_span()),
                severity: EvalSeverity::Warning,
                body: format!("Path '{}' does not exist", path),
                call_stack: None,
                child: None,
                source_error: None,
                suppressed: false,
            };

            // Wrap with each outer frame that has location info
            for (i, frame) in call_stack.frames.iter().enumerate().rev().skip(1) {
                if let Some(location) = &frame.location {
                    diagnostic = Diagnostic {
                        path: location.file.filename().to_string(),
                        span: Some(location.resolve_span()),
                        severity: EvalSeverity::Warning,
                        body: format!("Path does not exist in {} call", frame.name),
                        call_stack: if i == 0 {
                            Some(call_stack.clone())
                        } else {
                            None
                        },
                        suppressed: false,
                        child: Some(Box::new(diagnostic)),
                        source_error: None,
                    };
                }
            }

            eval.add_diagnostic(diagnostic);
        }

        // Return the absolute path as a string
        Ok(eval
            .heap()
            .alloc_str(&resolved_path.to_string_lossy())
            .to_value())
    }
}
