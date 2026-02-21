use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use clap::Args;
use colored::Colorize;
use indicatif::ProgressBar;
use inquire::{Select, Text};
use pcb_sexpr::PatchSet;
use pcb_sexpr::formatter::{FormatMode, prettify};
use pcb_zen_core::config::find_workspace_root;
use regex::Regex;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use walkdir::WalkDir;

pub use pcb_component_gen::sanitize_mpn_for_path;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelAvailability {
    #[serde(rename = "ECAD_model", default)]
    pub ecad_model: bool,
    #[serde(rename = "STEP_model", default)]
    pub step_model: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentSearchResult {
    pub part_number: String,
    pub manufacturer: Option<String>,
    pub description: Option<String>,
    pub package_category: Option<String>,
    pub component_id: String,
    #[serde(default)]
    pub datasheets: Vec<String>,
    #[serde(default)]
    pub model_availability: ModelAvailability,
    pub source: Option<String>,
    /// Search relevance score (if provided by API)
    #[serde(default)]
    pub score: Option<f64>,
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
    pub datasheet_source_path: Option<String>,
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
    #[serde(rename = "datasheetSourcePath")]
    datasheet_source_path: Option<String>,
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
            datasheet_source_path: download_response.metadata.datasheet_source_path,
            file_hashes: download_response.metadata.file_hashes,
        },
    })
}

pub fn download_file(url: &str, output_path: &Path) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(format!("diode-pcb/{}", env!("CARGO_PKG_VERSION")))
        .build()?;

    let response = client.get(url).send()?;

    if !response.status().is_success() {
        anyhow::bail!("File download failed: {} - URL: {}", response.status(), url);
    }

    let bytes = response.bytes()?;

    // Normalize line endings for text files (KiCad files)
    if let Some(ext) = output_path.extension().and_then(|e| e.to_str())
        && matches!(
            ext,
            "kicad_sym" | "kicad_mod" | "kicad_pcb" | "kicad_sch" | "kicad_pro"
        )
    {
        let text = String::from_utf8_lossy(&bytes);
        let normalized = text.replace("\r\n", "\n");
        std::fs::write(output_path, normalized.as_bytes())?;
        return Ok(());
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
    let mut encoder = zstd::Encoder::new(Vec::new(), 15)?;
    encoder.include_contentsize(true)?;
    encoder.set_pledged_src_size(Some(step_bytes.len() as u64))?;
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
    if !text.contains("(embedded_files")
        && let Some(pos) = text.rfind(')')
    {
        text.insert_str(pos, &embed_block);
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

/// Embed a STEP file into a footprint file, writing the result atomically.
/// Optionally deletes the standalone STEP file after embedding.
fn embed_step_into_footprint_file(
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

    // Normalize line endings, format as KiCad S-expression, then write atomically.
    let normalized_content = embedded_content.replace("\r\n", "\n");
    let formatted_content = format_kicad_sexpr_source(&normalized_content, footprint_path)?;
    AtomicFile::new(footprint_path, OverwriteBehavior::AllowOverwrite)
        .write(|f| {
            f.write_all(formatted_content.as_bytes())?;
            f.flush()
        })
        .map_err(|err| anyhow::anyhow!("Failed to write footprint file: {err}"))?;

    // Optionally delete standalone STEP file
    if delete_step {
        fs::remove_file(step_path).context("Failed to remove standalone STEP file")?;
    }

    Ok(())
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
    eprintln!(
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
    eprintln!(
        "{} Added {} to {}",
        "✓".green().bold(),
        component.part_number.bold(),
        display_path.display().to_string().cyan()
    );
}

pub fn add_component_to_workspace(
    auth_token: &str,
    component_id: &str,
    part_number: &str,
    workspace_root: &std::path::Path,
    search_manufacturer: Option<&str>,
    scan_model: Option<crate::scan::ScanModel>,
) -> Result<AddComponentResult> {
    // Show progress during API call (use stderr for MCP compatibility)
    let spinner = ProgressBar::with_draw_target(None, indicatif::ProgressDrawTarget::stderr());
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    spinner.set_message(format!("Fetching {}...", part_number));

    let download = download_component(auth_token, component_id)?;
    spinner.finish_and_clear();

    let manufacturer = search_manufacturer;
    let component_dir = component_dir_path(workspace_root, manufacturer, part_number);
    let sanitized_mpn = pcb_component_gen::sanitize_mpn_for_path(part_number);
    let zen_file = component_dir.join(format!("{}.zen", &sanitized_mpn));

    if zen_file.exists() {
        return Ok(AddComponentResult {
            component_path: zen_file,
            already_exists: true,
        });
    }

    fs::create_dir_all(&component_dir)?;

    // Track which files will be downloaded
    let has_symbol = download.symbol_url.is_some() && download.metadata.symbol_filename.is_some();
    let has_footprint =
        download.footprint_url.is_some() && download.metadata.footprint_filename.is_some();
    let has_step = download.step_url.is_some() && download.metadata.step_filename.is_some();
    let has_datasheet =
        download.datasheet_url.is_some() && download.metadata.datasheet_filename.is_some();

    // Collect download tasks
    let mut download_tasks = Vec::new();
    let mut upgrade_tasks = Vec::new();
    let mut scan_tasks: Vec<(String, String)> = Vec::new(); // (storage_path, filename)

    if has_symbol {
        let path = component_dir.join(format!("{}.kicad_sym", &sanitized_mpn));
        download_tasks.push((download.symbol_url.clone().unwrap(), path.clone(), "symbol"));
        upgrade_tasks.push(("symbol", path));
    }
    if has_footprint {
        let path = component_dir.join(format!("{}.kicad_mod", &sanitized_mpn));
        download_tasks.push((
            download.footprint_url.clone().unwrap(),
            path.clone(),
            "footprint",
        ));
        upgrade_tasks.push(("footprint", path));
    }
    if has_step {
        let path = component_dir.join(format!("{}.step", &sanitized_mpn));
        download_tasks.push((download.step_url.clone().unwrap(), path, "step"));
    }
    if has_datasheet {
        let url = download.datasheet_url.as_ref().unwrap();
        let path = component_dir.join(format!("{}.pdf", &sanitized_mpn));
        download_tasks.push((url.clone(), path.clone(), "datasheet"));

        // Queue datasheet for scanning if .md doesn't exist
        if let Some(source_path) = &download.metadata.datasheet_source_path
            && !path.with_extension("md").exists()
        {
            scan_tasks.push((source_path.clone(), format!("{}.pdf", &sanitized_mpn)));
        }
    }

    let file_count = download_tasks.len();
    let scan_count = scan_tasks.len();

    // Show task summary (use stderr for MCP compatibility)
    eprintln!("{} {}", "Downloading".green().bold(), part_number.bold());
    eprintln!(
        "• {} files{}",
        file_count,
        if scan_count > 0 {
            format!(", {} datasheets to scan", scan_count)
        } else {
            String::new()
        }
    );

    let start = std::time::Instant::now();

    // Execute all tasks in parallel with progress indicator (use stderr for MCP compatibility)
    let spinner = ProgressBar::with_draw_target(None, indicatif::ProgressDrawTarget::stderr());
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
                    scan_model,
                    true,  // images
                    false, // json
                    false, // show_output
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

    // Show results (use stderr for MCP compatibility)
    if errors.is_empty() {
        eprintln!(
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
        eprintln!(
            "  {} Completed with {} errors ({:.1}s)",
            "!".yellow(),
            errors.len(),
            elapsed.as_secs_f64()
        );
        for err in &errors {
            eprintln!("    • {}", err.dimmed());
        }
    }

    // Finalize: embed STEP, generate .zen file
    finalize_component(&component_dir, part_number, manufacturer)?;

    Ok(AddComponentResult {
        component_path: zen_file,
        already_exists: false,
    })
}

pub struct AddComponentResult {
    pub component_path: PathBuf,
    pub already_exists: bool,
}

/// Build component directory path: components/<manufacturer>/<mpn>/
fn component_dir_path(workspace_root: &Path, manufacturer: Option<&str>, mpn: &str) -> PathBuf {
    let sanitized_mfr = manufacturer
        .map(pcb_component_gen::sanitize_mpn_for_path)
        .unwrap_or_else(|| "unknown".to_string());
    let sanitized_mpn = pcb_component_gen::sanitize_mpn_for_path(mpn);
    workspace_root
        .join("components")
        .join(sanitized_mfr)
        .join(sanitized_mpn)
}

/// Embed STEP into footprint (if both exist) and generate .zen file
fn finalize_component(component_dir: &Path, mpn: &str, manufacturer: Option<&str>) -> Result<()> {
    let sanitized_mpn = pcb_component_gen::sanitize_mpn_for_path(mpn);
    let symbol_path = component_dir.join(format!("{}.kicad_sym", &sanitized_mpn));
    let footprint_path = component_dir.join(format!("{}.kicad_mod", &sanitized_mpn));
    let step_path = component_dir.join(format!("{}.step", &sanitized_mpn));
    let datasheet_path = component_dir.join(format!("{}.pdf", &sanitized_mpn));

    if footprint_path.exists() {
        if step_path.exists() {
            embed_step_into_footprint_file(&footprint_path, &step_path, true)?;
        } else {
            format_kicad_sexpr_file(&footprint_path)?;
        }
    }

    if !symbol_path.exists() {
        return Ok(());
    }

    let mut symbol_source = fs::read_to_string(&symbol_path)
        .with_context(|| format!("Failed to read KiCad symbol {}", symbol_path.display()))?;

    if footprint_path.exists() {
        let footprint_stem = footprint_path
            .file_stem()
            .ok_or_else(|| anyhow::anyhow!("Footprint path missing file stem"))?
            .to_string_lossy()
            .to_string();
        symbol_source = rewrite_symbol_footprint_property_text(&symbol_source, &footprint_stem)?;
    }

    let symbol_formatted = format_kicad_sexpr_source(&symbol_source, &symbol_path)?;
    fs::write(&symbol_path, &symbol_formatted)
        .with_context(|| format!("Failed to write KiCad symbol {}", symbol_path.display()))?;

    // Generate .zen file from the exact symbol content we just wrote.
    let symbol_lib = pcb_eda::SymbolLibrary::from_string(&symbol_formatted, "kicad_sym")?;
    let symbol = only_symbol_in_library(&symbol_lib, &symbol_path)?;

    let content = generate_zen_file(
        mpn,
        &sanitized_mpn,
        symbol,
        &format!("{}.kicad_sym", &sanitized_mpn),
        footprint_path
            .exists()
            .then(|| format!("{}.kicad_mod", &sanitized_mpn))
            .as_deref(),
        datasheet_path
            .exists()
            .then(|| format!("{}.pdf", &sanitized_mpn))
            .as_deref(),
        manufacturer,
    )?;

    let zen_file = component_dir.join(format!("{}.zen", &sanitized_mpn));
    write_component_files(&zen_file, component_dir, &content)?;

    Ok(())
}

fn only_symbol_in_library<'a>(
    symbol_lib: &'a pcb_eda::SymbolLibrary,
    symbol_path: &Path,
) -> Result<&'a pcb_eda::Symbol> {
    let symbols = symbol_lib.symbols();
    match symbols {
        [symbol] => Ok(symbol),
        [] => anyhow::bail!(
            "Expected exactly one symbol in {}, found none",
            symbol_path.display()
        ),
        _ => {
            let names = symbol_lib.symbol_names().join(", ");
            anyhow::bail!(
                "Expected exactly one symbol in {}, found {}: {}",
                symbol_path.display(),
                symbols.len(),
                names
            )
        }
    }
}

fn rewrite_symbol_footprint_property_text(source: &str, footprint_ref: &str) -> Result<String> {
    let parsed = pcb_sexpr::parse(source).map_err(|e| anyhow::anyhow!(e))?;
    let mut patches = PatchSet::new();

    parsed.walk(|node, _ctx| {
        let Some(items) = node.as_list() else {
            return;
        };

        let is_footprint_property = items.first().and_then(|n| n.as_sym()) == Some("property")
            && items.get(1).and_then(|n| n.as_str().or_else(|| n.as_sym())) == Some("Footprint");
        if !is_footprint_property {
            return;
        }

        let Some(value_node) = items.get(2) else {
            return;
        };
        let current = value_node.as_str().or_else(|| value_node.as_sym());
        if current != Some(footprint_ref) {
            patches.replace_string(value_node.span, footprint_ref);
        }
    });

    let mut out = Vec::new();
    patches
        .write_to(source, &mut out)
        .context("Failed to apply Footprint property patch")?;
    let updated = String::from_utf8(out).context("Patched symbol is not valid UTF-8")?;
    Ok(updated)
}

fn format_kicad_sexpr_source(source: &str, path_for_error: &Path) -> Result<String> {
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

fn format_kicad_sexpr_file(path: &Path) -> Result<()> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("Failed to read KiCad file {}", path.display()))?;
    let formatted = format_kicad_sexpr_source(&source, path)?;
    fs::write(path, formatted)
        .with_context(|| format!("Failed to write KiCad file {}", path.display()))?;

    Ok(())
}

/// Write a .zen file and create an empty pcb.toml in the component directory
fn write_component_files(component_file: &Path, component_dir: &Path, content: &str) -> Result<()> {
    // Format the content before writing
    let formatter = pcb_fmt::RuffFormatter::default();
    let formatted_content = formatter
        .format_source(content)
        .unwrap_or_else(|_| content.to_string());

    fs::write(component_file, formatted_content)?;

    let toml_path = component_dir.join("pcb.toml");
    if !toml_path.exists() {
        fs::write(&toml_path, "")?;
    }
    Ok(())
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
    pcb_component_gen::generate_component_zen(pcb_component_gen::GenerateComponentZenArgs {
        mpn,
        component_name,
        symbol,
        symbol_filename,
        footprint_filename,
        datasheet_filename,
        manufacturer,
        generated_by: "pcb search",
        include_skip_bom: false,
        include_skip_pos: false,
        skip_bom_default: false,
        skip_pos_default: false,
        pin_defs: None,
    })
}

#[derive(clap::ValueEnum, Debug, Clone, Default)]
pub enum SearchOutputFormat {
    #[default]
    Human,
    Json,
}

#[derive(Args, Debug)]
#[command(about = "Search for electronic components")]
pub struct SearchArgs {
    /// Search query (MPN, description, keywords)
    pub query: Option<String>,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value_t = SearchOutputFormat::Human)]
    pub format: SearchOutputFormat,

    /// Search mode to launch TUI in
    /// Default: registry:modules if registry access available, web:components otherwise
    #[arg(short = 'm', long, value_enum)]
    pub mode: Option<crate::registry::tui::SearchMode>,

    /// Generate .zen from local directory instead of search
    #[arg(long = "dir", value_name = "DIR", conflicts_with = "format")]
    pub dir: Option<PathBuf>,

    /// Model to use for datasheet scanning
    #[arg(
        long = "scan-model",
        value_enum,
        default_value = "mistral-ocr-2512",
        hide = true
    )]
    pub scan_model: crate::scan::ScanModelArg,
}

/// Files discovered in a local directory for component generation
struct DiscoveredFiles {
    symbols: Vec<PathBuf>,
    /// Backup symbol files (*.orig.kicad_sym) - excluded from selection but carried over
    orig_symbols: Vec<PathBuf>,
    footprints: Vec<PathBuf>,
    pdfs: Vec<PathBuf>,
    steps: Vec<PathBuf>,
}

/// Check if a path ends with .orig.kicad_sym
fn is_orig_symbol(path: &Path) -> bool {
    path.to_str()
        .map(|s| s.ends_with(".orig.kicad_sym"))
        .unwrap_or(false)
}

/// Recursively discover relevant files in a directory for component generation
fn discover_files_recursive(dir: &Path) -> Result<DiscoveredFiles> {
    let mut symbols = Vec::new();
    let mut orig_symbols = Vec::new();
    let mut footprints = Vec::new();
    let mut pdfs = Vec::new();
    let mut steps = Vec::new();

    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            match ext.to_lowercase().as_str() {
                "kicad_sym" => {
                    if is_orig_symbol(path) {
                        orig_symbols.push(entry.into_path());
                    } else {
                        symbols.push(entry.into_path());
                    }
                }
                "kicad_mod" => footprints.push(entry.into_path()),
                "pdf" => pdfs.push(entry.into_path()),
                "step" | "stp" | "wrl" => steps.push(entry.into_path()),
                _ => {}
            }
        }
    }

    // Sort for consistent ordering
    symbols.sort();
    orig_symbols.sort();
    footprints.sort();
    pdfs.sort();
    steps.sort();

    Ok(DiscoveredFiles {
        symbols,
        orig_symbols,
        footprints,
        pdfs,
        steps,
    })
}

/// Prompt user to select a symbol file if multiple are found
fn select_symbol(symbols: Vec<PathBuf>) -> Result<PathBuf> {
    if symbols.is_empty() {
        anyhow::bail!("No .kicad_sym files found in directory");
    }

    if symbols.len() == 1 {
        return Ok(symbols.into_iter().next().unwrap());
    }

    let items: Vec<String> = symbols
        .iter()
        .map(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
        })
        .collect();

    let selection = Select::new("Select a symbol file:", items)
        .with_formatter(&|_| String::new())
        .prompt()
        .context("Failed to get symbol selection")?;

    // Find the matching path
    symbols
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == selection)
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow::anyhow!("Selected symbol not found"))
}

/// Helper to get filename as &str from a path
fn path_filename(path: &Path) -> &str {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
}

/// Copy a file to the working directory with a new name, returning the new path
fn copy_file_to_dir(src: &Path, workdir: &Path, dest_filename: &str) -> Result<PathBuf> {
    let dest = workdir.join(dest_filename);
    fs::copy(src, &dest).with_context(|| format!("Failed to copy {}", src.display()))?;
    Ok(dest)
}

/// Generate a .zen component file from a local directory containing KiCad files.
/// Recursively searches for symbols, footprints, 3D models, and datasheets,
/// then installs the component to the current workspace's components directory.
fn execute_from_dir(dir: &Path, workspace_root: &Path) -> Result<()> {
    if !dir.is_dir() {
        anyhow::bail!("Path is not a directory: {}", dir.display());
    }

    eprintln!(
        "{} Discovering files recursively in {}",
        "→".blue().bold(),
        dir.display()
    );
    let files = discover_files_recursive(dir)?;

    if files.symbols.is_empty() {
        anyhow::bail!("No .kicad_sym files found in directory or subdirectories");
    }

    // Show discovered files
    eprintln!(
        "  Found {} symbol(s), {} footprint(s), {} 3D model(s), {} datasheet(s)",
        files.symbols.len(),
        files.footprints.len(),
        files.steps.len(),
        files.pdfs.len()
    );

    // Select symbol (prompts if multiple)
    let selected_symbol = select_symbol(files.symbols)?;
    eprintln!(
        "  {} Symbol: {}",
        "✓".green(),
        selected_symbol.display().to_string().cyan()
    );

    // Parse symbol to extract MPN and manufacturer
    let symbol_lib = pcb_eda::SymbolLibrary::from_file(&selected_symbol)
        .context("Failed to parse symbol file")?;
    let symbol = only_symbol_in_library(&symbol_lib, &selected_symbol)?;

    // Best-effort defaults from symbol, fall back to directory structure
    let default_mpn = if !symbol.name.is_empty() {
        symbol.name.clone()
    } else {
        dir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("component")
            .to_string()
    };
    let default_mfr = symbol.manufacturer.clone().unwrap_or_else(|| {
        // Fall back to parent directory name (e.g., .../components/SHOUHAN/TYPE-C24PQT -> SHOUHAN)
        // but not if parent is "components"
        dir.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .filter(|&name| name != "components")
            .unwrap_or("")
            .to_string()
    });

    // Prompt user to confirm/edit MPN and manufacturer
    let mpn = Text::new("MPN:")
        .with_default(&default_mpn)
        .prompt()
        .context("Failed to get MPN")?;

    let manufacturer_input = Text::new("Manufacturer:")
        .with_default(&default_mfr)
        .prompt()
        .context("Failed to get manufacturer")?;
    let manufacturer = if manufacturer_input.is_empty() {
        None
    } else {
        Some(manufacturer_input)
    };

    let component_dir = component_dir_path(workspace_root, manufacturer.as_deref(), &mpn);
    let sanitized_mpn = pcb_component_gen::sanitize_mpn_for_path(&mpn);
    let zen_file = component_dir.join(format!("{}.zen", &sanitized_mpn));

    // Check if component already exists
    if zen_file.exists() {
        let display_path = zen_file.strip_prefix(workspace_root).unwrap_or(&zen_file);
        println!(
            "{} Component already exists at: {}",
            "ℹ".blue().bold(),
            display_path.display().to_string().cyan()
        );
        return Ok(());
    }

    fs::create_dir_all(&component_dir)?;

    println!(
        "{} Copying files to component directory...",
        "→".blue().bold()
    );

    // Copy the selected symbol with standardized name
    let sym_filename = format!("{}.kicad_sym", &sanitized_mpn);
    copy_file_to_dir(&selected_symbol, &component_dir, &sym_filename)?;
    println!(
        "  {} Symbol: {} → {}",
        "✓".green(),
        path_filename(&selected_symbol).dimmed(),
        sym_filename.cyan()
    );

    // Copy backup symbol files (*.orig.kicad_sym)
    for orig_sym in &files.orig_symbols {
        let orig_filename = format!("{}.orig.kicad_sym", &sanitized_mpn);
        copy_file_to_dir(orig_sym, &component_dir, &orig_filename)?;
        println!(
            "  {} Backup: {} → {}",
            "✓".green(),
            path_filename(orig_sym).dimmed(),
            orig_filename.cyan()
        );
    }

    // Copy first footprint if available
    let has_footprint = !files.footprints.is_empty();
    if let Some(fp) = files.footprints.first() {
        let fp_filename = format!("{}.kicad_mod", &sanitized_mpn);
        copy_file_to_dir(fp, &component_dir, &fp_filename)?;
        println!(
            "  {} Footprint: {} → {}",
            "✓".green(),
            path_filename(fp).dimmed(),
            fp_filename.cyan()
        );
    }

    // Copy first STEP file if available
    if let Some(sp) = files.steps.first() {
        let step_filename = format!("{}.step", &sanitized_mpn);
        copy_file_to_dir(sp, &component_dir, &step_filename)?;
        println!(
            "  {} 3D Model: {} → {}",
            "✓".green(),
            path_filename(sp).dimmed(),
            step_filename.cyan()
        );
    }

    // Copy first PDF with standardized name
    let has_datasheet = !files.pdfs.is_empty();
    if let Some(pdf) = files.pdfs.first() {
        let pdf_filename = format!("{}.pdf", &sanitized_mpn);
        copy_file_to_dir(pdf, &component_dir, &pdf_filename)?;
        println!(
            "  {} Datasheet: {} → {}",
            "✓".green(),
            path_filename(pdf).dimmed(),
            pdf_filename.cyan()
        );
    }

    // Upgrade files
    println!("{} Upgrading files...", "→".blue().bold());
    let symbol_path = component_dir.join(format!("{}.kicad_sym", &sanitized_mpn));
    if let Err(e) = upgrade_symbol(&symbol_path) {
        println!("  {} Symbol upgrade skipped: {}", "!".yellow(), e);
    }
    if has_footprint {
        let footprint_path = component_dir.join(format!("{}.kicad_mod", &sanitized_mpn));
        if let Err(e) = upgrade_footprint(&footprint_path) {
            println!("  {} Footprint upgrade skipped: {}", "!".yellow(), e);
        }
    }

    // Scan datasheet (requires auth)
    if has_datasheet {
        println!("{} Scanning datasheet...", "→".blue().bold());
        let datasheet_path = component_dir.join(format!("{}.pdf", &sanitized_mpn));
        let token = crate::auth::get_valid_token()?;
        match crate::scan::scan_with_defaults(
            &token,
            datasheet_path,
            Some(component_dir.clone()),
            None,
            true,  // images
            false, // json
        ) {
            Ok(r) => println!("  {} ({} pages)", "✓".green(), r.page_count),
            Err(e) => println!("  {} scan failed: {}", "✗".red(), e),
        }
    }

    // Finalize: embed STEP, generate .zen file
    println!("{} Generating .zen file...", "→".blue().bold());
    finalize_component(&component_dir, &mpn, manufacturer.as_deref())?;

    // Show result
    let display_path = zen_file.strip_prefix(workspace_root).unwrap_or(&zen_file);
    println!(
        "\n{} Added {} to {}",
        "✓".green().bold(),
        mpn.bold(),
        display_path.display().to_string().cyan()
    );
    Ok(())
}

pub fn execute(args: SearchArgs) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let workspace_root = find_workspace_root(&pcb_zen_core::DefaultFileProvider::new(), &cwd)?;

    // Handle --dir mode (local directory)
    if let Some(ref dir) = args.dir {
        return execute_from_dir(dir, &workspace_root);
    }

    // Search mode (local registry database with TUI or API)
    let query = args.query.as_deref().unwrap_or("");
    let scan_model = Some(crate::scan::ScanModel::from(args.scan_model));
    let json = matches!(args.format, SearchOutputFormat::Json);
    execute_search(query, json, &workspace_root, scan_model, args.mode)
}

/// Handle a selected component from the TUI - download and add to workspace
fn handle_tui_component_selection(
    component: ComponentSearchResult,
    workspace_root: &Path,
    scan_model: Option<crate::scan::ScanModel>,
) -> Result<()> {
    println!(
        "{} {}",
        "Selected:".green().bold(),
        component.part_number.bold()
    );
    if let Some(ref description) = component.description {
        println!("{} {}", "Description:".cyan(), description);
    }

    let token = crate::auth::get_valid_token()?;
    let result = add_component_to_workspace(
        &token,
        &component.component_id,
        &component.part_number,
        workspace_root,
        component.manufacturer.as_deref(),
        scan_model,
    )?;

    if handle_already_exists(workspace_root, &result) {
        return Ok(());
    }

    show_component_added(&component, workspace_root, &result);
    Ok(())
}

/// Execute the component search TUI in WebComponents mode only (no registry access)
pub fn execute_web_components_tui(
    workspace_root: &Path,
    scan_model: Option<crate::scan::ScanModel>,
) -> Result<()> {
    let tui_result = crate::registry::tui::run_web_components_only()?;
    if let Some(component) = tui_result.selected_component {
        handle_tui_component_selection(component, workspace_root, scan_model)?;
    }
    Ok(())
}

fn execute_search(
    query: &str,
    json: bool,
    workspace_root: &Path,
    scan_model: Option<crate::scan::ScanModel>,
    mode: Option<crate::registry::tui::SearchMode>,
) -> Result<()> {
    use crate::registry::tui::SearchMode;

    // If no query provided, launch interactive TUI
    if query.is_empty() {
        let tui_result = crate::registry::tui::run_with_mode(mode)?;
        if let Some(component) = tui_result.selected_component {
            handle_tui_component_selection(component, workspace_root, scan_model)?;
        }
        return Ok(());
    }

    // Determine effective mode
    let effective_mode = mode.unwrap_or_else(|| {
        // Default: registry:modules if registry available, else web:components
        if crate::RegistryClient::open().is_ok() {
            SearchMode::RegistryModules
        } else {
            SearchMode::WebComponents
        }
    });

    match effective_mode {
        SearchMode::RegistryModules | SearchMode::RegistryComponents => {
            execute_registry_search_filtered(query, json, effective_mode)
        }
        SearchMode::WebComponents => execute_web_search(query, json),
    }
}

fn execute_registry_search_filtered(
    query: &str,
    json: bool,
    mode: crate::registry::tui::SearchMode,
) -> Result<()> {
    use crate::registry::tui::search::RegistryResultDisplay;

    let client = crate::RegistryClient::open()?;
    let filter = mode.search_filter();
    let is_modules_mode = mode == crate::registry::tui::SearchMode::RegistryModules;
    let is_components_mode = mode == crate::registry::tui::SearchMode::RegistryComponents;

    // Use search_filtered with RRF merging (same algorithm as TUI) for consistent results
    let results = client.search_filtered(query, 25, filter)?;

    if results.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("{} No results found for '{}'", "✗".red(), query);
        }
        return Ok(());
    }

    // Fetch availability for components mode
    let availability_map = if is_components_mode {
        crate::bom::fetch_availability_for_results(&results)
    } else {
        std::collections::HashMap::new()
    };

    if json {
        if is_components_mode {
            let combined: Vec<crate::mcp::RegistrySearchResult> = results
                .into_iter()
                .enumerate()
                .map(|(i, part)| crate::mcp::RegistrySearchResult {
                    availability: availability_map.get(&i).cloned(),
                    part,
                    dependencies: Vec::new(),
                    dependents: Vec::new(),
                    cache_path: None,
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&combined)?);
        } else {
            println!("{}", serde_json::to_string_pretty(&results)?);
        }
        return Ok(());
    }

    println!(
        "{} Found {} results for '{}' ({}):\n",
        "✓".green().bold(),
        results.len(),
        query,
        mode.display_name()
    );

    for (i, part) in results.iter().enumerate() {
        let display = RegistryResultDisplay::from_registry(
            &part.url,
            part.version.as_deref(),
            part.package_category.as_deref(),
            part.mpn.as_deref(),
            part.manufacturer.as_deref(),
            part.short_description.as_deref(),
            is_modules_mode,
        );
        for line in display.to_cli_lines() {
            println!("{}", line);
        }
        // Add availability summary line for components mode
        if let Some(p) = availability_map.get(&i) {
            print_availability_summary(p);
        }
    }

    Ok(())
}

/// Component search result with availability data
#[derive(Debug, Clone, Serialize)]
pub struct ComponentResult {
    #[serde(flatten)]
    pub component: ComponentSearchResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<pcb_sch::bom::Availability>,
}

/// Search for components and fetch availability data in batch
pub fn search_components_with_availability(
    auth_token: &str,
    query: &str,
) -> Result<Vec<ComponentResult>> {
    let results = search_components(auth_token, query)?;

    if results.is_empty() {
        return Ok(Vec::new());
    }

    let keys: Vec<_> = results
        .iter()
        .map(|r| crate::bom::ComponentKey {
            mpn: r.part_number.clone(),
            manufacturer: r.manufacturer.clone(),
        })
        .collect();

    let availability_results =
        crate::bom::fetch_pricing_batch(auth_token, &keys).unwrap_or_default();

    let all_availability: Vec<_> = availability_results
        .into_iter()
        .map(|p| {
            if p.us.is_some() || p.global.is_some() || !p.offers.is_empty() {
                Some(p)
            } else {
                None
            }
        })
        .collect();

    let combined: Vec<ComponentResult> = results
        .into_iter()
        .zip(all_availability)
        .map(|(component, availability)| ComponentResult {
            component,
            availability,
        })
        .collect();

    Ok(combined)
}

/// Search web API for components
fn execute_web_search(query: &str, json: bool) -> Result<()> {
    use crate::registry::tui::search::WebComponentDisplay;

    let token = crate::auth::get_valid_token()?;
    let results = search_components_with_availability(&token, query)?;

    if results.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("{} No results found for '{}'", "✗".red(), query);
        }
        return Ok(());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }

    println!(
        "{} Found {} results for '{}' (web:components):\n",
        "✓".green().bold(),
        results.len(),
        query,
    );

    for result in &results {
        let display = WebComponentDisplay::from_component(&result.component);
        for line in display.to_cli_lines() {
            println!("{}", line);
        }
        // Add availability summary line
        if let Some(p) = &result.availability {
            print_availability_summary(p);
        }
        println!(); // Extra line between results
    }

    Ok(())
}

/// Print a compact availability summary line for CLI output
fn print_availability_summary(avail: &pcb_sch::bom::Availability) {
    use crate::bom::{format_number_with_commas, format_price};

    let format_region = |avail: Option<&pcb_sch::bom::AvailabilitySummary>, name: &str| -> String {
        if let Some(a) = avail {
            let stock_str = format_number_with_commas(a.stock);
            let price_str = a.price.map(format_price).unwrap_or_else(|| "—".to_string());
            format!("{}: {} ({})", name, price_str, stock_str)
        } else {
            format!("{}: —", name)
        }
    };

    let global_str = format_region(avail.global.as_ref(), "Global");
    let us_str = format_region(avail.us.as_ref(), "US");

    println!("  {} │ {}", global_str.dimmed(), us_str.dimmed());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_mpn_for_path_basic() {
        // Normal alphanumeric MPNs pass through unchanged
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("STM32F407VGT6"),
            "STM32F407VGT6"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("TPS82140"),
            "TPS82140"
        );
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path("LM358"), "LM358");
    }

    #[test]
    fn test_sanitize_mpn_for_path_punctuation() {
        // ASCII punctuation replaced with underscore
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("PESD2CAN,215"),
            "PESD2CAN_215"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("IC@123#456"),
            "IC_123_456"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part.Number:Test"),
            "Part_Number_Test"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part/Number\\Test"),
            "Part_Number_Test"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Device(123)"),
            "Device_123"
        );
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path("IC[5V]"), "IC_5V");
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part;Number"),
            "Part_Number"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part'Number"),
            "Part_Number"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part\"Number"),
            "Part_Number"
        );

        // Real-world examples from user
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("AT86RF212B.ZU"),
            "AT86RF212B_ZU"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("MC34063A/D"),
            "MC34063A_D"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part#123@Test"),
            "Part_123_Test"
        );
    }

    #[test]
    fn test_sanitize_mpn_for_path_spaces() {
        // Spaces become underscores
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("TPS82140 SILR"),
            "TPS82140_SILR"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part Number Test"),
            "Part_Number_Test"
        );

        // Multiple spaces collapse to single underscore
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part  Number"),
            "Part_Number"
        );

        // Leading/trailing spaces trimmed
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("   spaces   "),
            "spaces"
        );
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path(" Part "), "Part");
    }

    #[test]
    fn test_sanitize_mpn_for_path_hyphens_underscores() {
        // Hyphens and underscores are preserved as safe characters
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("STM32-F407"),
            "STM32-F407"
        );
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path("IC_123"), "IC_123");
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part-Name_123"),
            "Part-Name_123"
        );

        // Multiple consecutive underscores collapse (hyphens preserved)
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part---Test"),
            "Part---Test"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part___Test"),
            "Part_Test"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Part-_-Test"),
            "Part-_-Test"
        );
    }

    #[test]
    fn test_sanitize_mpn_for_path_unicode() {
        // Unicode characters are transliterated
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("µController-2000"),
            "uController-2000"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("αβγ-Component"),
            "abg-Component"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Ω-Resistor"),
            "O-Resistor"
        );

        // Cyrillic
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Микроконтроллер"),
            "Mikrokontroller"
        );

        // Chinese (transliterates - exact output depends on deunicode library)
        let chinese_result = pcb_component_gen::sanitize_mpn_for_path("电阻器");
        assert!(!chinese_result.is_empty());
        assert!(chinese_result.is_ascii());

        // Accented characters
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Café-IC"),
            "Cafe-IC"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Résistance"),
            "Resistance"
        );

        // Real-world examples from user
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("Würth Elektronik"),
            "Wurth_Elektronik"
        );

        // Trademark symbol transliterates (deunicode produces "tm" without parens)
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("LM358™"),
            "LM358tm"
        );
    }

    #[test]
    fn test_sanitize_mpn_for_path_mixed() {
        // Complex real-world examples
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("TPS82140SILR (Texas Instruments)"),
            "TPS82140SILR_Texas_Instruments"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("µPD78F0730GB-GAH-AX"),
            "uPD78F0730GB-GAH-AX"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("AT91SAM7S256-AU"),
            "AT91SAM7S256-AU"
        );
    }

    #[test]
    fn test_sanitize_mpn_for_path_edge_cases() {
        // Empty string falls back to "component"
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path(""), "component");

        // Only punctuation falls back to "component"
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path("!!!"), "component");
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("@#$%"),
            "component"
        );
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path("..."), "component");

        // Only spaces falls back to "component"
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("     "),
            "component"
        );

        // Single character
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path("A"), "A");
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path("1"), "1");
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path(","), "component");
    }

    #[test]
    fn test_sanitize_mpn_for_path_numbers() {
        // Numbers are allowed anywhere (no special handling)
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("123-456"),
            "123-456"
        );
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path("9000IC"), "9000IC");
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path("0805"), "0805");
    }

    #[test]
    fn test_sanitize_mpn_for_path_case_preserved() {
        // Case is preserved
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("STM32f407"),
            "STM32f407"
        );
        assert_eq!(pcb_component_gen::sanitize_mpn_for_path("AbCdEf"), "AbCdEf");
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("lowercase"),
            "lowercase"
        );
        assert_eq!(
            pcb_component_gen::sanitize_mpn_for_path("UPPERCASE"),
            "UPPERCASE"
        );
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

    #[test]
    fn test_sanitize_pin_name_basic() {
        // Basic alphanumeric names get uppercased
        assert_eq!(pcb_component_gen::sanitize_pin_name("VCC"), "VCC");
        assert_eq!(pcb_component_gen::sanitize_pin_name("gnd"), "GND");
        assert_eq!(pcb_component_gen::sanitize_pin_name("GPIO1"), "GPIO1");
        assert_eq!(pcb_component_gen::sanitize_pin_name("sda"), "SDA");
    }

    #[test]
    fn test_sanitize_pin_name_plus_minus_at_end() {
        // + and - at end become _POS and _NEG
        assert_eq!(pcb_component_gen::sanitize_pin_name("V+"), "V_POS");
        assert_eq!(pcb_component_gen::sanitize_pin_name("V-"), "V_NEG");
        assert_eq!(pcb_component_gen::sanitize_pin_name("IN+"), "IN_POS");
        assert_eq!(pcb_component_gen::sanitize_pin_name("OUT-"), "OUT_NEG");
        assert_eq!(pcb_component_gen::sanitize_pin_name("VCC+"), "VCC_POS");
    }

    #[test]
    fn test_sanitize_pin_name_plus_minus_in_middle() {
        // + and - in middle become underscores
        assert_eq!(pcb_component_gen::sanitize_pin_name("A+B"), "A_B");
        assert_eq!(pcb_component_gen::sanitize_pin_name("IN-OUT"), "IN_OUT");
        assert_eq!(pcb_component_gen::sanitize_pin_name("V+REF"), "V_REF");
    }

    #[test]
    fn test_sanitize_pin_name_not_prefix() {
        // ~ and ! at start become N_ prefix
        assert_eq!(pcb_component_gen::sanitize_pin_name("~CS"), "N_CS");
        assert_eq!(pcb_component_gen::sanitize_pin_name("!RESET"), "N_RESET");
        assert_eq!(pcb_component_gen::sanitize_pin_name("~WR"), "N_WR");
        assert_eq!(pcb_component_gen::sanitize_pin_name("!OE"), "N_OE");
    }

    #[test]
    fn test_sanitize_pin_name_hash_suffix() {
        // # becomes H
        assert_eq!(pcb_component_gen::sanitize_pin_name("CS#"), "CSH");
        assert_eq!(pcb_component_gen::sanitize_pin_name("WE#"), "WEH");
    }

    #[test]
    fn test_sanitize_pin_name_special_chars() {
        // Other special chars become underscores
        assert_eq!(pcb_component_gen::sanitize_pin_name("PIN/A"), "PIN_A");
        assert_eq!(pcb_component_gen::sanitize_pin_name("PIN.B"), "PIN_B");
        assert_eq!(pcb_component_gen::sanitize_pin_name("PIN A"), "PIN_A");
    }

    #[test]
    fn test_sanitize_pin_name_leading_digit() {
        // Leading digit gets P prefix
        assert_eq!(pcb_component_gen::sanitize_pin_name("1"), "P1");
        assert_eq!(pcb_component_gen::sanitize_pin_name("1A"), "P1A");
        assert_eq!(pcb_component_gen::sanitize_pin_name("123"), "P123");
    }

    #[test]
    fn test_sanitize_pin_name_consecutive_underscores() {
        // Consecutive underscores get collapsed
        assert_eq!(pcb_component_gen::sanitize_pin_name("A__B"), "A_B");
        assert_eq!(pcb_component_gen::sanitize_pin_name("A___B"), "A_B");
        assert_eq!(pcb_component_gen::sanitize_pin_name("_A_"), "A");
    }

    #[test]
    fn test_generate_zen_file_duplicate_pads() {
        // Test that multiple pads with the same pin name don't create duplicate dict keys
        // This simulates a component like a voltage regulator with multiple GND pads
        let symbol = pcb_eda::Symbol {
            name: "TEST".to_string(),
            pins: vec![
                pcb_eda::Pin {
                    name: "VIN".to_string(),
                    number: "1".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "GND".to_string(),
                    number: "2".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "GND".to_string(),
                    number: "3".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "GND".to_string(),
                    number: "4".to_string(),
                    ..Default::default()
                },
                pcb_eda::Pin {
                    name: "VOUT".to_string(),
                    number: "5".to_string(),
                    ..Default::default()
                },
            ],
            description: Some("Test component".to_string()),
            ..Default::default()
        };

        let zen_content = generate_zen_file(
            "TEST-MPN",
            "TestComponent",
            &symbol,
            "symbol.kicad_sym",
            Some("footprint.kicad_mod"),
            None,
            Some("TestMfr"),
        )
        .unwrap();

        // The pins dict should NOT have duplicate "GND" keys
        // Count how many times "GND" appears as a dict key
        let gnd_key_count = zen_content.matches("\"GND\": Pins.GND").count();
        assert_eq!(
            gnd_key_count, 1,
            "Expected exactly 1 GND dict entry, found {}. Generated content:\n{}",
            gnd_key_count, zen_content
        );

        // Verify the file is valid Starlark (no duplicate dict keys)
        // The pins dict should only contain unique entries
        assert!(zen_content.contains("\"VIN\": Pins.VIN"));
        assert!(zen_content.contains("\"VOUT\": Pins.VOUT"));
    }

    #[test]
    fn test_only_symbol_in_library_accepts_single_symbol() {
        let source = r#"(kicad_symbol_lib
  (version 20211014)
  (generator "test")
  (symbol "Demo:Only"
    (symbol "Only_1_1"
      (pin passive line
        (at 0 0 0)
        (length 2.54)
        (name "P")
        (number "1")
      )
    )
  )
)"#;
        let lib = pcb_eda::SymbolLibrary::from_string(source, "kicad_sym").unwrap();
        let symbol = only_symbol_in_library(&lib, Path::new("single.kicad_sym")).unwrap();
        assert!(!symbol.name.is_empty());
    }

    #[test]
    fn test_only_symbol_in_library_rejects_multiple_symbols() {
        let source = r#"(kicad_symbol_lib
  (version 20211014)
  (generator "test")
  (symbol "Demo:A"
    (symbol "A_1_1"
      (pin passive line
        (at 0 0 0)
        (length 2.54)
        (name "P")
        (number "1")
      )
    )
  )
  (symbol "Demo:B"
    (symbol "B_1_1"
      (pin passive line
        (at 0 0 0)
        (length 2.54)
        (name "P")
        (number "1")
      )
    )
  )
)"#;
        let lib = pcb_eda::SymbolLibrary::from_string(source, "kicad_sym").unwrap();
        let err = only_symbol_in_library(&lib, Path::new("multi.kicad_sym")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Expected exactly one symbol"));
        assert!(msg.contains("found 2"));
    }

    #[test]
    fn test_rewrite_symbol_footprint_property_text() {
        let symbol = r#"(kicad_symbol_lib
	(symbol "TEST"
		(property "Reference" "U" (at 0 0 0))
		(property "Footprint" "OldLib:OldFootprint" (at 0 0 0))
	)
)"#;
        let updated = rewrite_symbol_footprint_property_text(symbol, "NewFootprint").unwrap();
        assert!(updated.contains("(property \"Footprint\" \"NewFootprint\""));
        assert!(!updated.contains("OldLib:OldFootprint"));
    }
}
