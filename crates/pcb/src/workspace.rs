//! Common workspace and dependency handling utilities

use anyhow::Result;
use log::debug;
use pcb_zen::EvalConfig;
use pcb_zen_core::config::{get_workspace_info, WorkspaceInfo as ConfigWorkspaceInfo};
use pcb_zen_core::{DefaultFileProvider, EvalOutput, WithDiagnostics};

use std::path::{Path, PathBuf};

/// Common workspace information used by both vendor and release commands
pub struct WorkspaceInfo {
    /// From config discovery
    pub config: ConfigWorkspaceInfo,
    /// Canonical path to the .zen file being processed
    pub zen_path: PathBuf,
    /// Evaluation result containing the parsed zen file
    pub eval_result: WithDiagnostics<EvalOutput>,
}

impl WorkspaceInfo {
    /// Get the board name for this workspace's zen file
    pub fn board_name(&self) -> Option<String> {
        self.config.board_name_for_zen(&self.zen_path)
    }

    /// Get a human-friendly board display name, with a safe fallback to the .zen file stem
    pub fn board_display_name(&self) -> String {
        self.board_name().unwrap_or_else(|| {
            self.zen_path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
    }

    pub fn root(&self) -> &Path {
        &self.config.root
    }
}

/// Gather common workspace information for both vendor and release commands
pub fn gather_workspace_info(zen_path: PathBuf, use_vendor: bool) -> Result<WorkspaceInfo> {
    debug!("Starting workspace information gathering");

    // Canonicalize the zen path
    let zen_path = zen_path.canonicalize()?;

    // 1. Reuse config.rs to get workspace + board list
    let config = get_workspace_info(&DefaultFileProvider::new(), &zen_path)?;

    // 2. Evaluate the zen file – workspace root comes out of config
    let cfg = EvalConfig {
        use_vendor,
        ..Default::default()
    };
    // Keep mode default (Build); offline decided by caller of higher-level flows
    let eval_result = pcb_zen::eval(&zen_path, cfg);

    Ok(WorkspaceInfo {
        config,
        zen_path,
        eval_result,
    })
}
