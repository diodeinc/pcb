//! Minimal native message dialogs (macOS / Linux / Windows) shared by the
//! pcb tools that run without a terminal, such as browser-launched opens.

use anyhow::{Result, bail};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Confirm,
    Decline,
    Cancel,
}

/// A three-way native dialog: a default confirm button, an alternative
/// decline button, and Cancel.
pub struct ConfirmDialog<'a> {
    pub title: &'a str,
    pub message: &'a str,
    pub confirm_label: &'a str,
    pub decline_label: &'a str,
}

impl ConfirmDialog<'_> {
    pub fn show(&self) -> Result<Choice> {
        platform::show_confirm(self)
    }

    fn parse_choice(&self, value: &str) -> Result<Choice> {
        match value.trim() {
            value if value == self.confirm_label || value == "Yes" => Ok(Choice::Confirm),
            value if value == self.decline_label || value == "No" => Ok(Choice::Decline),
            "Cancel" => Ok(Choice::Cancel),
            value => bail!("native dialog returned an unexpected choice: {value:?}"),
        }
    }

    #[cfg(any(target_os = "linux", test))]
    fn parse_zenity_choice(
        &self,
        status_success: bool,
        status_code: Option<i32>,
        stdout: &str,
    ) -> Result<Choice> {
        if !stdout.trim().is_empty() {
            return self.parse_choice(stdout);
        }
        if status_success {
            return Ok(Choice::Confirm);
        }
        if status_code == Some(1) {
            return Ok(Choice::Cancel);
        }
        bail!("zenity dialog failed with exit code {status_code:?}")
    }
}

/// Show a best-effort native error alert; failures to display are ignored.
pub fn show_error(title: &str, message: &str) {
    platform::show_error(title, message);
}

#[cfg(any(target_os = "macos", test))]
fn applescript_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use anyhow::Context;
    use std::process::Command;

    pub fn show_confirm(dialog: &ConfirmDialog) -> Result<Choice> {
        let confirm = applescript_string(dialog.confirm_label);
        let show_dialog = format!(
            "set dialogResult to display dialog (item 1 of argv) buttons {{\"Cancel\", {}, {confirm}}} default button {confirm} cancel button \"Cancel\" with title {} with icon caution",
            applescript_string(dialog.decline_label),
            applescript_string(dialog.title),
        );
        let output = Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "try",
                "-e",
                &show_dialog,
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
            .arg(dialog.message)
            .output()
            .context("failed to show the macOS dialog")?;
        if !output.status.success() {
            bail!(
                "macOS dialog failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        dialog.parse_choice(&String::from_utf8_lossy(&output.stdout))
    }

    pub fn show_error(title: &str, message: &str) {
        let show_alert = format!(
            "display alert {} message (item 1 of argv) as critical",
            applescript_string(title)
        );
        let _ = Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                &show_alert,
                "-e",
                "end run",
                "--",
            ])
            .arg(message)
            .status();
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use anyhow::Context;
    use std::process::Command;

    pub fn show_confirm(dialog: &ConfirmDialog) -> Result<Choice> {
        match show_zenity(dialog) {
            Ok(choice) => Ok(choice),
            Err(zenity_error) => show_kdialog(dialog).with_context(|| {
                format!("failed to show dialog with zenity ({zenity_error:#}) or kdialog")
            }),
        }
    }

    fn show_zenity(dialog: &ConfirmDialog) -> Result<Choice> {
        let output = Command::new("zenity")
            .arg("--question")
            .arg(format!("--title={}", dialog.title))
            .arg(format!("--ok-label={}", dialog.confirm_label))
            .arg("--cancel-label=Cancel")
            .arg(format!("--extra-button={}", dialog.decline_label))
            .arg("--text")
            .arg(dialog.message)
            .output()
            .context("failed to run zenity")?;
        dialog
            .parse_zenity_choice(
                output.status.success(),
                output.status.code(),
                &String::from_utf8_lossy(&output.stdout),
            )
            .with_context(|| {
                format!(
                    "zenity dialog failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )
            })
    }

    fn show_kdialog(dialog: &ConfirmDialog) -> Result<Choice> {
        let status = Command::new("kdialog")
            .args([
                "--title",
                dialog.title,
                "--yesnocancel",
                dialog.message,
                "--yes-label",
                dialog.confirm_label,
                "--no-label",
                dialog.decline_label,
                "--cancel-label",
                "Cancel",
            ])
            .status()
            .context("failed to run kdialog")?;
        match status.code() {
            Some(0) => Ok(Choice::Confirm),
            Some(1) => Ok(Choice::Decline),
            Some(2) => Ok(Choice::Cancel),
            code => bail!("kdialog dialog failed with exit code {code:?}"),
        }
    }

    pub fn show_error(title: &str, message: &str) {
        let shown = Command::new("zenity")
            .arg("--error")
            .arg(format!("--title={title}"))
            .arg("--text")
            .arg(message)
            .status()
            .is_ok_and(|status| status.success());
        if !shown {
            let _ = Command::new("kdialog")
                .args(["--title", title, "--error", message])
                .status();
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use anyhow::Context;
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn message_box(title: &str, message: &str, buttons: &str, icon: &str) -> Command {
        let mut command = Command::new("powershell.exe");
        command
            .creation_flags(CREATE_NO_WINDOW)
            .env("PCB_DIALOG_TITLE", title)
            .env("PCB_DIALOG_MESSAGE", message)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "Add-Type -AssemblyName PresentationFramework; [System.Windows.MessageBox]::Show($env:PCB_DIALOG_MESSAGE, $env:PCB_DIALOG_TITLE, '{buttons}', '{icon}')"
                ),
            ]);
        command
    }

    pub fn show_confirm(dialog: &ConfirmDialog) -> Result<Choice> {
        let message = format!(
            "{}\n\nYes \u{2014} {}\nNo \u{2014} {}",
            dialog.message, dialog.confirm_label, dialog.decline_label
        );
        let output = message_box(dialog.title, &message, "YesNoCancel", "Warning")
            .output()
            .context("failed to show the Windows dialog")?;
        if !output.status.success() {
            bail!(
                "Windows dialog failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        dialog.parse_choice(&String::from_utf8_lossy(&output.stdout))
    }

    pub fn show_error(title: &str, message: &str) {
        let _ = message_box(title, message, "OK", "Error").status();
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod platform {
    use super::*;

    pub fn show_confirm(_dialog: &ConfirmDialog) -> Result<Choice> {
        bail!("native dialogs are not supported on this platform")
    }

    pub fn show_error(title: &str, message: &str) {
        eprintln!("{title}: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> ConfirmDialog<'static> {
        ConfirmDialog {
            title: "Restore local KiCad backup?",
            message: "message",
            confirm_label: "Restore Backup",
            decline_label: "Open Sandbox Version",
        }
    }

    #[test]
    fn parses_native_dialog_choices() {
        assert_eq!(
            dialog().parse_choice("Restore Backup\n").unwrap(),
            Choice::Confirm
        );
        assert_eq!(dialog().parse_choice("No\r\n").unwrap(), Choice::Decline);
        assert_eq!(dialog().parse_choice("Cancel").unwrap(), Choice::Cancel);
        assert!(dialog().parse_choice("Maybe").is_err());
    }

    #[test]
    fn parses_zenity_extra_button_before_exit_code() {
        assert_eq!(
            dialog()
                .parse_zenity_choice(false, Some(1), "Open Sandbox Version\n")
                .unwrap(),
            Choice::Decline
        );
        assert_eq!(
            dialog().parse_zenity_choice(false, Some(1), "").unwrap(),
            Choice::Cancel
        );
        assert_eq!(
            dialog().parse_zenity_choice(true, Some(0), "").unwrap(),
            Choice::Confirm
        );
    }

    #[test]
    fn escapes_applescript_strings() {
        assert_eq!(applescript_string(r#"a\b"c"#), r#""a\\b\"c""#);
    }
}
