use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use clap::Args;
use colored::Colorize;
use indicatif::ProgressBar;
use inquire::Select;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use walkdir::WalkDir;

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
}

#[derive(Debug, Clone)]
pub struct ComponentDownloadResult {
    pub symbol_url: Option<String>,
    pub footprint_url: Option<String>,
    pub step_url: Option<String>,
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
    metadata: DownloadResponseMetadata,
}

#[derive(Deserialize)]
struct DownloadResponseMetadata {
    mpn: String,
    timestamp: String,
    source: String,
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
        anyhow::bail!("Download failed: {}", response.status());
    }

    let download_response: DownloadResponse = response.json()?;

    Ok(ComponentDownloadResult {
        symbol_url: download_response.symbol_url,
        footprint_url: download_response.footprint_url,
        step_url: download_response.step_url,
        metadata: ComponentDownloadMetadata {
            mpn: download_response.metadata.mpn,
            timestamp: download_response.metadata.timestamp,
            source: download_response.metadata.source,
        },
    })
}

pub fn download_file(_auth_token: &str, url: &str, output_path: &Path) -> Result<()> {
    let client = Client::builder().timeout(Duration::from_secs(60)).build()?;

    let response = client.get(url).send()?;

    if !response.status().is_success() {
        anyhow::bail!("File download failed: {}", response.status());
    }

    std::fs::write(output_path, response.bytes()?)?;
    Ok(())
}

pub fn decode_component_id(component_id: &str) -> Result<(String, String)> {
    let decoded = general_purpose::STANDARD.decode(component_id)?;
    let decoded_str = String::from_utf8(decoded)?;

    #[derive(Deserialize)]
    struct ComponentId {
        source: String,
        part_id: String,
    }

    let parsed: ComponentId = serde_json::from_str(&decoded_str)?;

    Ok((parsed.source, parsed.part_id))
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
            scanned_pdfs: vec![],
        });
    }

    let download = download_component(auth_token, &component.component_id)?;

    fs::create_dir_all(&component_dir)?;
    let eda_dir = component_dir.join("eda");
    fs::create_dir_all(&eda_dir)?;

    let mut symbol_path = None;

    if let Some(url) = &download.symbol_url {
        let path = eda_dir.join(format!("{}.kicad_sym", component.part_number));
        download_file(auth_token, url, &path)?;
        symbol_path = Some(path);
    }

    if let Some(url) = &download.footprint_url {
        let path = eda_dir.join(format!("{}.kicad_mod", component.part_number));
        download_file(auth_token, url, &path)?;
    }

    if let Some(url) = &download.step_url {
        let path = eda_dir.join(format!("{}.step", component.part_number));
        download_file(auth_token, url, &path)?;
    }

    for (idx, url) in component.datasheets.iter().enumerate() {
        if url.ends_with(".pdf") {
            let filename = if component.datasheets.len() == 1 {
                format!("{}.pdf", component.part_number)
            } else {
                format!("{}_{}.pdf", component.part_number, idx + 1)
            };
            download_file(auth_token, url, &component_dir.join(filename))?;
        }
    }

    if let Some(symbol_file) = &symbol_path {
        let symbol_lib = pcb_eda::SymbolLibrary::from_file(symbol_file)?;
        let symbol = symbol_lib
            .first_symbol()
            .ok_or_else(|| anyhow::anyhow!("No symbols in library"))?;

        generate_zen_file(
            &component_dir,
            &component.part_number,
            symbol,
            &component.datasheets,
        )?;

        let scanned_pdfs = scan_component_pdfs(&component_dir, auth_token)?;

        return Ok(AddComponentResult {
            component_path: component_file,
            already_exists: false,
            scanned_pdfs,
        });
    }

    Ok(AddComponentResult {
        component_path: component_file,
        already_exists: false,
        scanned_pdfs: vec![],
    })
}

pub struct AddComponentResult {
    pub component_path: std::path::PathBuf,
    pub already_exists: bool,
    pub scanned_pdfs: Vec<std::path::PathBuf>,
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

fn scan_component_pdfs(
    component_dir: &std::path::Path,
    auth_token: &str,
) -> Result<Vec<std::path::PathBuf>> {
    let pdfs: Vec<_> = WalkDir::new(component_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("pdf"))
        .collect();

    let mut scanned = Vec::new();

    for entry in pdfs {
        let pdf_path = entry.path();
        if pdf_path.with_extension("md").exists() {
            scanned.push(pdf_path.to_path_buf());
            continue;
        }

        let options = crate::scan::ScanOptions {
            file: pdf_path.to_path_buf(),
            output_dir: component_dir.to_path_buf(),
            model: None,
            images: false,
        };

        match crate::scan::scan_pdf(auth_token, options) {
            Ok(_) => {
                scanned.push(pdf_path.to_path_buf());
            }
            Err(_) => {
                // Silently continue on scan failures
            }
        }
    }

    Ok(scanned)
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

    let part = format!("{:<part_width$}", result.part_number).bold();

    let pkg_text = result
        .package_category
        .as_ref()
        .filter(|p| p.len() <= 10 && !p.contains(' '))
        .map(|p| p.as_str())
        .unwrap_or("");
    let pkg = format!("{:<pkg_width$}", pkg_text).yellow();

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

    let results = search_components(
        auth_token,
        ComponentSearchOptions {
            mpn: mpn.to_string(),
        },
    )?;
    spinner.finish_and_clear();

    let filtered_results: Vec<ComponentSearchResult> = results
        .into_iter()
        .filter(|r| r.model_availability.ecad_model)
        .collect();

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
    .prompt()?;

    let selected_index = items
        .iter()
        .position(|s| s == &selection)
        .context("Selected component not found")?;
    let selected_component = &filtered_results[selected_index];

    println!(
        "\n{} {}",
        "Selected:".green().bold(),
        selected_component.part_number.clone().bold()
    );
    if let Some(description) = &selected_component.description {
        println!("{} {}", "Description:".cyan(), description);
    }

    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    spinner.set_message(format!(
        "Downloading {}...",
        selected_component.part_number.cyan()
    ));

    let result = add_component_to_workspace(auth_token, selected_component, workspace_root)?;
    spinner.finish_and_clear();

    let display_path = result
        .component_path
        .strip_prefix(workspace_root)
        .unwrap_or(&result.component_path);

    if result.already_exists {
        println!(
            "{} Component already exists at: {}",
            "ℹ".blue().bold(),
            display_path.display().to_string().cyan()
        );
        return Ok(());
    }

    if !result.scanned_pdfs.is_empty() {
        println!(
            "{} Scanned {} datasheet(s)",
            "✓".green(),
            result.scanned_pdfs.len()
        );
    }

    println!(
        "{} Added {} to {}",
        "✓".green().bold(),
        selected_component.part_number.bold(),
        display_path.display().to_string().cyan()
    );

    Ok(())
}

pub fn search_json(auth_token: &str, mpn: &str) -> Result<String> {
    let results = search_components(
        auth_token,
        ComponentSearchOptions {
            mpn: mpn.to_string(),
        },
    )?;

    let filtered_results: Vec<ComponentSearchResult> = results
        .into_iter()
        .filter(|r| r.model_availability.ecad_model)
        .collect();

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
    let results = search_components(
        auth_token,
        ComponentSearchOptions {
            mpn: mpn.to_string(),
        },
    )?;

    let filtered_results: Vec<ComponentSearchResult> = results
        .into_iter()
        .filter(|r| r.model_availability.ecad_model)
        .collect();

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

    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    spinner.set_message(format!("Downloading {}...", component.part_number.cyan()));

    let result = add_component_to_workspace(auth_token, component, workspace_root)?;
    spinner.finish_and_clear();

    let display_path = result
        .component_path
        .strip_prefix(workspace_root)
        .unwrap_or(&result.component_path);

    if result.already_exists {
        println!(
            "{} Component already exists at: {}",
            "ℹ".blue().bold(),
            display_path.display().to_string().cyan()
        );
        return Ok(());
    }

    if !result.scanned_pdfs.is_empty() {
        println!(
            "{} Scanned {} datasheet(s)",
            "✓".green(),
            result.scanned_pdfs.len()
        );
    }

    println!(
        "{} Added {} to {}",
        "✓".green().bold(),
        component.part_number.bold(),
        display_path.display().to_string().cyan()
    );

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
