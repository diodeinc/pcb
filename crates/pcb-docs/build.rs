use regex::Regex;
use serde::Deserialize;
use std::env;
use std::fs;
use std::path::Path;
use walkdir::WalkDir;

#[derive(Deserialize, Default)]
struct Frontmatter {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
}

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let docs_dir = Path::new(&manifest_dir).join("../../docs/pages");

    println!("cargo:rerun-if-changed={}", docs_dir.display());

    let mut pages = Vec::new();
    let mut sections = Vec::new();

    for entry in WalkDir::new(&docs_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "mdx"))
    {
        let path = entry.path();
        println!("cargo:rerun-if-changed={}", path.display());

        let content = fs::read_to_string(path).expect("Failed to read MDX file");
        // Normalize line endings to LF (Windows uses CRLF which breaks byte offsets)
        let content = content.replace("\r\n", "\n");
        let slug = path.file_stem().unwrap().to_string_lossy().to_string();

        let (frontmatter, body) = parse_frontmatter(&content);
        let title = if frontmatter.title.is_empty() {
            slug.clone()
        } else {
            frontmatter.title
        };

        let markdown = convert_mdx_to_markdown(&body);
        let page_sections = extract_sections(&slug, &markdown);
        sections.extend(page_sections);

        pages.push((slug, title, frontmatter.description, markdown));
    }

    pages.sort_by(|a, b| a.0.cmp(&b.0));
    write_generated_code(&out_dir, &pages, &sections);
}

fn parse_frontmatter(content: &str) -> (Frontmatter, String) {
    if !content.starts_with("---") {
        return (Frontmatter::default(), content.to_string());
    }

    let parts: Vec<&str> = content.splitn(3, "---").collect();
    if parts.len() < 3 {
        return (Frontmatter::default(), content.to_string());
    }

    let frontmatter: Frontmatter = serde_yaml::from_str(parts[1]).unwrap_or_default();
    (frontmatter, parts[2].trim_start().to_string())
}

fn convert_mdx_to_markdown(content: &str) -> String {
    let mut result = content.to_string();

    // Callouts: <Info>, <Warning>, <Tip>, <Note> -> blockquotes
    let callout_re =
        Regex::new(r"(?s)<(Info|Warning|Tip|Note)>\s*(.*?)\s*</(Info|Warning|Tip|Note)>").unwrap();
    result = callout_re
        .replace_all(&result, |caps: &regex::Captures| {
            let tag = &caps[1];
            let inner = caps[2]
                .lines()
                .map(|line| format!("> {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!("> **{tag}**\n>\n{inner}")
        })
        .to_string();

    // Accordion -> heading
    let accordion_re =
        Regex::new(r#"(?s)<Accordion\s+title="([^"]*)">\s*(.*?)\s*</Accordion>"#).unwrap();
    result = accordion_re
        .replace_all(&result, |caps: &regex::Captures| {
            format!("### {}\n\n{}", &caps[1], &caps[2])
        })
        .to_string();

    // Strip CodeGroup wrappers
    result = Regex::new(r"</?CodeGroup>\s*")
        .unwrap()
        .replace_all(&result, "")
        .to_string();

    // Code fence titles: ```ts title="TypeScript" -> **TypeScript**\n\n```ts
    result = Regex::new(r#"```(\w+)\s+(\w+)\s*\n"#)
        .unwrap()
        .replace_all(&result, |caps: &regex::Captures| {
            format!("**{}**\n\n```{}\n", &caps[2], &caps[1])
        })
        .to_string();

    // <img src="..." alt="..."> -> ![alt](src)
    result = Regex::new(r#"<img\s+[^>]*src="([^"]*)"[^>]*alt="([^"]*)"[^>]*/?\s*>"#)
        .unwrap()
        .replace_all(&result, "![$2]($1)")
        .to_string();
    result = Regex::new(r#"<img\s+[^>]*alt="([^"]*)"[^>]*src="([^"]*)"[^>]*/?\s*>"#)
        .unwrap()
        .replace_all(&result, "![$1]($2)")
        .to_string();
    result = Regex::new(r#"<img\s+[^>]*src="([^"]*)"[^>]*/?\s*>"#)
        .unwrap()
        .replace_all(&result, "![]($1)")
        .to_string();

    // Strip self-closing JSX tags
    result = Regex::new(r"<[A-Z][a-zA-Z]*\s*[^>]*/\s*>")
        .unwrap()
        .replace_all(&result, "")
        .to_string();

    // Strip block JSX tags, keep content
    for tag in ["Frame", "Card", "Cards", "Steps", "Step", "Tabs", "Tab"] {
        if let Ok(re) = Regex::new(&format!(r"(?s)<{tag}[^>]*>\s*(.*?)\s*</{tag}>")) {
            result = re.replace_all(&result, "$1").to_string();
        }
    }

    // Clean up multiple blank lines
    result = Regex::new(r"\n{3,}")
        .unwrap()
        .replace_all(&result, "\n\n")
        .to_string();

    result.trim().to_string()
}

struct SectionData {
    page_slug: String,
    section_id: String,
    title: String,
    level: u8,
    start: usize,
    end: usize,
}

/// Check if a position in markdown is inside a fenced code block.
/// Only considers real fence lines (lines starting with ``` after up to 3 spaces),
/// not ``` appearing in comments or inline code.
fn is_inside_code_block(markdown: &str, pos: usize) -> bool {
    let mut in_code_block = false;

    for line in markdown[..pos].lines() {
        let trimmed = line.trim_start();
        // Only toggle on lines that start with ``` (real markdown fences)
        // Lines like "# ```zen" don't count - they start with #
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
        }
    }

    in_code_block
}

fn extract_sections(page_slug: &str, markdown: &str) -> Vec<SectionData> {
    let heading_re = Regex::new(r"(?m)^(#{1,4})\s+(.+)$").unwrap();

    let headings: Vec<(usize, u8, String)> = heading_re
        .captures_iter(markdown)
        .filter_map(|cap| {
            let start = cap.get(0).unwrap().start();
            // Skip headings inside code blocks
            if is_inside_code_block(markdown, start) {
                return None;
            }
            let level = cap[1].len() as u8;
            let title = cap[2].trim().to_string();
            Some((start, level, title))
        })
        .collect();

    headings
        .iter()
        .enumerate()
        .map(|(i, (start, level, title))| {
            let end = headings
                .iter()
                .skip(i + 1)
                .find(|(_, l, _)| *l <= *level)
                .map(|(s, _, _)| *s)
                .unwrap_or(markdown.len());

            SectionData {
                page_slug: page_slug.to_string(),
                section_id: slugify(title),
                title: title.clone(),
                level: *level,
                start: *start,
                end,
            }
        })
        .collect()
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn write_generated_code(
    out_dir: &str,
    pages: &[(String, String, String, String)],
    sections: &[SectionData],
) {
    let mut output = String::from("// Auto-generated by build.rs - do not edit\n\n");

    output.push_str(
        r#"#[derive(Debug, Clone)]
pub struct Page {
    pub slug: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub markdown: &'static str,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub page_slug: &'static str,
    pub section_id: &'static str,
    pub title: &'static str,
    pub level: u8,
    pub start: usize,
    pub end: usize,
}

"#,
    );

    output.push_str("pub static PAGES: &[Page] = &[\n");
    for (slug, title, description, markdown) in pages {
        output.push_str(&format!(
            "    Page {{ slug: {:?}, title: {:?}, description: {:?}, markdown: {:?} }},\n",
            slug, title, description, markdown
        ));
    }
    output.push_str("];\n\n");

    output.push_str("pub static SECTIONS: &[Section] = &[\n");
    for s in sections {
        output.push_str(&format!(
            "    Section {{ page_slug: {:?}, section_id: {:?}, title: {:?}, level: {}, start: {}, end: {} }},\n",
            s.page_slug, s.section_id, s.title, s.level, s.start, s.end
        ));
    }
    output.push_str("];\n");

    let dest_path = Path::new(out_dir).join("docs_index.rs");
    fs::write(dest_path, output).expect("Failed to write docs_index.rs");
}
