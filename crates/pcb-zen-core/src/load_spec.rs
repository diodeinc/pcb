use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const LEGACY_KICAD_STDLIB_ALIASES: &[(&str, &str)] = &[
    ("kicad-symbols", "kicad-symbols"),
    ("kicad-footprints", "kicad-footprints"),
];

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LoadSpec {
    Package {
        package: String,
        path: PathBuf,
    },
    Stdlib {
        path: PathBuf,
    },
    Url {
        url: String,
    },
    Path {
        path: PathBuf,
        allow_not_exist: bool,
    },
    PackageUri {
        uri: String,
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
            LoadSpec::Stdlib { path } => {
                if path.as_os_str().is_empty() {
                    write!(f, "@{}", crate::STDLIB_MODULE_PATH)
                } else {
                    write!(f, "@{}/{}", crate::STDLIB_MODULE_PATH, path.display())
                }
            }
            LoadSpec::Url { url } => {
                write!(f, "{url}")
            }
            LoadSpec::Path { path, .. } => {
                write!(f, "{}", path.display())
            }
            LoadSpec::PackageUri { uri } => {
                write!(f, "{uri}")
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

    /// Parse the raw string passed to `load()` into a [`LoadSpec`].
    ///
    /// The supported grammar is:
    ///
    /// • **Package reference** – `"@<package>[:<tag>]/<optional/path>"`.
    ///   If `<tag>` is omitted the [`DEFAULT_PKG_TAG`] (currently `"latest"`) is
    ///   assumed.
    ///   Example: `"@stdlib:1.2.3/math.zen"` or `"@stdlib/math.zen"`.
    ///
    /// • **Package URL** – `"<host>/<package>/<path>"`.
    ///   The host must be domain-like and versions are declared in `pcb.toml`.
    ///   Examples: `"github.com/foo/bar/scripts/build.zen"`,
    ///   `"code.example.com/acme/registry/components/Part/Part.zen"`.
    ///
    /// • **Package URI** – `"package://<url>/<path>"`.
    ///   A stable, machine-independent reference to a file within a resolved package.
    ///   Example: `"package://github.com/diodeinc/registry/reference/TPS54331/TPS54331.zen"`.
    ///
    /// • **Stdlib package URI** – `"package://stdlib/<path>"`.
    ///   A stable reference to a toolchain-managed stdlib file.
    ///   Example: `"package://stdlib/units.zen"`.
    ///
    /// • **Raw file path** – Any other string is treated as a raw file path (relative or absolute).
    ///   Examples: `"./math.zen"`, `"../utils/helper.zen"`, `"/absolute/path/file.zen"`.
    ///
    /// The function does not touch the filesystem – it only performs syntactic
    /// parsing.
    pub fn parse(s: &str) -> Option<LoadSpec> {
        if let Some(uri) = s.strip_prefix(pcb_sch::PACKAGE_URI_PREFIX) {
            if uri.is_empty() {
                return None;
            }
            return Some(LoadSpec::PackageUri { uri: s.to_string() });
        }

        if let Some(rest) = s.strip_prefix('@') {
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

            if package == crate::STDLIB_MODULE_PATH {
                return Some(LoadSpec::Stdlib {
                    path: PathBuf::from(rel_path),
                });
            }

            if let Some(stdlib_dir) = kicad_stdlib_alias(package) {
                let mut path = PathBuf::from(stdlib_dir);
                if !rel_path.is_empty() {
                    path.push(rel_path);
                }
                return Some(LoadSpec::Stdlib { path });
            }

            Some(LoadSpec::Package {
                package: package.to_string(),
                path: PathBuf::from(rel_path),
            })
        } else if is_package_url(s) {
            Some(LoadSpec::Url { url: s.to_string() })
        } else {
            // Raw file path (relative or absolute)
            Some(LoadSpec::local_path(s))
        }
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

fn kicad_stdlib_alias(package: &str) -> Option<&'static str> {
    LEGACY_KICAD_STDLIB_ALIASES
        .iter()
        .find_map(|(alias, stdlib_dir)| (*alias == package).then_some(*stdlib_dir))
}

fn is_package_url(s: &str) -> bool {
    if s.starts_with("./") || s.starts_with("../") || s.starts_with('/') {
        return false;
    }

    let Ok(url) = url::Url::parse(&format!("https://{s}")) else {
        return false;
    };

    url.host_str().is_some_and(|host| host.contains('.')) && url.path() != "/"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_load_spec_package_no_tag() {
        let spec = LoadSpec::parse("@stdlib/math.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Stdlib {
                path: PathBuf::from("math.zen"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_kicad_symbol_alias() {
        let spec = LoadSpec::parse("@kicad-symbols/Device.kicad_symdir/C.kicad_sym");
        assert_eq!(
            spec,
            Some(LoadSpec::Stdlib {
                path: PathBuf::from("kicad-symbols/Device.kicad_symdir/C.kicad_sym"),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_kicad_footprint_alias() {
        let spec =
            LoadSpec::parse("@kicad-footprints/Capacitor_SMD.pretty/C_0603_1608Metric.kicad_mod");
        assert_eq!(
            spec,
            Some(LoadSpec::Stdlib {
                path: PathBuf::from(
                    "kicad-footprints/Capacitor_SMD.pretty/C_0603_1608Metric.kicad_mod"
                ),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_github_no_rev() {
        let spec = LoadSpec::parse("github.com/foo/bar/scripts/build.zen");
        assert_eq!(
            spec,
            Some(LoadSpec::Url {
                url: "github.com/foo/bar/scripts/build.zen".to_string(),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_gitlab_nested_path() {
        let spec = LoadSpec::parse("gitlab.com/example/packages/components/Device.kicad_sym");
        assert_eq!(
            spec,
            Some(LoadSpec::Url {
                url: "gitlab.com/example/packages/components/Device.kicad_sym".to_string(),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_domain_like_package_url() {
        let spec = LoadSpec::parse(
            "code.diode.computer/diode/b/IP0010/components/Diodes_Inc/AP22653W6M7/AP22653W6M7.zen",
        );
        assert_eq!(
            spec,
            Some(LoadSpec::Url {
                url: "code.diode.computer/diode/b/IP0010/components/Diodes_Inc/AP22653W6M7/AP22653W6M7.zen".to_string(),
            })
        );
        assert_eq!(
            spec.and_then(|spec| spec.to_full_url()),
            Some("code.diode.computer/diode/b/IP0010/components/Diodes_Inc/AP22653W6M7/AP22653W6M7.zen".to_string())
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
    fn test_parse_load_spec_package_uri() {
        let spec = LoadSpec::parse(
            "package://github.com/diodeinc/registry/reference/TPS54331/TPS54331.zen",
        );
        assert_eq!(
            spec,
            Some(LoadSpec::PackageUri {
                uri: "package://github.com/diodeinc/registry/reference/TPS54331/TPS54331.zen"
                    .to_string(),
            })
        );
    }

    #[test]
    fn test_parse_load_spec_package_uri_empty() {
        let spec = LoadSpec::parse("package://");
        assert_eq!(spec, None);
    }

    #[test]
    fn test_load_spec_serialization() {
        let spec = LoadSpec::Stdlib {
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
    fn test_url_spec_serialization() {
        let spec = LoadSpec::Url {
            url: "github.com/foo/bar/src/lib.zen".to_string(),
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
            LoadSpec::Stdlib {
                path: PathBuf::from("math.zen"),
            },
            LoadSpec::Package {
                package: "github.com/example/lib".to_string(),
                path: PathBuf::from("math.zen"),
            },
            LoadSpec::Url {
                url: "code.diode.computer/diode/b/IP0010/module.zen".to_string(),
            },
            LoadSpec::local_path("./relative/file.zen"),
            LoadSpec::PackageUri {
                uri: "package://github.com/diodeinc/registry/reference/TPS54331/TPS54331.zen"
                    .to_string(),
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
}
