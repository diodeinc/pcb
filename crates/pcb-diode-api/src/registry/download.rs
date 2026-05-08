//! Download registry index from API server + S3

pub use crate::download_support::{DownloadProgress, DownloadSource};
use crate::download_support::{
    ProgressReader, ensure_parent_dir, http_client,
    save_local_version as save_shared_local_version, write_decoded_index,
};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::thread;

const REGISTRY_INDEX_ROUTE: &str = "/api/registry/index";
const REGISTRIES_ROUTE: &str = "/api/registry/registries";
pub const DEFAULT_REGISTRY_URL: &str = "github.com/diodeinc/registry";

#[derive(Debug, Clone, Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct RegistryInfo {
    pub id: String,
    pub workspace: String,
    pub name: String,
    pub provider: String,
    pub owner: String,
    pub repo: String,
    #[serde(rename = "registryUrl")]
    pub registry_url: String,
    #[serde(rename = "defaultBranch")]
    pub default_branch: Option<String>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
}

impl RegistryInfo {
    pub fn default_registry() -> Self {
        Self {
            id: "default".to_string(),
            workspace: "diodeinc".to_string(),
            name: "registry".to_string(),
            provider: "github".to_string(),
            owner: "diodeinc".to_string(),
            repo: "registry".to_string(),
            registry_url: DEFAULT_REGISTRY_URL.to_string(),
            default_branch: None,
            updated_at: None,
        }
    }

    pub fn display_name(&self) -> String {
        format!("{}/{}", self.workspace, self.name)
    }

    pub fn local(path: &Path) -> Self {
        Self {
            id: "local".to_string(),
            workspace: "local".to_string(),
            name: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("registry")
                .to_string(),
            provider: "local".to_string(),
            owner: "local".to_string(),
            repo: path.display().to_string(),
            registry_url: "local".to_string(),
            default_branch: None,
            updated_at: None,
        }
    }

    fn cached(id: String, path: &Path) -> Self {
        Self {
            id: id.clone(),
            workspace: "cached".to_string(),
            name: id.clone(),
            provider: "cached".to_string(),
            owner: "cached".to_string(),
            repo: id,
            registry_url: path.display().to_string(),
            default_branch: None,
            updated_at: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RegistriesResponse {
    registries: Vec<RegistryInfo>,
}

#[derive(Debug, Clone)]
pub struct RegistryIndexFile {
    pub registry: RegistryInfo,
    pub path: PathBuf,
    pub downloaded: bool,
}

#[derive(Debug, Clone)]
pub enum RegistrySearchScope {
    Registries(Vec<RegistryInfo>),
    IndexFiles(Vec<RegistryIndexFile>),
}

impl RegistrySearchScope {
    pub fn updates_disabled(&self) -> bool {
        matches!(self, Self::IndexFiles(_))
    }

    pub fn index_paths(&self) -> Result<Vec<PathBuf>> {
        match self {
            Self::Registries(registries) => registries.iter().map(registry_db_path).collect(),
            Self::IndexFiles(indexes) => {
                Ok(indexes.iter().map(|index| index.path.clone()).collect())
            }
        }
    }

    pub fn local_indexes_exist(&self) -> bool {
        match self.index_paths() {
            Ok(paths) => !paths.is_empty() && paths.iter().all(|path| path.exists()),
            Err(_) => false,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryIndexMetadata {
    pub url: String,
    pub sha256: String,
    #[serde(rename = "lastModified")]
    pub last_modified: String,
    #[serde(rename = "expiresAt")]
    #[allow(dead_code)]
    pub expires_at: String,
}

impl RegistryIndexMetadata {
    /// Stable token for local freshness checks.
    pub fn version_token(&self) -> Result<String> {
        crate::download_support::sha256_version_token(&self.sha256, "registry index")
    }
}

pub fn load_local_version(db_path: &Path) -> Option<String> {
    crate::download_support::load_local_version(db_path)
}

pub fn save_local_version(db_path: &Path, version: &str) -> Result<()> {
    save_shared_local_version(db_path, version, "registry")
}

/// Fetch registries visible to the current user.
pub fn fetch_registries() -> Result<Vec<RegistryInfo>> {
    let token = crate::auth::get_valid_token().context("Auth failed")?;
    let client = http_client()?;
    let api_url = crate::get_api_base_url();
    let url = format!("{api_url}{REGISTRIES_ROUTE}");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .with_context(|| format!("Request to {url} failed"))?
        .error_for_status()
        .with_context(|| format!("API error from {url}"))?;

    let response: RegistriesResponse = resp.json().context("Failed to parse registries")?;
    Ok(response.registries)
}

pub fn default_registry_scope(
    registries: Vec<RegistryInfo>,
    workspace_root: Option<&Path>,
) -> Vec<RegistryInfo> {
    let workspace_prefix = workspace_root.and_then(workspace_registry_prefix);
    default_registry_scope_for_prefix(registries, workspace_prefix.as_deref())
}

fn default_registry_scope_for_prefix(
    registries: Vec<RegistryInfo>,
    workspace_prefix: Option<&str>,
) -> Vec<RegistryInfo> {
    let mut selected = Vec::new();
    let mut selected_ids = HashSet::new();

    for registry in &registries {
        if registry_matches_selector(registry, DEFAULT_REGISTRY_URL) {
            push_unique_registry(&mut selected, &mut selected_ids, registry);
        }
    }

    if let Some(prefix) = workspace_prefix {
        for registry in &registries {
            if registry_in_workspace(registry, prefix) {
                push_unique_registry(&mut selected, &mut selected_ids, registry);
            }
        }
    }

    selected
}

fn push_unique_registry(
    selected: &mut Vec<RegistryInfo>,
    selected_ids: &mut HashSet<String>,
    registry: &RegistryInfo,
) {
    if selected_ids.insert(registry.id.clone()) {
        selected.push(registry.clone());
    }
}

pub fn resolve_registry_scope(
    registries: Vec<RegistryInfo>,
    selectors: &[String],
) -> Result<Vec<RegistryInfo>> {
    if selectors.is_empty() {
        return Ok(registries);
    }

    let mut selected = Vec::new();
    let mut selected_ids = HashSet::new();
    for selector in selectors {
        let matches = registries
            .iter()
            .filter(|registry| registry_matches_selector(registry, selector))
            .collect::<Vec<_>>();

        match matches.as_slice() {
            [] => {
                anyhow::bail!(
                    "Registry `{}` is not available. Available registries: {}",
                    selector,
                    available_registry_labels(&registries)
                );
            }
            [registry] => {
                push_unique_registry(&mut selected, &mut selected_ids, registry);
            }
            _ => {
                anyhow::bail!(
                    "Registry selector `{}` matched multiple registries: {}",
                    selector,
                    matches
                        .iter()
                        .map(|registry| registry_label(registry))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }

    Ok(selected)
}

pub fn resolve_registry_search_scope(
    selectors: &[String],
    workspace_root: Option<&Path>,
) -> Result<Option<RegistrySearchScope>> {
    match fetch_registries() {
        Ok(registries) => {
            let registries = if selectors.is_empty() {
                default_registry_scope(registries, workspace_root)
            } else {
                resolve_registry_scope(registries, selectors)?
            };

            Ok((!registries.is_empty()).then_some(RegistrySearchScope::Registries(registries)))
        }
        Err(err) => {
            log::debug!("registry discovery unavailable: {err}");

            let indexes = cached_registry_indexes()?;
            let indexes = if selectors.is_empty() {
                indexes
            } else {
                match resolve_cached_registry_indexes(indexes, selectors) {
                    Ok(indexes) if !indexes.is_empty() => indexes,
                    _ => return Err(err).context("Failed to fetch registries for registry scope"),
                }
            };

            Ok((!indexes.is_empty()).then_some(RegistrySearchScope::IndexFiles(indexes)))
        }
    }
}

fn resolve_cached_registry_indexes(
    indexes: Vec<RegistryIndexFile>,
    selectors: &[String],
) -> Result<Vec<RegistryIndexFile>> {
    let registries = indexes
        .iter()
        .map(|index| index.registry.clone())
        .collect::<Vec<_>>();
    let selected_ids = resolve_registry_scope(registries, selectors)?
        .into_iter()
        .map(|registry| registry.id)
        .collect::<HashSet<_>>();

    Ok(indexes
        .into_iter()
        .filter(|index| selected_ids.contains(&index.registry.id))
        .collect())
}

fn workspace_registry_prefix(workspace_root: &Path) -> Option<String> {
    let repo_root = pcb_zen::git::get_repo_root(workspace_root).ok()?;
    let repo_url = pcb_zen::git::detect_repository_url(&repo_root)
        .ok()
        .or_else(|| {
            pcb_zen::git::get_remote_url(&repo_root)
                .ok()
                .map(|url| normalize_registry_selector(&url))
        })?;
    registry_workspace_prefix_from_url(&repo_url)
}

fn registry_workspace_prefix_from_url(url: &str) -> Option<String> {
    let normalized = normalize_registry_selector(url);
    let mut parts = normalized.split('/').filter(|part| !part.is_empty());
    let host = parts.next()?;
    let workspace = parts.next()?;
    Some(format!("{host}/{workspace}"))
}

fn registry_in_workspace(registry: &RegistryInfo, workspace_prefix: &str) -> bool {
    let registry_url = normalize_registry_selector(&registry.registry_url);
    registry_url == workspace_prefix
        || registry_url
            .strip_prefix(workspace_prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn registry_matches_selector(registry: &RegistryInfo, selector: &str) -> bool {
    let selector = normalize_registry_selector(selector);
    if selector.is_empty() {
        return false;
    }

    registry_selector_keys(registry)
        .into_iter()
        .any(|key| key == selector)
}

fn registry_selector_keys(registry: &RegistryInfo) -> Vec<String> {
    let mut keys = vec![
        normalize_registry_selector(&registry.id),
        normalize_registry_selector(&registry.display_name()),
        normalize_registry_selector(&registry.registry_url),
        normalize_registry_selector(&format!("{}/{}", registry.owner, registry.repo)),
        normalize_registry_selector(&format!(
            "{}/{}/{}",
            registry.provider, registry.owner, registry.repo
        )),
    ];

    if !registry.provider.contains('.') {
        keys.push(normalize_registry_selector(&format!(
            "{}.com/{}/{}",
            registry.provider, registry.owner, registry.repo
        )));
    }

    keys.sort();
    keys.dedup();
    keys
}

fn normalize_registry_selector(value: &str) -> String {
    let mut value = value.trim().to_string();

    if let Some(rest) = value.strip_prefix("ssh://") {
        value = rest.to_string();
        value = strip_userinfo(value);
        value = strip_numeric_port_from_authority(value);
    } else if let Some(rest) = value.strip_prefix("https://") {
        value = strip_userinfo(rest.to_string());
        value = strip_numeric_port_from_authority(value);
    } else if let Some(rest) = value.strip_prefix("http://") {
        value = strip_userinfo(rest.to_string());
        value = strip_numeric_port_from_authority(value);
    } else if let Some(rest) = value.strip_prefix("git@") {
        value = rest.replacen(':', "/", 1);
    } else {
        value = strip_userinfo(value);
        value = strip_numeric_port_from_authority(value);
    }

    value
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .to_ascii_lowercase()
}

fn strip_userinfo(value: String) -> String {
    if let Some((user, rest)) = value.split_once('@')
        && !user.contains('/')
    {
        rest.to_string()
    } else {
        value
    }
}

fn strip_numeric_port_from_authority(value: String) -> String {
    let Some((authority, path)) = value.split_once('/') else {
        return strip_numeric_port(value);
    };
    format!("{}/{}", strip_numeric_port(authority.to_string()), path)
}

fn strip_numeric_port(authority: String) -> String {
    if let Some((host, port)) = authority.rsplit_once(':')
        && !host.is_empty()
        && port.chars().all(|ch| ch.is_ascii_digit())
    {
        host.to_string()
    } else {
        authority
    }
}

fn registry_label(registry: &RegistryInfo) -> String {
    format!("{} ({})", registry.display_name(), registry.registry_url)
}

fn available_registry_labels(registries: &[RegistryInfo]) -> String {
    if registries.is_empty() {
        "none".to_string()
    } else {
        registries
            .iter()
            .map(registry_label)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

pub fn default_registry_db_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".pcb").join("registry").join("packages.db"))
}

pub fn local_registry_index(path: PathBuf) -> RegistryIndexFile {
    RegistryIndexFile {
        registry: RegistryInfo::local(&path),
        path,
        downloaded: false,
    }
}

pub fn cached_default_registry_index() -> Result<Option<RegistryIndexFile>> {
    let path = default_registry_db_path()?;
    Ok(path.exists().then_some(RegistryIndexFile {
        registry: RegistryInfo::default_registry(),
        path,
        downloaded: false,
    }))
}

fn cached_registry_indexes() -> Result<Vec<RegistryIndexFile>> {
    let mut indexes = Vec::new();
    if let Some(index) = cached_default_registry_index()? {
        indexes.push(index);
    }

    let home = dirs::home_dir().context("Could not determine home directory")?;
    let index_dir = home.join(".pcb").join("registry").join("indexes");
    let Ok(entries) = std::fs::read_dir(index_dir) else {
        return Ok(indexes);
    };

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }

        let path = entry.path().join("packages.db");
        if !path.exists() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        indexes.push(RegistryIndexFile {
            registry: RegistryInfo::cached(id, &path),
            path,
            downloaded: false,
        });
    }

    indexes.sort_by_key(|index| index.registry.display_name());
    Ok(indexes)
}

/// Fetch registry index metadata without downloading the file.
pub fn fetch_registry_index_metadata(registry_id: &str) -> Result<RegistryIndexMetadata> {
    let token = crate::auth::get_valid_token().context("Auth failed")?;
    let client = http_client()?;
    let api_url = crate::get_api_base_url();
    let mut url = reqwest::Url::parse(&format!("{api_url}{REGISTRY_INDEX_ROUTE}"))
        .context("Invalid registry index URL")?;
    url.query_pairs_mut().append_pair("registryId", registry_id);

    let resp = client
        .get(url.clone())
        .bearer_auth(&token)
        .send()
        .with_context(|| format!("Request to {url} failed"))?
        .error_for_status()
        .with_context(|| format!("API error from {url}"))?;

    resp.json()
        .context("Failed to parse registry index metadata")
}

fn download_index_response(
    client: &reqwest::blocking::Client,
    index_url: &str,
) -> Result<reqwest::blocking::Response> {
    client
        .get(index_url)
        .send()
        .context("Failed to download registry index")?
        .error_for_status()
        .context("S3 returned error when downloading registry index")
}

pub fn registry_db_path(registry: &RegistryInfo) -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home
        .join(".pcb")
        .join("registry")
        .join("indexes")
        .join(&registry.id)
        .join("packages.db"))
}

pub fn ensure_registry_index(registry: &RegistryInfo, force: bool) -> Result<RegistryIndexFile> {
    let path = registry_db_path(registry)?;
    let metadata = match fetch_registry_index_metadata(&registry.id) {
        Ok(metadata) => metadata,
        Err(err) if !force && path.exists() => {
            log::debug!(
                "using cached registry index for {} after metadata fetch failed: {err}",
                registry.display_name()
            );
            return Ok(RegistryIndexFile {
                registry: registry.clone(),
                path,
                downloaded: false,
            });
        }
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "Failed to fetch index metadata for {}",
                    registry.display_name()
                )
            });
        }
    };
    let remote_version = metadata.version_token()?;
    let local_version = load_local_version(&path);

    if !force && path.exists() && local_version.as_deref() == Some(remote_version.as_str()) {
        return Ok(RegistryIndexFile {
            registry: registry.clone(),
            path,
            downloaded: false,
        });
    }

    let (progress_tx, progress_rx) = std::sync::mpsc::channel();
    let _ = progress_rx;
    download_registry_index_with_progress(&path, &progress_tx, force, &metadata)?;

    Ok(RegistryIndexFile {
        registry: registry.clone(),
        path,
        downloaded: true,
    })
}

pub fn ensure_registry_indexes(
    registries: Vec<RegistryInfo>,
    force: bool,
) -> Result<Vec<RegistryIndexFile>> {
    if registries.is_empty() {
        anyhow::bail!("No registries available");
    }

    let expected_count = registries.len();
    let (tx, rx) = std::sync::mpsc::channel();
    for registry in registries {
        let tx = tx.clone();
        thread::spawn(move || {
            let label = registry.display_name();
            let result = ensure_registry_index(&registry, force)
                .with_context(|| format!("Failed to load registry index for {label}"));
            let _ = tx.send(result);
        });
    }
    drop(tx);

    let mut files = Vec::new();
    for result in rx {
        files.push(result?);
    }
    if files.len() != expected_count {
        anyhow::bail!(
            "Failed to load all registry indexes ({}/{})",
            files.len(),
            expected_count
        );
    }
    files.sort_by_key(|file| file.registry.display_name());
    Ok(files)
}

pub fn ensure_registry_indexes_with_progress(
    registries: Vec<RegistryInfo>,
    progress_tx: &Sender<DownloadProgress>,
    is_update: bool,
    force: bool,
) -> Result<Vec<RegistryIndexFile>> {
    let _ = progress_tx.send(DownloadProgress {
        source: DownloadSource::Registry,
        pct: None,
        done: false,
        error: None,
        is_update,
    });

    match ensure_registry_indexes(registries, force) {
        Ok(files) => {
            let _ = progress_tx.send(DownloadProgress {
                source: DownloadSource::Registry,
                pct: Some(100),
                done: true,
                error: None,
                is_update,
            });
            Ok(files)
        }
        Err(err) => {
            let msg = err.to_string();
            let _ = progress_tx.send(DownloadProgress {
                source: DownloadSource::Registry,
                pct: None,
                done: true,
                error: Some(msg.clone()),
                is_update,
            });
            Err(anyhow::anyhow!(msg))
        }
    }
}

/// Download registry index with progress reporting via channel
///
pub fn download_registry_index_with_progress(
    dest_path: &Path,
    progress_tx: &Sender<DownloadProgress>,
    is_update: bool,
    index_metadata: &RegistryIndexMetadata,
) -> Result<()> {
    let send_progress = |pct: Option<u8>, done: bool, error: Option<String>| {
        let _ = progress_tx.send(DownloadProgress {
            source: DownloadSource::Registry,
            pct,
            done,
            error,
            is_update,
        });
    };

    send_progress(None, false, None);

    let client = http_client()?;

    ensure_parent_dir(dest_path, "registry")?;

    let response = match download_index_response(&client, &index_metadata.url) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("Failed to download registry index: {e}");
            send_progress(None, true, Some(msg.clone()));
            anyhow::bail!(msg);
        }
    };

    let total_size = response.content_length();

    // Wrap response in a progress-tracking reader, then decompress with zstd
    let progress_reader = ProgressReader::new(response, total_size, &send_progress);
    write_decoded_index(dest_path, progress_reader, "registry index")?;

    let version_token = index_metadata.version_token()?;
    let _ = save_local_version(dest_path, &version_token);

    send_progress(Some(100), true, None);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(id: &str, workspace: &str, name: &str, url: &str) -> RegistryInfo {
        let normalized = normalize_registry_selector(url);
        let mut parts = normalized.split('/');
        let provider = parts.next().unwrap_or("github").to_string();
        let owner = parts.next().unwrap_or(workspace).to_string();
        let repo = parts.next().unwrap_or(name).to_string();

        RegistryInfo {
            id: id.to_string(),
            workspace: workspace.to_string(),
            name: name.to_string(),
            provider,
            owner,
            repo,
            registry_url: url.to_string(),
            default_branch: None,
            updated_at: None,
        }
    }

    #[test]
    fn registry_scope_matches_registry_url() {
        let registries = vec![registry(
            "reg_1",
            "diode",
            "registry",
            "github.com/diodeinc/registry",
        )];

        let scoped = resolve_registry_scope(
            registries,
            &["https://github.com/diodeinc/registry.git".into()],
        )
        .unwrap();

        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].id, "reg_1");
    }

    #[test]
    fn cached_registry_scope_matches_selectors() {
        let indexes = vec![
            RegistryIndexFile {
                registry: registry("public", "diode", "registry", DEFAULT_REGISTRY_URL),
                path: "public.db".into(),
                downloaded: false,
            },
            RegistryIndexFile {
                registry: registry(
                    "workspace",
                    "revel",
                    "registry",
                    "code.diode.computer/revel/registry",
                ),
                path: "workspace.db".into(),
                downloaded: false,
            },
        ];

        let selected =
            resolve_cached_registry_indexes(indexes, &["github.com/diodeinc/registry".into()])
                .unwrap();

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].registry.id, "public");
    }

    #[test]
    fn default_scope_selects_public_and_workspace_registries() {
        let registries = vec![
            registry(
                "public",
                "diode",
                "registry",
                "github.com/diodeinc/registry",
            ),
            registry(
                "workspace",
                "weave",
                "parts",
                "https://code.diode.computer/weave/parts.git",
            ),
            registry("other", "other", "parts", "code.diode.computer/other/parts"),
        ];
        let workspace_prefix =
            registry_workspace_prefix_from_url("git@code.diode.computer:weave/WW0001.git").unwrap();
        let selected = default_registry_scope_for_prefix(registries, Some(&workspace_prefix));

        let ids = selected
            .iter()
            .map(|registry| registry.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["public", "workspace"]);
    }

    #[test]
    fn workspace_prefix_uses_host_and_first_path_segment() {
        assert_eq!(
            registry_workspace_prefix_from_url("code.diode.computer/weave/WW0001").unwrap(),
            "code.diode.computer/weave"
        );
        assert_eq!(
            registry_workspace_prefix_from_url("code.diode.computer/weave").unwrap(),
            "code.diode.computer/weave"
        );
        assert_eq!(
            registry_workspace_prefix_from_url("ssh://git@code.diode.computer:23231/revel")
                .unwrap(),
            "code.diode.computer/revel"
        );
        assert_eq!(
            registry_workspace_prefix_from_url("git@code.diode.computer:weave/WW0001.git").unwrap(),
            "code.diode.computer/weave"
        );
        assert_eq!(
            registry_workspace_prefix_from_url("https://user@code.diode.computer/revel/WW0001.git")
                .unwrap(),
            "code.diode.computer/revel"
        );
        assert_eq!(
            registry_workspace_prefix_from_url(
                "https://token:x-oauth-basic@code.diode.computer:23231/revel/WW0001.git"
            )
            .unwrap(),
            "code.diode.computer/revel"
        );
    }

    #[test]
    fn registry_scope_rejects_unknown_selector() {
        let registries = vec![registry(
            "reg_1",
            "diode",
            "registry",
            "github.com/diodeinc/registry",
        )];

        let err = resolve_registry_scope(registries, &["github.com/acme/parts".into()])
            .unwrap_err()
            .to_string();

        assert!(err.contains("github.com/acme/parts"));
        assert!(err.contains("github.com/diodeinc/registry"));
    }
}
