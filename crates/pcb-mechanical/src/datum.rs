use crate::locate::dedup_paths;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
struct FootprintDatumsFile {
    #[serde(default)]
    pub datum: Vec<FootprintDatum>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct FootprintDatum {
    pub idf_package: String,
    pub footprint: String,
    #[serde(default)]
    pub footprint_hash: Option<String>,
    pub mechanical_origin_in_footprint: LocalDatumPose,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub(crate) struct LocalDatumPose {
    pub x_mm: f64,
    pub y_mm: f64,
    #[serde(default)]
    pub rotation_deg: f64,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct FootprintDatums {
    entries: Vec<FootprintDatum>,
}

impl FootprintDatums {
    #[cfg(test)]
    pub(crate) fn from_entries_for_test(entries: Vec<FootprintDatum>) -> Self {
        Self { entries }
    }

    pub(crate) fn load_for_board(zen_path: &Path, workspace_root: &Path) -> anyhow::Result<Self> {
        Self::load(&catalog_paths_for_board(zen_path, workspace_root))
    }

    fn load(paths: &[PathBuf]) -> anyhow::Result<Self> {
        let mut entries = Vec::new();
        for path in paths {
            if !path.exists() {
                continue;
            }
            let raw = fs::read_to_string(path).map_err(|e| {
                anyhow::anyhow!("failed to read datum catalog {}: {e}", path.display())
            })?;
            let parsed: FootprintDatumsFile = toml::from_str(&raw).map_err(|e| {
                anyhow::anyhow!("failed to parse datum catalog {}: {e}", path.display())
            })?;
            entries.extend(parsed.datum);
        }
        Ok(Self { entries })
    }

    pub(crate) fn lookup(&self, idf_package: &str, footprint: &str) -> Option<&FootprintDatum> {
        self.entries.iter().find(|entry| {
            entry.idf_package.eq_ignore_ascii_case(idf_package) && entry.footprint == footprint
        })
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn catalog_paths_for_board(zen_path: &Path, workspace_root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(parent) = zen_path.parent() {
        paths.push(parent.join("mechanical/footprint-datums.toml"));
    }
    paths.push(workspace_root.join("mechanical/footprint-datums.toml"));
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".pcb/mechanical/footprint-datums.toml"));
    }
    dedup_paths(paths)
}
