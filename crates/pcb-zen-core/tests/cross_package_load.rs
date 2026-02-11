//! Tests for relative path loads that cross package boundaries.
//!
//! When a relative path like `../../modules/Led.zen` escapes the current package root,
//! it should be resolved via URL arithmetic and the package dependency system rather
//! than being rejected outright.

mod common;

use common::InMemoryFileProvider;
use pcb_zen_core::resolution::ResolutionResult;
use pcb_zen_core::workspace::{MemberPackage, WorkspaceInfo};
use pcb_zen_core::{EvalContext, FileProvider};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

/// Helper to set up a workspace with two packages where one loads from the other
/// using a relative path that crosses the package boundary.
fn setup_cross_package_workspace(
    repository: Option<&str>,
    board_deps: BTreeMap<String, pcb_zen_core::config::DependencySpec>,
) -> (Arc<dyn FileProvider>, ResolutionResult, PathBuf) {
    let mut files = HashMap::new();

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
        MemberPackage {
            rel_path: PathBuf::from("boards/Main"),
            config: pcb_zen_core::config::PcbToml {
                dependencies: board_deps.clone(),
                ..Default::default()
            },
            version: None,
            dirty: false,
        },
    );
    packages.insert(
        led_url.clone(),
        MemberPackage {
            rel_path: PathBuf::from("modules/Led"),
            config: pcb_zen_core::config::PcbToml::default(),
            version: None,
            dirty: false,
        },
    );

    let workspace_info = WorkspaceInfo {
        root: PathBuf::from("/workspace"),
        cache_dir: PathBuf::new(),
        config: None,
        packages,
        lockfile: None,
        errors: vec![],
    };

    // Build resolution maps
    let mut package_resolutions: HashMap<PathBuf, BTreeMap<String, PathBuf>> = HashMap::new();

    // Board's resolution map: only include Led if it's a declared dependency
    let mut board_deps_map = BTreeMap::new();
    if !board_deps.is_empty() {
        board_deps_map.insert(led_url.clone(), PathBuf::from("/workspace/modules/Led"));
    }
    package_resolutions.insert(PathBuf::from("/workspace/boards/Main"), board_deps_map);

    // Led's resolution map (empty, no deps)
    package_resolutions.insert(PathBuf::from("/workspace/modules/Led"), BTreeMap::new());

    // Workspace root resolution map
    package_resolutions.insert(PathBuf::from("/workspace"), BTreeMap::new());

    let resolution = ResolutionResult {
        workspace_info,
        package_resolutions,
        closure: HashMap::new(),
        assets: HashMap::new(),
        lockfile_changed: false,
    };

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
        setup_cross_package_workspace(Some("github.com/myorg/project"), deps);

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

    let (file_provider, resolution, main_path) = setup_cross_package_workspace(None, deps);

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
        setup_cross_package_workspace(Some("github.com/myorg/project"), deps);

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
