use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use deunicode::deunicode;
use indicatif::ProgressBar;
use inquire::Select;
use minijinja::Environment;
use regex::Regex;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAvailability {
    #[serde(rename = "ECAD_model")]
    pub ecad_model: bool,
    #[serde(rename = "STEP_model")]
    pub step_model: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentSearchResult {
    pub part_number: String,
    pub manufacturer: Option<String>,
    pub description: Option<String>,
    pub package_category: Option<String>,
    pub component_id: String,
    pub datasheets: Vec<String>,
    pub model_availability: ModelAvailability,
    pub source: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ComponentDownloadMetadata {
    pub mpn: String,
    pub timestamp: String,
    pub source: String,
    pub manufacturer: Option<String>,
    pub part_view_url: Option<String>,
    pub part_id: Option<String>,
    pub symbol_filename: Option<String>,
    pub footprint_filename: Option<String>,
    pub step_filename: Option<String>,
    pub datasheet_filename: Option<String>,
    pub datasheet_url: Option<String>,
    pub file_hashes: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone)]
pub struct ComponentDownloadResult {
    pub symbol_url: Option<String>,
    pub footprint_url: Option<String>,
    pub step_url: Option<String>,
    pub datasheet_url: Option<String>,
    pub metadata: ComponentDownloadMetadata,
}

#[derive(Serialize)]
struct SearchRequest {
    mpn: String,
}

#[derive(Serialize)]
struct DownloadRequest {
    component_id: String,
}

#[derive(Deserialize)]
struct DownloadResponse {
    #[serde(rename = "symbolUrl")]
    symbol_url: Option<String>,
    #[serde(rename = "footprintUrl")]
    footprint_url: Option<String>,
    #[serde(rename = "stepUrl")]
    step_url: Option<String>,
    #[serde(rename = "datasheetUrl")]
    datasheet_url: Option<String>,
    metadata: DownloadResponseMetadata,
}

#[derive(Deserialize)]
struct DownloadResponseMetadata {
    mpn: String,
    timestamp: String,
    source: String,
    manufacturer: Option<String>,
    #[serde(rename = "partViewUrl")]
    part_view_url: Option<String>,
    #[serde(rename = "partId")]
    part_id: Option<String>,
    #[serde(rename = "symbolFilename")]
    symbol_filename: Option<String>,
    #[serde(rename = "footprintFilename")]
    footprint_filename: Option<String>,
    #[serde(rename = "stepFilename")]
    step_filename: Option<String>,
    #[serde(rename = "datasheetFilename")]
    datasheet_filename: Option<String>,
    #[serde(rename = "datasheetUrl")]
    datasheet_url: Option<String>,
    #[serde(rename = "fileHashes")]
    file_hashes: Option<std::collections::HashMap<String, String>>,
}

pub fn search_components(auth_token: &str, mpn: &str) -> Result<Vec<ComponentSearchResult>> {
    let api_base_url = crate::get_api_base_url();
    let url = format!("{}/api/component/search", api_base_url);

    let client = Client::builder().timeout(Duration::from_secs(60)).build()?;

    let response = client
        .post(&url)
        .bearer_auth(auth_token)
        .json(&SearchRequest {
            mpn: mpn.to_string(),
        })
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("Search failed: {}", response.status());
    }

    Ok(response.json()?)
}

pub fn download_component(auth_token: &str, component_id: &str) -> Result<ComponentDownloadResult> {
    let api_base_url = crate::get_api_base_url();
    let url = format!("{}/api/component/download", api_base_url);

    let client = Client::builder().timeout(Duration::from_secs(60)).build()?;

    let response = client
        .post(&url)
        .bearer_auth(auth_token)
        .json(&DownloadRequest {
            component_id: component_id.to_string(),
        })
        .send()?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().unwrap_or_default();
        anyhow::bail!("Download failed ({}): {}", status, error_text);
    }

    let download_response: DownloadResponse = response.json()?;

    Ok(ComponentDownloadResult {
        symbol_url: download_response.symbol_url,
        footprint_url: download_response.footprint_url,
        step_url: download_response.step_url,
        datasheet_url: download_response.datasheet_url,
        metadata: ComponentDownloadMetadata {
            mpn: download_response.metadata.mpn,
            timestamp: download_response.metadata.timestamp,
            source: download_response.metadata.source,
            manufacturer: download_response.metadata.manufacturer,
            part_view_url: download_response.metadata.part_view_url,
            part_id: download_response.metadata.part_id,
            symbol_filename: download_response.metadata.symbol_filename,
            footprint_filename: download_response.metadata.footprint_filename,
            step_filename: download_response.metadata.step_filename,
            datasheet_filename: download_response.metadata.datasheet_filename,
            datasheet_url: download_response.metadata.datasheet_url,
            file_hashes: download_response.metadata.file_hashes,
        },
    })
}

pub fn download_file(url: &str, output_path: &Path) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent("pcb-cli")
        .build()?;

    let response = client.get(url).send()?;

    if !response.status().is_success() {
        anyhow::bail!("File download failed: {} - URL: {}", response.status(), url);
    }

    let bytes = response.bytes()?;

    // Normalize line endings for text files (KiCad files)
    if let Some(ext) = output_path.extension().and_then(|e| e.to_str()) {
        if matches!(
            ext,
            "kicad_sym" | "kicad_mod" | "kicad_pcb" | "kicad_sch" | "kicad_pro"
        ) {
            let text = String::from_utf8_lossy(&bytes);
            let normalized = text.replace("\r\n", "\n");
            std::fs::write(output_path, normalized.as_bytes())?;
            return Ok(());
        }
    }

    std::fs::write(output_path, bytes)?;
    Ok(())
}

/// Upgrade a .kicad_sym file to the latest version using kicad-cli
/// Returns Ok(()) if upgrade succeeds or kicad-cli is not available (non-fatal)
fn upgrade_symbol(symbol_path: &Path) -> Result<()> {
    pcb_kicad::KiCadCliBuilder::new()
        .command("sym")
        .subcommand("upgrade")
        .arg(symbol_path.to_string_lossy().as_ref())
        .run()
}

/// Upgrade a footprint library directory using kicad-cli
/// Returns Ok(()) if upgrade succeeds or kicad-cli is not available (non-fatal)
fn upgrade_footprint(footprint_path: &Path) -> Result<()> {
    let lib_dir = footprint_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Footprint path has no parent directory"))?;

    pcb_kicad::KiCadCliBuilder::new()
        .command("fp")
        .subcommand("upgrade")
        .arg(lib_dir.to_string_lossy().as_ref())
        .run()
}

/// Replace the model path in existing model blocks, preserving offset/scale/rotate.
/// Returns (new_text, number_of_replacements).
///
/// This function ONLY replaces the filename/path in `(model "path")`, leaving any
/// existing offset/scale/rotate parameters intact. This preserves manually-tuned
/// transformations that users may have configured.
fn replace_model_path(text: &str, new_path: &str) -> (String, usize) {
    use regex::Regex;

    // Regex to match: (whitespace)(model )(quoted or unquoted path)
    // Captures group 1: the prefix including "(model "
    // Matches but doesn't capture: the old path
    let model_pattern = Regex::new(r#"(?m)(^\s*\(model\s+)(?:"[^"]+"|[^\s)]+)"#).unwrap();

    let mut count = 0;
    let result = model_pattern.replace_all(text, |caps: &regex::Captures| {
        count += 1;
        format!("{}\"{}\"", &caps[1], new_path)
    });

    (result.to_string(), count)
}

/// Find and extract an S-expression block starting with a given pattern.
/// Returns Some((extracted_text, remaining_text)) or None if not found.
///
/// This function:
/// 1. Finds the pattern in the text
/// 2. Captures leading whitespace from the start of the line
/// 3. Matches balanced parentheses to find the complete block
/// 4. Includes trailing newline if present
/// 5. Returns both the extracted block and the text with the block removed
fn extract_sexp_block(text: &str, pattern: &str) -> Option<(String, String)> {
    // Use regex to find the pattern at the start of a line (with optional leading whitespace)
    let pattern_regex = Regex::new(&format!(r"(?m)^(\s*)({})", regex::escape(pattern))).unwrap();
    let captures = pattern_regex.captures(text)?;
    let line_start = captures.get(1)?.start();
    let block_start = captures.get(2)?.start();

    // Count parentheses to find the matching closing paren
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

    // Include trailing newline if present
    let extract_end = if text[end_pos..].starts_with('\n') {
        end_pos + 1
    } else {
        end_pos
    };

    let extracted = text[line_start..extract_end].to_string();
    let remaining = text[..line_start].to_string() + &text[extract_end..];

    Some((extracted, remaining))
}

/// Embed a STEP file into a KiCad footprint using the KiCad 8/9 embedded files format.
///
/// This function:
/// 1. Compresses the STEP data with ZSTD (level 3 - balanced)
/// 2. Base64 encodes the compressed data
/// 3. Computes SHA256 checksum of raw STEP data
/// 4. Inserts an (embedded_files ...) S-expression block into the footprint
/// 5. Updates the model reference to use kicad-embed:// URI
fn embed_step_in_footprint(
    footprint_content: String,
    step_bytes: Vec<u8>,
    step_filename: &str,
) -> Result<String> {
    use base64::Engine;
    use sha2::{Digest, Sha256};
    use std::io::Write;

    let filename = step_filename.replace(".stp", ".step");
    let indent = "\t";

    // Compress, encode, and checksum
    let mut encoder = zstd::Encoder::new(Vec::new(), 3)?;
    encoder.write_all(&step_bytes)?;
    let compressed = encoder.finish()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&compressed);
    let checksum = format!("{:x}", Sha256::digest(&step_bytes));

    // Format base64: first line unindented, rest indented
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

    // Try to replace the path in existing model blocks, preserving offset/scale/rotate
    let (new_text, num_replaced) = replace_model_path(&text, &format!("kicad-embed://{filename}"));
    text = new_text;

    // If a model block exists, we need to extract it and reinsert it at the end
    // (after embedded_files) to maintain the correct order: embedded_files → model
    let extracted_model = if num_replaced > 0 {
        extract_sexp_block(&text, "(model ").map(|(model_text, remaining_text)| {
            text = remaining_text;
            model_text
        })
    } else {
        None
    };

    // Add embedded_files block if not already present
    if !text.contains("(embedded_files") {
        if let Some(pos) = text.rfind(')') {
            text.insert_str(pos, &embed_block);
        }
    }

    // Add or re-insert model block at the end (after embedded_files)
    if let Some(existing_model) = extracted_model {
        // Re-insert the extracted model block
        if let Some(pos) = text.rfind(')') {
            text.insert_str(pos, &existing_model);
        }
    } else if num_replaced == 0 {
        // No existing model block, add a new one with default transforms
        if let Some(pos) = text.rfind(')') {
            text.insert_str(pos, &model_block);
        }
    }

    Ok(text)
}

// Helper: Search and filter for ECAD-available components
fn search_and_filter(auth_token: &str, mpn: &str) -> Result<Vec<ComponentSearchResult>> {
    let results = search_components(auth_token, mpn)?;
    Ok(results
        .into_iter()
        .filter(|r| r.model_availability.ecad_model)
        .collect())
}

// Helper: Show component already exists message and return early
fn handle_already_exists(workspace_root: &Path, result: &AddComponentResult) -> bool {
    if !result.already_exists {
        return false;
    }

    let display_path = result
        .component_path
        .strip_prefix(workspace_root)
        .unwrap_or(&result.component_path);
    println!(
        "{} Component already exists at: {}",
        "ℹ".blue().bold(),
        display_path.display().to_string().cyan()
    );
    true
}

// Helper: Show component added message
fn show_component_added(
    component: &ComponentSearchResult,
    workspace_root: &Path,
    result: &AddComponentResult,
) {
    let display_path = result
        .component_path
        .strip_prefix(workspace_root)
        .unwrap_or(&result.component_path);
    println!(
        "{} Added {} to {}",
        "✓".green().bold(),
        component.part_number.bold(),
        display_path.display().to_string().cyan()
    );
}

/// Parse storage path from Supabase presigned URL
/// Example: https://xxx.supabase.co/storage/v1/object/sign/components/cse/Bosch/BMI323/datasheet.pdf?token=...
/// Returns: "components/cse/Bosch/BMI323/datasheet.pdf"
fn parse_storage_path_from_url(url: &str) -> Option<String> {
    // Look for /storage/v1/object/sign/ or /storage/v1/object/
    let patterns = ["/storage/v1/object/sign/", "/storage/v1/object/"];

    for pattern in &patterns {
        if let Some(start) = url.find(pattern) {
            let path_start = start + pattern.len();
            // Extract until '?' (query params) or end of string
            let path_end = url[path_start..]
                .find('?')
                .unwrap_or(url.len() - path_start);
            return Some(url[path_start..path_start + path_end].to_string());
        }
    }

    None
}

pub fn add_component_to_workspace(
    auth_token: &str,
    component_id: &str,
    part_number: &str,
    workspace_root: &std::path::Path,
) -> Result<AddComponentResult> {
    // Show progress during API call
    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    spinner.set_message(format!("Fetching {}...", part_number));

    let download = download_component(auth_token, component_id)?;
    spinner.finish_and_clear();

    // Build path: components/<manufacturer>/<part_number>/<part_number>.zen
    let sanitized_mfr = download
        .metadata
        .manufacturer
        .as_deref()
        .map(sanitize_mpn_for_path)
        .unwrap_or_else(|| "unknown".to_string());
    let sanitized_mpn = sanitize_mpn_for_path(part_number);
    let component_dir = workspace_root
        .join("components")
        .join(&sanitized_mfr)
        .join(&sanitized_mpn);
    let component_file = component_dir.join(format!("{}.zen", &sanitized_mpn));

    if component_file.exists() {
        return Ok(AddComponentResult {
            component_path: component_file,
            already_exists: true,
        });
    }

    fs::create_dir_all(&component_dir)?;

    // Count tasks and collect work
    let mut file_count = 0;
    let mut scan_count = 0;
    let mut download_tasks = Vec::new();
    let mut scan_tasks = Vec::new();
    let mut upgrade_tasks = Vec::new();

    if let (Some(url), Some(filename)) = (&download.symbol_url, &download.metadata.symbol_filename)
    {
        file_count += 1;
        let path = component_dir.join(filename);
        download_tasks.push((url.clone(), path.clone(), "symbol"));
        upgrade_tasks.push(("symbol", path));
    }
    if let (Some(url), Some(filename)) = (
        &download.footprint_url,
        &download.metadata.footprint_filename,
    ) {
        file_count += 1;
        let path = component_dir.join(filename);
        download_tasks.push((url.clone(), path.clone(), "footprint"));
        upgrade_tasks.push(("footprint", path));
    }
    if let (Some(url), Some(filename)) = (&download.step_url, &download.metadata.step_filename) {
        file_count += 1;
        download_tasks.push((url.clone(), component_dir.join(filename), "step"));
    }

    if let (Some(url), Some(filename)) = (
        &download.datasheet_url,
        &download.metadata.datasheet_filename,
    ) {
        file_count += 1;
        download_tasks.push((url.clone(), component_dir.join(filename), "datasheet"));

        if let Some(storage_path) = parse_storage_path_from_url(url) {
            let md_path = component_dir.join(filename).with_extension("md");
            if !md_path.exists() {
                scan_count += 1;
                scan_tasks.push((storage_path, filename.clone()));
            }
        }
    }

    // Show task summary
    println!("{} {}", "Downloading".green().bold(), part_number.bold());
    println!(
        "• {} files{}",
        file_count,
        if scan_count > 0 {
            format!(", {} datasheets to scan", scan_count)
        } else {
            String::new()
        }
    );

    let start = std::time::Instant::now();

    // Execute all tasks in parallel with progress indicator
    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    spinner.set_message("Processing...");

    let scan_results = Arc::new(Mutex::new(Vec::new()));
    let errors = Arc::new(Mutex::new(Vec::new()));

    std::thread::scope(|s| {
        let mut handles = Vec::new();

        // Download tasks
        for (url, path, label) in download_tasks {
            let errors = Arc::clone(&errors);
            handles.push(s.spawn(move || {
                if let Err(e) = download_file(&url, &path) {
                    errors.lock().unwrap().push(format!("{}: {}", label, e));
                }
            }));
        }

        // Wait for downloads to complete
        for handle in handles {
            let _ = handle.join();
        }

        let mut handles = Vec::new();

        // Upgrade tasks (after downloads complete)
        for (file_type, path) in upgrade_tasks {
            let errors = Arc::clone(&errors);
            handles.push(s.spawn(move || {
                let result = match file_type {
                    "symbol" => upgrade_symbol(&path),
                    "footprint" => upgrade_footprint(&path),
                    _ => return,
                };

                // Silently skip if kicad-cli not available, report other errors
                if let Err(e) = result {
                    let err_msg = e.to_string();
                    if !err_msg.contains("KiCad CLI not found") {
                        errors
                            .lock()
                            .unwrap()
                            .push(format!("upgrade {}: {}", file_type, e));
                    }
                }
            }));
        }

        // Wait for upgrades to complete
        for handle in handles {
            let _ = handle.join();
        }

        let mut handles = Vec::new();

        // Scan tasks (after upgrades)
        for (storage_path, filename) in scan_tasks {
            let output_dir = component_dir.clone();
            let errors = Arc::clone(&errors);
            let scan_results = Arc::clone(&scan_results);
            handles.push(s.spawn(move || {
                match crate::scan::scan_from_source_path(
                    auth_token,
                    &storage_path,
                    &output_dir,
                    None,
                    true,
                    false,
                ) {
                    Ok(result) => {
                        scan_results.lock().unwrap().push(result);
                    }
                    Err(e) => {
                        errors
                            .lock()
                            .unwrap()
                            .push(format!("Scan {}: {}", filename, e));
                    }
                }
            }));
        }

        for handle in handles {
            let _ = handle.join();
        }
    });

    spinner.finish_and_clear();

    let elapsed = start.elapsed();
    let scan_results = Arc::try_unwrap(scan_results).unwrap().into_inner().unwrap();
    let errors = Arc::try_unwrap(errors).unwrap().into_inner().unwrap();

    // Show results
    if errors.is_empty() {
        println!(
            "{} Downloaded {} files{} ({:.1}s)",
            "✓".green(),
            file_count,
            if !scan_results.is_empty() {
                format!(", scanned {} datasheets", scan_results.len())
            } else {
                String::new()
            },
            elapsed.as_secs_f64()
        );
    } else {
        println!(
            "  {} Completed with {} errors ({:.1}s)",
            "!".yellow(),
            errors.len(),
            elapsed.as_secs_f64()
        );
        for err in &errors {
            println!("    • {}", err.dimmed());
        }
    }

    // Post-process: Embed STEP into footprint if both files exist
    if let (Some(footprint_filename), Some(step_filename)) = (
        &download.metadata.footprint_filename,
        &download.metadata.step_filename,
    ) {
        let footprint_path = component_dir.join(footprint_filename);
        let step_path = component_dir.join(step_filename);

        if footprint_path.exists() && step_path.exists() {
            // Read both files
            let footprint_content =
                fs::read_to_string(&footprint_path).context("Failed to read footprint file")?;
            let step_bytes = fs::read(&step_path).context("Failed to read STEP file")?;

            // Embed STEP into footprint
            let embedded_content =
                embed_step_in_footprint(footprint_content, step_bytes, step_filename)?;

            // Normalize line endings and write to temporary file
            let normalized_content = embedded_content.replace("\r\n", "\n");
            let temp_path = footprint_path.with_extension("kicad_mod.tmp");
            fs::write(&temp_path, normalized_content)
                .context("Failed to write temporary footprint file")?;

            // Atomic rename to replace original
            fs::rename(&temp_path, &footprint_path)
                .context("Failed to rename temporary footprint file")?;

            // Delete standalone STEP file
            fs::remove_file(&step_path).context("Failed to remove standalone STEP file")?;
        }
    }

    // Generate .zen file if symbol was downloaded
    if let Some(symbol_filename) = &download.metadata.symbol_filename {
        let symbol_path = component_dir.join(symbol_filename);
        if symbol_path.exists() {
            let symbol_lib = pcb_eda::SymbolLibrary::from_file(&symbol_path)?;
            let symbol = symbol_lib
                .first_symbol()
                .ok_or_else(|| anyhow::anyhow!("No symbols in library"))?;

            let sanitized_name = sanitize_mpn_for_path(part_number);
            let content = generate_zen_file(
                part_number,
                &sanitized_name,
                symbol,
                symbol_filename,
                download.metadata.footprint_filename.as_deref(),
                download.metadata.datasheet_filename.as_deref(),
                download.metadata.manufacturer.as_deref(),
            )?;

            fs::write(&component_file, content)?;
        }
    }

    Ok(AddComponentResult {
        component_path: component_file,
        already_exists: false,
    })
}

pub struct AddComponentResult {
    pub component_path: PathBuf,
    pub already_exists: bool,
}

/// Sanitize an MPN for use as a directory/file name and Component name
///
/// Process:
/// 1. Replace unsafe ASCII → underscore (keep a-z A-Z 0-9 - _, keep Unicode)
/// 2. Transliterate Unicode → ASCII
/// 3. Replace leftover unsafe chars → underscore
/// 4. Cleanup: collapse multiple underscores, trim leading/trailing
fn sanitize_mpn_for_path(mpn: &str) -> String {
    fn is_safe(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '-' || c == '_'
    }

    // Replace unsafe ASCII with _, keep Unicode for transliteration
    let ascii_cleaned: String = mpn
        .chars()
        .map(|c| if c.is_ascii() && !is_safe(c) { '_' } else { c })
        .collect();

    // Transliterate Unicode to ASCII
    let transliterated = deunicode(&ascii_cleaned);

    // Replace any remaining unsafe chars from transliteration
    let all_safe: String = transliterated
        .chars()
        .map(|c| if is_safe(c) { c } else { '_' })
        .collect();

    // Collapse multiple underscores and trim
    let cleaned: String = all_safe
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    if cleaned.is_empty() {
        "component".to_string()
    } else {
        cleaned
    }
}

/// Sanitize a pin name to create a valid Starlark identifier
fn sanitize(name: &str) -> String {
    let result = name
        .chars()
        .map(|c| match c {
            '+' => 'P', // Plus becomes P (e.g., V+ → VP)
            '-' => 'M', // Minus becomes M (e.g., V- → VM)
            '~' => 'n', // Tilde (NOT) becomes n (e.g., ~CS → nCS)
            '!' => 'n', // Bang (NOT) also becomes n
            '#' => 'h', // Hash becomes h (e.g., CS# → CSh)
            c if c.is_alphanumeric() => c,
            _ => '_',
        })
        .collect::<String>();

    // Remove consecutive underscores and trim underscores from start/end
    let sanitized = result
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");

    // Prefix with P if starts with digit
    if sanitized.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("P{}", sanitized)
    } else {
        sanitized
    }
}

fn generate_zen_file(
    mpn: &str,
    component_name: &str,
    symbol: &pcb_eda::Symbol,
    symbol_filename: &str,
    footprint_filename: Option<&str>,
    datasheet_filename: Option<&str>,
    manufacturer: Option<&str>,
) -> Result<String> {
    // Group pins by sanitized name to handle duplicates
    let mut pin_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for pin in &symbol.pins {
        pin_groups
            .entry(sanitize(&pin.name))
            .or_default()
            .push(pin.name.clone());
    }

    // Prepare template data - collect unique pins
    let pin_groups_vec: Vec<_> = pin_groups
        .keys()
        .map(|name| serde_json::json!({"sanitized_name": name}))
        .collect();

    let pin_mappings: Vec<_> = pin_groups
        .iter()
        .flat_map(|(sanitized, originals)| {
            originals.iter().map(move |orig| {
                serde_json::json!({
                    "original_name": orig,
                    "sanitized_name": sanitized
                })
            })
        })
        .collect();

    // Render template
    let mut env = Environment::new();
    env.add_template(
        "component.zen",
        include_str!("../templates/component.zen.jinja"),
    )?;

    let content = env
        .get_template("component.zen")?
        .render(serde_json::json!({
            "component_name": component_name,
            "mpn": mpn,
            "manufacturer": manufacturer,
            "sym_path": symbol_filename,
            "footprint_path": footprint_filename.unwrap_or(&format!("{}.kicad_mod", mpn)),
            "pin_groups": pin_groups_vec,
            "pin_mappings": pin_mappings,
            "description": symbol.description,
            "datasheet_file": datasheet_filename,
        }))?;

    Ok(content)
}

fn get_terminal_width() -> usize {
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(80)
}

fn get_terminal_height() -> usize {
    terminal_size::terminal_size()
        .map(|(_, h)| h.0 as usize)
        .unwrap_or(24)
}

fn truncate_text(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }

    let mut result = String::new();
    let mut width = 0;

    for ch in text.chars() {
        let char_width = ch.width().unwrap_or(0);
        if width + char_width + 3 > max_width {
            break;
        }
        result.push(ch);
        width += char_width;
    }

    result + "..."
}

struct ColumnWidths {
    src: usize,
    part: usize,
    mfr: usize,
    pkg: usize,
    #[allow(dead_code)] // Fixed width, not used in formatting but kept for completeness
    models: usize,
    desc: usize,
}

/// Helper to check if a package should be displayed
fn is_displayable_package(pkg: &str) -> bool {
    pkg.len() <= 15 && !pkg.contains(' ') && pkg != "Other"
}

/// Helper to get source abbreviation: "C" for CSE, "L" for LCSC
fn source_abbrev(source: Option<&str>) -> &'static str {
    source
        .and_then(|s| match s.to_lowercase().as_str() {
            s if s.contains("cse") => Some("C"),
            s if s.contains("lcsc") => Some("L"),
            _ => None,
        })
        .unwrap_or("?")
}

/// Helper to format a column with padding
fn format_column(text: &str, width: usize) -> String {
    format!("{:<width$}", truncate_text(text, width), width = width)
}

/// Helper to get check/cross icon
fn check_icon(available: bool) -> colored::ColoredString {
    if available {
        "✓".green()
    } else {
        "✗".red()
    }
}

/// Helper to calculate max width for a column with min/max bounds
fn calc_col_width<'a, I>(items: I, min: usize, max: usize) -> usize
where
    I: Iterator<Item = &'a str>,
{
    items
        .map(|s| s.width())
        .max()
        .unwrap_or(min)
        .max(min)
        .min(max)
}

/// Helper to clean description text (ASCII + whitespace only)
fn clean_description(desc: Option<&str>) -> String {
    desc.unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii() || c.is_whitespace())
        .collect()
}

fn calculate_column_widths(results: &[ComponentSearchResult]) -> ColumnWidths {
    let terminal_width = get_terminal_width().saturating_sub(2);

    let part = calc_col_width(results.iter().map(|r| r.part_number.as_str()), 10, 30);
    let mfr = calc_col_width(
        results.iter().filter_map(|r| r.manufacturer.as_deref()),
        3,
        20,
    );
    let pkg = calc_col_width(
        results
            .iter()
            .filter_map(|r| r.package_category.as_deref())
            .filter(|p| is_displayable_package(p)),
        3,
        12,
    );

    let used = 1 + part + mfr + pkg + 14 + 10; // src(1) + part + mfr + pkg + models(14) + spacing(10)
    let desc = terminal_width.saturating_sub(used).max(20);

    ColumnWidths {
        src: 1,
        part,
        mfr,
        pkg,
        models: 14,
        desc,
    }
}

fn format_search_result(result: &ComponentSearchResult, widths: &ColumnWidths) -> String {
    let src = format_column(source_abbrev(result.source.as_deref()), widths.src).bright_black();
    let part = format_column(&result.part_number, widths.part).bold();
    let mfr = format_column(result.manufacturer.as_deref().unwrap_or(""), widths.mfr).cyan();
    let pkg = format_column(
        result
            .package_category
            .as_deref()
            .filter(|p| is_displayable_package(p))
            .unwrap_or(""),
        widths.pkg,
    )
    .yellow();
    let models = format!(
        "[2D {}] [3D {}]",
        check_icon(result.model_availability.ecad_model),
        check_icon(result.model_availability.step_model)
    );
    let desc = format_column(
        &clean_description(result.description.as_deref()),
        widths.desc,
    )
    .dimmed();

    format!("{}  {}  {}  {}  {}  {}", src, part, mfr, pkg, models, desc)
}

pub fn search_interactive(
    auth_token: &str,
    mpn: &str,
    workspace_root: &std::path::Path,
) -> Result<()> {
    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    spinner.set_message("Searching for components...");

    let filtered_results = search_and_filter(auth_token, mpn)?;
    spinner.finish_and_clear();

    if filtered_results.is_empty() {
        println!("No results found with ECAD data available.");
        return Ok(());
    }

    println!(
        "{} {} components with ECAD data available:",
        "Found".green().bold(),
        filtered_results.len()
    );

    let widths = calculate_column_widths(&filtered_results);
    let items: Vec<String> = filtered_results
        .iter()
        .map(|r| format_search_result(r, &widths))
        .collect();
    let page_size = get_terminal_height().saturating_sub(5).max(5);

    let selection = Select::new(
        "Select a component to download and add to ./components:",
        items,
    )
    .with_page_size(page_size)
    .with_formatter(&|_| String::new()) // Hide the final selection output
    .prompt()?;

    let selected_index = filtered_results
        .iter()
        .position(|r| format_search_result(r, &widths) == selection)
        .context("Selected component not found")?;
    let selected_component = &filtered_results[selected_index];
    println!(
        "{} {}",
        "Selected:".green().bold(),
        selected_component.part_number.bold()
    );
    if let Some(description) = &selected_component.description {
        println!("{} {}", "Description:".cyan(), description);
    }

    let result = add_component_to_workspace(
        auth_token,
        &selected_component.component_id,
        &selected_component.part_number,
        workspace_root,
    )?;

    if handle_already_exists(workspace_root, &result) {
        return Ok(());
    }

    show_component_added(selected_component, workspace_root, &result);
    Ok(())
}

pub fn search_json(auth_token: &str, mpn: &str) -> Result<String> {
    let filtered_results = search_and_filter(auth_token, mpn)?;

    let json_results: Vec<serde_json::Value> = filtered_results
        .iter()
        .map(|r| {
            serde_json::json!({
                "part_number": r.part_number,
                "manufacturer": r.manufacturer,
                "description": r.description,
                "package_category": r.package_category,
                "component_id": r.component_id,
                "has_2d_model": r.model_availability.ecad_model,
                "has_3d_model": r.model_availability.step_model,
                "datasheets": r.datasheets,
                "source": r.source,
            })
        })
        .collect();

    Ok(serde_json::to_string_pretty(&json_results)?)
}

pub fn search_and_add_single(
    auth_token: &str,
    mpn: &str,
    workspace_root: &std::path::Path,
) -> Result<()> {
    let filtered_results = search_and_filter(auth_token, mpn)?;

    if filtered_results.is_empty() {
        println!("No results found with ECAD data available.");
        return Ok(());
    }

    if filtered_results.len() != 1 {
        println!(
            "{} Found {} components matching '{}'",
            "!".yellow().bold(),
            filtered_results.len(),
            mpn.cyan()
        );
        println!("\nMultiple components found. Please use interactive mode:");
        println!("  {} search {}", "pcb".bold().green(), mpn);
        anyhow::bail!("Multiple components found. Use interactive mode.");
    }

    let component = &filtered_results[0];
    println!(
        "{} Found exactly one component: {}",
        "✓".green().bold(),
        component.part_number.bold()
    );

    let result = add_component_to_workspace(
        auth_token,
        &component.component_id,
        &component.part_number,
        workspace_root,
    )?;

    if handle_already_exists(workspace_root, &result) {
        return Ok(());
    }

    show_component_added(component, workspace_root, &result);
    Ok(())
}

#[derive(Args, Debug)]
#[command(about = "Search for electronic components")]
pub struct SearchArgs {
    pub part_number: String,

    #[arg(long)]
    pub json: bool,

    #[arg(long)]
    pub add: bool,
}

pub fn execute(args: SearchArgs) -> Result<()> {
    let token = crate::auth::get_valid_token()?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let workspace_root =
        pcb_zen_core::config::get_workspace_info(&pcb_zen_core::DefaultFileProvider::new(), &cwd)
            .map(|info| info.root)
            .unwrap_or(cwd);

    if args.json {
        println!("{}", search_json(&token, &args.part_number)?);
    } else if args.add {
        search_and_add_single(&token, &args.part_number, &workspace_root)?;
    } else {
        search_interactive(&token, &args.part_number, &workspace_root)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_mpn_for_path_basic() {
        // Normal alphanumeric MPNs pass through unchanged
        assert_eq!(sanitize_mpn_for_path("STM32F407VGT6"), "STM32F407VGT6");
        assert_eq!(sanitize_mpn_for_path("TPS82140"), "TPS82140");
        assert_eq!(sanitize_mpn_for_path("LM358"), "LM358");
    }

    #[test]
    fn test_sanitize_mpn_for_path_punctuation() {
        // ASCII punctuation replaced with underscore
        assert_eq!(sanitize_mpn_for_path("PESD2CAN,215"), "PESD2CAN_215");
        assert_eq!(sanitize_mpn_for_path("IC@123#456"), "IC_123_456");
        assert_eq!(
            sanitize_mpn_for_path("Part.Number:Test"),
            "Part_Number_Test"
        );
        assert_eq!(
            sanitize_mpn_for_path("Part/Number\\Test"),
            "Part_Number_Test"
        );
        assert_eq!(sanitize_mpn_for_path("Device(123)"), "Device_123");
        assert_eq!(sanitize_mpn_for_path("IC[5V]"), "IC_5V");
        assert_eq!(sanitize_mpn_for_path("Part;Number"), "Part_Number");
        assert_eq!(sanitize_mpn_for_path("Part'Number"), "Part_Number");
        assert_eq!(sanitize_mpn_for_path("Part\"Number"), "Part_Number");

        // Real-world examples from user
        assert_eq!(sanitize_mpn_for_path("AT86RF212B.ZU"), "AT86RF212B_ZU");
        assert_eq!(sanitize_mpn_for_path("MC34063A/D"), "MC34063A_D");
        assert_eq!(sanitize_mpn_for_path("Part#123@Test"), "Part_123_Test");
    }

    #[test]
    fn test_sanitize_mpn_for_path_spaces() {
        // Spaces become underscores
        assert_eq!(sanitize_mpn_for_path("TPS82140 SILR"), "TPS82140_SILR");
        assert_eq!(
            sanitize_mpn_for_path("Part Number Test"),
            "Part_Number_Test"
        );

        // Multiple spaces collapse to single underscore
        assert_eq!(sanitize_mpn_for_path("Part  Number"), "Part_Number");

        // Leading/trailing spaces trimmed
        assert_eq!(sanitize_mpn_for_path("   spaces   "), "spaces");
        assert_eq!(sanitize_mpn_for_path(" Part "), "Part");
    }

    #[test]
    fn test_sanitize_mpn_for_path_hyphens_underscores() {
        // Hyphens and underscores are preserved as safe characters
        assert_eq!(sanitize_mpn_for_path("STM32-F407"), "STM32-F407");
        assert_eq!(sanitize_mpn_for_path("IC_123"), "IC_123");
        assert_eq!(sanitize_mpn_for_path("Part-Name_123"), "Part-Name_123");

        // Multiple consecutive underscores collapse (hyphens preserved)
        assert_eq!(sanitize_mpn_for_path("Part---Test"), "Part---Test");
        assert_eq!(sanitize_mpn_for_path("Part___Test"), "Part_Test");
        assert_eq!(sanitize_mpn_for_path("Part-_-Test"), "Part-_-Test");
    }

    #[test]
    fn test_sanitize_mpn_for_path_unicode() {
        // Unicode characters are transliterated
        assert_eq!(
            sanitize_mpn_for_path("µController-2000"),
            "uController-2000"
        );
        assert_eq!(sanitize_mpn_for_path("αβγ-Component"), "abg-Component");
        assert_eq!(sanitize_mpn_for_path("Ω-Resistor"), "O-Resistor");

        // Cyrillic
        assert_eq!(sanitize_mpn_for_path("Микроконтроллер"), "Mikrokontroller");

        // Chinese (transliterates - exact output depends on deunicode library)
        let chinese_result = sanitize_mpn_for_path("电阻器");
        assert!(!chinese_result.is_empty());
        assert_eq!(chinese_result.chars().all(|c| c.is_ascii()), true);

        // Accented characters
        assert_eq!(sanitize_mpn_for_path("Café-IC"), "Cafe-IC");
        assert_eq!(sanitize_mpn_for_path("Résistance"), "Resistance");

        // Real-world examples from user
        assert_eq!(
            sanitize_mpn_for_path("Würth Elektronik"),
            "Wurth_Elektronik"
        );

        // Trademark symbol transliterates (deunicode produces "tm" without parens)
        assert_eq!(sanitize_mpn_for_path("LM358™"), "LM358tm");
    }

    #[test]
    fn test_sanitize_mpn_for_path_mixed() {
        // Complex real-world examples
        assert_eq!(
            sanitize_mpn_for_path("TPS82140SILR (Texas Instruments)"),
            "TPS82140SILR_Texas_Instruments"
        );
        assert_eq!(
            sanitize_mpn_for_path("µPD78F0730GB-GAH-AX"),
            "uPD78F0730GB-GAH-AX"
        );
        assert_eq!(sanitize_mpn_for_path("AT91SAM7S256-AU"), "AT91SAM7S256-AU");
    }

    #[test]
    fn test_sanitize_mpn_for_path_edge_cases() {
        // Empty string falls back to "component"
        assert_eq!(sanitize_mpn_for_path(""), "component");

        // Only punctuation falls back to "component"
        assert_eq!(sanitize_mpn_for_path("!!!"), "component");
        assert_eq!(sanitize_mpn_for_path("@#$%"), "component");
        assert_eq!(sanitize_mpn_for_path("..."), "component");

        // Only spaces falls back to "component"
        assert_eq!(sanitize_mpn_for_path("     "), "component");

        // Single character
        assert_eq!(sanitize_mpn_for_path("A"), "A");
        assert_eq!(sanitize_mpn_for_path("1"), "1");
        assert_eq!(sanitize_mpn_for_path(","), "component");
    }

    #[test]
    fn test_sanitize_mpn_for_path_numbers() {
        // Numbers are allowed anywhere (no special handling)
        assert_eq!(sanitize_mpn_for_path("123-456"), "123-456");
        assert_eq!(sanitize_mpn_for_path("9000IC"), "9000IC");
        assert_eq!(sanitize_mpn_for_path("0805"), "0805");
    }

    #[test]
    fn test_sanitize_mpn_for_path_case_preserved() {
        // Case is preserved
        assert_eq!(sanitize_mpn_for_path("STM32f407"), "STM32f407");
        assert_eq!(sanitize_mpn_for_path("AbCdEf"), "AbCdEf");
        assert_eq!(sanitize_mpn_for_path("lowercase"), "lowercase");
        assert_eq!(sanitize_mpn_for_path("UPPERCASE"), "UPPERCASE");
    }

    #[test]
    fn test_embed_step_in_footprint_basic() {
        // Test adding a model to a footprint with no existing model
        let footprint = r#"(footprint "Test"
	(layer "F.Cu")
	(pad "1" smd rect
		(at 0 0)
		(size 1 1)
		(layers "F.Cu")
	)
)"#;
        let step_data = b"STEP DATA HERE".to_vec();
        let result =
            embed_step_in_footprint(footprint.to_string(), step_data, "test.step").unwrap();

        // Verify balanced parentheses
        assert_eq!(
            result.chars().filter(|&c| c == '(').count(),
            result.chars().filter(|&c| c == ')').count(),
        );

        insta::assert_snapshot!(result);
    }

    #[test]
    fn test_embed_step_in_footprint_preserves_transforms() {
        // Test that we preserve existing offset/scale/rotate when replacing the model path
        let footprint = r#"(footprint "Test"
	(layer "F.Cu")
	(pad "1" smd rect
		(at 0 0)
		(size 1 1)
		(layers "F.Cu")
	)
	(model "old-model.step"
		(offset
			(xyz 1 2 3)
		)
		(scale
			(xyz 2 2 2)
		)
		(rotate
			(xyz 45 90 180)
		)
	)
)"#;
        let step_data = b"NEW STEP DATA".to_vec();
        let result = embed_step_in_footprint(footprint.to_string(), step_data, "new.step").unwrap();

        // Verify the transforms were preserved
        assert!(result.contains("(xyz 1 2 3)"));
        assert!(result.contains("(xyz 2 2 2)"));
        assert!(result.contains("(xyz 45 90 180)"));

        // Verify balanced parentheses
        assert_eq!(
            result.chars().filter(|&c| c == '(').count(),
            result.chars().filter(|&c| c == ')').count(),
        );

        insta::assert_snapshot!(result);
    }
}
