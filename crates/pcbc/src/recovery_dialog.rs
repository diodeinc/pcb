use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, Utc};
use std::path::Path;
use std::process::Command;

pub const URL_LAUNCHER_ENV: &str = "PCB_URL_LAUNCHER";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryChoice {
    Restore,
    Discard,
    Cancel,
}

pub fn choose(layout_file: &Path, updated_at: DateTime<Utc>) -> Result<RecoveryChoice> {
    let message = recovery_message(layout_file, updated_at.with_timezone(&Local));
    platform::show(&message)
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

fn parse_choice(value: &str) -> Result<RecoveryChoice> {
    match value.trim() {
        "Restore Backup" | "Yes" => Ok(RecoveryChoice::Restore),
        "Open Sandbox Version" | "No" => Ok(RecoveryChoice::Discard),
        "Cancel" => Ok(RecoveryChoice::Cancel),
        value => bail!("recovery dialog returned an unexpected choice: {value:?}"),
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    pub fn show(message: &str) -> Result<RecoveryChoice> {
        let output = Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "try",
                "-e",
                "set dialogResult to display dialog (item 1 of argv) buttons {\"Cancel\", \"Open Sandbox Version\", \"Restore Backup\"} default button \"Restore Backup\" cancel button \"Cancel\" with title \"Restore local KiCad backup?\" with icon caution",
                "-e",
                "return button returned of dialogResult",
                "-e",
                "on error number -128",
                "-e",
                "return \"Cancel\"",
                "-e",
                "end try",
                "-e",
                "end run",
                "--",
            ])
            .arg(message)
            .output()
            .context("failed to show the macOS recovery dialog")?;
        if !output.status.success() {
            bail!(
                "macOS recovery dialog failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        parse_choice(&String::from_utf8_lossy(&output.stdout))
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    pub fn show(message: &str) -> Result<RecoveryChoice> {
        match show_zenity(message) {
            Ok(choice) => Ok(choice),
            Err(zenity_error) => show_kdialog(message).with_context(|| {
                format!("failed to show recovery dialog with zenity ({zenity_error:#}) or kdialog")
            }),
        }
    }

    fn show_zenity(message: &str) -> Result<RecoveryChoice> {
        let output = Command::new("zenity")
            .args([
                "--question",
                "--title=Restore local KiCad backup?",
                "--ok-label=Restore Backup",
                "--cancel-label=Cancel",
                "--extra-button=Open Sandbox Version",
                "--text",
            ])
            .arg(message)
            .output()
            .context("failed to run zenity")?;
        let choice = String::from_utf8_lossy(&output.stdout);
        if output.status.success() {
            if choice.trim().is_empty() {
                Ok(RecoveryChoice::Restore)
            } else {
                parse_choice(&choice)
            }
        } else if output.status.code() == Some(1) {
            Ok(RecoveryChoice::Cancel)
        } else {
            bail!(
                "zenity recovery dialog failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
        }
    }

    fn show_kdialog(message: &str) -> Result<RecoveryChoice> {
        let status = Command::new("kdialog")
            .args([
                "--title",
                "Restore local KiCad backup?",
                "--yesnocancel",
                message,
                "--yes-label",
                "Restore Backup",
                "--no-label",
                "Open Sandbox Version",
                "--cancel-label",
                "Cancel",
            ])
            .status()
            .context("failed to run kdialog")?;
        match status.code() {
            Some(0) => Ok(RecoveryChoice::Restore),
            Some(1) => Ok(RecoveryChoice::Discard),
            Some(2) => Ok(RecoveryChoice::Cancel),
            code => bail!("kdialog recovery dialog failed with exit code {code:?}"),
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn show(message: &str) -> Result<RecoveryChoice> {
        let message = format!(
            "{message}\n\nYes — Restore Backup\nNo — Open Sandbox Version\nCancel — Don't open"
        );
        let mut command = Command::new("powershell.exe");
        command.creation_flags(CREATE_NO_WINDOW);
        let output = command
            .env("PCB_RECOVERY_MESSAGE", message)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Add-Type -AssemblyName PresentationFramework; $choice = [System.Windows.MessageBox]::Show($env:PCB_RECOVERY_MESSAGE, 'Restore local KiCad backup?', 'YesNoCancel', 'Warning'); Write-Output $choice",
            ])
            .output()
            .context("failed to show the Windows recovery dialog")?;
        if !output.status.success() {
            bail!(
                "Windows recovery dialog failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        parse_choice(&String::from_utf8_lossy(&output.stdout))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod platform {
    use super::*;

    pub fn show(_message: &str) -> Result<RecoveryChoice> {
        bail!("native recovery dialogs are not supported on this platform")
    }
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

    #[test]
    fn parses_native_dialog_choices() {
        assert_eq!(
            parse_choice("Restore Backup\n").unwrap(),
            RecoveryChoice::Restore
        );
        assert_eq!(parse_choice("No\r\n").unwrap(), RecoveryChoice::Discard);
        assert_eq!(parse_choice("Cancel").unwrap(), RecoveryChoice::Cancel);
        assert!(parse_choice("Maybe").is_err());
    }
}
