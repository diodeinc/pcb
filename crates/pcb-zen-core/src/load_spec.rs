use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LoadSpec {
    Package {
        package: String,
        path: PathBuf,
    },
    Github {
        user: String,
        repo: String,
        path: PathBuf,
    },
    Gitlab {
        project_path: String, // Can be "user/repo" or "group/subgroup/repo"
        path: PathBuf,
    },
    Path {
        path: PathBuf,
        allow_not_exist: bool,
    },
}

impl std::fmt::Display for LoadSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadSpec::Package { package, path } => {
                if path.as_os_str().is_empty() {
                    write!(f, "@{package}")
                } else {
                    write!(f, "@{package}/{}", path.display())
                }
            }
            LoadSpec::Github { user, repo, path } => {
                let base = format!("github.com/{user}/{repo}");
                if path.as_os_str().is_empty() {
                    write!(f, "{base}")
                } else {
                    write!(f, "{base}/{}", path.display())
                }
            }
            LoadSpec::Gitlab { project_path, path } => {
                let base = format!("gitlab.com/{project_path}");
                if path.as_os_str().is_empty() {
                    write!(f, "{base}")
                } else {
                    write!(f, "{base}/{}", path.display())
                }
            }
            LoadSpec::Path { path, .. } => {
                write!(f, "{}", path.display())
            }
        }
    }
}

impl LoadSpec {
    /// Create a new local path LoadSpec
    pub fn local_path<P: Into<PathBuf>>(path: P) -> Self {
        LoadSpec::Path {
            path: path.into(),
            allow_not_exist: false,
        }
    }

    /// Returns true if this LoadSpec allows the path to not exist.
    pub fn allow_not_exist(&self) -> bool {
        match self {
            LoadSpec::Path {
                allow_not_exist, ..
            } => *allow_not_exist,
            _ => false,
        }
    }

    /// Get the path from this LoadSpec.
    pub fn path(&self) -> &PathBuf {
        match self {
            LoadSpec::Package { path, .. } => path,
            LoadSpec::Github { path, .. } => path,
            LoadSpec::Gitlab { path, .. } => path,
            LoadSpec::Path { path, .. } => path,
        }
    }

    /// Get the package URL for this spec (e.g. "github.com/user/repo")
    ///
    /// This returns the canonical package identifier used in resolution maps.
    /// It ignores the version/tag and internal path.
    pub fn package_url(&self) -> Option<String> {
        match self {
            LoadSpec::Github { user, repo, .. } => Some(format!("github.com/{}/{}", user, repo)),
            LoadSpec::Gitlab { project_path, .. } => Some(format!("gitlab.com/{}", project_path)),
            LoadSpec::Package { package, .. } => Some(package.clone()),
            _ => None,
        }
    }

    /// Create a new LoadSpec pointing to a different file in the same repository.
    ///
    /// This preserves the repository identity (GitHub user/repo, GitLab project, package name/tag)
    /// while changing only the file path within that repository.
    pub fn with_path(&self, new_path: PathBuf) -> LoadSpec {
        match self {
            LoadSpec::Github { user, repo, .. } => LoadSpec::Github {
                user: user.clone(),
                repo: repo.clone(),
                path: new_path,
            },
            LoadSpec::Gitlab { project_path, .. } => LoadSpec::Gitlab {
                project_path: project_path.clone(),
                path: new_path,
            },
            LoadSpec::Package { package, .. } => LoadSpec::Package {
                package: package.clone(),
                path: new_path,
            },
            LoadSpec::Path {
                allow_not_exist, ..
            } => LoadSpec::Path {
                path: new_path,
                allow_not_exist: *allow_not_exist,
            },
        }
    }

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
        if let Some(rest) = s.strip_prefix("github.com/") {
            // GitHub style: github.com/user/repo/path...
            // Assumes standard user/repo structure (2 components)
            let mut parts = rest.splitn(3, '/');
            let user = parts.next().unwrap_or("").to_string();
            let repo = parts.next().unwrap_or("").to_string();
            let path_str = parts.next().unwrap_or("");

            if user.is_empty() || repo.is_empty() {
                return None;
            }

            Some(LoadSpec::Github {
                user,
                repo,
                path: PathBuf::from(path_str),
            })
        } else if let Some(rest) = s.strip_prefix("gitlab.com/") {
            // GitLab style: gitlab.com/group/subgroup/project/path...
            // GitLab supports nested groups, so we need to find the boundary between
            // project path and file path using file extension heuristic

            let parts: Vec<&str> = rest.split('/').collect();

            // Find where the file path starts (first component with extension)
            let mut project_parts = Vec::new();
            let mut file_parts = Vec::new();
            let mut found_file = false;

            for part in parts {
                if !found_file && (part.contains('.') || !file_parts.is_empty()) {
                    found_file = true;
                }

                if found_file {
                    file_parts.push(part);
                } else {
                    project_parts.push(part);
                }
            }

            if project_parts.is_empty() {
                return None;
            }

            let project_path = project_parts.join("/");
            let file_path = file_parts.join("/");

            Some(LoadSpec::Gitlab {
                project_path,
                path: PathBuf::from(file_path),
            })
        } else if let Some(rest) = s.strip_prefix('@') {
            // Generic package: @<pkg>/optional/path
            // rest looks like "pkg/path..." or just "pkg"/"pkg:tag"
            let mut parts = rest.splitn(2, '/');
            let package = parts.next().unwrap_or("");
            let rel_path = parts.next().unwrap_or("");

            // Validate that we have a non-empty package name
            if package.is_empty() {
                return None;
            }

            // Reject invalid GitHub/GitLab specs that don't have the proper format
            if package == "github" || package == "gitlab" {
                return None;
            }

            Some(LoadSpec::Package {
                package: package.to_string(),
                path: PathBuf::from(rel_path),
            })
        } else {
            // Raw file path (relative or absolute)
            Some(LoadSpec::local_path(s))
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

    /// Check if this LoadSpec represents a local resource.
    pub fn is_local(&self) -> bool {
        matches!(self, LoadSpec::Path { .. })
    }

    /// Drop the path from the LoadSpec for all variants
    pub fn without_path(&self) -> Self {
        self.with_path(PathBuf::new())
    }

    /// Convert the LoadSpec back to a load string representation.
    /// This is useful for error messages and debugging.
    pub fn to_load_string(&self) -> String {
        self.to_string()
    }

    /// Get the full URL representation of this spec (without `@` prefix).
    ///
    /// Returns `None` for local `Path` specs.
    pub fn to_full_url(&self) -> Option<String> {
        match self {
            LoadSpec::Path { .. } => None,
            other => {
                let url = other.to_string();
                Some(url.strip_prefix('@').unwrap_or(&url).to_string())
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
                path: PathBuf::from("math.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_github_no_rev() {
        let spec = LoadSpec::parse("github.com/foo/bar/scripts/build.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Github {
                user: "foo".to_string(),
                repo: "bar".to_string(),
                path: PathBuf::from("scripts/build.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_relative_path() {
        let spec = LoadSpec::parse("./math.zen");
        assert_eq!(spec, Some(LoadSpec::local_path("./math.zen")));
    }

    #[test]
    fn test_parse_load_spec_relative_path_parent() {
        let spec = LoadSpec::parse("../utils/helper.zen");
        assert_eq!(spec, Some(LoadSpec::local_path("../utils/helper.zen")));
    }

    #[test]
    fn test_parse_load_spec_absolute_path() {
        let spec = LoadSpec::parse("/absolute/path/file.zen");
        assert_eq!(spec, Some(LoadSpec::local_path("/absolute/path/file.zen")));
    }

    #[test]
    fn test_parse_load_spec_simple_filename() {
        let spec = LoadSpec::parse("math.zen");
        assert_eq!(spec, Some(LoadSpec::local_path("math.zen")));
    }

    #[test]
    fn test_load_spec_serialization() {
        let spec = LoadSpec::Package {
            package: "stdlib".to_string(),
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
        let spec = LoadSpec::local_path("./relative/path/file.zen");

        // Test serialization
        let json = serde_json::to_string(&spec).expect("Failed to serialize LoadSpec");

        // Test deserialization
        let deserialized: LoadSpec =
            serde_json::from_str(&json).expect("Failed to deserialize LoadSpec");

        assert_eq!(spec, deserialized);
    }

    #[test]
    fn test_path_spec_serialization_absolute() {
        let spec = LoadSpec::local_path("/absolute/path/file.zen");

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
                path: PathBuf::from("math.zen"),
            },
            LoadSpec::Github {
                user: "user".to_string(),
                repo: "repo".to_string(),
                path: PathBuf::from("src/lib.zen"),
            },
            LoadSpec::Gitlab {
                project_path: "group/repo".to_string(),
                path: PathBuf::from("lib/module.zen"),
            },
            LoadSpec::local_path("./relative/file.zen"),
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

    // Tests for helper methods
    mod helper_tests {
        use super::*;

        #[test]
        fn test_local_path_helper() {
            let spec = LoadSpec::local_path("src/lib.zen");
            assert_eq!(spec, LoadSpec::local_path("src/lib.zen"));
            assert!(spec.is_local());
        }
    }
}
