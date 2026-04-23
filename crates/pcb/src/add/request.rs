use anyhow::{Context, Result, bail};
use pcb_zen::tags;
use pcb_zen_core::config::{DependencySpec, PcbToml, split_repo_and_subpath};
use semver::Version;

use super::dep_id::compatibility_lane;

const LATEST_SELECTOR: &str = "latest";

#[derive(Debug, Clone, PartialEq, Eq)]
enum RequestedVersion {
    Latest,
    Exact(Version),
}

pub(crate) fn resolve_direct_dependency_request(
    raw: &str,
    current_config: &PcbToml,
) -> Result<(String, DependencySpec)> {
    let (module_path, requested_version) = parse_dependency_request(raw)?;
    let current_lane = current_config
        .dependencies
        .direct
        .get(module_path)
        .and_then(dependency_lane);
    let version =
        resolve_requested_version(module_path, requested_version, current_lane.as_deref())
            .with_context(|| format!("Failed to resolve requested dependency {}", module_path))?;
    Ok((
        module_path.to_string(),
        DependencySpec::Version(version.to_string()),
    ))
}

fn parse_dependency_request(raw: &str) -> Result<(&str, RequestedVersion)> {
    let raw = raw.trim();
    let Some((module_path, selector)) = raw.rsplit_once('@') else {
        return Ok((raw, RequestedVersion::Latest));
    };
    if module_path.is_empty() {
        bail!(
            "Invalid dependency '{}'. Use `pcb mod add <url>@latest` or `pcb mod add <url>@1.2.3`.",
            raw
        );
    }

    let selector = selector.trim();
    if selector.is_empty() {
        bail!(
            "Missing version after '@' in '{}'. Use `pcb mod add <url>@latest` or `pcb mod add <url>@1.2.3`.",
            raw
        );
    }
    if selector.eq_ignore_ascii_case(LATEST_SELECTOR) {
        return Ok((module_path, RequestedVersion::Latest));
    }

    let version = tags::parse_version(selector).ok_or_else(|| {
        anyhow::anyhow!(
            "Unsupported version selector '{}' in '{}'. Only `@latest` and exact versions like `@1.2.3` are supported.",
            selector,
            raw
        )
    })?;
    Ok((module_path, RequestedVersion::Exact(version)))
}

fn dependency_lane(spec: &DependencySpec) -> Option<String> {
    let raw_version = match spec {
        DependencySpec::Version(version) => Some(version.as_str()),
        DependencySpec::Detailed(detail) => detail.version.as_deref(),
    }?;
    let version = tags::parse_relaxed_version(raw_version)?;
    Some(compatibility_lane(&version))
}

fn resolve_requested_version(
    module_path: &str,
    requested_version: RequestedVersion,
    current_lane: Option<&str>,
) -> Result<Version> {
    let versions = available_versions_for_module(module_path)?;
    match requested_version {
        RequestedVersion::Latest => select_latest_stable_version(&versions, current_lane)
            .ok_or_else(|| match current_lane {
                Some(lane) => {
                    anyhow::anyhow!(
                        "No stable published version found for {} in lane {}",
                        module_path,
                        lane
                    )
                }
                None => anyhow::anyhow!("No stable published version found for {}", module_path),
            }),
        RequestedVersion::Exact(version) => {
            if versions.contains(&version) {
                Ok(version)
            } else {
                bail!("Version {} not found for {}", version, module_path);
            }
        }
    }
}

fn available_versions_for_module(module_path: &str) -> Result<Vec<Version>> {
    let (repo_url, subpath) = split_repo_and_subpath(module_path);
    let all_versions = tags::get_all_versions_for_repo(repo_url)
        .with_context(|| format!("Failed to fetch versions from {}", repo_url))?;
    let versions = all_versions
        .get(subpath)
        .ok_or_else(|| anyhow::anyhow!("No published versions found for {}", module_path))?;
    Ok(versions.clone())
}

fn select_latest_stable_version(versions: &[Version], lane: Option<&str>) -> Option<Version> {
    versions
        .iter()
        .find(|version| {
            version.pre.is_empty()
                && lane
                    .map(|lane| compatibility_lane(version) == lane)
                    .unwrap_or(true)
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dependency_request_defaults_to_latest_and_accepts_exact_versions() {
        assert_eq!(
            parse_dependency_request("github.com/acme/foo").unwrap(),
            ("github.com/acme/foo", RequestedVersion::Latest)
        );
        assert_eq!(
            parse_dependency_request("github.com/acme/foo@latest").unwrap(),
            ("github.com/acme/foo", RequestedVersion::Latest)
        );
        assert_eq!(
            parse_dependency_request("github.com/acme/foo@v1.2.3").unwrap(),
            (
                "github.com/acme/foo",
                RequestedVersion::Exact(Version::new(1, 2, 3))
            )
        );
    }

    #[test]
    fn select_latest_stable_version_respects_requested_lane() {
        let versions = vec![
            Version::parse("2.0.0-beta.1").unwrap(),
            Version::new(2, 0, 0),
            Version::new(0, 8, 3),
            Version::new(0, 8, 1),
            Version::new(0, 1, 7),
            Version::new(0, 1, 4),
        ];

        assert_eq!(
            select_latest_stable_version(&versions, None),
            Some(Version::new(2, 0, 0))
        );
        assert_eq!(
            select_latest_stable_version(&versions, Some("0.8")),
            Some(Version::new(0, 8, 3))
        );
        assert_eq!(
            select_latest_stable_version(&versions, Some("0.1")),
            Some(Version::new(0, 1, 7))
        );
    }
}
