use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use base64::Engine;
use pcb_sexpr::formatter::{FormatMode, prettify};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::Path;

fn replace_model_path(text: &str, new_path: &str) -> (String, usize) {
    use regex::Regex;

    let model_pattern = Regex::new(r#"(?m)(^\s*\(model\s+)(?:"[^"]+"|[^\s)]+)"#).unwrap();

    let mut count = 0;
    let result = model_pattern.replace_all(text, |caps: &regex::Captures| {
        count += 1;
        format!("{}\"{}\"", &caps[1], new_path)
    });

    (result.to_string(), count)
}

fn extract_sexp_block(text: &str, pattern: &str) -> Option<(String, String)> {
    let pattern_regex =
        regex::Regex::new(&format!(r"(?m)^(\s*)({})", regex::escape(pattern))).unwrap();
    let captures = pattern_regex.captures(text)?;
    let line_start = captures.get(1)?.start();
    let block_start = captures.get(2)?.start();

    let mut depth = 0;
    let mut end_pos = block_start;

    for (i, ch) in text[block_start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end_pos = block_start + i + 1;
                    break;
                }
            }
            _ => {}
        }
    }

    if end_pos <= block_start || depth != 0 {
        return None;
    }

    let extract_end = if text[end_pos..].starts_with('\n') {
        end_pos + 1
    } else {
        end_pos
    };

    let extracted = text[line_start..extract_end].to_string();
    let remaining = text[..line_start].to_string() + &text[extract_end..];

    Some((extracted, remaining))
}

pub fn format_kicad_sexpr_source(source: &str, path_for_error: &Path) -> Result<String> {
    pcb_sexpr::parse(source)
        .map_err(|e| anyhow::anyhow!(e))
        .with_context(|| {
            format!(
                "Failed to parse KiCad S-expression file {}",
                path_for_error.display()
            )
        })?;

    Ok(prettify(source, FormatMode::Normal))
}

pub fn embed_step_in_footprint(
    footprint_content: String,
    step_bytes: Vec<u8>,
    step_filename: &str,
) -> Result<String> {
    let filename = step_filename.replace(".stp", ".step");
    let indent = "\t";

    let mut encoder = zstd::Encoder::new(Vec::new(), 17)?;
    encoder.include_contentsize(true)?;
    encoder.set_pledged_src_size(Some(step_bytes.len() as u64))?;
    encoder.write_all(&step_bytes)?;
    let compressed = encoder.finish()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&compressed);
    let checksum = format!("{:x}", Sha256::digest(&step_bytes));

    let b64_formatted = b64
        .as_bytes()
        .chunks(80)
        .enumerate()
        .map(|(i, chunk)| {
            let line = std::str::from_utf8(chunk).unwrap();
            if i == 0 {
                line.to_string()
            } else {
                format!("{indent}{indent}{indent}{indent}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let embed_block = format!(
        "{indent}(embedded_files\n\
         {indent}{indent}(file\n\
         {indent}{indent}{indent}(name {filename})\n\
         {indent}{indent}{indent}(type model)\n\
         {indent}{indent}{indent}(data |{b64_formatted}|)\n\
         {indent}{indent}{indent}(checksum \"{checksum}\")\n\
         {indent}{indent})\n\
         {indent})\n"
    );

    let model_block = format!(
        "{indent}(model \"kicad-embed://{filename}\"\n\
         {indent}{indent}(offset\n\
         {indent}{indent}{indent}(xyz 0 0 0)\n\
         {indent}{indent})\n\
         {indent}{indent}(scale\n\
         {indent}{indent}{indent}(xyz 1 1 1)\n\
         {indent}{indent})\n\
         {indent}{indent}(rotate\n\
         {indent}{indent}{indent}(xyz 0 0 0)\n\
         {indent}{indent})\n\
         {indent})\n"
    );

    let mut text = footprint_content;
    let (new_text, num_replaced) = replace_model_path(&text, &format!("kicad-embed://{filename}"));
    text = new_text;

    let extracted_model = if num_replaced > 0 {
        extract_sexp_block(&text, "(model ").map(|(model_text, remaining_text)| {
            text = remaining_text;
            model_text
        })
    } else {
        None
    };

    if !text.contains("(embedded_files")
        && let Some(pos) = text.rfind(')')
    {
        text.insert_str(pos, &embed_block);
    }

    if let Some(existing_model) = extracted_model {
        if let Some(pos) = text.rfind(')') {
            text.insert_str(pos, &existing_model);
        }
    } else if num_replaced == 0
        && let Some(pos) = text.rfind(')')
    {
        text.insert_str(pos, &model_block);
    }

    Ok(text)
}

pub fn embed_step_into_footprint_file(
    footprint_path: &Path,
    step_path: &Path,
    delete_step: bool,
) -> Result<()> {
    let footprint_content =
        fs::read_to_string(footprint_path).context("Failed to read footprint file")?;
    let step_bytes = fs::read(step_path).context("Failed to read STEP file")?;
    let step_filename = step_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("model.step");

    let embedded_content = embed_step_in_footprint(footprint_content, step_bytes, step_filename)?;
    let normalized_content = embedded_content.replace("\r\n", "\n");
    let formatted_content = format_kicad_sexpr_source(&normalized_content, footprint_path)?;

    AtomicFile::new(footprint_path, OverwriteBehavior::AllowOverwrite)
        .write(|f| {
            f.write_all(formatted_content.as_bytes())?;
            f.flush()
        })
        .map_err(|err| anyhow::anyhow!("Failed to write footprint file: {err}"))?;

    if delete_step {
        fs::remove_file(step_path).context("Failed to remove standalone STEP file")?;
    }

    Ok(())
}
