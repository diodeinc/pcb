use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Default tag that is assumed when the caller does not specify one in a
/// package spec, e.g. `@mypkg/utils.zen`.
pub const DEFAULT_PKG_TAG: &str = "latest";

/// Default git revision that is assumed when the caller omits one in a GitHub
/// spec, e.g. `@github/user/repo/path.zen`.
pub const DEFAULT_GITHUB_REV: &str = "HEAD";

/// Default git revision that is assumed when the caller omits one in a GitLab
/// spec, e.g. `@gitlab/user/repo/path.zen`.
pub const DEFAULT_GITLAB_REV: &str = "HEAD";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LoadSpec {
    Package {
        package: String,
        tag: String,
        path: PathBuf,
    },
    Github {
        user: String,
        repo: String,
        rev: String,
        path: PathBuf,
    },
    Gitlab {
        project_path: String, // Can be "user/repo" or "group/subgroup/repo"
        rev: String,
        path: PathBuf,
    },
    /// Raw file path (relative or absolute)
    Path { path: PathBuf },
    /// Workspace-relative path (starts with //)
    WorkspacePath { path: PathBuf },
}

impl LoadSpec {
    /// Parse the raw string passed to `load()` into a [`LoadSpec`].
    ///
    /// The supported grammar is:
    ///
    /// • **Package reference** – `"@<package>[:<tag>]/<optional/path>"`.
    ///   If `<tag>` is omitted the [`DEFAULT_PKG_TAG`] (currently `"latest"`) is
    ///   assumed.
    ///   Example: `"@stdlib:1.2.3/math.zen"` or `"@stdlib/math.zen"`.
    ///
    /// • **GitHub repository** –
    ///   `"@github/<user>/<repo>[:<rev>]/<path>"`.
    ///   If `<rev>` is omitted the special value [`DEFAULT_GITHUB_REV`] (currently
    ///   `"HEAD"`) is assumed.
    ///   The `<rev>` component can be a branch name, tag, or a short/long commit
    ///   SHA (7–40 hexadecimal characters).
    ///   Example: `"@github/foo/bar:abc123/scripts/build.zen".
    ///
    /// • **GitLab repository** –
    ///   `"@gitlab/<user>/<repo>[:<rev>]/<path>"`.
    ///   If `<rev>` is omitted the special value [`DEFAULT_GITLAB_REV`] (currently
    ///   `"HEAD"`) is assumed.
    ///   The `<rev>` component can be a branch name, tag, or a short/long commit
    ///   SHA (7–40 hexadecimal characters).
    ///   
    ///   For nested groups, include the full path before the revision:
    ///   `"@gitlab/group/subgroup/repo:rev/path"`.
    ///   Without a revision, the first two path components are assumed to be the project path.
    ///   
    ///   Examples:
    ///   - `"@gitlab/foo/bar:main/src/lib.zen"` - Simple user/repo with revision
    ///   - `"@gitlab/foo/bar/src/lib.zen"` - Simple user/repo without revision (assumes HEAD)
    ///   - `"@gitlab/kicad/libraries/kicad-symbols:main/Device.kicad_sym"` - Nested groups with revision
    ///
    /// • **Workspace-relative path** – `"//<path>"`.
    ///   Paths starting with `//` are resolved relative to the workspace root.
    ///   Example: `"//src/components/resistor.zen"`.
    ///
    /// • **Raw file path** – Any other string is treated as a raw file path (relative or absolute).
    ///   Examples: `"./math.zen"`, `"../utils/helper.zen"`, `"/absolute/path/file.zen"`.
    ///
    /// The function does not touch the filesystem – it only performs syntactic
    /// parsing.
    pub fn parse(s: &str) -> Option<LoadSpec> {
        if let Some(rest) = s.strip_prefix("@github/") {
            // GitHub: @github/user/repo:rev/path  (must come before generic "@pkg" handling)
            let mut user_repo_rev_and_path = rest.splitn(3, '/');
            let user = user_repo_rev_and_path.next().unwrap_or("").to_string();
            let repo_and_rev = user_repo_rev_and_path.next().unwrap_or("");
            let remaining_path = user_repo_rev_and_path.next().unwrap_or("");

            // Validate that we have both user and repo
            if user.is_empty() || repo_and_rev.is_empty() {
                return None;
            }

            let (repo, rev) = if let Some((repo, rev)) = repo_and_rev.split_once(':') {
                (repo.to_string(), rev.to_string())
            } else {
                (repo_and_rev.to_string(), DEFAULT_GITHUB_REV.to_string())
            };

            // Ensure repo name is not empty
            if repo.is_empty() {
                return None;
            }

            Some(LoadSpec::Github {
                user,
                repo,
                rev,
                path: PathBuf::from(remaining_path),
            })
        } else if let Some(rest) = s.strip_prefix("@gitlab/") {
            // GitLab: @gitlab/group/subgroup/repo:rev/path
            // We need to find where the project path ends and the file path begins
            // This is tricky because both can contain slashes

            // First, check if there's a revision marker ':'
            if let Some(colon_pos) = rest.find(':') {
                // We have a revision specified
                let project_part = &rest[..colon_pos];
                let after_colon = &rest[colon_pos + 1..];

                // Find the first slash after the colon to separate rev from path
                if let Some(slash_pos) = after_colon.find('/') {
                    let rev = after_colon[..slash_pos].to_string();
                    let file_path = after_colon[slash_pos + 1..].to_string();

                    Some(LoadSpec::Gitlab {
                        project_path: project_part.to_string(),
                        rev,
                        path: PathBuf::from(file_path),
                    })
                } else {
                    // No file path after revision
                    Some(LoadSpec::Gitlab {
                        project_path: project_part.to_string(),
                        rev: after_colon.to_string(),
                        path: PathBuf::new(),
                    })
                }
            } else {
                // No revision specified, assume first 2 parts are the project path
                let parts: Vec<&str> = rest.splitn(3, '/').collect();
                if parts.len() >= 2 {
                    let project_path = format!("{}/{}", parts[0], parts[1]);
                    let file_path = parts.get(2).unwrap_or(&"").to_string();

                    Some(LoadSpec::Gitlab {
                        project_path,
                        rev: DEFAULT_GITLAB_REV.to_string(),
                        path: PathBuf::from(file_path),
                    })
                } else {
                    None
                }
            }
        } else if let Some(rest) = s.strip_prefix('@') {
            // Generic package: @<pkg>[:<tag>]/optional/path
            // rest looks like "pkg[:tag]/path..." or just "pkg"/"pkg:tag"
            let mut parts = rest.splitn(2, '/');
            let pkg_and_tag = parts.next().unwrap_or("");
            let rel_path = parts.next().unwrap_or("");

            // Validate that we have a non-empty package name
            if pkg_and_tag.is_empty() {
                return None;
            }

            let (package, tag) = if let Some((pkg, tag)) = pkg_and_tag.split_once(':') {
                (pkg.to_string(), tag.to_string())
            } else {
                (pkg_and_tag.to_string(), DEFAULT_PKG_TAG.to_string())
            };

            // Ensure package name is not empty
            if package.is_empty() {
                return None;
            }

            // Reject invalid GitHub/GitLab specs that don't have the proper format
            if package == "github" || package == "gitlab" {
                return None;
            }

            Some(LoadSpec::Package {
                package,
                tag,
                path: PathBuf::from(rel_path),
            })
        } else if let Some(workspace_path) = s.strip_prefix("//") {
            // Workspace-relative path: //path/to/file.zen
            Some(LoadSpec::WorkspacePath {
                path: PathBuf::from(workspace_path),
            })
        } else {
            // Raw file path (relative or absolute)
            Some(LoadSpec::Path {
                path: PathBuf::from(s),
            })
        }
    }

    /// Default package aliases that are always available
    pub fn default_package_aliases() -> HashMap<String, String> {
        let mut map = HashMap::new();
        map.insert(
            "kicad-symbols".to_string(),
            "@gitlab/kicad/libraries/kicad-symbols:9.0.0".to_string(),
        );
        map.insert(
            "kicad-footprints".to_string(),
            "@gitlab/kicad/libraries/kicad-footprints:9.0.0".to_string(),
        );
        map.insert(
            "stdlib".to_string(),
            "@github/diodeinc/stdlib:HEAD".to_string(),
        );
        map
    }

    /// Resolve a LoadSpec by handling aliases and workspace-specific settings.
    /// This function handles package alias resolution and workspace-specific configuration.
    ///
    /// For Package specs, it checks for aliases in the workspace configuration and default aliases.
    /// If an alias is found, it parses the target and applies any tag overrides and path joining.
    /// Other spec types (Github, Gitlab, Path, WorkspacePath) are returned unchanged.
    ///
    /// # Arguments
    /// * `spec` - The LoadSpec to resolve
    /// * `workspace_aliases` - Optional workspace-specific aliases (overrides defaults)
    ///
    /// # Returns
    /// A resolved LoadSpec, which may be the same as the input or a new spec based on alias resolution.
    pub fn resolve(
        &self,
        workspace_aliases: Option<&HashMap<String, String>>,
    ) -> Result<LoadSpec, anyhow::Error> {
        match self {
            LoadSpec::Package { package, tag, path } => {
                // Check for package aliases (workspace or default)
                let aliases = if let Some(ws_aliases) = workspace_aliases {
                    // Use provided workspace aliases
                    ws_aliases
                } else {
                    // Fall back to default aliases only
                    &Self::default_package_aliases()
                };

                if let Some(target) = aliases.get(package) {
                    // Parse the alias target
                    if let Some(mut resolved_spec) = LoadSpec::parse(target) {
                        // If caller explicitly specified a tag (non-default), override the alias's tag
                        if tag != DEFAULT_PKG_TAG {
                            match &mut resolved_spec {
                                LoadSpec::Package { tag: alias_tag, .. } => {
                                    *alias_tag = tag.clone();
                                }
                                LoadSpec::Github { rev: alias_rev, .. } => {
                                    *alias_rev = tag.clone();
                                }
                                LoadSpec::Gitlab { rev: alias_rev, .. } => {
                                    *alias_rev = tag.clone();
                                }
                                // Path and WorkspacePath specs don't support tags
                                LoadSpec::Path { .. } | LoadSpec::WorkspacePath { .. } => {
                                    return Err(anyhow::anyhow!(
                                        "Cannot apply tag '{}' to path-based alias target '{}'",
                                        tag,
                                        target
                                    ));
                                }
                            }
                        }

                        // Append the path if needed
                        if !path.as_os_str().is_empty() {
                            match &mut resolved_spec {
                                LoadSpec::Package {
                                    path: alias_path, ..
                                } => {
                                    *alias_path = alias_path.join(path);
                                }
                                LoadSpec::Github {
                                    path: alias_path, ..
                                } => {
                                    *alias_path = alias_path.join(path);
                                }
                                LoadSpec::Gitlab {
                                    path: alias_path, ..
                                } => {
                                    *alias_path = alias_path.join(path);
                                }
                                LoadSpec::Path { path: alias_path } => {
                                    *alias_path = alias_path.join(path);
                                }
                                LoadSpec::WorkspacePath { path: alias_path } => {
                                    *alias_path = alias_path.join(path);
                                }
                            }
                        }

                        Ok(resolved_spec)
                    } else {
                        // Invalid alias target
                        Err(anyhow::anyhow!(
                            "Invalid alias target for package '{}': '{}'",
                            package,
                            target
                        ))
                    }
                } else {
                    // No alias found, return original spec
                    Ok(self.clone())
                }
            }
            // Other spec types pass through unchanged
            _ => Ok(self.clone()),
        }
    }
    /// Check if this LoadSpec represents a remote resource that needs to be downloaded.
    /// Returns true for Package, Github, and Gitlab specs.
    /// Returns false for Path and WorkspacePath specs.
    pub fn is_remote(&self) -> bool {
        matches!(
            self,
            LoadSpec::Package { .. } | LoadSpec::Github { .. } | LoadSpec::Gitlab { .. }
        )
    }

    /// Convert the LoadSpec back to a load string representation.
    /// This is useful for error messages and debugging.
    pub fn to_load_string(&self) -> String {
        match self {
            LoadSpec::Package { package, tag, path } => {
                let base = if tag == DEFAULT_PKG_TAG {
                    format!("@{package}")
                } else {
                    format!("@{package}:{tag}")
                };
                if path.as_os_str().is_empty() {
                    base
                } else {
                    format!("{}/{}", base, path.display())
                }
            }
            LoadSpec::Github {
                user,
                repo,
                rev,
                path,
            } => {
                let base = if rev == DEFAULT_GITHUB_REV {
                    format!("@github/{user}/{repo}")
                } else {
                    format!("@github/{user}/{repo}:{rev}")
                };
                if path.as_os_str().is_empty() {
                    base
                } else {
                    format!("{}/{}", base, path.display())
                }
            }
            LoadSpec::Gitlab {
                project_path,
                rev,
                path,
            } => {
                let base = if rev == DEFAULT_GITLAB_REV {
                    format!("@gitlab/{project_path}")
                } else {
                    format!("@gitlab/{project_path}:{rev}")
                };
                if path.as_os_str().is_empty() {
                    base
                } else {
                    format!("{}/{}", base, path.display())
                }
            }
            LoadSpec::Path { path } => path.display().to_string(),
            LoadSpec::WorkspacePath { path } => format!("//{}", path.display()),
        }
    }

    /// Generate a cache key for a LoadSpec.
    /// This ensures consistent caching across different environments and implementations.
    ///
    /// The cache key format is designed to be:
    /// - Unique for each distinct spec
    /// - Consistent across platforms
    /// - Human-readable for debugging
    ///
    /// # Returns
    /// A string that uniquely identifies the LoadSpec for caching purposes.
    pub fn cache_key(&self) -> String {
        match self {
            LoadSpec::Package { package, tag, path } => {
                if path.as_os_str().is_empty() {
                    format!("pkg:{package}:{tag}")
                } else {
                    format!("pkg:{}:{}:{}", package, tag, path.display())
                }
            }
            LoadSpec::Github {
                user,
                repo,
                rev,
                path,
            } => {
                if path.as_os_str().is_empty() {
                    format!("gh:{user}:{repo}:{rev}")
                } else {
                    format!("gh:{}:{}:{}:{}", user, repo, rev, path.display())
                }
            }
            LoadSpec::Gitlab {
                project_path,
                rev,
                path,
            } => {
                if path.as_os_str().is_empty() {
                    format!("gl:{project_path}:{rev}")
                } else {
                    format!("gl:{}:{}:{}", project_path, rev, path.display())
                }
            }
            LoadSpec::Path { path } => {
                format!("path:{}", path.display())
            }
            LoadSpec::WorkspacePath { path } => {
                format!("ws:{}", path.display())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_load_spec_package_no_tag() {
        let spec = LoadSpec::parse("@stdlib/math.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: DEFAULT_PKG_TAG.to_string(),
                path: PathBuf::from("math.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_package_with_tag() {
        let spec = LoadSpec::parse("@stdlib:1.2.3/math.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: "1.2.3".to_string(),
                path: PathBuf::from("math.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_github_no_rev() {
        let spec = LoadSpec::parse("@github/foo/bar/scripts/build.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Github {
                user: "foo".to_string(),
                repo: "bar".to_string(),
                rev: DEFAULT_GITHUB_REV.to_string(),
                path: PathBuf::from("scripts/build.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_github_with_rev() {
        let spec = LoadSpec::parse("@github/foo/bar:abc123/scripts/build.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Github {
                user: "foo".to_string(),
                repo: "bar".to_string(),
                rev: "abc123".to_string(),
                path: PathBuf::from("scripts/build.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_github_empty_path() {
        let spec = LoadSpec::parse("@github/foo/bar:abc123/");
        assert_eq!(
            spec,
            Some(LoadSpec::Github {
                user: "foo".to_string(),
                repo: "bar".to_string(),
                rev: "abc123".to_string(),
                path: PathBuf::from(""),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_github_no_path() {
        let spec = LoadSpec::parse("@github/foo/bar:abc123");
        assert_eq!(
            spec,
            Some(LoadSpec::Github {
                user: "foo".to_string(),
                repo: "bar".to_string(),
                rev: "abc123".to_string(),
                path: PathBuf::from(""),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_gitlab_with_rev() {
        let spec = LoadSpec::parse("@gitlab/foo/bar:abc123/src/lib.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "foo/bar".to_string(),
                rev: "abc123".to_string(),
                path: PathBuf::from("src/lib.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_gitlab_no_rev() {
        let spec = LoadSpec::parse("@gitlab/foo/bar/src/lib.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "foo/bar".to_string(),
                rev: DEFAULT_GITLAB_REV.to_string(),
                path: PathBuf::from("src/lib.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_gitlab_nested_groups_with_rev() {
        let spec = LoadSpec::parse("@gitlab/kicad/libraries/kicad-symbols:main/Device.kicad_sym");
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "kicad/libraries/kicad-symbols".to_string(),
                rev: "main".to_string(),
                path: PathBuf::from("Device.kicad_sym"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_gitlab_sha() {
        let sha = "a1b2c3d4e5f6789012345678901234567890abcd";
        let spec = LoadSpec::parse(&format!("@gitlab/foo/bar:{sha}/src/lib.zen"));
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "foo/bar".to_string(),
                rev: sha.to_string(),
                path: PathBuf::from("src/lib.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_gitlab_nested_groups_no_rev() {
        let spec = LoadSpec::parse("@gitlab/user/repo/src/lib.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "user/repo".to_string(),
                rev: DEFAULT_GITLAB_REV.to_string(),
                path: PathBuf::from("src/lib.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_gitlab_nested_groups_with_tag() {
        let spec = LoadSpec::parse("@gitlab/kicad/libraries/kicad-symbols:v7.0.0/Device.kicad_sym");
        assert_eq!(
            spec,
            Some(LoadSpec::Gitlab {
                project_path: "kicad/libraries/kicad-symbols".to_string(),
                rev: "v7.0.0".to_string(),
                path: PathBuf::from("Device.kicad_sym"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_workspace_path() {
        let spec = LoadSpec::parse("//src/components/resistor.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::WorkspacePath {
                path: PathBuf::from("src/components/resistor.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_workspace_path_root() {
        let spec = LoadSpec::parse("//math.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::WorkspacePath {
                path: PathBuf::from("math.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_workspace_path_empty() {
        let spec = LoadSpec::parse("//");
        assert_eq!(
            spec,
            Some(LoadSpec::WorkspacePath {
                path: PathBuf::from(""),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_relative_path() {
        let spec = LoadSpec::parse("./math.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Path {
                path: PathBuf::from("./math.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_relative_path_parent() {
        let spec = LoadSpec::parse("../utils/helper.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Path {
                path: PathBuf::from("../utils/helper.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_absolute_path() {
        let spec = LoadSpec::parse("/absolute/path/file.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Path {
                path: PathBuf::from("/absolute/path/file.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_simple_filename() {
        let spec = LoadSpec::parse("math.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Path {
                path: PathBuf::from("math.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_invalid() {
        // These should still return Some(LoadSpec::Path) since we now handle all strings
        let spec = LoadSpec::parse("not_a_load_spec");
        assert_eq!(
            spec,
            Some(LoadSpec::Path {
                path: PathBuf::from("not_a_load_spec"),
            })
        );

        // Invalid @ specs should still return None
        assert_eq!(LoadSpec::parse("@"), None);
        assert_eq!(LoadSpec::parse("@github"), None);
        assert_eq!(LoadSpec::parse("@github/"), None);
        assert_eq!(LoadSpec::parse("@github/user"), None);
    }

    #[test]
    fn test_load_spec_serialization() {
        let spec = LoadSpec::Package {
            package: "stdlib".to_string(),
            tag: "latest".to_string(),
            path: PathBuf::from("math.zen"),
        };

        // Test serialization
        let json = serde_json::to_string(&spec).expect("Failed to serialize LoadSpec");

        // Test deserialization
        let deserialized: LoadSpec =
            serde_json::from_str(&json).expect("Failed to deserialize LoadSpec");

        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_github_spec_serialization() {
        let spec = LoadSpec::Github {
            user: "foo".to_string(),
            repo: "bar".to_string(),
            rev: "main".to_string(),
            path: PathBuf::from("src/lib.zen"),
        };

        // Test serialization
        let json = serde_json::to_string(&spec).expect("Failed to serialize LoadSpec");

        // Test deserialization
        let deserialized: LoadSpec =
            serde_json::from_str(&json).expect("Failed to deserialize LoadSpec");

        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_gitlab_spec_serialization() {
        let spec = LoadSpec::Gitlab {
            project_path: "group/subgroup/repo".to_string(),
            rev: "v1.0.0".to_string(),
            path: PathBuf::from("lib/module.zen"),
        };

        // Test serialization
        let json = serde_json::to_string(&spec).expect("Failed to serialize LoadSpec");

        // Test deserialization
        let deserialized: LoadSpec =
            serde_json::from_str(&json).expect("Failed to deserialize LoadSpec");

        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_path_spec_serialization() {
        let spec = LoadSpec::Path {
            path: PathBuf::from("./relative/path/file.zen"),
        };

        // Test serialization
        let json = serde_json::to_string(&spec).expect("Failed to serialize LoadSpec");

        // Test deserialization
        let deserialized: LoadSpec =
            serde_json::from_str(&json).expect("Failed to deserialize LoadSpec");

        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_path_spec_serialization_absolute() {
        let spec = LoadSpec::Path {
            path: PathBuf::from("/absolute/path/file.zen"),
        };

        // Test serialization
        let json = serde_json::to_string(&spec).expect("Failed to serialize LoadSpec");

        // Test deserialization
        let deserialized: LoadSpec =
            serde_json::from_str(&json).expect("Failed to deserialize LoadSpec");

        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_workspace_path_spec_serialization() {
        let spec = LoadSpec::WorkspacePath {
            path: PathBuf::from("src/components/resistor.zen"),
        };

        // Test serialization
        let json = serde_json::to_string(&spec).expect("Failed to serialize LoadSpec");

        // Test deserialization
        let deserialized: LoadSpec =
            serde_json::from_str(&json).expect("Failed to deserialize LoadSpec");

        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_all_load_spec_variants_serialization() {
        let specs = vec![
            LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::from("math.zen"),
            },
            LoadSpec::Github {
                user: "user".to_string(),
                repo: "repo".to_string(),
                rev: "main".to_string(),
                path: PathBuf::from("src/lib.zen"),
            },
            LoadSpec::Gitlab {
                project_path: "group/repo".to_string(),
                rev: "v1.0.0".to_string(),
                path: PathBuf::from("lib/module.zen"),
            },
            LoadSpec::Path {
                path: PathBuf::from("./relative/file.zen"),
            },
            LoadSpec::WorkspacePath {
                path: PathBuf::from("workspace/file.zen"),
            },
        ];

        for spec in specs {
            // Test serialization
            let json = serde_json::to_string(&spec).expect("Failed to serialize LoadSpec");

            // Test deserialization
            let deserialized: LoadSpec =
                serde_json::from_str(&json).expect("Failed to deserialize LoadSpec");

            assert_eq!(spec, deserialized);
        }
    }

    // Tests for resolve_load_spec function
    mod resolve_load_spec_tests {
        use super::*;

        #[test]
        fn test_resolve_package_no_alias() {
            let spec = LoadSpec::Package {
                package: "unknown-package".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::from("math.zen"),
            };

            let resolved = spec.resolve(None).unwrap();
            assert_eq!(resolved, spec); // Should return unchanged
        }

        #[test]
        fn test_resolve_package_with_default_alias() {
            let spec = LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::from("math.zen"),
            };

            let resolved = spec.resolve(None).unwrap();

            // Should resolve to GitHub spec based on default alias
            assert_eq!(
                resolved,
                LoadSpec::Github {
                    user: "diodeinc".to_string(),
                    repo: "stdlib".to_string(),
                    rev: "HEAD".to_string(),
                    path: PathBuf::from("math.zen"),
                }
            );
        }

        #[test]
        fn test_resolve_package_with_custom_tag() {
            let spec = LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: "v1.2.3".to_string(),
                path: PathBuf::from("math.zen"),
            };

            let resolved = spec.resolve(None).unwrap();

            // Should resolve to GitHub spec with custom tag overriding default
            assert_eq!(
                resolved,
                LoadSpec::Github {
                    user: "diodeinc".to_string(),
                    repo: "stdlib".to_string(),
                    rev: "v1.2.3".to_string(), // Custom tag should override default
                    path: PathBuf::from("math.zen"),
                }
            );
        }

        #[test]
        fn test_resolve_package_with_workspace_aliases() {
            let spec = LoadSpec::Package {
                package: "custom-lib".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::from("utils.zen"),
            };

            let mut workspace_aliases = HashMap::new();
            workspace_aliases.insert(
                "custom-lib".to_string(),
                "@github/myorg/custom-lib:main".to_string(),
            );

            let resolved = spec.resolve(Some(&workspace_aliases)).unwrap();

            assert_eq!(
                resolved,
                LoadSpec::Github {
                    user: "myorg".to_string(),
                    repo: "custom-lib".to_string(),
                    rev: "main".to_string(),
                    path: PathBuf::from("utils.zen"),
                }
            );
        }

        #[test]
        fn test_resolve_package_workspace_overrides_default() {
            let spec = LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::from("math.zen"),
            };

            let mut workspace_aliases = HashMap::new();
            workspace_aliases.insert(
                "stdlib".to_string(),
                "@github/myorg/my-stdlib:v2.0.0".to_string(),
            );

            let resolved = spec.resolve(Some(&workspace_aliases)).unwrap();

            // Workspace alias should override default
            assert_eq!(
                resolved,
                LoadSpec::Github {
                    user: "myorg".to_string(),
                    repo: "my-stdlib".to_string(),
                    rev: "v2.0.0".to_string(),
                    path: PathBuf::from("math.zen"),
                }
            );
        }

        #[test]
        fn test_resolve_package_alias_to_gitlab() {
            let spec = LoadSpec::Package {
                package: "kicad-symbols".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::from("Device.kicad_sym"),
            };

            let resolved = spec.resolve(None).unwrap();

            // Should resolve to GitLab spec based on default alias
            assert_eq!(
                resolved,
                LoadSpec::Gitlab {
                    project_path: "kicad/libraries/kicad-symbols".to_string(),
                    rev: "9.0.0".to_string(),
                    path: PathBuf::from("Device.kicad_sym"),
                }
            );
        }

        #[test]
        fn test_resolve_package_alias_to_path() {
            let spec = LoadSpec::Package {
                package: "local-lib".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::from("utils.zen"),
            };

            let mut workspace_aliases = HashMap::new();
            workspace_aliases.insert("local-lib".to_string(), "./local/lib".to_string());

            let resolved = spec.resolve(Some(&workspace_aliases)).unwrap();

            assert_eq!(
                resolved,
                LoadSpec::Path {
                    path: PathBuf::from("./local/lib/utils.zen"),
                }
            );
        }

        #[test]
        fn test_resolve_package_alias_to_workspace_path() {
            let spec = LoadSpec::Package {
                package: "workspace-lib".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::from("utils.zen"),
            };

            let mut workspace_aliases = HashMap::new();
            workspace_aliases.insert(
                "workspace-lib".to_string(),
                "//libs/workspace-lib".to_string(),
            );

            let resolved = spec.resolve(Some(&workspace_aliases)).unwrap();

            assert_eq!(
                resolved,
                LoadSpec::WorkspacePath {
                    path: PathBuf::from("libs/workspace-lib/utils.zen"),
                }
            );
        }

        #[test]
        fn test_resolve_package_empty_path() {
            let spec = LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::new(),
            };

            let resolved = spec.resolve(None).unwrap();

            // Should resolve without adding path
            assert_eq!(
                resolved,
                LoadSpec::Github {
                    user: "diodeinc".to_string(),
                    repo: "stdlib".to_string(),
                    rev: "HEAD".to_string(),
                    path: PathBuf::new(),
                }
            );
        }

        #[test]
        fn test_resolve_package_tag_on_path_alias_error() {
            let spec = LoadSpec::Package {
                package: "local-lib".to_string(),
                tag: "v1.0.0".to_string(), // Non-default tag
                path: PathBuf::from("utils.zen"),
            };

            let mut workspace_aliases = HashMap::new();
            workspace_aliases.insert(
                "local-lib".to_string(),
                "./local/lib".to_string(), // Path-based alias
            );

            let result = spec.resolve(Some(&workspace_aliases));

            // Should error because we can't apply tags to path-based aliases
            assert!(result.is_err());
            assert!(result.unwrap_err().to_string().contains("Cannot apply tag"));
        }

        #[test]
        fn test_resolve_package_invalid_alias_target() {
            let spec = LoadSpec::Package {
                package: "bad-alias".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::from("utils.zen"),
            };

            let mut workspace_aliases = HashMap::new();
            workspace_aliases.insert(
                "bad-alias".to_string(),
                "@".to_string(), // Invalid load spec - just @ with nothing after
            );

            let result = spec.resolve(Some(&workspace_aliases));

            // Should error because alias target is invalid
            assert!(result.is_err());
            assert!(result
                .unwrap_err()
                .to_string()
                .contains("Invalid alias target"));
        }

        #[test]
        fn test_resolve_non_package_specs_unchanged() {
            let specs = vec![
                LoadSpec::Github {
                    user: "user".to_string(),
                    repo: "repo".to_string(),
                    rev: "main".to_string(),
                    path: PathBuf::from("src/lib.zen"),
                },
                LoadSpec::Gitlab {
                    project_path: "group/repo".to_string(),
                    rev: "v1.0.0".to_string(),
                    path: PathBuf::from("lib/module.zen"),
                },
                LoadSpec::Path {
                    path: PathBuf::from("./relative/file.zen"),
                },
                LoadSpec::WorkspacePath {
                    path: PathBuf::from("workspace/file.zen"),
                },
            ];

            for spec in specs {
                let resolved = spec.resolve(None).unwrap();
                assert_eq!(resolved, spec); // Should return unchanged
            }
        }
    }

    // Tests for cache_key_for_spec function
    mod cache_key_tests {
        use super::*;

        #[test]
        fn test_cache_key_package() {
            let spec = LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::from("math.zen"),
            };

            let key = spec.cache_key();
            assert_eq!(key, "pkg:stdlib:latest:math.zen");
        }

        #[test]
        fn test_cache_key_package_empty_path() {
            let spec = LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::new(),
            };

            let key = spec.cache_key();
            assert_eq!(key, "pkg:stdlib:latest");
        }

        #[test]
        fn test_cache_key_github() {
            let spec = LoadSpec::Github {
                user: "user".to_string(),
                repo: "repo".to_string(),
                rev: "main".to_string(),
                path: PathBuf::from("src/lib.zen"),
            };

            let key = spec.cache_key();
            assert_eq!(key, "gh:user:repo:main:src/lib.zen");
        }

        #[test]
        fn test_cache_key_github_empty_path() {
            let spec = LoadSpec::Github {
                user: "user".to_string(),
                repo: "repo".to_string(),
                rev: "main".to_string(),
                path: PathBuf::new(),
            };

            let key = spec.cache_key();
            assert_eq!(key, "gh:user:repo:main");
        }

        #[test]
        fn test_cache_key_gitlab() {
            let spec = LoadSpec::Gitlab {
                project_path: "group/subgroup/repo".to_string(),
                rev: "v1.0.0".to_string(),
                path: PathBuf::from("lib/module.zen"),
            };

            let key = spec.cache_key();
            assert_eq!(key, "gl:group/subgroup/repo:v1.0.0:lib/module.zen");
        }

        #[test]
        fn test_cache_key_gitlab_empty_path() {
            let spec = LoadSpec::Gitlab {
                project_path: "group/repo".to_string(),
                rev: "main".to_string(),
                path: PathBuf::new(),
            };

            let key = spec.cache_key();
            assert_eq!(key, "gl:group/repo:main");
        }

        #[test]
        fn test_cache_key_path() {
            let spec = LoadSpec::Path {
                path: PathBuf::from("./relative/file.zen"),
            };

            let key = spec.cache_key();
            assert_eq!(key, "path:./relative/file.zen");
        }

        #[test]
        fn test_cache_key_workspace_path() {
            let spec = LoadSpec::WorkspacePath {
                path: PathBuf::from("src/components/resistor.zen"),
            };

            let key = spec.cache_key();
            assert_eq!(key, "ws:src/components/resistor.zen");
        }

        #[test]
        fn test_cache_key_uniqueness() {
            // Test that different specs produce different cache keys
            let specs = vec![
                LoadSpec::Package {
                    package: "stdlib".to_string(),
                    tag: "latest".to_string(),
                    path: PathBuf::from("math.zen"),
                },
                LoadSpec::Package {
                    package: "stdlib".to_string(),
                    tag: "v1.0.0".to_string(),
                    path: PathBuf::from("math.zen"),
                },
                LoadSpec::Github {
                    user: "user".to_string(),
                    repo: "repo".to_string(),
                    rev: "main".to_string(),
                    path: PathBuf::from("lib.zen"),
                },
                LoadSpec::Gitlab {
                    project_path: "user/repo".to_string(),
                    rev: "main".to_string(),
                    path: PathBuf::from("lib.zen"),
                },
                LoadSpec::Path {
                    path: PathBuf::from("lib.zen"),
                },
                LoadSpec::WorkspacePath {
                    path: PathBuf::from("lib.zen"),
                },
            ];

            let mut keys = std::collections::HashSet::new();
            for spec in specs {
                let key = spec.cache_key();
                assert!(keys.insert(key), "Cache key collision detected");
            }
        }

        #[test]
        fn test_cache_key_consistency() {
            // Test that the same spec always produces the same cache key
            let spec = LoadSpec::Package {
                package: "stdlib".to_string(),
                tag: "latest".to_string(),
                path: PathBuf::from("math.zen"),
            };

            let key1 = spec.cache_key();
            let key2 = spec.cache_key();
            assert_eq!(key1, key2);
        }
    }
}
