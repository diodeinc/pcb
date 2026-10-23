#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

#[test]
fn self_update_skips_the_desktop_launcher_without_xdg_mime() {
    for mode in [None, Some(0o644), Some(0o755)] {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("bin");
        fs::create_dir(&bin).unwrap();
        if let Some(mode) = mode {
            let xdg_mime = bin.join("xdg-mime");
            fs::write(&xdg_mime, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&xdg_mime, fs::Permissions::from_mode(mode)).unwrap();
        }

        let output = Command::new(env!("CARGO_BIN_EXE_pcb"))
            .args(["self", "update"])
            .env_clear()
            .env("HOME", home.path())
            .env("PATH", &bin)
            // Exercise the update after shim re-execution without replacing
            // the test binary. Block downloads so the test stays offline.
            .env("PCB_SELF_UPDATE_REEXEC", "1")
            .env(
                "PCB_SELF_UPDATE_RELEASE",
                format!(
                    r#"{{"version":"{0}","tag":"pcb/v{0}"}}"#,
                    env!("CARGO_PKG_VERSION")
                ),
            )
            .env("HTTPS_PROXY", "http://127.0.0.1:0")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(stdout.contains("pcb launcher: update skipped"), "{stdout}");
        assert_eq!(
            stderr.contains("failed to update the optional pcb launcher"),
            mode == Some(0o755),
            "{stderr}"
        );
        // Skipping an optional desktop component must not hide real failures.
        assert!(!output.status.success());
        assert!(stderr.contains("failed to update managed pcbc toolchains"));
    }
}
