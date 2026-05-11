use anyhow::{Context, Result};
use clap::Args;
use semver::Version;

const CHANGELOG_URL: &str =
    "https://raw.githubusercontent.com/diodeinc/pcb/refs/heads/main/CHANGELOG.md";

#[derive(Debug, Args)]
pub struct ChangelogArgs {
    /// Version selector: latest, unreleased, 0.3.80, or 0.3.78..0.3.80
    #[arg(default_value = "")]
    selector: String,
}

pub fn execute(args: ChangelogArgs) -> Result<()> {
    let changelog = fetch_changelog()?;
    let rendered = render_from_content(&changelog, &args.selector)?;
    crate::doc::print_markdown(&rendered);
    Ok(())
}

fn fetch_changelog() -> Result<String> {
    reqwest::blocking::Client::new()
        .get(CHANGELOG_URL)
        .header(reqwest::header::USER_AGENT, "pcb")
        .send()
        .context("Failed to fetch changelog")?
        .error_for_status()
        .context("Failed to fetch changelog")?
        .text()
        .context("Failed to read changelog response")
}

fn render_from_content(content: &str, selector: &str) -> Result<String> {
    let releases = parse_releases(content);
    let selector = selector.trim();
    if selector.is_empty() {
        return Ok(format_changelog_markdown(content));
    }

    let selected = if selector.eq_ignore_ascii_case("latest") {
        releases
            .iter()
            .find(|release| release.version.is_some() && release.has_content)
            .into_iter()
            .collect::<Vec<_>>()
    } else if selector.eq_ignore_ascii_case("unreleased") {
        releases
            .iter()
            .find(|release| release.is_unreleased && release.has_content)
            .into_iter()
            .collect::<Vec<_>>()
    } else if let Some((start, end)) = selector.split_once("..") {
        let start = parse_optional_version(start, "range start")?;
        let end = parse_optional_version(end, "range end")?;
        releases
            .iter()
            .filter(|release| {
                let Some(version) = &release.version else {
                    return false;
                };
                start.as_ref().is_none_or(|start| version > start)
                    && end.as_ref().is_none_or(|end| version <= end)
                    && release.has_content
            })
            .collect::<Vec<_>>()
    } else {
        let version = parse_version(selector, "version")?;
        releases
            .iter()
            .filter(|release| release.version.as_ref() == Some(&version) && release.has_content)
            .collect::<Vec<_>>()
    };

    anyhow::ensure!(
        !selected.is_empty(),
        "No changelog entries found for selector '{selector}'"
    );

    Ok(format_changelog_markdown(
        &selected
            .into_iter()
            .map(|release| release.markdown.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
    ))
}

fn parse_optional_version(raw: &str, label: &str) -> Result<Option<Version>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    parse_version(raw, label).map(Some)
}

fn parse_version(raw: &str, label: &str) -> Result<Version> {
    pcb_zen::tags::parse_version(raw).ok_or_else(|| anyhow::anyhow!("Invalid {label} '{raw}'"))
}

#[derive(Debug)]
struct Release {
    version: Option<Version>,
    is_unreleased: bool,
    has_content: bool,
    markdown: String,
}

fn parse_releases(content: &str) -> Vec<Release> {
    let mut releases = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current = Vec::new();

    for line in content.lines() {
        if parse_release_heading(line).is_some() {
            if let Some(heading) = current_heading.take() {
                releases.push(build_release(&heading, &current));
            }
            current_heading = Some(line.to_string());
            current.clear();
        } else if current_heading.is_some() {
            current.push(line.to_string());
        }
    }

    if let Some(heading) = current_heading {
        releases.push(build_release(&heading, &current));
    }

    releases
}

fn build_release(heading: &str, body: &[String]) -> Release {
    let label = parse_release_heading(heading).unwrap_or_default();
    let is_unreleased = label.eq_ignore_ascii_case("unreleased");
    let version = (!is_unreleased)
        .then(|| pcb_zen::tags::parse_version(label))
        .flatten();
    let has_content = body.iter().any(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("- ") || trimmed.starts_with("* ")
    });

    let mut lines = Vec::with_capacity(body.len() + 1);
    lines.push(heading.to_string());
    lines.extend(body.iter().cloned());
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }

    Release {
        version,
        is_unreleased,
        has_content,
        markdown: lines.join("\n"),
    }
}

fn parse_release_heading(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("## [")?;
    let end = rest.find(']')?;
    Some(&rest[..end])
}

/// Format changelog markdown while preserving internal spacing and fenced blocks.
fn format_changelog_markdown(content: &str) -> String {
    let mut result = Vec::new();
    let mut seen_content = false;
    let mut in_fence = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if !seen_content && trimmed.is_empty() {
            continue;
        }

        seen_content = true;

        if !in_fence {
            if let Some(header) = trimmed.strip_prefix("### ") {
                result.push(format!("**{}**", header));
            } else {
                result.push(line.to_string());
            }
        } else {
            result.push(line.to_string());
        }

        if trimmed.starts_with("```") {
            in_fence = !in_fence;
        }
    }

    while result.last().is_some_and(|line| line.trim().is_empty()) {
        result.pop();
    }

    result.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Changelog\n\n## [Unreleased]\n\n### Added\n\n- Future\n\n## [0.3.80] - 2026-05-11\n\n### Fixed\n\n- New\n\n## [0.3.79] - 2026-05-08\n\n### Added\n\n- Middle\n\n## [0.3.78] - 2026-05-07\n\n### Changed\n\n- Old\n";

    #[test]
    fn latest_selects_first_released_version() {
        let rendered = render_from_content(SAMPLE, "latest").unwrap();
        assert!(rendered.contains("0.3.80"));
        assert!(rendered.contains("New"));
        assert!(!rendered.contains("Future"));
    }

    #[test]
    fn range_is_exclusive_start_inclusive_end() {
        let rendered = render_from_content(SAMPLE, "0.3.78..0.3.80").unwrap();
        assert!(rendered.contains("0.3.80"));
        assert!(rendered.contains("0.3.79"));
        assert!(!rendered.contains("0.3.78"));
    }

    #[test]
    fn version_selects_exact_release() {
        let rendered = render_from_content(SAMPLE, "v0.3.79").unwrap();
        assert!(rendered.contains("Middle"));
        assert!(!rendered.contains("New"));
    }
}
