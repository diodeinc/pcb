use anyhow::{Context, Result, bail};
use pcb_zen::tags;
use pcb_zen_core::config::{DependencySpec, PcbToml, split_repo_and_subpath};
use semver::Version;

use pcb_zen::package_resolver::{SpecVersionResolver, compatibility_lane};

const LATEST_SELECTOR: &str = "latest";

#[derive(Debug, Clone, PartialEq, Eq)]
enum RequestedVersion {
    Latest,
    Exact(Version),
    RefOrBranch(String),
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
            "Invalid dependency '{}'. Use `<url>@latest` or `<url>@1.2.3`.",
            raw
        );
    }

    let selector = selector.trim();
    if selector.is_empty() {
        bail!(
            "Missing version after '@' in '{}'. Use `<url>@latest` or `<url>@1.2.3`.",
            raw
        );
    }
    if selector.eq_ignore_ascii_case(LATEST_SELECTOR) {
        return Ok((module_path, RequestedVersion::Latest));
    }

    if let Some(version) = tags::parse_version(selector) {
        return Ok((module_path, RequestedVersion::Exact(version)));
    }

    Ok((
        module_path,
        RequestedVersion::RefOrBranch(selector.to_string()),
    ))
}

fn dependency_lane(spec: &DependencySpec) -> Option<String> {
    let raw_version = match spec {
        DependencySpec::Version(version) => Some(version.as_str()),
        DependencySpec::Detailed(detail) => detail.version.as_deref(),
    }?;
    let version = pcb_zen_core::parse_relaxed_version(raw_version)?;
    Some(compatibility_lane(&version))
}

fn resolve_requested_version(
    module_path: &str,
    requested_version: RequestedVersion,
    current_lane: Option<&str>,
) -> Result<Version> {
    match requested_version {
        RequestedVersion::RefOrBranch(selector) => {
            SpecVersionResolver::default().resolve_ref_or_branch(module_path, &selector)
        }
        RequestedVersion::Latest => {
            let versions = available_versions_for_module(module_path)?;
            select_latest_stable_version(&versions, current_lane).ok_or_else(
                || match current_lane {
                    Some(lane) => {
                        anyhow::anyhow!(
                            "No stable published version found for {} in lane {}",
                            module_path,
                            lane
                        )
                    }
                    None => {
                        anyhow::anyhow!("No stable published version found for {}", module_path)
                    }
                },
            )
        }
        RequestedVersion::Exact(version) => {
            let versions = available_versions_for_module(module_path)?;
            if versions.contains(&version) {
                Ok(version)
            } else {
                bail!("Version {} not found for {}", version, module_path);
            }
        }
    }
}

pub(crate) fn available_versions_for_module(module_path: &str) -> Result<Vec<Version>> {
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
