use anyhow::{Context, Result};
use std::path::Path;

use crate::pcb_mod::{self, SyncArgs};

pub fn hydrate_workspace(workspace_root: &Path) -> Result<()> {
    let lockfile = workspace_root.join("pcb.sum");
    let old_lockfile = match std::fs::read(&lockfile) {
        Ok(contents) => Some(contents),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(err).with_context(|| format!("Failed to read {}", lockfile.display()));
        }
    };
    if old_lockfile.is_some() {
        std::fs::remove_file(&lockfile)
            .with_context(|| format!("Failed to remove {}", lockfile.display()))?;
    }

    let sync_result = pcb_mod::execute_sync_from(
        workspace_root,
        SyncArgs {
            verbose: true,
            offline: false,
        },
    );

    if let Err(err) = sync_result {
        if let Some(contents) = old_lockfile {
            std::fs::write(&lockfile, contents)
                .with_context(|| format!("Failed to restore {}", lockfile.display()))?;
        }
        return Err(err);
    }

    if old_lockfile.is_some() {
        eprintln!("  ✓ Removed {}", lockfile.display());
    }

    Ok(())
}
