use anyhow::Result;
use std::path::{Path, PathBuf};

use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::FileProvider;
use pcb_zen_core::config::{PcbToml, find_workspace_root};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EndpointConfig {
    pub api_base_url: String,
    pub web_base_url: String,
    pub use_legacy_auth_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceContext {
    workspace_root: Option<PathBuf>,
    endpoint: EndpointConfig,
}

impl Default for WorkspaceContext {
    fn default() -> Self {
        Self {
            workspace_root: None,
            endpoint: resolve_endpoint_config(None),
        }
    }
}

impl WorkspaceContext {
    pub fn from_workspace_root(workspace_root: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        let endpoint = resolve_endpoint_config(Some(&workspace_root));
        Self {
            workspace_root: Some(workspace_root),
            endpoint,
        }
    }

    pub fn from_path(path: &Path) -> Self {
        match workspace_root_for(path) {
            Some(workspace_root) => Self::from_workspace_root(workspace_root),
            None => Self::default(),
        }
    }

    pub fn from_cwd() -> Result<Self> {
        Ok(Self::from_path(&std::env::current_dir()?))
    }

    pub fn workspace_root(&self) -> Option<&Path> {
        self.workspace_root.as_deref()
    }

    pub fn api_base_url(&self) -> &str {
        &self.endpoint.api_base_url
    }

    pub fn web_base_url(&self) -> &str {
        &self.endpoint.web_base_url
    }

    pub(crate) fn use_legacy_auth_file(&self) -> bool {
        self.endpoint.use_legacy_auth_file
    }
}

fn default_api_base_url() -> String {
    #[cfg(debug_assertions)]
    return "http://localhost:3001".to_string();
    #[cfg(not(debug_assertions))]
    return "https://api.diode.computer".to_string();
}

fn default_web_base_url() -> String {
    #[cfg(debug_assertions)]
    return "http://localhost:3000".to_string();
    #[cfg(not(debug_assertions))]
    return "https://app.diode.computer".to_string();
}

fn normalize_endpoint_host(value: &str) -> Option<String> {
    let value = value
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn workspace_root_for(start_path: &Path) -> Option<PathBuf> {
    let file_provider = DefaultFileProvider::new();
    let workspace_root = find_workspace_root(&file_provider, start_path).ok()?;
    file_provider
        .exists(&workspace_root.join("pcb.toml"))
        .then_some(workspace_root)
}

fn workspace_endpoint(workspace_root: &Path) -> Option<String> {
    let config = PcbToml::from_path(&workspace_root.join("pcb.toml")).ok()?;
    config
        .workspace
        .and_then(|workspace| workspace.endpoint)
        .and_then(|endpoint| normalize_endpoint_host(&endpoint))
}

fn should_use_legacy_auth_file(api_base_url: &str, web_base_url: &str) -> bool {
    api_base_url == default_api_base_url() && web_base_url == default_web_base_url()
}

pub(crate) fn resolve_endpoint_config(workspace_root: Option<&Path>) -> EndpointConfig {
    let api_env = std::env::var("DIODE_API_URL").ok();
    let web_env = std::env::var("DIODE_APP_URL").ok();
    let configured_endpoint = workspace_root.and_then(workspace_endpoint);
    let default_api_base_url = default_api_base_url();
    let default_web_base_url = default_web_base_url();

    let api_base_url = api_env.unwrap_or_else(|| {
        configured_endpoint
            .as_ref()
            .map(|endpoint| format!("https://api.{endpoint}"))
            .unwrap_or_else(|| default_api_base_url.clone())
    });
    let web_base_url = web_env.unwrap_or_else(|| {
        configured_endpoint
            .as_ref()
            .map(|endpoint| format!("https://app.{endpoint}"))
            .unwrap_or_else(|| default_web_base_url.clone())
    });

    EndpointConfig {
        use_legacy_auth_file: should_use_legacy_auth_file(&api_base_url, &web_base_url),
        api_base_url,
        web_base_url,
    }
}

pub(crate) fn auth_scope_slug(api_base_url: &str) -> String {
    let mut slug = String::with_capacity(api_base_url.len());
    let mut last_was_sep = false;

    for ch in api_base_url.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            slug.push('_');
            last_was_sep = true;
        }
    }

    let slug = slug.trim_matches('_');
    if slug.is_empty() {
        "endpoint".to_string()
    } else {
        slug.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_endpoint_host() {
        assert_eq!(
            normalize_endpoint_host(" https://sandbox.example.com/ "),
            Some("sandbox.example.com".to_string())
        );
    }

    #[test]
    fn creates_stable_auth_scope_slug() {
        assert_eq!(
            auth_scope_slug("https://api.sandbox.example.com:8443"),
            "https_api_sandbox_example_com_8443"
        );
    }

    #[test]
    fn keeps_legacy_auth_for_default_endpoint_urls() {
        assert!(should_use_legacy_auth_file(
            &default_api_base_url(),
            &default_web_base_url()
        ));
    }

    #[test]
    fn uses_scoped_auth_for_non_default_endpoint_urls() {
        assert!(!should_use_legacy_auth_file(
            "https://api.sandbox.example.com",
            "https://app.sandbox.example.com"
        ));
    }

    #[test]
    fn scope_without_workspace_uses_default_endpoint() {
        let scope = WorkspaceContext::default();
        assert_eq!(scope.api_base_url(), default_api_base_url());
        assert_eq!(scope.web_base_url(), default_web_base_url());
    }
}
