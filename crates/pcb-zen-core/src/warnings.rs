use crate::{Diagnostic, LoadResolver, ResolveContext, UnstableRefError};
use regex::Regex;
use starlark::{
    codemap::{CodeMap, Pos, ResolvedSpan, Span},
    errors::EvalSeverity,
};

/// Extract the package name from a LoadSpec::Package
fn get_package_name_from_spec(spec: &crate::LoadSpec) -> String {
    let crate::LoadSpec::Package { package, .. } = spec else {
        unreachable!("First spec should always be Package when alias_info is present")
    };
    package.clone()
}

/// Format a LoadSpec without the path component (e.g. "@stdlib:latest" instead of "@stdlib:latest/path.zen")
fn format_spec_without_path(spec: &crate::LoadSpec) -> String {
    match spec {
        crate::LoadSpec::Package { package, tag, .. } => {
            format!("@{package}:{tag}")
        }
        crate::LoadSpec::Github {
            user, repo, rev, ..
        } => {
            format!("@github/{user}/{repo}:{rev}")
        }
        crate::LoadSpec::Gitlab {
            project_path, rev, ..
        } => {
            format!("@gitlab/{project_path}:{rev}")
        }
        crate::LoadSpec::Path { path } => path.display().to_string(),
        crate::LoadSpec::WorkspacePath { path } => {
            format!("//{}", path.display())
        }
    }
}

/// Find the span for a package alias value in a PCB.toml file
fn find_toml_alias_span(
    file_provider: &dyn crate::FileProvider,
    toml_path: &std::path::Path,
    package_name: &str,
) -> Option<ResolvedSpan> {
    let toml_content = file_provider.read_file(toml_path).ok()?;

    // Find line with the alias definition - matches "package = " or "package="
    let pattern = format!(r"^\s*{}\s*=", regex::escape(package_name));
    let re = Regex::new(&pattern).ok()?;

    let (line_idx, line) = toml_content
        .lines()
        .enumerate()
        .find(|(_, line)| re.is_match(line))?;

    // Find the value after the = sign
    let equals_pos = line.find('=')?;
    let value_part = &line[equals_pos + 1..];
    let value_start = value_part.find(|c: char| !c.is_whitespace())? + equals_pos + 1;

    // Create span covering the value (from first non-whitespace to end of line)
    let codemap = CodeMap::new(
        toml_path.to_string_lossy().to_string(),
        toml_content.clone(),
    );
    let line_start = toml_content
        .lines()
        .take(line_idx)
        .map(|l| l.len() + 1)
        .sum::<usize>();
    let start_pos = line_start + value_start;
    let end_pos = line_start + line.trim_end().len();

    let span = Span::new(Pos::new(start_pos as u32), Pos::new(end_pos as u32));
    Some(codemap.file_span(span).resolve_span())
}

/// Check if we should warn about an unstable reference and create the warning diagnostic if needed.
///
/// Warns for:
/// - Package/GitHub/GitLab loads that resolve to unstable remote references (HEAD, branches)
///
/// Skips warnings for:
/// - Local Path loads (./file.zen, ../file.zen) - always internal to the same repo
/// - Stable remote references (tags, commits)
///
/// For non-default aliases, creates nested diagnostics with:
/// - Root diagnostic: Points to the call site in the .zen file
/// - Child diagnostic: Points to the PCB.toml file where the alias is defined with proper span
pub fn check_and_create_unstable_ref_warning(
    load_resolver: &dyn LoadResolver,
    current_file: &std::path::Path,
    resolve_context: &ResolveContext,
    span: Option<ResolvedSpan>,
) -> Option<Diagnostic> {
    let first_spec = resolve_context.spec_history.first().unwrap();

    // If the original spec was a local Path, this is an internal load within the same repo - don't warn
    if matches!(first_spec, crate::LoadSpec::Path { .. }) {
        return None;
    }

    // Get the remote ref from the final resolved LoadSpec in the history
    let callee_remote = resolve_context.spec_history.last()?.remote_ref()?;

    // Check if the remote ref is unstable
    let remote_ref_meta = load_resolver.remote_ref_meta(&callee_remote)?;
    if !remote_ref_meta.stable() {
        let spec_without_path = format_spec_without_path(first_spec);

        // Create the simplified error with only essential information
        let mut unstable_ref_error = Some(UnstableRefError {
            spec_chain: resolve_context.spec_history.clone(),
            calling_file: current_file.to_path_buf(),
            remote_ref_meta: remote_ref_meta.clone(),
            remote_ref: callee_remote.clone(),
        });

        let main_message =
            format!("'{spec_without_path}' is an unstable reference. Use a pinned version.");

        // Try to create a child diagnostic for non-default aliases
        let child = resolve_context
            .get_alias_info()
            .and_then(|alias_info| alias_info.source_path.as_ref())
            .map(|toml_source_path| {
                let package = get_package_name_from_spec(first_spec);
                let resolved_span =
                    find_toml_alias_span(load_resolver.file_provider(), toml_source_path, &package);

                // For PCB.toml diagnostic, show the actual unstable reference (last spec)
                let last_spec = resolve_context.spec_history.last().unwrap();
                let resolved_spec_without_path = format_spec_without_path(last_spec);
                let toml_message = format!(
                    "'{resolved_spec_without_path}' is an unstable reference. Use a pinned version."
                );

                Diagnostic::new(toml_message, EvalSeverity::Warning, toml_source_path)
                    .with_span(resolved_span)
                    .with_source_error(unstable_ref_error.take())
                    .boxed()
            });

        let diagnostic = Diagnostic::new(main_message, EvalSeverity::Warning, current_file)
            .with_span(span)
            .with_source_error(unstable_ref_error)
            .with_child(child);
        return Some(diagnostic);
    }

    None
}
