use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use indicatif::ProgressBar;
use inquire::Select;
use pcb_component::{ComponentClient, SearchOptions, SearchResult};
use pcb_eda::SymbolLibrary;
use std::fs;
use std::path::{Path, PathBuf};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use walkdir::WalkDir;

use crate::auth;

fn get_valid_token() -> Result<String> {
    let tokens = auth::load_tokens()?
        .context("Not authenticated. Run `pcb auth login` to authenticate.")?;

    if tokens.is_expired() {
        match auth::refresh_tokens() {
            Ok(new_tokens) => {
                println!("{}", "Token refreshed".dimmed());
                return Ok(new_tokens.access_token);
            }
            Err(e) => {
                anyhow::bail!(
                    "Authentication token expired and refresh failed: {}\nRun `pcb auth login` to re-authenticate.",
                    e
                );
            }
        }
    }

    Ok(tokens.access_token)
}

fn get_api_base_url() -> String {
    if let Ok(url) = std::env::var("DIODE_API_URL") {
        return url;
    }

    #[cfg(debug_assertions)]
    return "http://localhost:3001".to_string();
    #[cfg(not(debug_assertions))]
    return "https://api.diode.computer".to_string();
}

fn get_workspace_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    pcb_zen_core::config::get_workspace_info(&pcb_zen_core::DefaultFileProvider::new(), &cwd)
        .map(|info| info.root)
        .unwrap_or(cwd)
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

fn format_search_result(result: &SearchResult) -> String {
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

fn create_spinner(message: &str) -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    spinner.set_message(message.to_string());
    spinner
}

fn generate_zen_file(component_dir: &Path, mpn: &str, symbol: &pcb_eda::Symbol, datasheet_urls: &[String]) -> Result<()> {
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

    fs::write(component_dir.join(format!("{}.zen", mpn)), content)?;
    Ok(())
}

fn scan_pdfs_in_directory(component_dir: &Path, api_base_url: &str, auth_token: &str) -> Result<Vec<PathBuf>> {
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

        let filename = pdf_path.file_name().unwrap_or_default().to_string_lossy();
        let spinner = create_spinner(&format!("Scanning {}...", filename.cyan()));

        let options = pcb_scan::ScanOptions {
            file: pdf_path.to_path_buf(),
            output_dir: component_dir.to_path_buf(),
            model: None,
            images: false,
        };

        match pcb_scan::scan_pdf(api_base_url, auth_token, options) {
            Ok(_) => {
                spinner.finish_with_message(format!("{} Scanned {}", "✓".green(), filename));
                scanned.push(pdf_path.to_path_buf());
            }
            Err(e) => {
                spinner.finish_with_message(format!("{} Failed: {}", "✗".red(), e));
            }
        }
    }

    Ok(scanned)
}

#[derive(Args, Debug)]
#[command(about = "Search for electronic components")]
pub struct SearchArgs {
    #[arg(help = "The part number to search for")]
    part_number: String,

    #[arg(long, help = "Return search results in JSON format")]
    json: bool,

    #[arg(
        long,
        help = "Add component directly to the workspace (saves to //components/<PART>/<PART>.zen)"
    )]
    add: bool,
}

pub fn execute(args: SearchArgs) -> Result<()> {
    let token = get_valid_token()?;
    let api_base_url = get_api_base_url();
    let client = ComponentClient::new(api_base_url, token)?;

    let spinner = create_spinner("Searching for components...");
    let results = client.search(SearchOptions {
        mpn: args.part_number.clone(),
    })?;
    spinner.finish_and_clear();

    let filtered_results: Vec<SearchResult> = results
        .into_iter()
        .filter(|r| r.model_availability.ecad_model)
        .collect();

    if filtered_results.is_empty() {
        println!("No results found with ECAD data available.");
        return Ok(());
    }

    if args.add {
        if filtered_results.len() == 1 {
            let component = &filtered_results[0];
            println!(
                "{} Found exactly one component: {}",
                "✓".green().bold(),
                component.part_number.clone().bold()
            );
            add_component(&client, component)?;
        } else {
            println!(
                "{} Found {} components matching '{}'",
                "!".yellow().bold(),
                filtered_results.len(),
                args.part_number.clone().cyan()
            );
            println!("\nMultiple components found. Please use interactive mode:");
            println!("  {} search {}", "pcb".bold().green(), args.part_number);
            anyhow::bail!("Multiple components found. Use interactive mode.");
        }
        return Ok(());
    }

    if args.json {
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
        println!("{}", serde_json::to_string_pretty(&json_results)?);
    } else {
        interactive_selection(&client, &filtered_results)?;
    }

    Ok(())
}

fn interactive_selection(client: &ComponentClient, results: &[SearchResult]) -> Result<()> {
    println!(
        "{} {} components with ECAD data available:",
        "Found".green().bold(),
        results.len()
    );

    let items: Vec<String> = results.iter().map(format_search_result).collect();
    let page_size = get_terminal_height().saturating_sub(5).max(5);

    let selection = Select::new("Select a component to download and add to ./components:", items.clone())
        .with_page_size(page_size)
        .prompt()?;

    let selected_index = items
        .iter()
        .position(|s| s == &selection)
        .context("Selected component not found")?;
    let selected_component = &results[selected_index];

    println!(
        "\n{} {}",
        "Selected:".green().bold(),
        selected_component.part_number.clone().bold()
    );
    if let Some(description) = &selected_component.description {
        println!("{} {}", "Description:".cyan(), description);
    }

    add_component(client, selected_component)?;

    Ok(())
}

fn add_component(client: &ComponentClient, component: &SearchResult) -> Result<()> {
    let workspace_root = get_workspace_root();
    let component_dir = workspace_root
        .join("components")
        .join(&component.part_number);
    let component_file = component_dir.join(format!("{}.zen", &component.part_number));

    if component_file.exists() {
        let display_path = component_file
            .strip_prefix(&workspace_root)
            .unwrap_or(&component_file)
            .display()
            .to_string();
        println!(
            "{} Component already exists at: {}",
            "ℹ".blue().bold(),
            display_path.cyan()
        );
        return Ok(());
    }

    let spinner = create_spinner(&format!("Downloading {}...", component.part_number.cyan()));
    let download = client.download(&component.component_id)?;
    spinner.finish_with_message(format!("{} Downloaded component data", "✓".green()));

    fs::create_dir_all(&component_dir).context("Failed to create component directory")?;
    let eda_dir = component_dir.join("eda");
    fs::create_dir_all(&eda_dir).context("Failed to create eda directory")?;

    let mut symbol_path = None;

    if let Some(url) = &download.symbol_url {
        let path = eda_dir.join(format!("{}.kicad_sym", component.part_number));
        client.download_file(url, &path)?;
        symbol_path = Some(path);
    }

    if let Some(url) = &download.footprint_url {
        let path = eda_dir.join(format!("{}.kicad_mod", component.part_number));
        client.download_file(url, &path)?;
    }

    if let Some(url) = &download.step_url {
        let path = eda_dir.join(format!("{}.step", component.part_number));
        client.download_file(url, &path)?;
    }

    for (idx, url) in component.datasheets.iter().enumerate() {
        if url.ends_with(".pdf") {
            let filename = if component.datasheets.len() == 1 {
                format!("{}.pdf", component.part_number)
            } else {
                format!("{}_{}.pdf", component.part_number, idx + 1)
            };
            client.download_file(url, &component_dir.join(filename))?;
        }
    }

    if let Some(symbol_file) = &symbol_path {
        let symbol_lib = SymbolLibrary::from_file(symbol_file)?;
        let symbol = symbol_lib.first_symbol().context("No symbols in library")?;

        generate_zen_file(&component_dir, &component.part_number, symbol, &component.datasheets)?;

        let api_base_url = get_api_base_url();
        let scanned_pdfs = scan_pdfs_in_directory(&component_dir, &api_base_url, &client.auth_token)?;

        if !scanned_pdfs.is_empty() {
            println!("\n{} Scanned {} datasheet(s)", "✓".green(), scanned_pdfs.len());
        }

        let display_path = component_file
            .strip_prefix(&workspace_root)
            .unwrap_or(&component_file);

        println!(
            "\n{} Added {} to {}",
            "✓".green().bold(),
            component.part_number.bold(),
            display_path.display().to_string().cyan()
        );
    }

    Ok(())
}

