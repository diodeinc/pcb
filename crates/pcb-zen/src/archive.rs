//! HTTP archive download support for package fetching.

use anyhow::Result;
use std::io::BufReader;
use std::path::{Path, PathBuf};

const ZSTD_WINDOW_LOG_MAX: u32 = 31;
const IO_BUFFER_SIZE: usize = 8 * 1024 * 1024;

/// Render an HTTP mirror URL from template placeholders.
///
/// Supported placeholders:
/// - `{repo}` full repo path, e.g. `gitlab.com/kicad/libraries/kicad-footprints`
/// - `{repo_name}` last path segment, e.g. `kicad-footprints`
/// - `{version}` concrete version, e.g. `9.0.3`
/// - `{major}` major version segment, e.g. `9`
pub fn render_http_mirror_url(template: &str, module_path: &str, version: &str) -> Result<String> {
    let repo_name = module_path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Invalid module path: {}", module_path))?;
    let major = version
        .split('.')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(version);

    Ok(template
        .replace("{repo}", module_path)
        .replace("{repo_name}", repo_name)
        .replace("{version}", version)
        .replace("{major}", major))
}

/// Download and extract an HTTP `.tar.zst` archive to target directory.
pub fn fetch_http_archive(url: &str, target_dir: &Path) -> Result<PathBuf> {
    log::debug!("Downloading archive from {}", url);

    let response = reqwest::blocking::get(url)?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {} from {}", response.status(), url);
    }

    let mut decoder = zstd::stream::read::Decoder::new(response)?;
    decoder.window_log_max(ZSTD_WINDOW_LOG_MAX)?;
    let reader = BufReader::with_capacity(IO_BUFFER_SIZE, decoder);
    let mut archive = tar::Archive::new(reader);
    std::fs::create_dir_all(target_dir)?;
    archive.unpack(target_dir)?;

    log::debug!("Extracted archive to {}", target_dir.display());
    Ok(target_dir.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_http_mirror_url() {
        let template = "https://mirror.example/{repo_name}-{version}.tar.zst";
        let url = render_http_mirror_url(
            template,
            "gitlab.com/kicad/libraries/kicad-footprints",
            "9.0.3",
        )
        .unwrap();
        assert_eq!(url, "https://mirror.example/kicad-footprints-9.0.3.tar.zst");
    }

    #[test]
    fn test_render_http_mirror_url_with_all_placeholders() {
        let template = "https://mirror.example/{major}/{repo}/{repo_name}/{version}";
        let url = render_http_mirror_url(
            template,
            "gitlab.com/kicad/libraries/kicad-symbols",
            "9.0.3",
        )
        .unwrap();
        assert_eq!(
            url,
            "https://mirror.example/9/gitlab.com/kicad/libraries/kicad-symbols/kicad-symbols/9.0.3"
        );
    }
}
