//! Tests for relative path loads that cross package boundaries.
//!
//! When a relative path like `../../modules/Led.zen` escapes the current package root,
//! it should be resolved via URL arithmetic and the package dependency system rather
//! than being rejected outright.

mod common;

use common::InMemoryFileProvider;
use pcb_zen_core::config::DependencyTable;
use pcb_zen_core::resolution::ResolutionResult;
use pcb_zen_core::workspace::{WorkspaceInfo, WorkspacePackage};
use pcb_zen_core::{EvalContext, FileProvider};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

/// Helper to set up a workspace with two packages where one loads from the other
/// using a relative path that crosses the package boundary.
fn setup_cross_package_workspace(
    repository: Option<&str>,
    board_deps: BTreeMap<String, pcb_zen_core::config::DependencySpec>,
    include_target_in_frozen_resolution: bool,
) -> (Arc<dyn FileProvider>, ResolutionResult, PathBuf) {
    let workspace_root = PathBuf::from("/workspace");
    let stdlib_root = pcb_zen_core::workspace_stdlib_root(&workspace_root);
    let mut files = common::stdlib_test_files_at(&workspace_root);

    // modules/Led/Led.zen — the target package
    files.insert(
        "workspace/modules/Led/Led.zen".to_string(),
        r#"
LedValue = "hello from Led"
"#
        .to_string(),
    );

    // modules/Led/pcb.toml
    files.insert("workspace/modules/Led/pcb.toml".to_string(), "".to_string());

    // boards/Main/Main.zen — loads Led via relative path that escapes package
    files.insert(
        "workspace/boards/Main/Main.zen".to_string(),
        r#"
load("../../modules/Led/Led.zen", "LedValue")
check(LedValue == "hello from Led", "should load from Led")
"#
        .to_string(),
    );

    // boards/Main/pcb.toml
    files.insert("workspace/boards/Main/pcb.toml".to_string(), "".to_string());

    // workspace/pcb.toml
    files.insert("workspace/pcb.toml".to_string(), "".to_string());

    let file_provider: Arc<dyn FileProvider> = Arc::new(InMemoryFileProvider::new(files));

    let base_url = repository.map(|r| r.to_string());

    let board_url = base_url
        .as_ref()
        .map(|b| format!("{}/boards/Main", b))
        .unwrap_or_else(|| "boards/Main".to_string());

    let led_url = base_url
        .as_ref()
        .map(|b| format!("{}/modules/Led", b))
        .unwrap_or_else(|| "modules/Led".to_string());

    let mut packages = BTreeMap::new();
    packages.insert(
        board_url.clone(),
        WorkspacePackage {
            rel_path: PathBuf::from("boards/Main"),
            config: pcb_zen_core::config::PcbToml {
                dependencies: DependencyTable {
                    direct: board_deps.clone(),
                    indirect: BTreeMap::new(),
                },
                ..Default::default()
            },
            version: None,
            published_at: None,
            preferred: false,
            dirty: false,
            entrypoints: Vec::new(),
            symbol_files: Vec::new(),
        },
    );
    packages.insert(
        led_url.clone(),
        WorkspacePackage {
            rel_path: PathBuf::from("modules/Led"),
            config: pcb_zen_core::config::PcbToml::default(),
            version: None,
            published_at: None,
            preferred: false,
            dirty: false,
            entrypoints: Vec::new(),
            symbol_files: Vec::new(),
        },
    );

    let workspace_info = WorkspaceInfo {
        root: workspace_root.clone(),
        cache_dir: PathBuf::new(),
        config: None,
        packages,
        errors: vec![],
    };

    // Board's resolution map: only include Led if it's a declared dependency
    let mut board_deps_map = BTreeMap::new();
    if !board_deps.is_empty() {
        board_deps_map.insert(led_url.clone(), PathBuf::from("/workspace/modules/Led"));
    }

    let mut frozen_packages = BTreeMap::from([
        (
            PathBuf::from("/workspace/boards/Main"),
            pcb_zen_core::resolution::FrozenPackage {
                identity: pcb_zen_core::resolution::FrozenPackageIdentity::Workspace(
                    board_url.clone(),
                ),
                deps: board_deps_map,
                parts: Vec::new(),
            },
        ),
        (
            stdlib_root,
            pcb_zen_core::resolution::FrozenPackage {
                identity: pcb_zen_core::resolution::FrozenPackageIdentity::Stdlib,
                deps: BTreeMap::new(),
                parts: Vec::new(),
            },
        ),
        (
            workspace_root.clone(),
            pcb_zen_core::resolution::FrozenPackage {
                identity: pcb_zen_core::resolution::FrozenPackageIdentity::Workspace(
                    "github.com/myorg/project".to_string(),
                ),
                deps: BTreeMap::new(),
                parts: Vec::new(),
            },
        ),
    ]);
    if include_target_in_frozen_resolution {
        frozen_packages.insert(
            PathBuf::from("/workspace/modules/Led"),
            pcb_zen_core::resolution::FrozenPackage {
                identity: pcb_zen_core::resolution::FrozenPackageIdentity::Workspace(led_url),
                deps: BTreeMap::new(),
                parts: Vec::new(),
            },
        );
    }

    let resolution = ResolutionResult::frozen(
        workspace_info,
        BTreeMap::from([(
            board_url.clone(),
            pcb_zen_core::resolution::FrozenResolutionMap {
                selected_remote: BTreeMap::new(),
                packages: frozen_packages,
            },
        )]),
        HashMap::new(),
    );

    let main_path = PathBuf::from("/workspace/boards/Main/Main.zen");
    (file_provider, resolution, main_path)
}

#[test]
#[cfg(not(target_os = "windows"))]
fn cross_package_relative_load_with_repository() {
    let deps = BTreeMap::from([(
        "github.com/myorg/project/modules/Led".to_string(),
        pcb_zen_core::config::DependencySpec::Version("0.1.0".to_string()),
    )]);

    let (file_provider, resolution, main_path) =
        setup_cross_package_workspace(Some("github.com/myorg/project"), deps, true);

    let result = EvalContext::new(file_provider, resolution)
        .set_source_path(main_path)
        .eval();

    assert!(
        result.is_success(),
        "Cross-package load should succeed when dependency is declared. Errors: {:?}",
        result
            .diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn cross_package_relative_load_without_repository() {
    let deps = BTreeMap::from([(
        "modules/Led".to_string(),
        pcb_zen_core::config::DependencySpec::Version("0.1.0".to_string()),
    )]);

    let (file_provider, resolution, main_path) = setup_cross_package_workspace(None, deps, true);

    let result = EvalContext::new(file_provider, resolution)
        .set_source_path(main_path)
        .eval();

    assert!(
        result.is_success(),
        "Cross-package load should succeed with synthetic URLs. Errors: {:?}",
        result
            .diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn cross_package_relative_load_undeclared_dependency() {
    // No dependencies declared — should fail with "No declared dependency matches"
    let deps = BTreeMap::new();

    let (file_provider, resolution, main_path) =
        setup_cross_package_workspace(Some("github.com/myorg/project"), deps, true);

    let result = EvalContext::new(file_provider, resolution)
        .set_source_path(main_path)
        .eval();

    assert!(
        !result.is_success(),
        "Cross-package load should fail when dependency is not declared"
    );

    let errors: Vec<String> = result.diagnostics.iter().map(|d| d.to_string()).collect();
    let has_dep_error = errors
        .iter()
        .any(|e| e.contains("No declared dependency matches"));
    assert!(
        has_dep_error,
        "Should get 'No declared dependency matches' error, got: {:?}",
        errors
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn cross_package_relative_load_undeclared_dependency_missing_from_frozen_resolution() {
    let deps = BTreeMap::new();

    let (file_provider, resolution, main_path) =
        setup_cross_package_workspace(Some("github.com/myorg/project"), deps, false);

    let result = EvalContext::new(file_provider, resolution)
        .set_source_path(main_path)
        .eval();

    assert!(
        !result.is_success(),
        "Cross-package load should fail even when stale resolution omitted the target package"
    );

    let errors: Vec<String> = result.diagnostics.iter().map(|d| d.to_string()).collect();
    let has_sync_error = errors.iter().any(|e| e.contains("Run `pcb sync`"));
    assert!(
        has_sync_error,
        "Should ask the user to run `pcb sync`, got: {:?}",
        errors
    );
}
