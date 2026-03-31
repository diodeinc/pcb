mod common;

use common::{InMemoryFileProvider, stdlib_test_files_at, test_resolution_at};
use pcb_zen_core::lang::eval::EvalSession;
use pcb_zen_core::{EvalContext, EvalContextConfig, FileProvider};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[test]
fn shared_session_reuses_caches_without_leaking_module_tree_between_roots() {
    let workspace_root = Path::new("/workspace");
    let mut files = stdlib_test_files_at(workspace_root);
    files.extend(HashMap::from([
        (
            "/workspace/shared/lib.zen".to_string(),
            r#"
VALUE = "shared"
"#
            .to_string(),
        ),
        (
            "/workspace/child.zen".to_string(),
            r#"
ChildValue = "child"
"#
            .to_string(),
        ),
        (
            "/workspace/a.zen".to_string(),
            r#"
load("shared/lib.zen", "VALUE")

Child = Module("child.zen")

check(VALUE == "shared", "shared module should load")
Child(name = "leaked")
"#
            .to_string(),
        ),
        (
            "/workspace/b.zen".to_string(),
            r#"
load("shared/lib.zen", "VALUE")

check(VALUE == "shared", "shared module should load")
"#
            .to_string(),
        ),
    ]));

    let file_provider: Arc<dyn FileProvider> = Arc::new(InMemoryFileProvider::new(files));
    let mut resolution = test_resolution_at(workspace_root);
    resolution.canonicalize_keys(file_provider.as_ref());
    let resolution = Arc::new(resolution);
    let session = EvalSession::default();

    let eval = |path: &str| {
        session.prepare_for_root_eval();
        EvalContext::from_session_and_config(
            session.clone(),
            EvalContextConfig::new(file_provider.clone(), resolution.clone()),
        )
        .set_source_path(PathBuf::from(path))
        .eval()
    };

    let first = eval("/workspace/a.zen");
    assert!(
        first.is_success(),
        "first eval failed: {:?}",
        first.diagnostics
    );

    let first_schematic = first
        .output
        .as_ref()
        .expect("first output")
        .to_schematic()
        .expect("first schematic");
    assert!(
        first_schematic.instances.keys().any(|instance| {
            instance
                .instance_path
                .last()
                .is_some_and(|name| name == "leaked")
        }),
        "first schematic should include the instantiated child module",
    );

    let second = eval("/workspace/b.zen");
    assert!(
        second.is_success(),
        "second eval failed: {:?}",
        second.diagnostics
    );

    let second_schematic = second
        .output
        .as_ref()
        .expect("second output")
        .to_schematic()
        .expect("second schematic");
    assert!(
        !second_schematic.instances.keys().any(|instance| {
            instance
                .instance_path
                .last()
                .is_some_and(|name| name == "leaked")
        }),
        "module tree from the previous root evaluation leaked into the next schematic",
    );
}

#[test]
fn shared_session_replays_load_warnings_on_cache_hits() {
    let workspace_root = Path::new("/workspace");
    let mut files = stdlib_test_files_at(workspace_root);
    files.extend(HashMap::from([
        (
            "/workspace/shared/lib.zen".to_string(),
            r#"
VALUE = "shared"
warn("cached warning")
"#
            .to_string(),
        ),
        (
            "/workspace/a.zen".to_string(),
            r#"
load("shared/lib.zen", "VALUE")
"#
            .to_string(),
        ),
        (
            "/workspace/b.zen".to_string(),
            r#"
load("shared/lib.zen", "VALUE")
"#
            .to_string(),
        ),
    ]));

    let file_provider: Arc<dyn FileProvider> = Arc::new(InMemoryFileProvider::new(files));
    let mut resolution = test_resolution_at(workspace_root);
    resolution.canonicalize_keys(file_provider.as_ref());
    let resolution = Arc::new(resolution);
    let session = EvalSession::default();

    let eval = |path: &str| {
        session.prepare_for_root_eval();
        EvalContext::from_session_and_config(
            session.clone(),
            EvalContextConfig::new(file_provider.clone(), resolution.clone()),
        )
        .set_source_path(PathBuf::from(path))
        .eval()
    };

    let warning_count = |result: &pcb_zen_core::WithDiagnostics<pcb_zen_core::EvalOutput>| {
        result
            .diagnostics
            .iter()
            .filter(|diag| matches!(diag.severity, starlark::errors::EvalSeverity::Warning))
            .count()
    };

    let first = eval("/workspace/a.zen");
    let second = eval("/workspace/b.zen");

    assert_eq!(
        warning_count(&first),
        1,
        "first eval should report load warning"
    );
    assert_eq!(
        warning_count(&second),
        1,
        "cached load warning should still be reported on subsequent roots",
    );
}
