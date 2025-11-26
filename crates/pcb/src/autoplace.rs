// crates/pcb/src/autoplace.rs
// Simplified version that works with existing pcb CLI structure

use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::process::Command;
use pcb_autoplace::{AutoPlacer, AutoPlaceConfig, BoardData, Component, Net};

#[derive(Args)]
pub struct AutoplaceArgs {
    #[arg(value_name = "PATHS")]
    pub paths: Vec<PathBuf>,
    #[arg(short, long, default_value = "5000")]
    pub iterations: usize,
    #[arg(long, default_value = "100.0")]
    pub width: f64,
    #[arg(long, default_value = "80.0")]
    pub height: f64,
    
    /// Skip applying to KiCad layout
    #[arg(long)]
    pub no_kicad: bool,
}

pub fn execute(args: AutoplaceArgs) -> Result<()> {
    println!("{}", "🔧 PCB Auto-Placement".blue().bold());
    println!();
    
    let paths = if args.paths.is_empty() {
        vec![std::env::current_dir()?]
    } else {
        args.paths
    };
    
    for path in paths {
        println!("Processing: {}", path.display().to_string().yellow());
        
        // Extract components from KiCad layout instead of building circuit
        let board_data = extract_from_kicad(&path, args.width, args.height)?;
        
        println!("  Components: {}", board_data.components.len());
        println!("  Nets: {}", board_data.nets.len());
        println!();
        
        let config = AutoPlaceConfig {
            board_width: args.width,
            board_height: args.height,
            iterations: args.iterations,
        };
        
        println!("{}", "  Running optimization...".cyan());
        
        let placer = AutoPlacer::new(config);
        let optimized = placer.optimize(&board_data)?;
        
        println!();
        println!("{}", "  ✓ Optimization complete!".green());
        
        save_results(&optimized, &path)?;
        
        // Apply to KiCad layout if it exists and --no-kicad not specified
        if !args.no_kicad {
            if let Ok(layout_path) = find_kicad_layout(&path) {
                println!();
                println!("{}", "  Applying to KiCad layout...".cyan());
                match apply_to_kicad_layout(&optimized, &layout_path) {
                    Ok(count) => {
                        println!("{}", format!("  ✓ Updated {} components in KiCad!", count).green());
                    }
                    Err(e) => {
                        println!("{}", format!("  ⚠ Warning: Could not apply to KiCad: {}", e).yellow());
                        println!("{}", "  (Positions saved to JSON file)".yellow());
                    }
                }
            } else {
                println!();
                println!("{}", "  ℹ No KiCad layout found (run 'pcb layout' first)".blue());
            }
        }
    }
    
    println!();
    println!("{}", "✅ Auto-placement complete!".green().bold());
    
    Ok(())
}

/// Extract component data from existing KiCad layout
fn extract_from_kicad(zen_path: &Path, board_width: f64, board_height: f64) -> Result<BoardData> {
    
    // Find the KiCad layout
    let layout_path = find_kicad_layout(zen_path)?;
    
    println!("{}", "  Reading KiCad layout...".cyan());
    
    // Use Python to extract components from KiCad
    let script = format!(r#"
import json
import sys

try:
    import pcbnew
except ImportError:
    # Fallback to test data if pcbnew not available
    print(json.dumps({{"error": "pcbnew not available"}}))
    sys.exit(1)

board = pcbnew.LoadBoard("{}")

components = []
nets_dict = {{}}

for footprint in board.GetFootprints():
    ref = footprint.GetReference()
    pos = footprint.GetPosition()
    bbox = footprint.GetBoundingBox()
    
    # Convert from nanometers to mm
    x = pos.x / 1000000.0
    y = pos.y / 1000000.0
    width = bbox.GetWidth() / 1000000.0
    height = bbox.GetHeight() / 1000000.0
    rotation = footprint.GetOrientationDegrees()
    
    # Get connected nets
    nets = []
    for pad in footprint.Pads():
        net = pad.GetNetname()
        if net and net not in nets:
            nets.append(net)
            if net not in nets_dict:
                nets_dict[net] = []
            nets_dict[net].append(ref)
    
    components.append({{
        "ref": ref,
        "x": x,
        "y": y,
        "width": max(width, 2.0),
        "height": max(height, 1.0),
        "rotation": rotation,
        "nets": nets,
        "is_fixed": ref.startswith("J"),
        "thermal_power": 0.1 if ref.startswith("U") else 0.01
    }})

nets = [
    {{"name": name, "components": comps}}
    for name, comps in nets_dict.items()
    if len(comps) > 1
]

result = {{
    "board_width": {},
    "board_height": {},
    "components": components,
    "nets": nets
}}

print(json.dumps(result))
"#, layout_path.display(), board_width, board_height);
    
    let script_file = tempfile::NamedTempFile::new()?;
    std::fs::write(script_file.path(), script)?;
    
    // Try different Python paths
    let python_paths = vec![
        "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3",
        "/usr/bin/python3",
        "python3",
    ];
    
    for python_cmd in python_paths {
        let output = Command::new(python_cmd).arg(script_file.path()).output();
        
        if let Ok(output) = output {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Ok(board_data) = serde_json::from_str::<BoardData>(&stdout) {
                    println!("  Extracted {} components from KiCad", board_data.components.len());
                    return Ok(board_data);
                }
            }
        }
    }
    
    // Fallback: create test data
    println!("{}", "  ⚠ Could not extract from KiCad, using test data".yellow());
    create_test_data(board_width, board_height)
}

fn create_test_data(board_width: f64, board_height: f64) -> Result<BoardData> {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    
    Ok(BoardData {
        board_width,
        board_height,
        components: vec![
            Component {
                ref_: "U1".to_string(),
                x: rng.gen_range(10.0..board_width - 10.0),
                y: rng.gen_range(10.0..board_height - 10.0),
                width: 7.62, height: 9.0, rotation: 0.0,
                nets: vec!["VCC".to_string(), "GND".to_string()],
                is_fixed: false, thermal_power: 0.5,
            },
            Component {
                ref_: "R1".to_string(),
                x: rng.gen_range(10.0..board_width - 10.0),
                y: rng.gen_range(10.0..board_height - 10.0),
                width: 2.0, height: 1.25, rotation: 0.0,
                nets: vec!["VCC".to_string(), "LED".to_string()],
                is_fixed: false, thermal_power: 0.01,
            },
            Component {
                ref_: "D1".to_string(),
                x: rng.gen_range(10.0..board_width - 10.0),
                y: rng.gen_range(10.0..board_height - 10.0),
                width: 2.0, height: 1.25, rotation: 0.0,
                nets: vec!["LED".to_string(), "GND".to_string()],
                is_fixed: false, thermal_power: 0.05,
            },
        ],
        nets: vec![
            Net { name: "VCC".to_string(), components: vec!["U1".to_string(), "R1".to_string()] },
            Net { name: "GND".to_string(), components: vec!["U1".to_string(), "D1".to_string()] },
            Net { name: "LED".to_string(), components: vec!["R1".to_string(), "D1".to_string()] },
        ],
    })
}

fn save_results(board_data: &BoardData, path: &Path) -> Result<()> {
    let output_path = path.with_extension("autoplace.json");
    let json = serde_json::to_string_pretty(board_data).context("Failed to serialize")?;
    std::fs::write(&output_path, json).context("Failed to write file")?;
    println!("  Saved to: {}", output_path.display().to_string().cyan());
    Ok(())
}

fn find_kicad_layout(zen_path: &Path) -> Result<PathBuf> {
    let name = zen_path.file_stem().context("Invalid filename")?.to_str().context("Invalid UTF-8")?;
    
    // Check all possible layout locations
    let possible_paths = vec![
        // Most common: layout/layout.kicad_pcb
        PathBuf::from("layout/layout.kicad_pcb"),
        // With subdirectory
        PathBuf::from(format!("layout/{}/layout.kicad_pcb", name)),
        PathBuf::from(format!("layout/{}.kicad_pcb", name)),
        // Relative to zen file
        zen_path.parent().unwrap_or(Path::new(".")).join("layout/layout.kicad_pcb"),
        zen_path.parent().unwrap_or(Path::new(".")).join(format!("layout/{}/layout.kicad_pcb", name)),
    ];
    
    for path in &possible_paths {
        if path.exists() {
            return Ok(path.clone());
        }
    }
    
    anyhow::bail!("KiCad layout file not found. Checked: {}", 
        possible_paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))
}

fn apply_to_kicad_layout(board_data: &BoardData, layout_path: &Path) -> Result<usize> {
    let script = format!(r#"
import json, sys
try:
    import pcbnew
except ImportError:
    sys.exit(1)
board = pcbnew.LoadBoard("{}")
components = json.loads('''{}''')
count = 0
for c in components:
    fp = board.FindFootprintByReference(c['ref'])
    if fp:
        fp.SetPosition(pcbnew.VECTOR2I(int(c['x']*1e6), int(c['y']*1e6)))
        fp.SetOrientationDegrees(c['rotation'])
        count += 1
board.Save("{}")
print(count)
"#, layout_path.display(), serde_json::to_string(&board_data.components)?, layout_path.display());
    
    let script_file = tempfile::NamedTempFile::new()?;
    std::fs::write(script_file.path(), script)?;
    
    let python_paths = vec![
        "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3",
        "python3",
    ];
    
    for python_cmd in python_paths {
        if let Ok(output) = Command::new(python_cmd).arg(script_file.path()).output() {
            if output.status.success() {
                let count = String::from_utf8_lossy(&output.stdout).trim().parse().unwrap_or(0);
                return Ok(count);
            }
        }
    }
    
    anyhow::bail!("Could not apply to KiCad")
}