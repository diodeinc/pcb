use anyhow::{Context, Result as AnyhowResult};
use log::{debug, info};
use pcb_sch::{AttributeValue, Schematic, ATTR_LAYOUT_PATH};
use pcb_zen_core::lang::stackup::{
    ApproxEq, BoardConfig, BoardConfigError, NetClass, Stackup, StackupError, THICKNESS_EPS,
};
use rust_decimal::prelude::ToPrimitive;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use thiserror::Error;

use pcb_kicad::PythonScriptBuilder;
use pcb_sch::kicad_netlist::{format_footprint, write_fp_lib_table};

pub mod fab_drawing;

/// Result of layout generation/update
#[derive(Debug)]
pub struct LayoutResult {
    pub source_file: PathBuf,
    pub layout_dir: PathBuf,
    pub pcb_file: PathBuf,
    pub netlist_file: PathBuf,
    pub snapshot_file: PathBuf,
    pub log_file: PathBuf,
    pub created: bool, // true if new, false if updated
}

/// Error types for layout operations
#[derive(Debug, Error)]
pub enum LayoutError {
    #[error("No layout path found in schematic")]
    NoLayoutPath,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    PcbGeneration(#[from] anyhow::Error),

    #[error("Stackup patching error: {0}")]
    StackupPatchingError(String),

    #[error("Stackup error: {0}")]
    StackupError(#[from] StackupError),

    #[error("Board config error: {0}")]
    BoardConfigError(#[from] BoardConfigError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Helper struct for layout file paths
#[derive(Debug)]
pub struct LayoutPaths {
    pub netlist: PathBuf,
    pub pcb: PathBuf,
    pub snapshot: PathBuf,
    pub log: PathBuf,
    pub json_netlist: PathBuf,
    pub board_config: PathBuf,
    pub temp_dir: TempDir,
}

/// Process a schematic and generate/update its layout files
/// This will:
/// 1. Extract the layout path from the schematic's root instance attributes
/// 2. Create the layout directory if it doesn't exist
/// 3. Generate/update the netlist file
/// 4. Write the footprint library table
/// 5. Create or update the KiCad PCB file
pub fn process_layout(
    schematic: &Schematic,
    source_path: &Path,
    sync_board_config: bool,
) -> Result<LayoutResult, LayoutError> {
    // Extract layout path from schematic
    let layout_path = utils::extract_layout_path(schematic).ok_or(LayoutError::NoLayoutPath)?;

    // Convert relative path to absolute based on source file location
    let layout_dir = if layout_path.is_relative() {
        source_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(&layout_path)
    } else {
        layout_path
    };

    // Get all the file paths
    let paths = utils::get_layout_paths(&layout_dir);

    debug!(
        "Generating layout for {} in {}",
        source_path.display(),
        layout_dir.display()
    );

    // Create layout directory
    fs::create_dir_all(&layout_dir).with_context(|| {
        format!(
            "Failed to create layout directory: {}",
            layout_dir.display()
        )
    })?;

    // Write netlist
    let netlist_content = pcb_sch::kicad_netlist::to_kicad_netlist(schematic);
    fs::write(&paths.netlist, netlist_content)
        .with_context(|| format!("Failed to write netlist: {}", paths.netlist.display()))?;

    // Write JSON netlist into the temp directory (owned by LayoutPaths)
    let json_content = schematic
        .to_json()
        .context("Failed to serialize schematic to JSON")?;
    fs::write(&paths.json_netlist, json_content).with_context(|| {
        format!(
            "Failed to write JSON netlist: {}",
            paths.json_netlist.display()
        )
    })?;

    // Extract board config once for both Python script and patching
    let board_config = utils::extract_board_config(schematic);

    // Write board config for Python script if it exists
    let board_config_path = board_config.as_ref().and_then(|config| {
        serde_json::to_string(config).ok().and_then(|json| {
            fs::write(&paths.board_config, json).ok()?;
            Some(paths.board_config.to_str().unwrap().to_string())
        })
    });

    // Write footprint library table
    utils::write_footprint_library_table(&layout_dir, schematic)?;

    // Check if PCB file exists to determine if this is create or update
    let pcb_exists = paths.pcb.exists();

    // Update or create the KiCad PCB file using the new API
    if pcb_exists {
        debug!("Updating existing layout file: {}", paths.pcb.display());
    } else {
        debug!("Creating new layout file: {}", paths.pcb.display());
    }

    // Load the update_layout_file_star.py script
    let script = include_str!("scripts/update_layout_file.py");

    // Build and run the Python script using the new pcbnew API
    let mut script_builder = PythonScriptBuilder::new(script)
        .arg("-j")
        .arg(paths.json_netlist.to_str().unwrap())
        .arg("-o")
        .arg(paths.pcb.to_str().unwrap())
        .arg("-s")
        .arg(paths.snapshot.to_str().unwrap());

    // Add board config argument if we have one
    if let Some(ref board_config) = board_config_path {
        script_builder = script_builder.arg("--board-config").arg(board_config);
    }

    // Add sync-board-config flag
    script_builder = script_builder
        .arg("--sync-board-config")
        .arg(sync_board_config.to_string());

    script_builder
        .log_file(
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&paths.log)
                .with_context(|| format!("Failed to open log file: {}", paths.log.display()))?,
        )
        .run()
        .with_context(|| {
            format!(
                "Failed to {} layout file",
                if pcb_exists { "update" } else { "create" },
            )
        })?;

    // Apply board config (stackup + netclass patterns)
    if sync_board_config {
        if let Some(ref config) = board_config {
            if let Some(ref stackup) = config.stackup {
                patch_stackup_if_needed(&paths.pcb, stackup)?;
            }

            let assignments = build_netclass_assignments(schematic, config.netclasses());
            if !assignments.is_empty() {
                patch_netclass_patterns(&paths.pcb, &assignments)?;
            }
        }
    }

    Ok(LayoutResult {
        source_file: source_path.to_path_buf(),
        layout_dir,
        pcb_file: paths.pcb,
        netlist_file: paths.netlist,
        snapshot_file: paths.snapshot,
        log_file: paths.log,
        created: !pcb_exists,
    })
}

/// Utility functions
pub mod utils {
    use super::*;
    use pcb_sch::InstanceKind;
    use std::collections::HashMap;

    /// Extract layout path from schematic's root instance attributes
    pub fn extract_layout_path(schematic: &Schematic) -> Option<PathBuf> {
        let root_ref = schematic.root_ref.as_ref()?;
        let root = schematic.instances.get(root_ref)?;
        let layout_path_str = root
            .attributes
            .get(ATTR_LAYOUT_PATH)
            .and_then(|v| v.string())?;
        Some(PathBuf::from(layout_path_str))
    }

    /// Get all the file paths that would be generated for a layout
    pub fn get_layout_paths(layout_dir: &Path) -> LayoutPaths {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp directory for netlist");
        let json_netlist = temp_dir.path().join("netlist.json");
        let board_config = temp_dir.path().join("board_config.json");
        LayoutPaths {
            netlist: layout_dir.join("default.net"),
            pcb: layout_dir.join("layout.kicad_pcb"),
            snapshot: layout_dir.join("snapshot.layout.json"),
            log: layout_dir.join("layout.log"),
            json_netlist,
            board_config,
            temp_dir,
        }
    }

    /// Extract and parse board config from schematic's root instance attributes
    pub fn extract_board_config(schematic: &Schematic) -> Option<BoardConfig> {
        let root = schematic.instances.get(schematic.root_ref.as_ref()?)?;

        // Find board_config.* property (prefer "default")
        let config_json = root
            .attributes
            .iter()
            .filter(|(k, _)| k.starts_with("board_config."))
            .find(|(k, _)| k == &"board_config.default")
            .or_else(|| {
                root.attributes
                    .iter()
                    .find(|(k, _)| k.starts_with("board_config."))
            })
            .and_then(|(_, v)| v.string())?;

        BoardConfig::from_json_str(config_json).ok()
    }

    /// Write footprint library table for a layout
    pub fn write_footprint_library_table(
        layout_dir: &Path,
        schematic: &Schematic,
    ) -> AnyhowResult<()> {
        let mut fp_libs: HashMap<String, PathBuf> = HashMap::new();

        for inst in schematic.instances.values() {
            if inst.kind != InstanceKind::Component {
                continue;
            }

            if let Some(AttributeValue::String(fp_attr)) = inst.attributes.get("footprint") {
                if let (_, Some((lib_name, dir))) = format_footprint(fp_attr) {
                    fp_libs.entry(lib_name).or_insert(dir);
                }
            }
        }

        // Canonicalize the layout directory to avoid symlink issues on macOS
        let canonical_layout_dir = layout_dir
            .canonicalize()
            .unwrap_or_else(|_| layout_dir.to_path_buf());

        // Write or update the fp-lib-table for this layout directory
        write_fp_lib_table(&canonical_layout_dir, &fp_libs).with_context(|| {
            format!("Failed to write fp-lib-table for {}", layout_dir.display())
        })?;

        Ok(())
    }
}

/// Build netclass assignments from net impedance properties
fn build_netclass_assignments(
    schematic: &Schematic,
    netclasses: &[NetClass],
) -> std::collections::HashMap<String, String> {
    const TOLERANCE: f64 = 0.05; // ±5%

    let mut assignments = std::collections::HashMap::new();

    for (net_name, net) in &schematic.nets {
        // Check for differential impedance (from DiffPair propagation)
        let diff_impedance =
            net.properties
                .get("differential_impedance")
                .and_then(|attr| match attr {
                    AttributeValue::Physical(pv) if pv.unit == pcb_sch::PhysicalUnit::Ohms => {
                        pv.value.to_f64()
                    }
                    _ => None,
                });

        // Check for single-ended impedance (from individual nets)
        let se_impedance = net.properties.get("impedance").and_then(|attr| match attr {
            AttributeValue::Physical(pv) if pv.unit == pcb_sch::PhysicalUnit::Ohms => {
                pv.value.to_f64()
            }
            _ => None,
        });

        // Match differential impedance to differential netclasses
        if let Some(imp) = diff_impedance {
            let matched = netclasses
                .iter()
                .filter_map(|nc| {
                    let target = nc.differential_pair_impedance_ohms()?;
                    let error = ((imp - target) / target).abs();
                    (error <= TOLERANCE).then_some((nc, error))
                })
                .min_by(|(_, e1), (_, e2)| e1.partial_cmp(e2).unwrap());

            if let Some((nc, _)) = matched {
                assignments.insert(net_name.clone(), nc.name.clone());
            }
        }
        // Match single-ended impedance to single-ended netclasses
        else if let Some(imp) = se_impedance {
            let matched = netclasses
                .iter()
                .filter_map(|nc| {
                    let target = nc.single_ended_impedance_ohms()?;
                    let error = ((imp - target) / target).abs();
                    (error <= TOLERANCE).then_some((nc, error))
                })
                .min_by(|(_, e1), (_, e2)| e1.partial_cmp(e2).unwrap());

            if let Some((nc, _)) = matched {
                assignments.insert(net_name.clone(), nc.name.clone());
            }
        }
    }

    assignments
}

/// Apply netclass pattern assignments to .kicad_pro file
fn patch_netclass_patterns(
    pcb_path: &Path,
    assignments: &std::collections::HashMap<String, String>,
) -> Result<(), LayoutError> {
    if assignments.is_empty() {
        debug!("No netclass assignments to write");
        return Ok(());
    }

    let pro_path = pcb_path.with_extension("kicad_pro");
    info!(
        "Writing {} netclass patterns to {}",
        assignments.len(),
        pro_path.display()
    );

    // Read, modify, and write .kicad_pro JSON
    let pro_content = fs::read_to_string(&pro_path)
        .with_context(|| format!("Failed to read {}", pro_path.display()))?;

    let mut pro_json: serde_json::Value = serde_json::from_str(&pro_content)
        .with_context(|| format!("Failed to parse {}", pro_path.display()))?;

    pro_json["net_settings"]["netclass_patterns"] = serde_json::json!(assignments
        .iter()
        .map(|(net_name, netclass_name)| {
            serde_json::json!({"pattern": net_name, "netclass": netclass_name})
        })
        .collect::<Vec<_>>());

    fs::write(&pro_path, serde_json::to_string_pretty(&pro_json)?)
        .with_context(|| format!("Failed to write {}", pro_path.display()))?;

    Ok(())
}

/// Apply stackup configuration if it differs from existing PCB file
fn patch_stackup_if_needed(pcb_path: &Path, zen_stackup: &Stackup) -> Result<(), LayoutError> {
    // Read current PCB file
    let pcb_content = fs::read_to_string(pcb_path).map_err(|e| {
        LayoutError::StackupPatchingError(format!("Failed to read PCB file: {}", e))
    })?;

    // Parse existing stackup from PCB file
    let existing_stackup = Stackup::from_kicad_pcb(&pcb_content)?;

    // Compare stackups - only patch if they're different
    let needs_update = match existing_stackup {
        Some(existing) => {
            let equivalent = zen_stackup.approx_eq(&existing, THICKNESS_EPS);
            if !equivalent {
                debug!("Zen stackup: {:?}", zen_stackup);
                debug!("Existing stackup: {:?}", existing);
            }
            !equivalent
        }
        None => {
            debug!("No existing stackup found in PCB file");
            true // No existing stackup, so we need to add it
        }
    };

    if !needs_update {
        info!("Stackup configuration matches existing PCB file, no update needed");
        return Ok(());
    }

    info!("Updating stackup configuration in {}", pcb_path.display());

    // Generate new S-expressions (using default user layers)
    let layers_sexpr = zen_stackup.generate_layers_sexpr(4);
    let stackup_sexpr = zen_stackup.generate_stackup_sexpr();

    // Use surgical string replacement to avoid parsing issues with hex numbers
    let mut updated_content = pcb_content;
    updated_content = replace_section_in_pcb_content(&updated_content, "layers", &layers_sexpr)?;
    updated_content = replace_section_in_pcb_content(&updated_content, "stackup", &stackup_sexpr)?;

    // Write updated content back to file
    fs::write(pcb_path, updated_content).map_err(|e| {
        LayoutError::StackupPatchingError(format!("Failed to write updated PCB file: {}", e))
    })?;

    info!("Successfully updated stackup configuration");
    Ok(())
}

/// Replace a section in KiCad PCB content using careful string matching
fn replace_section_in_pcb_content(
    content: &str,
    section_name: &str,
    new_section: &str,
) -> Result<String, LayoutError> {
    // Find the section by parsing just enough to locate it
    let section_start = find_section_start(content, section_name)?;

    if let Some(start_pos) = section_start {
        let end_pos = find_matching_paren(content, start_pos)?;

        // Replace the section with the new content
        let mut result = String::with_capacity(content.len() + new_section.len());
        result.push_str(&content[..start_pos]);
        result.push_str(new_section);
        result.push_str(&content[end_pos + 1..]);
        Ok(result)
    } else {
        // Section doesn't exist, need to add it
        add_section_to_pcb_content(content, section_name, new_section)
    }
}

/// Find the start position of a section in PCB content
fn find_section_start(content: &str, section_name: &str) -> Result<Option<usize>, LayoutError> {
    let pattern = format!("({}", section_name);
    let mut pos = 0;

    while let Some(found) = content[pos..].find(&pattern) {
        let abs_pos = pos + found;

        // Check if this is a word boundary (not part of a larger identifier)
        let next_char_pos = abs_pos + pattern.len();
        if next_char_pos < content.len() {
            let next_char = content.chars().nth(next_char_pos).unwrap();
            if next_char.is_whitespace() || next_char == '\n' || next_char == '\t' {
                return Ok(Some(abs_pos));
            }
        } else {
            return Ok(Some(abs_pos));
        }

        pos = abs_pos + 1;
    }

    Ok(None)
}

/// Find the matching closing parenthesis for an opening parenthesis
fn find_matching_paren(content: &str, start_pos: usize) -> Result<usize, LayoutError> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;

    let chars: Vec<char> = content.chars().collect();

    for (i, &ch) in chars.iter().enumerate().skip(start_pos) {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '(' if !in_string => depth += 1,
            ')' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            _ => {}
        }
    }

    Err(LayoutError::StackupPatchingError(
        "Could not find matching closing parenthesis".to_string(),
    ))
}

/// Add a new section to PCB content
fn add_section_to_pcb_content(
    content: &str,
    section_name: &str,
    new_section: &str,
) -> Result<String, LayoutError> {
    match section_name {
        "layers" => {
            // Add after general section
            if let Some(general_start) = find_section_start(content, "general")? {
                let general_end = find_matching_paren(content, general_start)?;
                let insert_pos = general_end + 1;

                let mut result = String::with_capacity(content.len() + new_section.len() + 10);
                result.push_str(&content[..insert_pos]);
                result.push('\n');
                result.push('\t');
                result.push_str(new_section);
                result.push_str(&content[insert_pos..]);
                Ok(result)
            } else {
                Err(LayoutError::StackupPatchingError(
                    "Could not find general section for layers insertion".to_string(),
                ))
            }
        }
        "stackup" => {
            // Add within setup section
            if let Some(setup_start) = find_section_start(content, "setup")? {
                let setup_end = find_matching_paren(content, setup_start)?;
                let insert_pos = setup_end; // Before closing paren

                let mut result = String::with_capacity(content.len() + new_section.len() + 20);
                result.push_str(&content[..insert_pos]);
                result.push('\n');
                result.push_str("\t\t");
                result.push_str(new_section);
                result.push('\n');
                result.push('\t');
                result.push_str(&content[insert_pos..]);
                Ok(result)
            } else {
                Err(LayoutError::StackupPatchingError(
                    "Could not find setup section for stackup insertion".to_string(),
                ))
            }
        }
        _ => Err(LayoutError::StackupPatchingError(format!(
            "Unknown section type: {}",
            section_name
        ))),
    }
}
