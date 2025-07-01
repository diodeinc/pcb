use anyhow::{Context, Result as AnyhowResult};
use log::debug;
use pcb_sch::{AttributeValue, Schematic, ATTR_LAYOUT_PATH};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

use pcb_kicad::PythonScriptBuilder;
use pcb_sch::kicad_netlist::{format_footprint, write_fp_lib_table};

/// Result of layout generation/update
#[derive(Debug, Clone)]
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

    #[error("PCB generation error: {0}")]
    PcbGeneration(#[from] anyhow::Error),
}

/// Helper struct for layout file paths
#[derive(Debug, Clone)]
pub struct LayoutPaths {
    pub netlist: PathBuf,
    pub pcb: PathBuf,
    pub snapshot: PathBuf,
    pub log: PathBuf,
    pub json_netlist: PathBuf,
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

    // Write JSON netlist
    let json_content = schematic
        .to_json()
        .context("Failed to serialize schematic to JSON")?;
    fs::write(&paths.json_netlist, json_content).with_context(|| {
        format!(
            "Failed to write JSON netlist: {}",
            paths.json_netlist.display()
        )
    })?;

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
    PythonScriptBuilder::new(script)
        .arg("-j")
        .arg(paths.json_netlist.to_str().unwrap())
        .arg("-o")
        .arg(paths.pcb.to_str().unwrap())
        .arg("-s")
        .arg(paths.snapshot.to_str().unwrap())
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
                "Failed to {} layout file: {}",
                if pcb_exists { "update" } else { "create" },
                paths.pcb.display()
            )
        })?;

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
        LayoutPaths {
            netlist: layout_dir.join("default.net"),
            pcb: layout_dir.join("layout.kicad_pcb"),
            snapshot: layout_dir.join("snapshot.layout.json"),
            log: layout_dir.join("layout.log"),
            json_netlist: layout_dir.join("netlist.json"),
        }
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
