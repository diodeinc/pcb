// crates/pcb-autoplace/src/lib.rs

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    #[serde(rename = "ref")]
    pub ref_: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub rotation: f64,
    pub nets: Vec<String>,
    pub is_fixed: bool,
    pub thermal_power: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Net {
    pub name: String,
    pub components: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardData {
    pub board_width: f64,
    pub board_height: f64,
    pub components: Vec<Component>,
    pub nets: Vec<Net>,
}

#[derive(Debug, Clone)]
pub struct AutoPlaceConfig {
    pub board_width: f64,
    pub board_height: f64,
    pub iterations: usize,
}

pub struct AutoPlacer {
    config: AutoPlaceConfig,
}

impl AutoPlacer {
    pub fn new(config: AutoPlaceConfig) -> Self {
        Self { config }
    }
    
    /// Run the Python optimizer
    pub fn optimize(&self, board_data: &BoardData) -> Result<BoardData> {
        // Write input to temp file
        let input_file = tempfile::NamedTempFile::new()?;
        let input_path = input_file.path();
        
        let input_json = serde_json::to_string_pretty(board_data)
            .context("Failed to serialize board data")?;
        std::fs::write(input_path, input_json)?;
        
        // Create temp file for output
        let output_file = tempfile::NamedTempFile::new()?;
        let output_path = output_file.path();
        
        // Find the Python script - check multiple locations
        let script_path = find_autoplace_script()
            .context("Could not find autoplace.py script")?;
        
        // Run Python optimizer
        let output = Command::new("python3")
            .arg(script_path)
            .arg(input_path)
            .arg(output_path)
            .arg(self.config.iterations.to_string())
            .output()
            .context("Failed to run Python optimizer - make sure python3 is installed")?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Optimization failed: {}", stderr);
        }
        
        // Print stdout (progress messages)
        print!("{}", String::from_utf8_lossy(&output.stdout));
        
        // Read optimized result
        let result_json = std::fs::read_to_string(output_path)
            .context("Failed to read optimization result")?;
        
        let optimized: BoardData = serde_json::from_str(&result_json)
            .context("Failed to parse optimization result")?;
        
        Ok(optimized)
    }
}

/// Helper to estimate footprint size from footprint name
pub fn estimate_footprint_size(footprint: &str) -> (f64, f64) {
    // Parse common footprint names
    if footprint.contains("0402") {
        (1.0, 0.5)
    } else if footprint.contains("0603") {
        (1.6, 0.8)
    } else if footprint.contains("0805") {
        (2.0, 1.25)
    } else if footprint.contains("1206") {
        (3.2, 1.6)
    } else if footprint.contains("DIP-8") {
        (7.62, 9.0)
    } else if footprint.contains("USB") {
        (8.0, 6.0)
    } else if footprint.contains("SW_PUSH") {
        (6.0, 6.0)
    } else {
        // Default size
        (5.0, 5.0)
    }
}

/// Find the autoplace.py script in common locations
fn find_autoplace_script() -> Result<std::path::PathBuf> {
    use std::path::PathBuf;
    
    // Check multiple possible locations
    let mut possible_paths = vec![
        // Current directory
        PathBuf::from("scripts/autoplace.py"),
        PathBuf::from("autoplace.py"),
        // Parent directories (for when running from subdirectories)
        PathBuf::from("../scripts/autoplace.py"),
        PathBuf::from("../../scripts/autoplace.py"),
        PathBuf::from("../../../scripts/autoplace.py"),
    ];
    
    // Add repo root if we can find it
    if let Ok(root) = find_repo_root() {
        possible_paths.push(root.join("scripts/autoplace.py"));
    }
    
    for path in &possible_paths {
        if path.exists() {
            return Ok(path.clone());
        }
    }
    
    anyhow::bail!(
        "Could not find autoplace.py script. Tried:\n  - scripts/autoplace.py\n  - autoplace.py\n  - ../scripts/autoplace.py\n  - Repository root scripts/\n\nPlease place autoplace.py in the repository root's scripts/ directory."
    )
}

/// Try to find the repository root by looking for Cargo.toml
fn find_repo_root() -> Result<std::path::PathBuf> {
    
    let mut current = std::env::current_dir()?;
    
    // Walk up directories looking for Cargo.toml
    for _ in 0..10 {
        if current.join("Cargo.toml").exists() {
            return Ok(current);
        }
        if let Some(parent) = current.parent() {
            current = parent.to_path_buf();
        } else {
            break;
        }
    }
    
    anyhow::bail!("Could not find repository root")
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_footprint_size_estimation() {
        assert_eq!(estimate_footprint_size("R_0805_2012Metric"), (2.0, 1.25));
        assert_eq!(estimate_footprint_size("LED_0402"), (1.0, 0.5));
        assert_eq!(estimate_footprint_size("DIP-8_W7.62mm"), (7.62, 9.0));
    }
}