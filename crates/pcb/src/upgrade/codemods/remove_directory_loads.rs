use anyhow::Result;
use pcb_zen::load::DefaultRemoteFetcher;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::Codemod;
use pcb_zen_core::FileProvider;
use pcb_zen_core::{file_extensions, CoreLoadResolver, DefaultFileProvider, LoadResolver};
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::{ArgumentP, AstArgumentP, ExprP, StmtP};
use starlark_syntax::syntax::module::AstModuleFields;

pub struct RemoveDirectoryLoads;

impl Codemod for RemoveDirectoryLoads {
    fn apply(&self, current_file: &Path, content: &str) -> Result<Option<String>> {
        let file_provider = Arc::new(DefaultFileProvider);
        let remote_fetcher = Arc::new(DefaultRemoteFetcher);
        let resolver =
            CoreLoadResolver::for_file(file_provider.clone(), remote_fetcher, current_file, true);

        let ast = match AstModule::parse("<memory>", content.to_owned(), &Dialect::Extended) {
            Ok(a) => a,
            Err(_) => return Ok(None),
        };

        // Work with line-based edits using AST spans
        let mut lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
        let mut deletions: Vec<(usize, usize)> = Vec::new();
        let mut generated: Vec<String> = Vec::new();
        let mut last_load_end_line_excl: usize = 0;
        // Collect KiCad symbol alias -> filename mappings across loads
        let mut kicad_alias_to_symbol_file: HashMap<String, String> = HashMap::new();
        // Record loads we might delete later: (start, end_excl, has_starlark_targets, kicad_aliases)
        let mut pending_loads: Vec<(usize, usize, bool, Vec<String>)> = Vec::new();

        for stmt in starlark_syntax::syntax::top_level_stmts::top_level_stmts(ast.statement()) {
            let StmtP::Load(load) = &stmt.node else {
                continue;
            };

            // Module path string from the AST (already dequoted)
            let module_path: &str = &load.module.node;

            if is_file_like(module_path) {
                continue;
            }

            // Resolve
            let resolved_dir =
                match resolver.resolve_path(file_provider.as_ref(), module_path, current_file) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
            if !file_provider.is_directory(&resolved_dir) {
                continue;
            }

            // Collect file entries in directory (files only)
            let Ok(dir_entries) = file_provider.list_directory(&resolved_dir) else {
                continue;
            };
            let mut files: Vec<PathBuf> = Vec::new();
            for entry in dir_entries {
                if file_provider.exists(&entry) && !file_provider.is_directory(&entry) {
                    files.push(entry);
                }
            }

            // Resolve locals: use `their` for the file stem and `local` for the bound name
            let mut local_to_path: Vec<(String, PathBuf)> = Vec::new();
            let mut local_kicad_to_symbol_file: Vec<(String, String)> = Vec::new();
            let all_exist = true;
            for arg in &load.args {
                let local = arg.local.to_string();
                let their = arg.their.to_string();
                // For this alias, determine best match: prefer exact starlark, then starlark prefix,
                // else exact kicad_sym, then kicad_sym prefix. Ignore other extensions.
                let mut starlark_exact: Option<PathBuf> = None;
                let mut starlark_candidates: Vec<PathBuf> = Vec::new();
                let mut kicad_exact: Option<PathBuf> = None;
                let mut kicad_candidates: Vec<PathBuf> = Vec::new();
                for file in &files {
                    let stem_opt = file.file_stem().and_then(|s| s.to_str());
                    let ext_opt = file.extension().and_then(|e| e.to_str());
                    let Some(stem) = stem_opt else { continue };
                    let Some(ext) = ext_opt else { continue };
                    if file_extensions::is_starlark_file(Some(std::ffi::OsStr::new(ext))) {
                        if stem == their {
                            starlark_exact = Some(file.clone());
                        } else if stem.starts_with(&their) {
                            starlark_candidates.push(file.clone());
                        }
                    } else if Some(ext) == Some("kicad_sym") {
                        if stem == their {
                            kicad_exact = Some(file.clone());
                        } else if stem.starts_with(&their) {
                            kicad_candidates.push(file.clone());
                        }
                    }
                }
                let chosen_starlark: Option<PathBuf> = starlark_exact.or_else(|| {
                    if starlark_candidates.is_empty() {
                        None
                    } else {
                        let mut v = starlark_candidates;
                        v.sort();
                        v.into_iter().next()
                    }
                });
                if let Some(p) = chosen_starlark {
                    local_to_path.push((local, p));
                    continue;
                }
                let chosen_kicad: Option<PathBuf> = kicad_exact.or_else(|| {
                    if kicad_candidates.is_empty() {
                        None
                    } else {
                        let mut v = kicad_candidates;
                        v.sort();
                        v.into_iter().next()
                    }
                });
                if let Some(p) = chosen_kicad {
                    let filename = p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default()
                        .to_string();
                    local_kicad_to_symbol_file.push((local, filename));
                    continue;
                }
                // Neither starlark nor kicad symbol found for this prefix: error
                anyhow::bail!(
                    "Cannot convert directory load: '{}' not found as .zen/.star or .kicad_sym in {}",
                    their,
                    resolved_dir.display()
                );
            }
            if !all_exist || (local_to_path.is_empty() && local_kicad_to_symbol_file.is_empty()) {
                continue;
            }

            // Compute full statement replacement line range and track last load
            let span = ast.codemap().resolve_span(stmt.span);
            let start_line = span.begin.line;
            let end_line_excl = (span.end.line) + 1;
            if end_line_excl > last_load_end_line_excl {
                last_load_end_line_excl = end_line_excl;
            }

            // Indent
            let _indent = " ".repeat(span.begin.column);
            let mut new_lines: Vec<String> = Vec::with_capacity(local_to_path.len());
            for (local, full_path) in local_to_path {
                let file_name = full_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                let rendered = format!("{module_path}/{file_name}");
                // Insert module assignments as a clean block at top-level
                new_lines.push(format!("{local} = Module(\"{rendered}\")"));
            }
            // Remember potential deletion of this load (always if we generated module lines;
            // for KiCad-only we will decide after scanning call sites)
            let has_starlark_targets = !new_lines.is_empty();
            let kicad_aliases: Vec<String> = local_kicad_to_symbol_file
                .iter()
                .map(|(l, _)| l.clone())
                .collect();
            pending_loads.push((
                start_line,
                end_line_excl,
                has_starlark_targets,
                kicad_aliases,
            ));
            // Record KiCad alias -> stem for call-site rewrites
            for (local, symbol_file) in local_kicad_to_symbol_file {
                let rendered = format!("{module_path}/{symbol_file}");
                kicad_alias_to_symbol_file.insert(local, rendered);
            }
            generated.extend(new_lines);
        }
        // Call-site rewrites for KiCad aliases
        let mut text_edits: Vec<(usize, usize, usize, usize, String)> = Vec::new();
        let mut replaced_aliases: HashSet<String> = HashSet::new();
        if !kicad_alias_to_symbol_file.is_empty() {
            let statement = ast.statement();
            statement.visit_expr(|expr| {
                if let ExprP::Call(name, args) = &expr.node {
                    if let ExprP::Identifier(ident) = &name.node {
                        let callee = ident.node.to_string();
                        if let Some(symbol_file) = kicad_alias_to_symbol_file.get(&callee) {
                            // Replace callee identifier with Component
                            let ns = ast.codemap().resolve_span(name.span);
                            text_edits.push((
                                ns.begin.line,
                                ns.begin.column,
                                ns.end.line,
                                ns.end.column,
                                "Component".to_string(),
                            ));
                            replaced_aliases.insert(callee);

                            // If no symbol kwarg present, insert one before ')'
                            let has_symbol = args.args.iter().any(|a| match a {
                                AstArgumentP {
                                    node: ArgumentP::Named(name, _),
                                    ..
                                } => name.node == "symbol",
                                _ => false,
                            });
                            if !has_symbol {
                                let call_resolved = ast.codemap().resolve_span(expr.span);
                                let insert_line = call_resolved.end.line;
                                let insert_col = call_resolved.end.column.saturating_sub(1);

                                // Determine if we need a leading comma based on existing text
                                let current_line_prefix = if insert_line < lines.len() {
                                    &lines[insert_line][..insert_col.min(lines[insert_line].len())]
                                } else {
                                    ""
                                };
                                let mut needs_leading_comma = !args.args.is_empty();
                                if current_line_prefix.trim_end().ends_with(',') {
                                    needs_leading_comma = false;
                                } else if insert_line > 0 {
                                    let prev = lines[insert_line - 1].trim_end();
                                    if prev.ends_with(',') {
                                        needs_leading_comma = false;
                                    }
                                }
                                let insertion = if args.args.is_empty() {
                                    format!("symbol = Symbol(\"{symbol_file}\")")
                                } else if needs_leading_comma {
                                    format!(", symbol = Symbol(\"{symbol_file}\")")
                                } else {
                                    format!(" symbol = Symbol(\"{symbol_file}\")")
                                };
                                text_edits.push((
                                    insert_line,
                                    insert_col,
                                    insert_line,
                                    insert_col,
                                    insertion,
                                ));
                            }
                        }
                    }
                }
            });
        }

        // Decide which loads to delete
        for (start, end, has_starlark_targets, kicad_aliases) in pending_loads {
            if has_starlark_targets
                || kicad_aliases
                    .iter()
                    .any(|alias| replaced_aliases.contains(alias))
            {
                deletions.push((start, end));
            }
        }

        // If nothing to change, bail out
        if generated.is_empty() && deletions.is_empty() && text_edits.is_empty() {
            return Ok(None);
        }

        // Apply KiCad call-site edits first (so line indices for deletions/generation still match)
        if !text_edits.is_empty() {
            // Sort by start position, then apply in reverse
            text_edits.sort_by(|a, b| (a.0, a.1, a.2, a.3).cmp(&(b.0, b.1, b.2, b.3)));
            for (start_line, start_col, end_line, end_col, replacement) in
                text_edits.into_iter().rev()
            {
                if start_line == end_line {
                    if start_line >= lines.len() {
                        continue;
                    }
                    let line = &mut lines[start_line];
                    if start_col > line.len() || end_col > line.len() || end_col < start_col {
                        continue;
                    }
                    let (pre, rest) = line.split_at(start_col);
                    let (_, post) = rest.split_at(end_col - start_col);
                    let mut new_line =
                        String::with_capacity(pre.len() + replacement.len() + post.len());
                    new_line.push_str(pre);
                    new_line.push_str(&replacement);
                    new_line.push_str(post);
                    *line = new_line;
                } else {
                    if start_line >= lines.len() || end_line >= lines.len() {
                        continue;
                    }
                    let first_prefix =
                        lines[start_line][..start_col.min(lines[start_line].len())].to_string();
                    let last_suffix =
                        lines[end_line][end_col.min(lines[end_line].len())..].to_string();
                    lines.splice(
                        start_line..=end_line,
                        vec![format!("{}{}{}", first_prefix, replacement, last_suffix)],
                    );
                }
            }
        }
        // Determine insertion index after accounting for deletions before it
        deletions.sort_by_key(|(s, _)| *s);
        let mut removed_before = 0usize;
        for (start, end) in &deletions {
            if *start < last_load_end_line_excl {
                let delta = (end.min(&last_load_end_line_excl)) - start;
                removed_before += delta;
            }
        }
        let mut insertion_index = last_load_end_line_excl.saturating_sub(removed_before);

        // Apply deletions bottom-up
        for (start, end) in deletions.into_iter().rev() {
            let capped_start = start.min(lines.len());
            let capped_end = end.min(lines.len());
            if capped_start < capped_end {
                lines.drain(capped_start..capped_end);
            }
        }

        // Build chunk with blank line before and after if we generated any Module assignments
        if !generated.is_empty() {
            let mut chunk: Vec<String> = Vec::with_capacity(generated.len() + 2);
            chunk.push(String::new());
            chunk.extend(generated);
            chunk.push(String::new());
            insertion_index = insertion_index.min(lines.len());
            lines.splice(insertion_index..insertion_index, chunk);
        }

        Ok(Some(lines.join("\n")))
    }
}

fn is_file_like(path: &str) -> bool {
    path.ends_with(".zen") || path.ends_with(".star")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn rewrites_directory_loads_when_all_symbols_exist() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        let mods_dir = tmp.path().join("mods");
        fs::create_dir(&mods_dir)?;
        fs::write(mods_dir.join("A.zen"), "# A")?;
        fs::write(mods_dir.join("B.zen"), "# B")?;

        let file = tmp.path().join("main.zen");
        let content = "load(\"./mods\", \"A\", \"B\")\n";
        fs::write(&file, content)?;

        let codemod = RemoveDirectoryLoads;
        let updated = codemod.apply(&file, content)?;
        assert!(updated.is_some());
        let out = updated.unwrap();
        insta::assert_snapshot!(out, @r#"A = Module("./mods/A.zen")
B = Module("./mods/B.zen")"#);
        Ok(())
    }

    #[test]
    fn does_not_rewrite_when_symbol_missing() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        let mods_dir = tmp.path().join("mods");
        fs::create_dir(&mods_dir)?;
        fs::write(mods_dir.join("A.zen"), "# A")?;

        let file = tmp.path().join("main.zen");
        let content = "load(\"./mods\", \"A\", \"B\")\n";
        fs::write(&file, content)?;

        let codemod = RemoveDirectoryLoads;
        let updated = codemod.apply(&file, content);
        assert!(updated.is_err());
        Ok(())
    }

    #[test]
    fn supports_aliasing_with_named_args() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        let mods_dir = tmp.path().join("mods");
        fs::create_dir(&mods_dir)?;
        fs::write(mods_dir.join("A.zen"), "# A")?;
        fs::write(mods_dir.join("B.zen"), "# B")?;

        let file = tmp.path().join("main.zen");
        let content = "load(\"./mods\", X = \"A\", \"B\")\n";
        fs::write(&file, content)?;

        let codemod = RemoveDirectoryLoads;
        let updated = codemod.apply(&file, content)?;
        assert!(updated.is_some());
        let out = updated.unwrap();
        insta::assert_snapshot!(out, @r#"X = Module("./mods/A.zen")
B = Module("./mods/B.zen")"#);
        Ok(())
    }

    #[test]
    fn inserts_block_after_last_load_with_blank_lines() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        let mods_dir = tmp.path().join("mods");
        std::fs::create_dir(&mods_dir)?;
        std::fs::write(mods_dir.join("A.zen"), "# A")?;
        std::fs::write(mods_dir.join("B.zen"), "# B")?;

        let file = tmp.path().join("main.zen");
        let content = r#"# header
load("./mods", "A")
mid()
load("./mods", "B")
# footer
"#;
        std::fs::write(&file, content)?;

        let codemod = RemoveDirectoryLoads;
        let updated = codemod.apply(&file, content)?;
        assert!(updated.is_some());
        let out = updated.unwrap();
        insta::assert_snapshot!(out, @r#"# header
mid()

A = Module("./mods/A.zen")
B = Module("./mods/B.zen")

# footer"#);
        Ok(())
    }

    #[test]
    fn errors_on_non_starlark_target() {
        let tmp = tempfile::tempdir().unwrap();

        let mods_dir = tmp.path().join("mods");
        std::fs::create_dir(&mods_dir).unwrap();
        // Create a non-starlark file with desired stem
        std::fs::write(mods_dir.join("A.txt"), "bad").unwrap();

        let file = tmp.path().join("main.zen");
        let content = "load(\"./mods\", \"A\")\n";
        std::fs::write(&file, content).unwrap();

        let codemod = RemoveDirectoryLoads;
        let res = codemod.apply(&file, content);
        assert!(
            res.is_err(),
            "expected error when non-starlark file matches stem"
        );
    }

    #[test]
    fn rewrites_kicad_symbol_calls_and_removes_load() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        let eda_dir = tmp.path().join("eda");
        std::fs::create_dir(&eda_dir)?;
        std::fs::write(eda_dir.join("419-10-210-30-007000.kicad_sym"), "sym")?;

        let file = tmp.path().join("main.zen");
        let content = r#"load("./eda", _419_10_210_30_007000 = "419-10-210-30-007000")

_419_10_210_30_007000(
  name = "U1",
)
"#;
        std::fs::write(&file, content)?;

        let codemod = RemoveDirectoryLoads;
        let updated = codemod.apply(&file, content)?;
        assert!(updated.is_some());
        let out = updated.unwrap();
        insta::assert_snapshot!(out, @r#"Component(
  name = "U1",
 symbol = Symbol("./eda/419-10-210-30-007000.kicad_sym"))"#);
        Ok(())
    }

    #[test]
    fn rewrites_kicad_symbol_calls_without_dup_symbol_and_inserts_module_for_mixed() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        let mods_dir = tmp.path().join("mods");
        std::fs::create_dir(&mods_dir)?;
        std::fs::write(mods_dir.join("A.zen"), "# A")?;
        std::fs::write(mods_dir.join("S.kicad_sym"), "sym")?;

        let file = tmp.path().join("main.zen");
        let content = r#"load("./mods", X = "A", S = "S")

S(name = "U1", symbol = Symbol("already.kicad_sym"))
"#;
        std::fs::write(&file, content)?;

        let codemod = RemoveDirectoryLoads;
        let updated = codemod.apply(&file, content)?;
        assert!(updated.is_some());
        let out = updated.unwrap();
        insta::assert_snapshot!(out, @r#"
X = Module("./mods/A.zen")


Component(name = "U1", symbol = Symbol("already.kicad_sym"))
"#);
        Ok(())
    }
}
