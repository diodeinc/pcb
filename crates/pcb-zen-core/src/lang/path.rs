use std::path::Path;

use crate::EvalContext;
use crate::resolution::ResolutionResult;

/// Normalize a user-provided path to `package://...` when it can be resolved.
///
/// This handles:
/// - already-normalized package URIs (returned as-is)
/// - paths resolvable from the current evaluation source path
/// - absolute filesystem paths inside known package roots
pub(crate) fn normalize_path_to_package_uri(path: &str, ctx: Option<&EvalContext>) -> String {
    if path.starts_with(pcb_sch::PACKAGE_URI_PREFIX) {
        return path.to_owned();
    }
    let Some(eval_ctx) = ctx else {
        return path.to_owned();
    };

    let resolution = eval_ctx.resolution();

    if let Some(current_file) = eval_ctx.get_source_path()
        && let Ok(resolved) = eval_ctx.get_config().resolve_path(path, current_file)
    {
        return resolution
            .format_package_uri(&resolved)
            .unwrap_or_else(|| resolved.to_string_lossy().into_owned());
    }

    let absolute = Path::new(path);
    if absolute.is_absolute()
        && let Some(uri) = resolution.format_package_uri(absolute)
    {
        return uri;
    }

    path.to_owned()
}

/// Resolve a non-URI path relative to `base_dir` and format it as `package://...`
/// when possible.
pub(crate) fn format_relative_path_as_package_uri(
    raw_path: &str,
    base_dir: Option<&Path>,
    resolution: &ResolutionResult,
) -> Option<String> {
    if raw_path.starts_with(pcb_sch::PACKAGE_URI_PREFIX) {
        return Some(raw_path.to_owned());
    }
    let base_dir = base_dir?;
    let abs = base_dir.join(raw_path);
    resolution.format_package_uri(&abs)
}
