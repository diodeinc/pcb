#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct TestContext {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    home: PathBuf,
    xdg_data_home: PathBuf,
    pcb_data_dir: PathBuf,
}

impl TestContext {
    fn new(workspace_version: Option<&str>, release_versions: &[&str]) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let home = temp.path().join("home");
        let xdg_data_home = temp.path().join("data");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&xdg_data_home).unwrap();

        if let Some(version) = workspace_version {
            fs::write(
                workspace.join("pcb.toml"),
                format!("[workspace]\npcb-version = \"{version}\"\n"),
            )
            .unwrap();
        }

        let pcb_data_dir = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/pcb")
        } else {
            xdg_data_home.join("pcb")
        };
        fs::create_dir_all(&pcb_data_dir).unwrap();
        let fetched_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        fs::write(
            pcb_data_dir.join("release-list-cache.json"),
            serde_json::to_vec(&serde_json::json!({
                "fetched_at": fetched_at,
                "versions": release_versions,
            }))
            .unwrap(),
        )
        .unwrap();

        Self {
            _temp: temp,
            workspace,
            home,
            xdg_data_home,
            pcb_data_dir,
        }
    }

    fn install_fake_toolchain(&self, version: &str) {
        let install_dir = self
            .pcb_data_dir
            .join("toolchains")
            .join(version)
            .join(target_triple());
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(install_dir.join(".sidecars-checked"), "").unwrap();

        let binary = install_dir.join("pcbc");
        fs::write(
            &binary,
            format!("#!/bin/sh\nprintf 'toolchain={version}\\n'\nprintf 'args=%s\\n' \"$*\"\n"),
        )
        .unwrap();
        let mut permissions = fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(binary, permissions).unwrap();
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pcb"))
            .current_dir(&self.workspace)
            .env("HOME", &self.home)
            .env("XDG_DATA_HOME", &self.xdg_data_home)
            .args(args)
            .output()
            .unwrap()
    }
}

#[test]
fn auth_uses_latest_stable_inside_an_older_workspace() {
    let context = TestContext::new(Some("0.3"), &["0.4.20"]);
    context.install_fake_toolchain("0.3.99");
    context.install_fake_toolchain("0.4.20");

    let output = context.run(&["auth", "git", "get"]);

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("toolchain=0.4.20"));
    assert!(stdout.contains("args=auth git get"));
}

#[test]
fn auth_uses_latest_stable_without_a_workspace() {
    let context = TestContext::new(None, &["0.4.20"]);
    context.install_fake_toolchain("0.4.20");

    let output = context.run(&["auth", "status"]);

    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("toolchain=0.4.20")
    );
}

#[test]
fn workspace_commands_continue_to_use_the_workspace_pin() {
    let context = TestContext::new(Some("0.3"), &["0.4.20"]);
    context.install_fake_toolchain("0.3.99");
    context.install_fake_toolchain("0.4.20");

    let output = context.run(&["build"]);

    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("toolchain=0.3.99")
    );
}

#[test]
fn explicit_toolchain_override_wins_for_auth() {
    let context = TestContext::new(Some("0.3"), &["0.4.20"]);
    context.install_fake_toolchain("0.3.99");
    context.install_fake_toolchain("0.4.20");

    let output = context.run(&["+0.3", "auth", "git", "configure"]);

    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("toolchain=0.3.99")
    );
}

#[test]
fn auth_falls_back_to_the_newest_installed_stable_toolchain() {
    let context = TestContext::new(Some("0.3"), &[]);
    context.install_fake_toolchain("0.3.99");
    context.install_fake_toolchain("0.4.20");

    let output = context.run(&["auth", "status"]);

    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("toolchain=0.4.20")
    );
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("using installed pcbc 0.4.20")
    );
}

#[test]
fn unavailable_latest_toolchain_has_an_actionable_error() {
    let context = TestContext::new(Some("0.3"), &[]);

    let output = context.run(&["auth", "status"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("latest stable pcbc toolchain is unavailable"));
    assert!(stderr.contains("pcb toolchain install latest"));
}

fn target_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        _ => panic!("unsupported test target"),
    }
}
