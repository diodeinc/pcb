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
        workspace_root: Option<&Path>,
    ) -> Result<PathBuf, anyhow::Error> {
        self.fetch_calls
            .lock()
            .unwrap()
            .push((spec.clone(), workspace_root.map(|p| p.to_path_buf())));

        let spec_str = spec.to_load_string();
        self.fetch_results
            .lock()
            .unwrap()
            .get(&spec_str)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No mock result for spec: {}", spec_str))
    }
}

#[test]
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

    let resolver = CoreLoadResolver::new(
        file_provider.clone(),
        remote_fetcher.clone(),
        Some(PathBuf::from("/workspace")),
    );

    let spec = LoadSpec::Github {
        user: "diodeinc".to_string(),
        repo: "stdlib".to_string(),
        rev: "HEAD".to_string(),
        path: PathBuf::from("zen/generics/Resistor.zen"),
    };

    let current_file = PathBuf::from("/workspace/main.zen");
    let resolved = resolver
        .resolve_spec(file_provider.as_ref(), &spec, &current_file)
        .unwrap();

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
        Some(PathBuf::from("/workspace")),
    );

    // First, let's test resolving a relative path from the cached Resistor.zen
    // This test will fail with the current implementation, but shows what we want to achieve
    let relative_spec = LoadSpec::Path {
        path: PathBuf::from("../units.zen"),
    };

    // When resolving from the cached file, it should understand that this file
    // came from @github/diodeinc/stdlib:zen/generics/Resistor.zen
    // and resolve ../units.zen as @github/diodeinc/stdlib:zen/units.zen

    // For now, this will resolve as a regular relative path
    let resolved = resolver
        .resolve_spec(file_provider.as_ref(), &relative_spec, &resistor_cache_path)
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
        Some(PathBuf::from("/workspace")),
    );

    // When resolving a workspace path from a remote file, it should be
    // resolved relative to the remote repository's root, not the local workspace
    let _workspace_spec = LoadSpec::WorkspacePath {
        path: PathBuf::from("common/utils.zen"),
    };

    // This test shows what we want to achieve - workspace paths in remote files
    // should resolve within that remote repository
    // Currently this will fail as it tries to resolve in the local workspace
}

#[test]
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
        Some(workspace_root.clone()),
    );

    // Test resolving a package alias
    let spec = LoadSpec::Package {
        package: "stdlib".to_string(),
        tag: "latest".to_string(),
        path: PathBuf::from("zen/generics/Resistor.zen"),
    };

    let current_file = workspace_root.join("main.zen");
    let resolved = resolver
        .resolve_spec(file_provider.as_ref(), &spec, &current_file)
        .unwrap();

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
        Some(PathBuf::from("/workspace")),
    );

    // First resolve the GitHub spec for Resistor.zen
    let github_spec = LoadSpec::Github {
        user: "diodeinc".to_string(),
        repo: "stdlib".to_string(),
        rev: "HEAD".to_string(),
        path: PathBuf::from("zen/generics/Resistor.zen"),
    };

    let resolved_resistor = resolver
        .resolve_spec(
            file_provider.as_ref(),
            &github_spec,
            &PathBuf::from("/workspace/main.zen"),
        )
        .unwrap();

    assert_eq!(resolved_resistor, resistor_cache_path);

    // Now when we resolve a relative path from the cached Resistor.zen
    let relative_spec = LoadSpec::Path {
        path: PathBuf::from("../units.zen"),
    };

    // This should understand that resistor_cache_path came from
    // @github/diodeinc/stdlib/zen/generics/Resistor.zen
    // and resolve ../units.zen as @github/diodeinc/stdlib/zen/units.zen
    let resolved_units = resolver
        .resolve_spec(file_provider.as_ref(), &relative_spec, &resistor_cache_path)
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
        Some(PathBuf::from("/workspace")),
    );

    // Resolve the initial file
    let module_spec = LoadSpec::Github {
        user: "diodeinc".to_string(),
        repo: "stdlib".to_string(),
        rev: "HEAD".to_string(),
        path: PathBuf::from("zen/module.zen"),
    };

    let resolved_module = resolver
        .resolve_spec(
            file_provider.as_ref(),
            &module_spec,
            &PathBuf::from("/workspace/main.zen"),
        )
        .unwrap();

    assert_eq!(resolved_module, remote_cache_path);

    // Now resolve a workspace path from within the remote file
    let workspace_spec = LoadSpec::WorkspacePath {
        path: PathBuf::from("common/utils.zen"),
    };

    // This should understand that remote_cache_path is from @github/diodeinc/stdlib
    // and resolve //common/utils.zen relative to that repository's root
    let resolved_utils = resolver
        .resolve_spec(file_provider.as_ref(), &workspace_spec, &remote_cache_path)
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
