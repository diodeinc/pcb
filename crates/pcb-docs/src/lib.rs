//! Embedded documentation for the pcb CLI.
//!
//! This crate provides access to the Zener language documentation,
//! embedded at compile time from the MDX files in docs/pages/.

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

// Include the generated docs index
include!(concat!(env!("OUT_DIR"), "/docs_index.rs"));

/// Error type for doc lookup operations
#[derive(Debug, Clone)]
pub enum DocError {
    /// No match found for the query
    NoMatch {
        query: String,
        suggestions: Vec<String>,
    },
    /// Multiple ambiguous matches found
    Ambiguous {
        query: String,
        candidates: Vec<String>,
    },
}

impl std::fmt::Display for DocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocError::NoMatch { query, suggestions } => {
                write!(f, "No documentation found matching '{}'", query)?;
                if !suggestions.is_empty() {
                    write!(f, "\n\nDid you mean:")?;
                    for s in suggestions.iter().take(5) {
                        write!(f, "\n  {}", s)?;
                    }
                }
                Ok(())
            }
            DocError::Ambiguous { query, candidates } => {
                write!(f, "'{}' matches multiple items:", query)?;
                for c in candidates.iter().take(10) {
                    write!(f, "\n  {}", c)?;
                }
                write!(f, "\n\nPlease be more specific.")
            }
        }
    }
}

impl std::error::Error for DocError {}

/// Returns all available pages
pub fn list_pages() -> &'static [Page] {
    PAGES
}

/// Returns all sections for a given page slug
pub fn list_sections(page_slug: &str) -> Vec<&'static Section> {
    SECTIONS
        .iter()
        .filter(|s| s.page_slug == page_slug)
        .collect()
}

/// Find a page by exact slug
pub fn get_page_by_slug(slug: &str) -> Option<&'static Page> {
    PAGES.iter().find(|p| p.slug == slug)
}

/// Find a page using fuzzy matching
pub fn find_page(query: &str) -> Result<&'static Page, DocError> {
    let matcher = SkimMatcherV2::default();
    let query_lower = query.to_lowercase();

    let mut scored: Vec<(&Page, i64)> = PAGES
        .iter()
        .filter_map(|page| {
            // Try matching against slug, title, and combined
            let slug_score = matcher.fuzzy_match(&page.slug.to_lowercase(), &query_lower);
            let title_score = matcher.fuzzy_match(&page.title.to_lowercase(), &query_lower);
            let combined = format!("{} {}", page.slug, page.title).to_lowercase();
            let combined_score = matcher.fuzzy_match(&combined, &query_lower);

            let best = [slug_score, title_score, combined_score]
                .into_iter()
                .flatten()
                .max();

            best.map(|score| (page, score))
        })
        .collect();

    scored.sort_by(|a, b| b.1.cmp(&a.1));

    if scored.is_empty() {
        return Err(DocError::NoMatch {
            query: query.to_string(),
            suggestions: PAGES.iter().map(|p| p.slug.to_string()).collect(),
        });
    }

    // Check for ambiguity: if top two scores are very close
    if scored.len() > 1 {
        let top_score = scored[0].1;
        let second_score = scored[1].1;
        // If scores are within 20% of each other, consider it ambiguous
        if second_score > 0 && (top_score - second_score) < (top_score / 5) {
            let candidates: Vec<String> = scored
                .iter()
                .take(5)
                .filter(|(_, s)| *s > top_score - (top_score / 3))
                .map(|(p, _)| p.slug.to_string())
                .collect();
            if candidates.len() > 1 {
                return Err(DocError::Ambiguous {
                    query: query.to_string(),
                    candidates,
                });
            }
        }
    }

    Ok(scored[0].0)
}

/// Find a section within a page using fuzzy matching
pub fn find_section(page: &Page, query: &str) -> Result<&'static Section, DocError> {
    let query_lower = query.to_lowercase();

    let page_sections: Vec<&Section> = SECTIONS
        .iter()
        .filter(|s| s.page_slug == page.slug)
        .collect();

    if page_sections.is_empty() {
        return Err(DocError::NoMatch {
            query: query.to_string(),
            suggestions: vec![],
        });
    }

    // Check for exact match first (on section_id)
    if let Some(section) = page_sections.iter().find(|s| s.section_id == query_lower) {
        return Ok(*section);
    }

    let matcher = SkimMatcherV2::default();

    let mut scored: Vec<(&Section, i64)> = page_sections
        .iter()
        .filter_map(|section| {
            let id_score = matcher.fuzzy_match(&section.section_id.to_lowercase(), &query_lower);
            let title_score = matcher.fuzzy_match(&section.title.to_lowercase(), &query_lower);

            let best = [id_score, title_score].into_iter().flatten().max();
            best.map(|score| (*section, score))
        })
        .collect();

    scored.sort_by(|a, b| b.1.cmp(&a.1));

    if scored.is_empty() {
        return Err(DocError::NoMatch {
            query: query.to_string(),
            suggestions: page_sections
                .iter()
                .map(|s| format!("{}/{}", page.slug, s.section_id))
                .collect(),
        });
    }

    // Check for ambiguity - but only if scores are very close
    if scored.len() > 1 {
        let top_score = scored[0].1;
        let second_score = scored[1].1;
        // Use a tighter threshold - only ambiguous if within 10% and both high scores
        if second_score > 0 && top_score > 0 {
            let ratio = (top_score - second_score) as f64 / top_score as f64;
            if ratio < 0.1 {
                let candidates: Vec<String> = scored
                    .iter()
                    .take(5)
                    .filter(|(_, s)| (*s as f64) > (top_score as f64 * 0.9))
                    .map(|(sec, _)| format!("{}/{}", page.slug, sec.section_id))
                    .collect();
                if candidates.len() > 1 {
                    return Err(DocError::Ambiguous {
                        query: query.to_string(),
                        candidates,
                    });
                }
            }
        }
    }

    Ok(scored[0].0)
}

/// Get the markdown content for an entire page
pub fn render_page(page: &Page) -> &'static str {
    page.markdown
}

/// Get the markdown content for a specific section
pub fn render_section<'a>(page: &'a Page, section: &Section) -> &'a str {
    &page.markdown[section.start..section.end]
}

/// Parse a path like "spec" or "spec/net" and return the matching content
pub fn lookup(path: &str) -> Result<String, DocError> {
    let parts: Vec<&str> = path.split('/').collect();

    match parts.as_slice() {
        [] | [""] => {
            // List all pages
            let mut output = String::from("# Available Documentation\n\n");
            for page in PAGES {
                output.push_str(&format!("- **{}** - {}\n", page.slug, page.title));
                if !page.description.is_empty() {
                    output.push_str(&format!("  _{}_\n", page.description));
                }
            }
            Ok(output)
        }
        [page_query] => {
            let page = find_page(page_query)?;
            Ok(render_page(page).to_string())
        }
        [page_query, section_query] => {
            let page = find_page(page_query)?;
            let section = find_section(page, section_query)?;
            Ok(render_section(page, section).to_string())
        }
        _ => Err(DocError::NoMatch {
            query: path.to_string(),
            suggestions: vec!["Use format: page or page/section".to_string()],
        }),
    }
}

/// List sections for a page (for --list flag)
pub fn lookup_list(path: &str) -> Result<String, DocError> {
    if path.is_empty() {
        // Same as lookup("") - list all pages
        return lookup(path);
    }

    let page = find_page(path)?;
    let sections = list_sections(page.slug);

    let mut output = format!("Sections in '{}':\n\n", page.title);
    for section in sections {
        let indent = "  ".repeat((section.level - 1) as usize);
        output.push_str(&format!("{}- {}\n", indent, section.section_id));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pages_loaded() {
        assert!(!PAGES.is_empty(), "Should have at least one page");
    }

    #[test]
    fn test_sections_loaded() {
        assert!(!SECTIONS.is_empty(), "Should have at least one section");
    }

    #[test]
    fn test_find_page_exact() {
        let result = find_page("spec");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().slug, "spec");
    }

    #[test]
    fn test_find_page_fuzzy() {
        let result = find_page("spe");
        assert!(result.is_ok());
    }

    #[test]
    fn test_lookup_empty() {
        let result = lookup("");
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Available Documentation"));
    }

    #[test]
    fn test_section_offsets_valid() {
        for section in SECTIONS.iter() {
            let page = get_page_by_slug(section.page_slug);
            assert!(page.is_some(), "Section references unknown page");
            let page = page.unwrap();
            assert!(
                section.start <= section.end,
                "Section start > end for {}",
                section.section_id
            );
            assert!(
                section.end <= page.markdown.len(),
                "Section end exceeds page length for {}",
                section.section_id
            );
            let content = &page.markdown[section.start..section.end];
            assert!(
                content.starts_with('#'),
                "Section {} doesn't start with heading",
                section.section_id
            );
        }
    }

    #[test]
    fn test_spec_page_has_sections() {
        let page = get_page_by_slug("spec").expect("spec page missing");
        let sections = list_sections(page.slug);
        assert!(sections.len() > 10, "spec page should have many sections");
    }

    #[test]
    fn test_lookup_page_returns_content() {
        let content = lookup("spec").expect("lookup spec failed");
        assert!(
            content.len() > 1000,
            "spec page should have substantial content"
        );
    }

    #[test]
    fn test_lookup_section_returns_content() {
        let page = get_page_by_slug("spec").unwrap();
        let sections = list_sections(page.slug);
        let first_section = sections.first().expect("no sections");

        let content = render_section(page, first_section);
        assert!(!content.is_empty(), "section should have content");
    }
}
