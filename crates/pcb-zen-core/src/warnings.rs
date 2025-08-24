use crate::{AliasInfo, Diagnostic, LoadResolver, RemoteRef, ResolveContext};
use starlark::{codemap::ResolvedSpan, errors::EvalSeverity};

/// Generate a warning message for unstable references
pub fn generate_unstable_ref_warning_message(
    remote_ref: &RemoteRef,
    alias_info: Option<&AliasInfo>,
    resolve_context: &ResolveContext,
) -> String {
    if let Some(alias_info) = alias_info {
        // Extract package name from first spec (always LoadSpec::Package when alias_info is present)
        let crate::LoadSpec::Package { package, .. } =
            resolve_context.spec_history.first().unwrap()
        else {
            unreachable!("First spec should always be Package when alias_info is present")
        };

        if alias_info.source_path.is_none() {
            format!(
                "'{package}' default alias uses unstable reference '{}'. Use a pinned version (inline :tag or pcb.toml).",
                remote_ref.rev()
            )
        } else {
            format!(
                "'{package}' uses unstable reference '{}'. The alias defined in '{}' points to an unstable reference. Use a pinned version (inline :tag or pcb.toml).",
                remote_ref.rev(),
                alias_info.source_path.as_ref().unwrap().display()
            )
        }
    } else {
        // For direct references (no alias), use the first spec (what the user originally wrote)
        let first_spec = resolve_context.spec_history.first().unwrap();
        format!(
            "'{first_spec}' is an unstable reference. Use a pinned version (inline :tag or pcb.toml).",
        )
    }
}

/// Check if we should warn about an unstable reference and create the warning diagnostic if needed.
///
/// Warns for:
/// - Package/GitHub/GitLab loads that resolve to unstable remote references (HEAD, branches)
///
/// Skips warnings for:
/// - Local Path loads (./file.zen, ../file.zen) - always internal to the same repo
/// - Stable remote references (tags, commits)
pub fn check_and_create_unstable_ref_warning(
    load_resolver: &dyn LoadResolver,
    current_file: &std::path::Path,
    resolve_context: &ResolveContext,
    span: Option<ResolvedSpan>,
) -> Option<Diagnostic> {
    // If the original spec was a local Path, this is an internal load within the same repo - don't warn
    if let Some(original_spec) = resolve_context.spec_history.first() {
        if matches!(original_spec, crate::LoadSpec::Path { .. }) {
            return None;
        }
    }

    // Get the remote ref from the final resolved LoadSpec in the history
    let callee_remote = resolve_context.spec_history.last()?.remote_ref()?;

    // Check if the remote ref is unstable
    let remote_ref_meta = load_resolver.remote_ref_meta(&callee_remote)?;
    if !remote_ref_meta.stable() {
        // Get alias info if resolution went through alias resolution
        let alias_info = resolve_context.get_alias_info();
        let warning_message =
            generate_unstable_ref_warning_message(&callee_remote, alias_info, resolve_context);

        return Some(Diagnostic {
            path: current_file.to_string_lossy().to_string(),
            span,
            severity: EvalSeverity::Warning,
            body: warning_message,
            call_stack: None,
            child: None,
        });
    }

    None
}
