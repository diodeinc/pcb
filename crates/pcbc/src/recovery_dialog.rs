use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use pcb_native_dialog::{Choice, ConfirmDialog};
use std::fs;
use std::path::Path;

pub const URL_LAUNCHER_ENV: &str = "PCB_URL_LAUNCHER";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryChoice {
    Restore,
    Discard,
    Cancel,
}

pub fn choose(layout_file: &Path) -> Result<RecoveryChoice> {
    let modified = fs::metadata(layout_file)
        .with_context(|| format!("failed to inspect recovery file {}", layout_file.display()))?
        .modified()
        .with_context(|| {
            format!(
                "failed to read recovery file modification time {}",
                layout_file.display()
            )
        })?;
    let message = recovery_message(layout_file, DateTime::<Local>::from(modified));
    let choice = ConfirmDialog {
        title: "Restore local KiCad backup?",
        message: &message,
        confirm_label: "Restore Backup",
        decline_label: "Open Sandbox Version",
    }
    .show()?;
    Ok(match choice {
        Choice::Confirm => RecoveryChoice::Restore,
        Choice::Decline => RecoveryChoice::Discard,
        Choice::Cancel => RecoveryChoice::Cancel,
    })
}

fn recovery_message<Tz>(layout_file: &Path, updated_at: DateTime<Tz>) -> String
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    let board = layout_file
        .file_name()
        .unwrap_or(layout_file.as_os_str())
        .to_string_lossy();
    format!(
        "KiCad didn’t finish syncing {board}.\n\n\
         Backup saved {}.\n\n\
         Restore it, or open the current sandbox version?",
        updated_at.format("%b %-d, %Y at %-I:%M %p %Z")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};

    #[test]
    fn recovery_message_has_board_and_local_save_time() {
        let saved_at = FixedOffset::west_opt(4 * 60 * 60)
            .unwrap()
            .with_ymd_and_hms(2026, 8, 11, 14, 37, 0)
            .unwrap();
        let message = recovery_message(Path::new("/tmp/power-board.kicad_pcb"), saved_at);

        assert!(message.contains("KiCad didn’t finish syncing power-board.kicad_pcb."));
        assert!(message.contains("Backup saved Aug 11, 2026 at 2:37 PM -04:00."));
        assert!(message.contains("open the current sandbox version"));
    }
}
