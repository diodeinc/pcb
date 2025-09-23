use pcb_zen_core::{
    CoreLoadResolver, FileProvider, FileProviderError, LoadResolver, LoadSpec, RemoteFetcher,
};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Mock implementation of FileProvider for testing
#[derive(Debug, Clone)]
struct MockFileProvider {
    files: Arc<Mutex<HashMap<PathBuf, String>>>,
    directories: Arc<Mutex<Vec<PathBuf>>>,
}

impl MockFileProvider {
    fn new() -> Self {
        Self {
            files: Arc::new(Mutex::new(HashMap::new())),
            directories: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn add_file(&self, path: impl Into<PathBuf>, content: impl Into<String>) {
        let path = path.into();
        let content = content.into();

        // Add all parent directories
        let mut current = path.parent();
        while let Some(dir) = current {
            self.directories.lock().unwrap().push(dir.to_path_buf());
            current = dir.parent();
        }

        self.files.lock().unwrap().insert(path, content);
    }

    fn add_directory(&self, path: impl Into<PathBuf>) {
        self.directories.lock().unwrap().push(path.into());
    }
}

impl FileProvider for MockFileProvider {
    fn read_file(&self, path: &Path) -> Result<String, FileProviderError> {
        self.files
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| FileProviderError::NotFound(path.to_path_buf()))
    }

    fn exists(&self, path: &Path) -> bool {
        self.files.lock().unwrap().contains_key(path)
            || self.directories.lock().unwrap().iter().any(|d| d == path)
    }

    fn is_directory(&self, path: &Path) -> bool {
        self.directories.lock().unwrap().iter().any(|d| d == path)
    }

    fn list_directory(&self, path: &Path) -> Result<Vec<PathBuf>, FileProviderError> {
        let files = self.files.lock().unwrap();
        let dirs = self.directories.lock().unwrap();

        let mut entries = Vec::new();

        // Add files in this directory
        for file_path in files.keys() {
            if let Some(parent) = file_path.parent() {
                if parent == path {
                    entries.push(file_path.clone());
                }
            }
        }

        // Add subdirectories
        for dir in dirs.iter() {
            if let Some(parent) = dir.parent() {
                if parent == path && !entries.contains(dir) {
                    entries.push(dir.clone());
                }
            }
        }

        if entries.is_empty() && !self.is_directory(path) {
            Err(FileProviderError::NotFound(path.to_path_buf()))
        } else {
            Ok(entries)
        }
    }

    fn canonicalize(&self, path: &Path) -> Result<PathBuf, FileProviderError> {
        // Simple canonicalization for tests - just normalize the path
        let mut components = Vec::new();
        for component in path.components() {
            match component {
                std::path::Component::Normal(name) => {
                    components.push(name.to_string_lossy().to_string());
                }
                std::path::Component::ParentDir => {
                    components.pop();
                }
                std::path::Component::RootDir => {
                    components.clear();
                }
                _ => {}
            }
        }

        let result = if path.is_absolute() {
            PathBuf::from("/").join(components.join("/"))
        } else {
            PathBuf::from(components.join("/"))
        };

        Ok(result)
    }
}

/// Mock implementation of RemoteFetcher for testing
#[derive(Debug, Clone)]
struct MockRemoteFetcher {
    /// Maps LoadSpec strings to local cache paths
    fetch_results: Arc<Mutex<HashMap<String, PathBuf>>>,
    /// Tracks fetch calls for assertions
    #[allow(clippy::type_complexity)]
    fetch_calls: Arc<Mutex<Vec<(LoadSpec, Option<PathBuf>)>>>,
}

impl MockRemoteFetcher {
    fn new() -> Self {
        Self {
            fetch_results: Arc::new(Mutex::new(HashMap::new())),
            fetch_calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn add_fetch_result(&self, spec_str: impl Into<String>, local_path: impl Into<PathBuf>) {
        self.fetch_results
            .lock()
            .unwrap()
            .insert(spec_str.into(), local_path.into());
    }

    fn get_fetch_calls(&self) -> Vec<(LoadSpec, Option<PathBuf>)> {
        self.fetch_calls.lock().unwrap().clone()
    }
}

impl RemoteFetcher for MockRemoteFetcher {
    fn fetch_remote(
        &self,
        spec: &LoadSpec,
        workspace_root: &Path,
    ) -> Result<PathBuf, anyhow::Error> {
        self.fetch_calls
            .lock()
            .unwrap()
            .push((spec.clone(), Some(workspace_root.to_path_buf())));

        let spec_str = spec.to_load_string();
        self.fetch_results
            .lock()
            .unwrap()
            .get(&spec_str)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No mock result for spec: {}", spec_str))
    }

    fn remote_ref_meta(
        &self,
        _remote_ref: &pcb_zen_core::RemoteRef,
    ) -> Option<pcb_zen_core::RemoteRefMeta> {
        None // Mock doesn't provide metadata
    }
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_resolve_github_spec() {
    let file_provider = Arc::new(MockFileProvider::new());
    let remote_fetcher = Arc::new(MockRemoteFetcher::new());

    // Set up the mock to return a local cache path for the GitHub spec
    let cache_path =
        PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/zen/generics/Resistor.zen");
    remote_fetcher.add_fetch_result(
        "@github/diodeinc/stdlib/zen/generics/Resistor.zen",
        &cache_path,
    );

    // The fetched file should exist in our mock file system
    file_provider.add_file(&cache_path, "# Resistor implementation");
    file_provider.add_file("/workspace/main.zen", "# Main file");

    let resolver = CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher.clone(),
        PathBuf::from("/workspace"),
        true,
    );

    let spec = LoadSpec::Github {
        user: "diodeinc".to_string(),
        repo: "stdlib".to_string(),
        rev: "HEAD".to_string(),
        path: PathBuf::from("zen/generics/Resistor.zen"),
    };

    let current_file = PathBuf::from("/workspace/main.zen");
    let resolved = resolver.resolve_spec(&spec, &current_file).unwrap();

    assert_eq!(resolved, cache_path);

    // Verify the remote fetcher was called
    let calls = remote_fetcher.get_fetch_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1, Some(PathBuf::from("/workspace")));
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_resolve_relative_from_github_spec() {
    let file_provider = Arc::new(MockFileProvider::new());
    let remote_fetcher = Arc::new(MockRemoteFetcher::new());

    // Set up the cache structure
    let resistor_cache_path =
        PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/zen/generics/Resistor.zen");
    let units_cache_path =
        PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/zen/units.zen");

    // Set up files in the mock file system
    file_provider.add_file(&resistor_cache_path, "load(\"../units.zen\", \"ohm\")");
    file_provider.add_file(&units_cache_path, "ohm = \"Ω\"");

    // Set up remote fetcher for the units file (which would be resolved as a GitHub spec)
    remote_fetcher.add_fetch_result("@github/diodeinc/stdlib/zen/units.zen", &units_cache_path);

    let resolver = CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher.clone(),
        PathBuf::from("/workspace"),
        true,
    );

    // First, let's test resolving a relative path from the cached Resistor.zen
    // This test will fail with the current implementation, but shows what we want to achieve
    let relative_spec = LoadSpec::local_path("../units.zen");

    // When resolving from the cached file, it should understand that this file
    // came from @github/diodeinc/stdlib:zen/generics/Resistor.zen
    // and resolve ../units.zen as @github/diodeinc/stdlib:zen/units.zen

    // For now, this will resolve as a regular relative path
    let resolved = resolver
        .resolve_spec(&relative_spec, &resistor_cache_path)
        .unwrap();

    assert_eq!(resolved, units_cache_path);
}

#[test]
fn test_resolve_workspace_path_from_remote() {
    let file_provider = Arc::new(MockFileProvider::new());
    let remote_fetcher = Arc::new(MockRemoteFetcher::new());

    // Set up a remote file that uses workspace-relative paths
    let remote_cache_path =
        PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/zen/module.zen");
    let workspace_file_path =
        PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/common/utils.zen");

    file_provider.add_file(&remote_cache_path, "load(\"//common/utils.zen\", \"util\")");
    file_provider.add_file(&workspace_file_path, "util = \"utility\"");

    remote_fetcher.add_fetch_result(
        "@github/diodeinc/stdlib/common/utils.zen",
        &workspace_file_path,
    );

    let _resolver = CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher.clone(),
        PathBuf::from("/workspace"),
        true,
    );

    // When resolving a workspace path from a remote file, it should be
    // resolved relative to the remote repository's root, not the local workspace
    let _workspace_spec = LoadSpec::workspace_path("common/utils.zen");

    // This test shows what we want to achieve - workspace paths in remote files
    // should resolve within that remote repository
    // Currently this will fail as it tries to resolve in the local workspace
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_package_alias_resolution() {
    let file_provider = Arc::new(MockFileProvider::new());
    let remote_fetcher = Arc::new(MockRemoteFetcher::new());

    // Set up workspace with pcb.toml containing package aliases
    let workspace_root = PathBuf::from("/workspace");
    file_provider.add_directory(&workspace_root);
    file_provider.add_file(
        workspace_root.join("pcb.toml"),
        r#"
[packages]
stdlib = "@github/diodeinc/stdlib"
"#,
    );

    // Set up the expected resolution
    let cache_path =
        PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/zen/generics/Resistor.zen");
    remote_fetcher.add_fetch_result(
        "@github/diodeinc/stdlib/zen/generics/Resistor.zen",
        &cache_path,
    );
    file_provider.add_file(&cache_path, "# Resistor");

    let resolver = CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher.clone(),
        workspace_root.clone(),
        true,
    );

    // Test resolving a package alias
    let spec = LoadSpec::Package {
        package: "stdlib".to_string(),
        tag: "latest".to_string(),
        path: PathBuf::from("zen/generics/Resistor.zen"),
    };

    let current_file = workspace_root.join("main.zen");
    file_provider.add_file(&current_file, "# Main file");
    let resolved = resolver.resolve_spec(&spec, &current_file).unwrap();

    assert_eq!(resolved, cache_path);
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_resolve_relative_from_remote_with_mapping() {
    let file_provider = Arc::new(MockFileProvider::new());
    let remote_fetcher = Arc::new(MockRemoteFetcher::new());

    // This test demonstrates what we need:
    // 1. When we resolve @github/diodeinc/stdlib/zen/generics/Resistor.zen
    //    it gets cached at /home/user/.cache/pcb/github/diodeinc/stdlib/zen/generics/Resistor.zen
    // 2. When that cached file loads "../units.zen", we need to understand that
    //    this is relative to the original @github/diodeinc/stdlib location
    // 3. So "../units.zen" should resolve to @github/diodeinc/stdlib/zen/units.zen
    //    which then gets fetched and cached

    // Set up the cache structure
    let resistor_cache_path =
        PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/zen/generics/Resistor.zen");
    let units_cache_path =
        PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/zen/units.zen");

    // Set up files in the mock file system
    file_provider.add_file(&resistor_cache_path, "load(\"../units.zen\", \"ohm\")");
    file_provider.add_file(&units_cache_path, "ohm = \"Ω\"");

    // When we fetch the resistor file initially
    remote_fetcher.add_fetch_result(
        "@github/diodeinc/stdlib/zen/generics/Resistor.zen",
        &resistor_cache_path,
    );

    // When the relative load from Resistor.zen is resolved, it should trigger
    // a fetch for the units file as a GitHub spec
    remote_fetcher.add_fetch_result("@github/diodeinc/stdlib/zen/units.zen", &units_cache_path);

    // TODO: The resolver needs to maintain a mapping:
    // resistor_cache_path -> @github/diodeinc/stdlib/zen/generics/Resistor.zen
    // So when resolving relative paths from resistor_cache_path, it knows
    // to resolve them relative to the GitHub repository structure

    let resolver = CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher.clone(),
        PathBuf::from("/workspace"),
        true,
    );

    // First resolve the GitHub spec for Resistor.zen
    let github_spec = LoadSpec::Github {
        user: "diodeinc".to_string(),
        repo: "stdlib".to_string(),
        rev: "HEAD".to_string(),
        path: PathBuf::from("zen/generics/Resistor.zen"),
    };

    file_provider.add_file("/workspace/main.zen", "# Main file");
    let resolved_resistor = resolver
        .resolve_spec(&github_spec, &PathBuf::from("/workspace/main.zen"))
        .unwrap();

    assert_eq!(resolved_resistor, resistor_cache_path);

    // Now when we resolve a relative path from the cached Resistor.zen
    let relative_spec = LoadSpec::local_path("../units.zen");

    // This should understand that resistor_cache_path came from
    // @github/diodeinc/stdlib/zen/generics/Resistor.zen
    // and resolve ../units.zen as @github/diodeinc/stdlib/zen/units.zen
    let resolved_units = resolver
        .resolve_spec(&relative_spec, &resistor_cache_path)
        .unwrap();

    assert_eq!(resolved_units, units_cache_path);

    // Verify that the remote fetcher was called for both files
    let calls = remote_fetcher.get_fetch_calls();
    assert_eq!(calls.len(), 2);

    // The second call should be for the units file resolved as a GitHub spec
    match &calls[1].0 {
        LoadSpec::Github {
            user,
            repo,
            rev,
            path,
        } => {
            assert_eq!(user, "diodeinc");
            assert_eq!(repo, "stdlib");
            assert_eq!(rev, "HEAD");
            assert_eq!(path, &PathBuf::from("zen/units.zen"));
        }
        _ => panic!("Expected GitHub spec for units.zen"),
    }
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_resolve_workspace_path_from_remote_with_mapping() {
    let file_provider = Arc::new(MockFileProvider::new());
    let remote_fetcher = Arc::new(MockRemoteFetcher::new());

    // Test that workspace paths (//foo) in remote files resolve within the remote repo
    let remote_cache_path =
        PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/zen/module.zen");
    let utils_cache_path =
        PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/common/utils.zen");

    file_provider.add_file(&remote_cache_path, "load(\"//common/utils.zen\", \"util\")");
    file_provider.add_file(&utils_cache_path, "util = \"utility\"");

    // Initial fetch for module.zen
    remote_fetcher.add_fetch_result("@github/diodeinc/stdlib/zen/module.zen", &remote_cache_path);

    // When //common/utils.zen is resolved from within the remote file,
    // it should resolve to @github/diodeinc/stdlib/common/utils.zen
    remote_fetcher.add_fetch_result(
        "@github/diodeinc/stdlib/common/utils.zen",
        &utils_cache_path,
    );

    let resolver = CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher.clone(),
        PathBuf::from("/workspace"),
        true,
    );

    // Resolve the initial file
    let module_spec = LoadSpec::Github {
        user: "diodeinc".to_string(),
        repo: "stdlib".to_string(),
        rev: "HEAD".to_string(),
        path: PathBuf::from("zen/module.zen"),
    };

    file_provider.add_file("/workspace/main.zen", "# Main file");
    let resolved_module = resolver
        .resolve_spec(&module_spec, &PathBuf::from("/workspace/main.zen"))
        .unwrap();

    assert_eq!(resolved_module, remote_cache_path);

    // Now resolve a workspace path from within the remote file
    let workspace_spec = LoadSpec::workspace_path("common/utils.zen");

    // This should understand that remote_cache_path is from @github/diodeinc/stdlib
    // and resolve //common/utils.zen relative to that repository's root
    let resolved_utils = resolver
        .resolve_spec(&workspace_spec, &remote_cache_path)
        .unwrap();

    assert_eq!(resolved_utils, utils_cache_path);

    // Verify the remote fetcher was called correctly
    let calls = remote_fetcher.get_fetch_calls();
    assert_eq!(calls.len(), 2);

    match &calls[1].0 {
        LoadSpec::Github {
            user,
            repo,
            rev,
            path,
        } => {
            assert_eq!(user, "diodeinc");
            assert_eq!(repo, "stdlib");
            assert_eq!(rev, "HEAD");
            assert_eq!(path, &PathBuf::from("common/utils.zen"));
        }
        _ => panic!("Expected GitHub spec for utils.zen"),
    }
}

// ===== HIERARCHICAL ALIAS RESOLUTION TESTS =====

#[test]
#[cfg(not(target_os = "windows"))]
fn test_hierarchical_alias_workspace_only() {
    let file_provider = Arc::new(MockFileProvider::new());
    let remote_fetcher = Arc::new(MockRemoteFetcher::new());

    // Set up workspace with pcb.toml
    let workspace_root = PathBuf::from("/workspace");
    file_provider.add_directory(&workspace_root);
    file_provider.add_file(
        workspace_root.join("pcb.toml"),
        r#"
[workspace]
name = "test"

[packages]
stdlib = "@github/diodeinc/stdlib:v1.0.0"
"#,
    );

    let resolver = CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher.clone(),
        workspace_root.clone(),
        true,
    );

    // Test resolving package from workspace root directory
    let spec = LoadSpec::Package {
        package: "stdlib".to_string(),
        tag: "latest".to_string(),
        path: PathBuf::from("units.zen"),
    };

    let current_file = workspace_root.join("main.zen");
    file_provider.add_file(&current_file, "# Main file");

    // Set up expected resolution
    let cache_path = PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/units.zen");
    remote_fetcher.add_fetch_result("@github/diodeinc/stdlib:v1.0.0/units.zen", &cache_path);
    file_provider.add_file(&cache_path, "# Units");

    let resolved = resolver.resolve_spec(&spec, &current_file).unwrap();

    assert_eq!(resolved, cache_path);

    // Test passed - hierarchical alias resolution working correctly
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_hierarchical_alias_nested_override() {
    let file_provider = Arc::new(MockFileProvider::new());
    let remote_fetcher = Arc::new(MockRemoteFetcher::new());

    // Set up workspace hierarchy
    let workspace_root = PathBuf::from("/workspace");
    let modules_dir = workspace_root.join("modules");

    file_provider.add_directory(&workspace_root);
    file_provider.add_directory(&modules_dir);

    // Workspace pcb.toml
    file_provider.add_file(
        workspace_root.join("pcb.toml"),
        r#"
[workspace]
name = "test"

[packages]
stdlib = "@github/diodeinc/stdlib:v1.0.0"
custom = "@github/workspace/custom"
"#,
    );

    // Module-level pcb.toml that overrides stdlib but adds new alias
    file_provider.add_file(
        modules_dir.join("pcb.toml"),
        r#"
[module]
name = "modules"

[packages]
stdlib = "@github/diodeinc/stdlib:v2.0.0"
local = "./local"
"#,
    );

    let resolver = CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher.clone(),
        workspace_root.clone(),
        true,
    );

    // Test from workspace root - should use v1.0.0
    let spec_workspace = LoadSpec::Package {
        package: "stdlib".to_string(),
        tag: "latest".to_string(),
        path: PathBuf::from("units.zen"),
    };

    let workspace_file = workspace_root.join("main.zen");
    file_provider.add_file(&workspace_file, "# Main file");
    let workspace_cache =
        PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/v1/units.zen");
    remote_fetcher.add_fetch_result("@github/diodeinc/stdlib:v1.0.0/units.zen", &workspace_cache);
    file_provider.add_file(&workspace_cache, "# Units v1");

    let resolved_workspace = resolver
        .resolve_spec(&spec_workspace, &workspace_file)
        .unwrap();
    assert_eq!(resolved_workspace, workspace_cache);

    // Test from modules directory - should use v2.0.0
    let spec_modules = LoadSpec::Package {
        package: "stdlib".to_string(),
        tag: "latest".to_string(),
        path: PathBuf::from("units.zen"),
    };

    let modules_file = modules_dir.join("module.zen");
    file_provider.add_file(&modules_file, "# Module file");
    let modules_cache = PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/v2/units.zen");
    remote_fetcher.add_fetch_result("@github/diodeinc/stdlib:v2.0.0/units.zen", &modules_cache);
    file_provider.add_file(&modules_cache, "# Units v2");

    let resolved_modules = resolver.resolve_spec(&spec_modules, &modules_file).unwrap();
    assert_eq!(resolved_modules, modules_cache);

    // Test custom alias from workspace (should work from modules too)
    let spec_custom = LoadSpec::Package {
        package: "custom".to_string(),
        tag: "latest".to_string(),
        path: PathBuf::from("lib.zen"),
    };

    let custom_cache = PathBuf::from("/home/user/.cache/pcb/github/workspace/custom/lib.zen");
    remote_fetcher.add_fetch_result("@github/workspace/custom/lib.zen", &custom_cache);
    file_provider.add_file(&custom_cache, "# Custom lib");

    let resolved_custom = resolver.resolve_spec(&spec_custom, &modules_file).unwrap();
    assert_eq!(resolved_custom, custom_cache);

    // Test local alias only available in modules - this would resolve to a path spec
    // We can't easily test path resolution here as it would require actual filesystem setup
}

#[test]
#[cfg(not(target_os = "windows"))]
fn test_concurrent_alias_resolution() {
    use std::thread;

    let file_provider = Arc::new(MockFileProvider::new());
    let remote_fetcher = Arc::new(MockRemoteFetcher::new());

    // Set up workspace
    let workspace_root = PathBuf::from("/workspace");
    file_provider.add_directory(&workspace_root);
    file_provider.add_file(
        workspace_root.join("pcb.toml"),
        r#"
[workspace]
name = "test"

[packages]
stdlib = "@github/diodeinc/stdlib"
"#,
    );

    let resolver = Arc::new(CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher.clone(),
        workspace_root.clone(),
        true,
    ));

    let spec = LoadSpec::Package {
        package: "stdlib".to_string(),
        tag: "latest".to_string(),
        path: PathBuf::from("units.zen"),
    };

    let cache_path = PathBuf::from("/home/user/.cache/pcb/github/diodeinc/stdlib/units.zen");
    remote_fetcher.add_fetch_result("@github/diodeinc/stdlib/units.zen", &cache_path);
    file_provider.add_file(&cache_path, "# Units");

    // Spawn multiple threads resolving from the same directory
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let file_provider = file_provider.clone();
            let resolver = resolver.clone();
            let spec = spec.clone();
            let workspace_root = workspace_root.clone();

            thread::spawn(move || {
                let file = workspace_root.join(format!("file{i}.zen"));
                file_provider.add_file(&file, format!("# File {i}"));
                resolver.resolve_spec(&spec, &file).unwrap()
            })
        })
        .collect();

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // All threads should get the same result
    for result in &results {
        assert_eq!(*result, cache_path);
    }
}
