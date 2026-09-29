use anyhow::{Context, Result};
use clap::Args;
use semver::Version;
use std::io::{self, IsTerminal, Write};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::{LinesWithEndings, as_24_bit_terminal_escaped};
use termimad::MadSkin;

const CHANGELOG_URL: &str =
    "https://raw.githubusercontent.com/diodeinc/pcb/refs/heads/main/CHANGELOG.md";

#[derive(Debug, Args)]
pub struct ChangelogArgs {
    /// Version selector: latest, unreleased, 0.3.80, or 0.3.78..0.3.80
    #[arg(default_value = "")]
    pub(crate) selector: String,
}

pub fn execute(args: ChangelogArgs) -> Result<()> {
    let changelog = fetch_changelog()?;
    let rendered = render_from_content(&changelog, &args.selector)?;
    print_markdown(&rendered);
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

fn print_markdown(content: &str) {
    if io::stdout().is_terminal() {
        print_highlighted_markdown(content);
    } else {
        println!("{}", content);
    }
}

fn print_highlighted_markdown(content: &str) {
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let theme = &ts.themes["base16-mocha.dark"];
    let skin = make_skin();

    let mut stdout = io::stdout().lock();
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_buffer = String::new();
    let mut text_buffer = String::new();

    for line in content.lines() {
        if line.starts_with("```") {
            if in_code_block {
                // End of code block - highlight and print the accumulated code
                let syntax = ps
                    .find_syntax_by_token(&code_lang)
                    .unwrap_or_else(|| ps.find_syntax_plain_text());
                let mut h = HighlightLines::new(syntax, theme);

                for code_line in LinesWithEndings::from(&code_buffer) {
                    if let Ok(ranges) = h.highlight_line(code_line, &ps) {
                        let escaped = as_24_bit_terminal_escaped(&ranges[..], false);
                        let _ = write!(stdout, "{}", escaped);
                    }
                }
                let _ = write!(stdout, "\x1b[0m");

                code_buffer.clear();
                in_code_block = false;
            } else {
                // Start of code block - first flush any pending text
                if !text_buffer.is_empty() {
                    skin.write_text_on(&mut stdout, &text_buffer).ok();
                    text_buffer.clear();
                }

                // Extract language hint
                code_lang = line.trim_start_matches('`').trim().to_string();
                // Map common language names
                if code_lang == "python" || code_lang == "starlark" || code_lang == "zen" {
                    code_lang = "Python".to_string();
                } else if code_lang == "toml" {
                    code_lang = "TOML".to_string();
                } else if code_lang == "rust" {
                    code_lang = "Rust".to_string();
                }
                in_code_block = true;
            }
        } else if in_code_block {
            code_buffer.push_str(line);
            code_buffer.push('\n');
        } else {
            text_buffer.push_str(line);
            text_buffer.push('\n');
        }
    }

    // Flush remaining text
    if !text_buffer.is_empty() {
        skin.write_text_on(&mut stdout, &text_buffer).ok();
    }
    let _ = stdout.flush();
}

fn make_skin() -> MadSkin {
    use termimad::crossterm::style::{Attribute, Color::Rgb};

    let mut skin = MadSkin::default();

    // Gruvbox Dark palette
    let bright_orange = Rgb {
        r: 254,
        g: 128,
        b: 25,
    }; // #fe8019
    let bright_yellow = Rgb {
        r: 250,
        g: 189,
        b: 47,
    }; // #fabd2f
    let bright_green = Rgb {
        r: 184,
        g: 187,
        b: 38,
    }; // #b8bb26
    let bright_aqua = Rgb {
        r: 142,
        g: 192,
        b: 124,
    }; // #8ec07c
    let bright_blue = Rgb {
        r: 131,
        g: 165,
        b: 152,
    }; // #83a598
    let bright_purple = Rgb {
        r: 211,
        g: 134,
        b: 155,
    }; // #d3869b
    let fg3 = Rgb {
        r: 189,
        g: 174,
        b: 147,
    }; // #bdae93
    let bg1 = Rgb {
        r: 60,
        g: 56,
        b: 54,
    }; // #3c3836

    // Headers
    skin.headers[0].set_fg(bright_orange);
    skin.headers[0].add_attr(Attribute::Bold);
    skin.headers[1].set_fg(bright_yellow);
    skin.headers[1].add_attr(Attribute::Bold);
    skin.headers[2].set_fg(bright_aqua);
    skin.headers[3].set_fg(bright_blue);

    // Bold and italic
    skin.bold.set_fg(bright_orange);
    skin.italic.set_fg(fg3);
    skin.italic.add_attr(Attribute::Italic);

    // Code
    skin.code_block.set_bg(bg1);
    skin.code_block.set_fg(bright_green);
    skin.inline_code.set_fg(bright_yellow);

    // Bullet points
    skin.bullet.set_fg(bright_aqua);

    // Quote marks
    skin.quote_mark.set_fg(bright_purple);

    skin
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
