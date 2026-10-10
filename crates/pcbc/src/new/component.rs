use anyhow::{Context, Result};
use colored::Colorize;
use inquire::{Select, Text};
use pcb_eda::kicad::metadata::SymbolMetadata;
use pcb_sexpr::formatter::{FormatMode, format_tree};
use pcb_sexpr::kicad::symbol::{
    find_symbol_index, kicad_symbol_lib_items_mut, rewrite_symbol_properties, symbol_names,
    symbol_properties,
};
use pcb_zen_core::config::find_workspace_root;
use std::fs;
use std::path::{Path, PathBuf};

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

struct ComponentFilePaths {
    sanitized_mpn: String,
    symbol_path: PathBuf,
    footprint_path: PathBuf,
    step_path: PathBuf,
    zen_path: PathBuf,
}

fn component_file_paths(component_dir: &Path, mpn: &str) -> ComponentFilePaths {
    let sanitized_mpn = pcb_component_gen::sanitize_mpn_for_path(mpn);
    ComponentFilePaths {
        symbol_path: component_dir.join(format!("{}.kicad_sym", sanitized_mpn)),
        footprint_path: component_dir.join(format!("{}.kicad_mod", sanitized_mpn)),
        step_path: component_dir.join(format!("{}.step", sanitized_mpn)),
        zen_path: component_dir.join(format!("{}.zen", sanitized_mpn)),
        sanitized_mpn,
    }
}

fn component_docs_dir(component_dir: &Path) -> PathBuf {
    component_dir.join("docs")
}

fn component_datasheet_ref(sanitized_mpn: &str) -> String {
    format!("docs/{}.pdf", sanitized_mpn)
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
fn finalize_component(
    component_dir: &Path,
    mpn: &str,
    manufacturer: Option<&str>,
    datasheet_ref: Option<&str>,
) -> Result<()> {
    let files = component_file_paths(component_dir, mpn);

    if files.footprint_path.exists() {
        if files.step_path.exists() {
            pcb_kicad::footprint::embed_step_into_footprint_file(
                &files.footprint_path,
                &files.step_path,
                true,
            )?;
        } else {
            format_kicad_sexpr_file(&files.footprint_path)?;
        }
    }

    if !files.symbol_path.exists() {
        anyhow::bail!(
            "Expected symbol file not found: {}",
            files.symbol_path.display()
        );
    }

    let symbol_source = fs::read_to_string(&files.symbol_path).with_context(|| {
        format!(
            "Failed to read KiCad symbol {}",
            files.symbol_path.display()
        )
    })?;
    let symbol_formatted = rewrite_symbol_component_metadata_text(
        &symbol_source,
        &files.symbol_path,
        footprint_stem_if_exists(&files.footprint_path)?.as_deref(),
        datasheet_ref,
        mpn,
        manufacturer,
    )?;
    fs::write(&files.symbol_path, &symbol_formatted).with_context(|| {
        format!(
            "Failed to write KiCad symbol {}",
            files.symbol_path.display()
        )
    })?;

    // Generate .zen file from the exact symbol content we just wrote.
    let symbol_lib = pcb_eda::SymbolLibrary::from_string(&symbol_formatted, "kicad_sym")?;
    let symbol = only_symbol_in_library(&symbol_lib, &files.symbol_path)?;

    let content = generate_zen_file(
        &files.sanitized_mpn,
        symbol,
        &format!("{}.kicad_sym", files.sanitized_mpn),
    )?;

    write_component_files(&files.zen_path, component_dir, &content)?;

    Ok(())
}

fn footprint_stem_if_exists(footprint_path: &Path) -> Result<Option<String>> {
    if !footprint_path.exists() {
        return Ok(None);
    }

    let stem = footprint_path
        .file_stem()
        .ok_or_else(|| anyhow::anyhow!("Footprint path missing file stem"))?
        .to_string_lossy()
        .to_string();
    Ok(Some(stem))
}

fn only_symbol_in_library<'a>(
    symbol_lib: &'a pcb_eda::SymbolLibrary,
    symbol_path: &Path,
) -> Result<&'a pcb_eda::Symbol> {
    let symbols = symbol_lib.symbols();
    let names = symbol_lib.symbol_names();
    ensure_exactly_one_symbol(symbols.len(), &names, symbol_path)?;
    Ok(symbols
        .first()
        .expect("ensure_exactly_one_symbol guarantees a single symbol"))
}

fn rewrite_symbol_component_metadata_text(
    source: &str,
    symbol_path: &Path,
    footprint_ref: Option<&str>,
    datasheet_ref: Option<&str>,
    mpn: &str,
    manufacturer: Option<&str>,
) -> Result<String> {
    let mut parsed = pcb_sexpr::parse(source).map_err(|e| anyhow::anyhow!(e))?;
    let root = kicad_symbol_lib_items_mut(&mut parsed).ok_or_else(|| {
        anyhow::anyhow!("{} is not a KiCad symbol library", symbol_path.display())
    })?;
    let symbol_items = only_symbol_in_library_mut(root, symbol_path)?;

    let current_metadata = SymbolMetadata::from_property_iter(symbol_properties(symbol_items));
    let mut next_properties = current_metadata.to_properties_map();
    if let Some(footprint_ref) = footprint_ref {
        next_properties.insert("Footprint".to_string(), footprint_ref.to_string());
    }
    if let Some(datasheet_ref) = datasheet_ref {
        next_properties.insert("Datasheet".to_string(), datasheet_ref.to_string());
    }
    next_properties.insert("Manufacturer_Part_Number".to_string(), mpn.to_string());
    if let Some(manufacturer) = manufacturer.filter(|m| !m.trim().is_empty()) {
        next_properties.insert("Manufacturer_Name".to_string(), manufacturer.to_string());
    }

    rewrite_symbol_properties(symbol_items, &next_properties);
    Ok(format_tree(&parsed, FormatMode::Normal))
}

fn only_symbol_in_library_mut<'a>(
    root_items: &'a mut [pcb_sexpr::Sexpr],
    symbol_path: &Path,
) -> Result<&'a mut Vec<pcb_sexpr::Sexpr>> {
    let names = symbol_names(root_items);
    ensure_exactly_one_symbol(names.len(), &names, symbol_path)?;
    let symbol_name = names[0].as_str();

    let idx = find_symbol_index(root_items, symbol_name).ok_or_else(|| {
        anyhow::anyhow!(
            "Symbol '{}' not found in {}",
            symbol_name,
            symbol_path.display()
        )
    })?;

    root_items
        .get_mut(idx)
        .and_then(pcb_sexpr::Sexpr::as_list_mut)
        .ok_or_else(|| anyhow::anyhow!("Invalid symbol structure for '{}'", symbol_name))
}

fn ensure_exactly_one_symbol<N: AsRef<str>>(
    symbol_count: usize,
    symbol_names: &[N],
    symbol_path: &Path,
) -> Result<()> {
    match symbol_count {
        1 => Ok(()),
        0 => anyhow::bail!(
            "Expected exactly one symbol in {}, found none",
            symbol_path.display()
        ),
        _ => anyhow::bail!(
            "Expected exactly one symbol in {}, found {}: {}",
            symbol_path.display(),
            symbol_count,
            symbol_names
                .iter()
                .map(|name| name.as_ref())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn format_kicad_sexpr_file(path: &Path) -> Result<()> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("Failed to read KiCad file {}", path.display()))?;
    let formatted = pcb_kicad::footprint::format_kicad_sexpr_source(&source, path)?;
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
    component_name: &str,
    symbol: &pcb_eda::Symbol,
    symbol_filename: &str,
) -> Result<String> {
    pcb_component_gen::generate_component_zen(pcb_component_gen::GenerateComponentZenArgs {
        component_name,
        symbol,
        symbol_filename,
        generated_by: "pcb new component",
        include_skip_bom: false,
        include_skip_pos: false,
        skip_bom_default: false,
        skip_pos_default: false,
    })
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

    for entry in ignore::WalkBuilder::new(dir)
        .standard_filters(false)
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
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
    if src == dest {
        return Ok(dest);
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(src, &dest).with_context(|| format!("Failed to copy {}", src.display()))?;
    Ok(dest)
}

fn install_component_asset(
    src: &Path,
    component_dir: &Path,
    dest_filename: &str,
    label: &str,
) -> Result<PathBuf> {
    let dest = copy_file_to_dir(src, component_dir, dest_filename)?;
    println!(
        "  {} {}: {} → {}",
        "✓".green(),
        label,
        path_filename(src).dimmed(),
        dest.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(dest_filename)
            .cyan()
    );
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
    let component_files = component_file_paths(&component_dir, &mpn);
    let zen_file = component_files.zen_path.clone();

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
    let sym_filename = format!("{}.kicad_sym", component_files.sanitized_mpn);
    install_component_asset(&selected_symbol, &component_dir, &sym_filename, "Symbol")?;

    // Copy backup symbol files (*.orig.kicad_sym)
    for orig_sym in &files.orig_symbols {
        let orig_filename = format!("{}.orig.kicad_sym", component_files.sanitized_mpn);
        install_component_asset(orig_sym, &component_dir, &orig_filename, "Backup")?;
    }

    // Copy first footprint if available
    let has_footprint = !files.footprints.is_empty();
    if let Some(fp) = files.footprints.first() {
        let fp_filename = format!("{}.kicad_mod", component_files.sanitized_mpn);
        install_component_asset(fp, &component_dir, &fp_filename, "Footprint")?;
    }

    // Copy first STEP file if available
    if let Some(sp) = files.steps.first() {
        let step_filename = format!("{}.step", component_files.sanitized_mpn);
        install_component_asset(sp, &component_dir, &step_filename, "3D Model")?;
    }

    // Copy first PDF with standardized name
    let has_datasheet = !files.pdfs.is_empty();
    if let Some(pdf) = files.pdfs.first() {
        let pdf_filename = format!("{}.pdf", component_files.sanitized_mpn);
        copy_file_to_dir(pdf, &component_docs_dir(&component_dir), &pdf_filename)?;
        println!(
            "  {} Datasheet: {} → {}",
            "✓".green(),
            path_filename(pdf).dimmed(),
            component_datasheet_ref(&component_files.sanitized_mpn).cyan()
        );
    }

    // Upgrade files
    println!("{} Upgrading files...", "→".blue().bold());
    if let Err(e) = upgrade_symbol(&component_files.symbol_path) {
        println!("  {} Symbol upgrade skipped: {}", "!".yellow(), e);
    }
    if has_footprint && let Err(e) = upgrade_footprint(&component_files.footprint_path) {
        println!("  {} Footprint upgrade skipped: {}", "!".yellow(), e);
    }

    // Finalize: embed STEP, generate .zen file
    println!("{} Generating .zen file...", "→".blue().bold());
    let datasheet_ref =
        has_datasheet.then(|| component_datasheet_ref(&component_files.sanitized_mpn));
    finalize_component(
        &component_dir,
        &mpn,
        manufacturer.as_deref(),
        datasheet_ref.as_deref(),
    )?;

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

pub(super) fn execute_component_from_local_dir(dir: &Path) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let workspace_root = find_workspace_root(&pcb_zen_core::DefaultFileProvider::new(), &cwd)?;
    execute_from_dir(dir, &workspace_root)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_rewrite_symbol_component_metadata_text() {
        let symbol = r#"(kicad_symbol_lib
			(symbol "TEST"
				(property "Reference" "U" (at 0 0 0))
				(property "Footprint" "OldLib:OldFootprint" (at 0 0 0))
			)
)"#;
        let updated = rewrite_symbol_component_metadata_text(
            symbol,
            Path::new("TEST.kicad_sym"),
            Some("NewFootprint"),
            Some("docs/NEW-MPN.pdf"),
            "NEW-MPN",
            Some("NewMfr"),
        )
        .unwrap();
        assert!(updated.contains("(property \"Footprint\" \"NewFootprint\""));
        assert!(!updated.contains("OldLib:OldFootprint"));
        assert!(updated.contains("(property \"Datasheet\" \"docs/NEW-MPN.pdf\""));
        assert!(updated.contains("(property \"Manufacturer_Part_Number\" \"NEW-MPN\""));
        assert!(updated.contains("(property \"Manufacturer_Name\" \"NewMfr\""));
    }

    #[test]
    fn test_finalize_component_fails_without_symbol() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "pcb-diode-api-test-{}-{}",
            std::process::id(),
            nonce
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let err = finalize_component(&dir, "MISSING", None, None).unwrap_err();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.to_string().contains("Expected symbol file not found"));
    }
}
