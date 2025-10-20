use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use indicatif::ProgressBar;
use inquire::Select;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
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
    pub description: Option<String>,
    pub package_category: Option<String>,
    pub component_id: String,
    pub datasheets: Vec<String>,
    pub model_availability: ModelAvailability,
}

#[derive(Debug, Clone)]
pub struct ComponentSearchOptions {
    pub mpn: String,
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
    pub datasheet_filenames: Option<Vec<String>>,
    pub datasheet_urls: Option<Vec<String>>,
    pub file_hashes: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone)]
pub struct ComponentDownloadResult {
    pub symbol_url: Option<String>,
    pub footprint_url: Option<String>,
    pub step_url: Option<String>,
    pub datasheet_urls: Option<Vec<String>>,
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
    #[serde(rename = "datasheetUrls")]
    datasheet_urls: Option<Vec<String>>,
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
    #[serde(rename = "datasheetFilenames")]
    datasheet_filenames: Option<Vec<String>>,
    #[serde(rename = "datasheetUrls")]
    datasheet_urls: Option<Vec<String>>,
    #[serde(rename = "fileHashes")]
    file_hashes: Option<std::collections::HashMap<String, String>>,
}

pub fn search_components(
    auth_token: &str,
    options: ComponentSearchOptions,
) -> Result<Vec<ComponentSearchResult>> {
    let api_base_url = crate::get_api_base_url();
    let url = format!("{}/api/component/search", api_base_url);

    let client = Client::builder().timeout(Duration::from_secs(60)).build()?;

    let response = client
        .post(&url)
        .bearer_auth(auth_token)
        .json(&SearchRequest { mpn: options.mpn })
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
        datasheet_urls: download_response.datasheet_urls,
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
            datasheet_filenames: download_response.metadata.datasheet_filenames,
            datasheet_urls: download_response.metadata.datasheet_urls,
            file_hashes: download_response.metadata.file_hashes,
        },
    })
}

pub fn download_file(_auth_token: &str, url: &str, output_path: &Path) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent("pcb-cli")
        .build()?;

    let response = client.get(url).send()?;

    if !response.status().is_success() {
        anyhow::bail!("File download failed: {} - URL: {}", response.status(), url);
    }

    std::fs::write(output_path, response.bytes()?)?;
    Ok(())
}

// Helper: Search and filter for ECAD-available components
fn search_and_filter(auth_token: &str, mpn: &str) -> Result<Vec<ComponentSearchResult>> {
    let results = search_components(
        auth_token,
        ComponentSearchOptions {
            mpn: mpn.to_string(),
        },
    )?;
    Ok(results
        .into_iter()
        .filter(|r| r.model_availability.ecad_model)
        .collect())
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
    component: &ComponentSearchResult,
    workspace_root: &std::path::Path,
) -> Result<AddComponentResult> {
    let component_dir = workspace_root
        .join("components")
        .join(&component.part_number);
    let component_file = component_dir.join(format!("{}.zen", &component.part_number));

    if component_file.exists() {
        return Ok(AddComponentResult {
            component_path: component_file,
            already_exists: true,
        });
    }

    // Show progress during API call
    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    spinner.set_message(format!("Fetching {}...", component.part_number));

    let download = download_component(auth_token, &component.component_id)?;
    spinner.finish_and_clear();

    fs::create_dir_all(&component_dir)?;

    // Count tasks and collect work
    let mut file_count = 0;
    let mut scan_count = 0;
    let mut download_tasks = Vec::new();
    let mut scan_tasks = Vec::new();

    if let (Some(url), Some(filename)) = (&download.symbol_url, &download.metadata.symbol_filename)
    {
        file_count += 1;
        download_tasks.push((url.clone(), component_dir.join(filename), "symbol"));
    }
    if let (Some(url), Some(filename)) = (
        &download.footprint_url,
        &download.metadata.footprint_filename,
    ) {
        file_count += 1;
        download_tasks.push((url.clone(), component_dir.join(filename), "footprint"));
    }
    if let (Some(url), Some(filename)) = (&download.step_url, &download.metadata.step_filename) {
        file_count += 1;
        download_tasks.push((url.clone(), component_dir.join(filename), "step"));
    }

    if let (Some(urls), Some(filenames)) = (
        &download.datasheet_urls,
        &download.metadata.datasheet_filenames,
    ) {
        for (url, filename) in urls.iter().zip(filenames.iter()) {
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
    }

    // Show task summary
    println!(
        "{} {}",
        "Downloading".green().bold(),
        component.part_number.bold()
    );
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
                if let Err(e) = download_file(auth_token, &url, &path) {
                    errors.lock().unwrap().push(format!("{}: {}", label, e));
                }
            }));
        }

        // Scan tasks (parallel with downloads)
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

    // Generate .zen file if symbol was downloaded
    if let Some(filename) = &download.metadata.symbol_filename {
        let symbol_path = component_dir.join(filename);
        if symbol_path.exists() {
            let symbol_lib = pcb_eda::SymbolLibrary::from_file(&symbol_path)?;
            let symbol = symbol_lib
                .first_symbol()
                .ok_or_else(|| anyhow::anyhow!("No symbols in library"))?;

            let datasheet_urls = download
                .metadata
                .datasheet_urls
                .as_deref()
                .unwrap_or(&component.datasheets);

            generate_zen_file(
                &component_dir,
                &component.part_number,
                symbol,
                datasheet_urls,
            )?;
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

fn generate_zen_file(
    component_dir: &std::path::Path,
    mpn: &str,
    symbol: &pcb_eda::Symbol,
    datasheet_urls: &[String],
) -> Result<()> {
    let pins = symbol
        .pins
        .iter()
        .map(|pin| format!("        \"{}\": \"{}\",", pin.number, pin.name))
        .collect::<Vec<_>>()
        .join("\n");

    let datasheet = datasheet_urls
        .first()
        .map(|url| format!("    datasheet=\"{}\",\n", url))
        .unwrap_or_default();

    let description = symbol
        .description
        .as_ref()
        .map(|d| format!("    description=\"{}\",\n", d.replace('"', "\\\"")))
        .unwrap_or_default();

    let content = format!(
        "load(\"@stdlib/Component.zen\", \"Component\")\n\n{} = Component(\n    footprint=\"{}\",\n{}{}    pins={{\n{}\n    }},\n)\n",
        mpn, symbol.footprint, datasheet, description, pins
    );

    std::fs::write(component_dir.join(format!("{}.zen", mpn)), content)?;
    Ok(())
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

fn format_search_result(result: &ComponentSearchResult) -> String {
    let part_width = 20;
    let pkg_width = 12;
    let models_width = 14;
    let desc_width = get_terminal_width().saturating_sub(part_width + pkg_width + models_width + 3);

    let part = format!(
        "{:<part_width$}",
        truncate_text(&result.part_number, part_width)
    )
    .bold();

    let pkg_text = result
        .package_category
        .as_ref()
        .filter(|p| p.len() <= 10 && !p.contains(' '))
        .map(|p| p.as_str())
        .unwrap_or("");
    let pkg_truncated = truncate_text(pkg_text, pkg_width);
    let pkg = format!("{:<pkg_width$}", pkg_truncated).yellow();

    let models = format!(
        "[2D {}] [3D {}]",
        if result.model_availability.ecad_model {
            "✓".green()
        } else {
            "✗".red()
        },
        if result.model_availability.step_model {
            "✓".green()
        } else {
            "✗".red()
        }
    );

    let desc = result
        .description
        .as_deref()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii() || c.is_whitespace())
        .collect::<String>();
    let desc_truncated = truncate_text(&desc, desc_width).dimmed();

    format!("{} {} {} {}", part, pkg, models, desc_truncated)
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

    let items: Vec<String> = filtered_results.iter().map(format_search_result).collect();
    let page_size = get_terminal_height().saturating_sub(5).max(5);

    let selection = Select::new(
        "Select a component to download and add to ./components:",
        items.clone(),
    )
    .with_page_size(page_size)
    .with_formatter(&|_| String::new()) // Hide the final selection output
    .prompt()?;

    let selected_index = items
        .iter()
        .position(|s| s == &selection)
        .context("Selected component not found")?;
    let selected_component = &filtered_results[selected_index];
    println!(
        "{} {}",
        "Selected:".green().bold(),
        selected_component.part_number.clone().bold()
    );
    if let Some(description) = &selected_component.description {
        println!("{} {}", "Description:".cyan(), description);
    }

    let result = add_component_to_workspace(auth_token, selected_component, workspace_root)?;

    if result.already_exists {
        let display_path = result
            .component_path
            .strip_prefix(workspace_root)
            .unwrap_or(&result.component_path);
        println!(
            "{} Component already exists at: {}",
            "ℹ".blue().bold(),
            display_path.display().to_string().cyan()
        );
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
                "description": r.description,
                "package_category": r.package_category,
                "component_id": r.component_id,
                "has_2d_model": r.model_availability.ecad_model,
                "has_3d_model": r.model_availability.step_model,
                "datasheets": r.datasheets,
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
        component.part_number.clone().bold()
    );

    let result = add_component_to_workspace(auth_token, component, workspace_root)?;

    if result.already_exists {
        let display_path = result
            .component_path
            .strip_prefix(workspace_root)
            .unwrap_or(&result.component_path);
        println!(
            "{} Component already exists at: {}",
            "ℹ".blue().bold(),
            display_path.display().to_string().cyan()
        );
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
