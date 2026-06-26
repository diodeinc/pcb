//! Write inferred transforms back into `.kicad_mod` files.
//!
//! Mirrors `cmd_patch` in the Python solver: replace or insert the
//! `(rotate (xyz ...))` and `(offset (xyz ...))` inside the top-level
//! `(model ...)` block, optionally writing a backup first.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use regex::Regex;

use crate::footprint;
use crate::fs_util;
use crate::pose::EulerPose;
use crate::solver;

pub struct Args {
    pub paths: Vec<PathBuf>,
    pub dry_run: bool,
    pub backup: bool,
    pub backup_suffix: String,
    pub verbose: bool,
}

pub fn run(args: Args) -> Result<()> {
    let mut exit_code: i32 = 0;
    for path in iter_footprint_paths(&args.paths)? {
        match patch_one(&path, &args) {
            Ok(msg) => println!("{msg}"),
            Err(err) => {
                exit_code = 1;
                eprintln!("error: {}: {err:#}", path.display());
            }
        }
    }
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
    Ok(())
}

fn patch_one(path: &Path, args: &Args) -> Result<String> {
    let fp = footprint::parse(path)?;
    let model = fp.require_model()?;
    let model_path = model.path.clone();
    let best = solver::solve_best(&fp)?;
    let predicted_offset = [best.translation[0], -best.translation[1], best.z_offset];
    let content =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let patched = patch_model_transform(&content, &model_path, best.pose, predicted_offset)?;

    if !args.dry_run {
        if args.backup {
            let backup_path = path.with_file_name(format!(
                "{}{}",
                path.file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("footprint.kicad_mod"),
                args.backup_suffix
            ));
            std::fs::write(&backup_path, &content)?;
        }
        std::fs::write(path, &patched)?;
    }

    let action = if args.dry_run {
        "would_patch"
    } else {
        "patched"
    };
    let mut msg = format!(
        "{action} {} rotate=({},{},{}) offset=({}) score={:.4}",
        path.display(),
        best.pose.x,
        best.pose.y,
        best.pose.z,
        format_xyz(predicted_offset),
        best.score
    );
    if args.verbose {
        let model = fp.require_model()?;
        msg.push_str(&format!(
            "  previous_rotate=({},{},{}) previous_offset=({}) translation_source={}",
            model.rotate.x,
            model.rotate.y,
            model.rotate.z,
            format_xyz(model.offset),
            best.translation_source,
        ));
    }
    Ok(msg)
}

fn iter_footprint_paths(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for raw in inputs {
        let p = std::fs::canonicalize(raw).unwrap_or_else(|_| raw.clone());
        if p.is_dir() {
            let mut entries = Vec::new();
            fs_util::collect_footprints(&p, &mut entries)?;
            for entry in entries {
                if seen.insert(entry.clone()) {
                    out.push(entry);
                }
            }
        } else if fs_util::is_kicad_mod(&p) && seen.insert(p.clone()) {
            out.push(p);
        }
    }
    Ok(out)
}

fn format_xyz(v: [f64; 3]) -> String {
    format!(
        "{} {} {}",
        format_cli(v[0]),
        format_cli(v[1]),
        format_cli(v[2])
    )
}

fn format_cli(v: f64) -> String {
    let v = if v.abs() < 5e-7 { 0.0 } else { v };
    let text = format!("{v:.6}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.').to_string();
    if trimmed.is_empty() {
        "0".into()
    } else {
        trimmed
    }
}

/// Replace or insert `(rotate (xyz ...))` and `(offset (xyz ...))` clauses
/// inside the `(model ...)` block whose path matches `model_path`. When
/// `model_path` is empty, falls back to the first model block.
pub fn patch_model_transform(
    content: &str,
    model_path: &str,
    rotate: EulerPose,
    offset: [f64; 3],
) -> Result<String> {
    let model_open = Regex::new(r"(?m)^\s*\(model\b").unwrap();
    let model_path_capture = Regex::new(r#"\(model\s+"([^"]+)""#).unwrap();

    let (start, end) =
        find_model_block_by_path(content, model_path, &model_open, &model_path_capture)?;

    let block = &content[start..=end];
    let block = patch_xyz(
        block,
        "rotate",
        [rotate.x as f64, rotate.y as f64, rotate.z as f64],
    );
    let block = patch_xyz(&block, "offset", offset);
    let mut out = String::with_capacity(content.len());
    out.push_str(&content[..start]);
    out.push_str(&block);
    out.push_str(&content[end + 1..]);
    Ok(out)
}

/// Find the `(model ...)` block whose path string matches `model_path`.
/// Falls back to the first model block if no path match is found.
fn find_model_block_by_path(
    content: &str,
    model_path: &str,
    model_open: &Regex,
    model_path_capture: &Regex,
) -> Result<(usize, usize)> {
    let mut first_match: Option<(usize, usize)> = None;

    for m in model_open.find_iter(content) {
        let start = m.start();
        let end = find_closing_paren(content, start)
            .ok_or_else(|| anyhow!("unable to locate end of model block"))?;
        let block = &content[start..=end];

        if first_match.is_none() {
            first_match = Some((start, end));
        }

        if !model_path.is_empty()
            && extract_model_path(block, model_path_capture) == Some(model_path)
        {
            return Ok((start, end));
        }
    }

    first_match.ok_or_else(|| anyhow!("footprint has no model block"))
}

fn extract_model_path<'a>(block: &'a str, model_path_capture: &Regex) -> Option<&'a str> {
    model_path_capture
        .captures(block)
        .and_then(|caps| caps.get(1).map(|m| m.as_str()))
}

fn patch_xyz(block: &str, key: &str, values: [f64; 3]) -> String {
    let replacement = format!("({key} (xyz {}))", format_xyz(values));
    let pattern = Regex::new(&format!(r"\({key}\s*\(\s*xyz\s+[^\)]*\)\s*\)")).unwrap();
    if pattern.is_match(block) {
        return pattern.replace(block, replacement.as_str()).into_owned();
    }
    // No existing clause: insert before the closing paren of the model block.
    let mut lines: Vec<String> = block.lines().map(str::to_string).collect();
    let (insert_idx, indent) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(i, line)| {
            let stripped = line.trim_start();
            if stripped.is_empty() || stripped.starts_with(')') {
                None
            } else {
                let leading = line.len() - stripped.len();
                Some((i, line[..leading].to_string()))
            }
        })
        .unwrap_or((lines.len().saturating_sub(1), "  ".into()));
    lines.insert(insert_idx, format!("{indent}{replacement}"));
    lines.join("\n")
}

fn find_closing_paren(content: &str, open_pos: usize) -> Option<usize> {
    let bytes = content.as_bytes();
    let mut depth = 0i32;
    let mut in_pipe = false;
    let mut i = open_pos;
    while i < bytes.len() {
        let ch = bytes[i];
        if in_pipe {
            if ch == b'|' {
                in_pipe = false;
            }
        } else if ch == b'|' {
            in_pipe = true;
        } else if ch == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
        } else if ch == b'(' {
            depth += 1;
        } else if ch == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_inserts_missing_clauses() {
        let src = "(footprint \"x\"\n  (model \"m.step\"\n  )\n)";
        let patched =
            patch_model_transform(src, "m.step", EulerPose::new(-90, 0, 180), [1.0, -2.0, 0.5])
                .unwrap();
        assert!(patched.contains("(rotate (xyz -90 0 180))"));
        assert!(patched.contains("(offset (xyz 1 -2 0.5))"));
    }

    #[test]
    fn patch_replaces_existing_clauses() {
        let src = "(footprint \"x\"\n  (model \"m.step\"\n    (offset (xyz 0 0 0))\n    (rotate (xyz 0 0 0))\n  )\n)";
        let patched =
            patch_model_transform(src, "m.step", EulerPose::new(90, 0, 0), [0.1, 0.2, 0.3])
                .unwrap();
        assert!(patched.contains("(rotate (xyz 90 0 0))"));
        assert!(patched.contains("(offset (xyz 0.1 0.2 0.3))"));
    }

    #[test]
    fn patch_targets_correct_block_in_multi_model() {
        let src = "\
(footprint \"x\"
  (model \"a.wrl\"
    (offset (xyz 0 0 0))
    (rotate (xyz 0 0 0))
  )
  (model \"b.step\"
    (offset (xyz 0 0 0))
    (rotate (xyz 0 0 0))
  )
)";
        let patched =
            patch_model_transform(src, "b.step", EulerPose::new(90, 0, 0), [1.0, 2.0, 3.0])
                .unwrap();
        // The .wrl block should be untouched.
        assert!(
            patched
                .contains("(model \"a.wrl\"\n    (offset (xyz 0 0 0))\n    (rotate (xyz 0 0 0))")
        );
        // The .step block should be patched.
        assert!(patched.contains("(rotate (xyz 90 0 0))"));
        assert!(patched.contains("(offset (xyz 1 2 3))"));
    }

    #[test]
    fn patch_uses_exact_model_path_match() {
        let src = "\
(footprint \"x\"
  (model \"ab.step\"
    (offset (xyz 0 0 0))
    (rotate (xyz 0 0 0))
  )
  (model \"b.step\"
    (offset (xyz 5 5 5))
    (rotate (xyz 5 5 5))
  )
)";
        let patched =
            patch_model_transform(src, "b.step", EulerPose::new(90, 0, 0), [1.0, 2.0, 3.0])
                .unwrap();
        assert!(
            patched
                .contains("(model \"ab.step\"\n    (offset (xyz 0 0 0))\n    (rotate (xyz 0 0 0))")
        );
        assert!(
            patched
                .contains("(model \"b.step\"\n    (offset (xyz 1 2 3))\n    (rotate (xyz 90 0 0))")
        );
    }

    #[test]
    fn patch_falls_back_to_first_block_when_path_empty() {
        let src = "(footprint \"x\"\n  (model \"m.step\"\n  )\n)";
        let patched =
            patch_model_transform(src, "", EulerPose::new(90, 0, 0), [1.0, 2.0, 3.0]).unwrap();
        assert!(patched.contains("(rotate (xyz 90 0 0))"));
    }
}
