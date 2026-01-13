//! Render documentation to Markdown format.

use crate::types::*;
use std::collections::BTreeMap;

/// Render the complete documentation.
///
/// - `package_url`: The fully qualified package URL (e.g. "github.com/diodeinc/stdlib")
/// - `local_path`: The local filesystem path where the package source is located
pub fn render_docs(
    files: &[FileDoc],
    package_url: Option<&str>,
    local_path: Option<&str>,
) -> String {
    let mut out = String::new();

    // Add source path comment so consumers know where to find the actual source
    if let Some(path) = local_path {
        out.push_str(&format!("<!-- source: {} -->\n\n", path));
    }

    // Add package URL as h1 header if provided
    if let Some(url) = package_url {
        // Special case: display github.com/diodeinc/stdlib as @stdlib
        let display_url = if url == "github.com/diodeinc/stdlib" {
            "@stdlib"
        } else {
            url
        };
        out.push_str(&format!("# {}\n\n", display_url));
    }

    // Render files grouped by directory, with proper heading depth
    render_directory(&mut out, "", files, 2);

    out
}

/// Render all files in a directory and its subdirectories.
fn render_directory(out: &mut String, dir: &str, files: &[FileDoc], depth: usize) {
    let heading = "#".repeat(depth);
    let prefix = if dir.is_empty() {
        String::new()
    } else {
        format!("{}/", dir)
    };

    // Get files directly in this directory
    let direct_files: Vec<_> = files
        .iter()
        .filter(|f| {
            let path = f.path();
            let parent = path.rfind('/').map(|i| &path[..i]).unwrap_or("");
            parent == dir
        })
        .collect();

    // Get immediate subdirectories
    let mut subdirs: BTreeMap<String, ()> = BTreeMap::new();
    for file in files {
        let path = file.path();
        // Check if this file is under our directory
        let rest = if prefix.is_empty() {
            path
        } else if let Some(r) = path.strip_prefix(&prefix) {
            r
        } else {
            continue;
        };
        // If there's a slash in the remaining path, it's in a subdirectory
        if let Some(slash) = rest.find('/') {
            let subdir_name = &rest[..slash];
            let full_subdir = if prefix.is_empty() {
                subdir_name.to_string()
            } else {
                format!("{}{}", prefix, subdir_name)
            };
            subdirs.insert(full_subdir, ());
        }
    }

    // Render files in this directory
    for file in &direct_files {
        out.push_str(&render_file(file, depth));
    }

    // Render subdirectories
    for subdir in subdirs.keys() {
        let dir_name = subdir.rsplit('/').next().unwrap_or(subdir);
        out.push_str(&format!("{} {}/\n\n", heading, dir_name));
        render_directory(out, subdir, files, depth + 1);
    }
}

/// Render a single file's documentation.
fn render_file(file: &FileDoc, depth: usize) -> String {
    match file {
        FileDoc::Library(lib) => render_library(lib, depth),
        FileDoc::Module(module) => render_module(module, depth),
    }
}

/// Render a library file's documentation.
fn render_library(lib: &LibraryDoc, depth: usize) -> String {
    let mut out = String::new();
    let heading = "#".repeat(depth);

    let filename = lib.path.rsplit('/').next().unwrap_or(&lib.path);
    out.push_str(&format!("{} {}\n\n", heading, filename));

    if let Some(doc) = &lib.file_doc {
        out.push_str(&doc.summary);
        out.push_str("\n\n");
        if !doc.description.is_empty() {
            out.push_str(&doc.description);
            out.push_str("\n\n");
        }
    }

    if !lib.types.is_empty() {
        out.push_str("**Types:**\n\n");
        for t in &lib.types {
            out.push_str(&format!("- {} ({})\n", t.name, t.kind));
        }
        out.push('\n');
    }

    if !lib.constants.is_empty() {
        out.push_str("**Constants:**\n\n");
        for c in &lib.constants {
            out.push_str(&format!("- {}\n", c.name));
        }
        out.push('\n');
    }

    if !lib.functions.is_empty() {
        out.push_str("**Functions:**\n\n");
        for f in &lib.functions {
            out.push_str("```python\n");
            if let Some(doc) = &f.doc {
                out.push_str(&format!("# {}\n", doc.summary));
                if !doc.description.is_empty() {
                    for line in doc.description.lines() {
                        if line.is_empty() {
                            out.push_str("#\n");
                        } else {
                            out.push_str(&format!("# {}\n", line));
                        }
                    }
                }
            }
            out.push_str(&format!("{}\n", f.signature));
            out.push_str("```\n\n");
        }
    }

    out
}

/// Render a module's documentation.
fn render_module(module: &ModuleDoc, depth: usize) -> String {
    let mut out = String::new();
    let heading = "#".repeat(depth);

    let filename = module.path.rsplit('/').next().unwrap_or(&module.path);
    out.push_str(&format!("{} {}\n\n", heading, filename));

    if let Some(doc) = &module.file_doc {
        out.push_str(&doc.summary);
        out.push_str("\n\n");
        if !doc.description.is_empty() {
            out.push_str(&doc.description);
            out.push_str("\n\n");
        }
    }

    if !module.signature.ios.is_empty() {
        out.push_str("**IO:**\n\n");
        out.push_str("| Name | Type |\n");
        out.push_str("|------|------|\n");
        for io in &module.signature.ios {
            let type_repr = io.type_repr.replace('|', "\\|");
            out.push_str(&format!("| {} | {} |\n", io.name, type_repr));
        }
        out.push('\n');
    }

    if !module.signature.configs.is_empty() {
        out.push_str("**Config:**\n\n");
        out.push_str("| Parameter | Type | Default |\n");
        out.push_str("|-----------|------|--------|\n");
        for param in &module.signature.configs {
            let default = if param.has_default && !param.default_repr.is_empty() {
                param.default_repr.clone()
            } else if param.optional {
                "optional".to_string()
            } else {
                "required".to_string()
            };
            let type_repr = param.type_repr.replace('|', "\\|");
            out.push_str(&format!(
                "| {} | {} | {} |\n",
                param.name, type_repr, default
            ));
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_library_basic() {
        let lib = LibraryDoc {
            path: "utils.zen".to_string(),
            file_doc: Some(DocString {
                summary: "General utilities for Zen.".to_string(),
                description: String::new(),
            }),
            functions: vec![FunctionDoc {
                name: "e96".to_string(),
                signature: "def e96(physical_value):".to_string(),
                doc: Some(DocString {
                    summary: "Return the closest E96 series value.".to_string(),
                    description: String::new(),
                }),
            }],
            types: vec![],
            constants: vec![ConstDoc {
                name: "E_SERIES".to_string(),
            }],
        };

        let output = render_library(&lib, 2);
        assert!(output.contains("## utils.zen"));
        assert!(output.contains("General utilities for Zen."));
        assert!(output.contains("def e96(physical_value):"));
        assert!(output.contains("E_SERIES"));
    }

    #[test]
    fn test_render_module_basic() {
        let module = ModuleDoc {
            path: "generics/Resistor.zen".to_string(),
            file_doc: None,
            signature: ModuleSignature {
                configs: vec![
                    ParamDoc {
                        name: "package".to_string(),
                        type_repr: "0603 | 0805".to_string(),
                        has_default: true,
                        default_repr: "\"0603\"".to_string(),
                        optional: false,
                    },
                    ParamDoc {
                        name: "value".to_string(),
                        type_repr: "Resistance".to_string(),
                        has_default: false,
                        default_repr: String::new(),
                        optional: false,
                    },
                ],
                ios: vec![
                    ParamDoc {
                        name: "P1".to_string(),
                        type_repr: "Net".to_string(),
                        has_default: true,
                        default_repr: String::new(),
                        optional: false,
                    },
                    ParamDoc {
                        name: "P2".to_string(),
                        type_repr: "Net".to_string(),
                        has_default: true,
                        default_repr: String::new(),
                        optional: false,
                    },
                ],
            },
        };

        let output = render_module(&module, 3);
        assert!(output.contains("### Resistor.zen"));
        assert!(output.contains("| P1 |"));
        assert!(output.contains("| package |"));
    }

    #[test]
    fn test_render_docs_with_package_url() {
        let files = vec![];
        let output = render_docs(&files, Some("github.com/user/repo"), Some("/path/to/pkg"));
        assert!(output.contains("<!-- source: /path/to/pkg -->"));
        assert!(output.contains("# github.com/user/repo\n"));
    }

    #[test]
    fn test_render_docs_stdlib_alias() {
        let files = vec![];
        let output = render_docs(&files, Some("github.com/diodeinc/stdlib"), None);
        assert!(output.contains("# @stdlib\n"));
    }
}
