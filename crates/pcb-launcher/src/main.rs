#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const PCB_TOOLCHAIN: &str = "+0.4";

fn main() {
    if let Some(result) = run_from_args() {
        if let Err(error) = result {
            eprintln!("pcb-launcher failed: {error:#}");
            std::process::exit(1);
        }
        return;
    }

    #[cfg(target_os = "macos")]
    macos::run();

    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("pcb-launcher failed: expected --install or a diode:// sandbox file URI");
        std::process::exit(1);
    }
}

fn run_from_args() -> Option<Result<()>> {
    let mut args = std::env::args_os();
    let _program = args.next();
    let first = args.next()?;
    if args.next().is_some() {
        return Some(Err(anyhow::anyhow!(
            "expected --install or exactly one diode:// sandbox file URI"
        )));
    }
    if first == "--install" {
        Some(install_protocol_handler())
    } else {
        Some(launch_pcb(first.to_string_lossy().as_ref()))
    }
}

fn install_protocol_handler() -> Result<()> {
    let launcher = std::env::current_exe().context("failed to locate pcb-launcher")?;
    let pcb = sibling_pcb_executable()?;
    platform::install(&launcher, &pcb)
}

fn launch_pcb(uri: &str) -> Result<()> {
    validate_uri(uri)?;
    let pcb = sibling_pcb_executable()?;
    let mut command = Command::new(&pcb);
    command
        .arg(PCB_TOOLCHAIN)
        .arg("open")
        .arg(uri)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        // CREATE_BREAKAWAY_FROM_JOB lets pcb survive when a browser starts this
        // protocol handler inside a job with kill-on-close semantics.
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
    }

    command
        .spawn()
        .with_context(|| format!("failed to start {}", pcb.display()))?;
    Ok(())
}

fn validate_uri(uri: &str) -> Result<()> {
    if !uri
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("diode://"))
    {
        bail!("expected a diode:// sandbox file URI");
    }
    if uri.contains('\0') {
        bail!("URI must not contain a NUL byte");
    }
    Ok(())
}

fn sibling_pcb_executable() -> Result<PathBuf> {
    let current = std::env::current_exe().context("failed to locate pcb-launcher")?;
    let parent = current
        .parent()
        .context("pcb-launcher has no parent directory")?;

    #[cfg(target_os = "macos")]
    if let Some(contents) = parent.parent() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let pcb_path = contents.join("Resources/pcb-path");
        if pcb_path.is_file() {
            let pcb = PathBuf::from(OsString::from_vec(
                std::fs::read(&pcb_path)
                    .with_context(|| format!("failed to read {}", pcb_path.display()))?,
            ));
            if !pcb.is_file() {
                bail!(
                    "pcb executable not found at bundled path: {}",
                    pcb.display()
                );
            }
            return Ok(pcb);
        }
    }

    let name = if cfg!(target_os = "windows") {
        "pcb.exe"
    } else {
        "pcb"
    };
    let pcb = parent.join(name);
    if !pcb.is_file() {
        bail!(
            "pcb executable not found beside launcher: {}",
            pcb.display()
        );
    }
    Ok(pcb)
}

#[cfg(any(target_os = "linux", test))]
fn escape_desktop_exec_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\\\\\")
        .replace('"', "\\\\\"")
        .replace('%', "%%")
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn run_checked(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to {description}"))?;
    if !status.success() {
        bail!("failed to {description}: {status}");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;

    const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDisplayName</key><string>Diode PCB Launcher</string>
  <key>CFBundleExecutable</key><string>pcb-launcher</string>
  <key>CFBundleIdentifier</key><string>computer.diode.pcb-launcher</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleShortVersionString</key><string>1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleName</key><string>Diode PCB Launcher</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleURLTypes</key>
  <array><dict>
    <key>CFBundleURLName</key><string>Diode sandbox file</string>
    <key>CFBundleURLSchemes</key><array><string>diode</string></array>
  </dict></array>
  <key>LSBackgroundOnly</key><true/>
</dict>
</plist>
"#;

    struct StagedAppCleanup(PathBuf);

    impl Drop for StagedAppCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub fn install(launcher: &Path, pcb: &Path) -> Result<()> {
        let home = dirs::home_dir().context("could not locate the home directory")?;
        let applications = home.join("Applications");
        fs::create_dir_all(&applications)
            .with_context(|| format!("failed to create {}", applications.display()))?;
        let app = applications.join("Diode PCB Launcher.app");
        let staged_app = applications.join(format!(
            ".Diode PCB Launcher-{}.app.tmp",
            std::process::id()
        ));
        let backup_app = applications.join(format!(
            ".Diode PCB Launcher-{}.app.old",
            std::process::id()
        ));
        remove_dir_if_exists(&staged_app)?;
        remove_dir_if_exists(&backup_app)?;
        let _staged_app_cleanup = StagedAppCleanup(staged_app.clone());

        let contents = staged_app.join("Contents");
        let macos = contents.join("MacOS");
        let resources = contents.join("Resources");
        fs::create_dir_all(&macos)
            .with_context(|| format!("failed to create {}", macos.display()))?;
        fs::create_dir_all(&resources)
            .with_context(|| format!("failed to create {}", resources.display()))?;
        fs::copy(launcher, macos.join("pcb-launcher"))
            .context("failed to install the macOS launcher executable")?;
        let pcb =
            fs::canonicalize(pcb).context("failed to resolve the installed pcb executable")?;
        fs::write(resources.join("pcb-path"), pcb.as_os_str().as_bytes())
            .context("failed to store the installed pcb path")?;
        fs::write(contents.join("Info.plist"), INFO_PLIST)
            .context("failed to write the macOS launcher Info.plist")?;

        if let Err(error) = run_checked(
            Command::new("codesign")
                .args(["--force", "--deep", "--sign", "-"])
                .arg(&staged_app),
            "sign the macOS launcher app",
        ) {
            let _ = fs::remove_dir_all(&staged_app);
            return Err(error);
        }

        let had_app = app.exists();
        if had_app {
            fs::rename(&app, &backup_app)
                .context("failed to back up the existing macOS launcher app")?;
        }
        if let Err(error) = fs::rename(&staged_app, &app) {
            if had_app {
                fs::rename(&backup_app, &app)
                    .context("failed to restore the existing macOS launcher app")?;
            }
            return Err(error).context("failed to install the macOS launcher app");
        }

        let lsregister = Path::new(
            "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
        );
        if lsregister.is_file()
            && let Err(error) = run_checked(
                Command::new(lsregister).arg("-f").arg(&app),
                "register the macOS launcher app",
            )
        {
            fs::remove_dir_all(&app)
                .context("failed to remove the unregistered macOS launcher app")?;
            if had_app {
                fs::rename(&backup_app, &app)
                    .context("failed to restore the existing macOS launcher app")?;
                let _ = Command::new(lsregister).arg("-f").arg(&app).status();
            }
            return Err(error);
        }
        if had_app && let Err(error) = fs::remove_dir_all(&backup_app) {
            eprintln!(
                "warning: failed to remove macOS launcher backup {}: {error}",
                backup_app.display()
            );
        }
        Ok(())
    }

    fn remove_dir_if_exists(path: &Path) -> Result<()> {
        if path.exists() {
            fs::remove_dir_all(path)
                .with_context(|| format!("failed to remove stale {}", path.display()))?;
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use std::fs;

    pub fn install(launcher: &Path, _pcb: &Path) -> Result<()> {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
            .context("could not locate the user data directory")?;
        let applications = data_home.join("applications");
        fs::create_dir_all(&applications)
            .with_context(|| format!("failed to create {}", applications.display()))?;
        let escaped = escape_desktop_exec_path(launcher);
        let desktop = format!(
            "[Desktop Entry]\nType=Application\nName=Diode PCB Launcher\nNoDisplay=true\nTerminal=false\nExec=\"{escaped}\" %u\nMimeType=x-scheme-handler/diode;\n"
        );
        fs::write(applications.join("diode-pcb-launcher.desktop"), desktop)
            .context("failed to write the Linux desktop entry")?;

        run_optional(
            Command::new("update-desktop-database").arg(&applications),
            "update the desktop database",
        )?;
        run_optional(
            Command::new("xdg-mime").args([
                "default",
                "diode-pcb-launcher.desktop",
                "x-scheme-handler/diode",
            ]),
            "register the diode URL handler",
        )?;
        Ok(())
    }

    fn run_optional(command: &mut Command, description: &str) -> Result<()> {
        match command.status() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => {
                eprintln!("warning: failed to {description}: {status}");
                Ok(())
            }
            Err(error) => {
                eprintln!("warning: failed to {description}: {error}");
                Ok(())
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;

    pub fn install(launcher: &Path, _pcb: &Path) -> Result<()> {
        let command = format!("\"{}\" \"%1\"", launcher.display());
        reg_add(
            r"HKCU\Software\Classes\diode",
            None,
            "URL:Diode PCB Launcher",
        )?;
        reg_add(r"HKCU\Software\Classes\diode", Some("URL Protocol"), "")?;
        reg_add(
            r"HKCU\Software\Classes\diode\shell\open\command",
            None,
            &command,
        )?;
        Ok(())
    }

    fn reg_add(key: &str, name: Option<&str>, value: &str) -> Result<()> {
        let mut command = Command::new("reg.exe");
        command.args(["ADD", key]);
        match name {
            Some(name) => command.args(["/v", name]),
            None => command.arg("/ve"),
        };
        command.args(["/t", "REG_SZ", "/d", value, "/f"]);
        run_checked(&mut command, "register the Windows diode URL handler")
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
    use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSURL};

    define_class!(
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = ()]
        struct LauncherDelegate;

        unsafe impl NSObjectProtocol for LauncherDelegate {}

        unsafe impl NSApplicationDelegate for LauncherDelegate {
            #[unsafe(method(application:openURLs:))]
            fn application_open_urls(&self, application: &NSApplication, urls: &NSArray<NSURL>) {
                for url in urls {
                    if let Some(value) = url.absoluteString()
                        && let Err(error) = super::launch_pcb(&value.to_string())
                    {
                        eprintln!("Failed to open in KiCad: {error:#}");
                    }
                }
                application.terminate(None);
            }
        }
    );

    impl LauncherDelegate {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            unsafe { msg_send![super(this), init] }
        }
    }

    pub fn run() {
        // LaunchServices may start the app bundle without delivering an open-URL
        // event, so bound the lifetime of this background-only helper.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(10));
            std::process::exit(0);
        });

        let mtm = MainThreadMarker::new().expect("pcb-launcher must run on the main thread");
        let application = NSApplication::sharedApplication(mtm);
        application.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        let delegate = LauncherDelegate::new(mtm);
        application.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        application.run();
    }
}

#[cfg(test)]
mod tests {
    use super::{escape_desktop_exec_path, validate_uri};
    use std::path::Path;

    #[test]
    fn accepts_diode_uri() {
        validate_uri(
            "diode://api.diode.computer/sandboxes/sandbox/fs/read?path=%2Fworkspace%2Flayout.kicad_pcb",
        )
        .unwrap();
    }

    #[test]
    fn rejects_other_schemes() {
        assert!(validate_uri("https://example.com/layout.kicad_pcb").is_err());
    }

    #[test]
    fn escapes_linux_desktop_exec_paths() {
        assert_eq!(
            escape_desktop_exec_path(Path::new("/tmp/a\\b\"c%d")),
            "/tmp/a\\\\\\\\b\\\\\"c%%d"
        );
    }
}
