use starlark::environment::GlobalsBuilder;
use starlark::errors::EvalSeverity;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;

use crate::Diagnostic;
use crate::lang::evaluator_ext::EvaluatorExt;
use crate::load_spec::LoadSpec;
use crate::resolution::ResolutionResult;

fn stable_path_string(resolution: &ResolutionResult, resolved_path: &std::path::Path) -> String {
    if let Some(uri) = resolution.format_package_uri(resolved_path) {
        return uri;
    }
    resolved_path.to_string_lossy().into_owned()
}

/// File system access primitives for Starlark.
///
/// Currently this exposes:
///  • File(path): resolves a file or directory path using the load resolver.
///  • Path(path, allow_not_exist=false): resolves any LoadSpec, with optional non-existence tolerance.
///
/// These functions always enforce package boundaries (via the load resolver), but return a stable
/// `package://…` URI whenever the resolved path is within a known package. This avoids embedding
/// machine-specific absolute paths in downstream artifacts (netlists, layouts, etc.).
#[starlark_module]
pub(crate) fn file_globals(builder: &mut GlobalsBuilder) {
    /// Resolve a file or directory path using the load resolver and return a stable path string.
    ///
    /// The path is resolved relative to the current file, just like load() statements.
    /// If the path cannot be resolved or doesn't exist, an error is raised.
    fn File<'v>(
        #[starlark(require = pos)] path: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let eval_context = eval
            .eval_context()
            .ok_or_else(|| anyhow::anyhow!("No evaluation context available"))?;

        let current_file = eval_context
            .get_source_path()
            .ok_or_else(|| anyhow::anyhow!("No source path available"))?;

        let resolution = eval_context.resolution();

        let resolved_path = eval_context
            .get_config()
            .resolve_path(&path, current_file)
            .map_err(|e| anyhow::anyhow!("Failed to resolve file path '{}': {}", path, e))?;

        let stable_str = stable_path_string(resolution, &resolved_path);

        Ok(eval.heap().alloc_str(&stable_str).to_value())
    }

    /// Resolve a file or directory path using the load resolver and return a stable path string.
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
        let eval_context = eval
            .eval_context()
            .ok_or_else(|| anyhow::anyhow!("No evaluation context available"))?;

        let current_file = eval_context
            .get_source_path()
            .ok_or_else(|| anyhow::anyhow!("No source path available"))?;

        let resolution = eval_context.resolution();

        let mut load_spec = LoadSpec::parse(&path)
            .ok_or_else(|| anyhow::anyhow!("Invalid load path spec: {}", path))?;

        if allow_not_exist {
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

        let resolved_path = eval_context
            .get_config()
            .resolve_spec(&load_spec, current_file)
            .map_err(|e| anyhow::anyhow!("Failed to resolve path '{}': {}", path, e))?;

        if !eval_context.file_provider().exists(&resolved_path) {
            let call_stack = eval.call_stack();

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

        let stable_str = stable_path_string(resolution, &resolved_path);

        Ok(eval.heap().alloc_str(&stable_str).to_value())
    }
}
