#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use anyhow::{Context, Result, bail};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use url::Url;

const PCB_TOOLCHAIN: &str = "+latest";
const LAUNCHER_LOG_MAX_BYTES: u64 = 1024 * 1024;

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
        let result = install_protocol_handler();
        if let Err(error) = &result {
            append_launcher_error(error);
        }
        Some(result)
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
    let result = launch_pcb_inner(uri);
    if let Err(error) = &result {
        report_launch_error(error);
    }
    result
}

fn report_launch_error(error: &anyhow::Error) {
    append_launcher_error(error);
    platform::show_error(error, &launcher_log_path());
}

#[cfg(target_os = "macos")]
fn spawn_uri_worker(uri: &str) -> Result<()> {
    let launcher = std::env::current_exe().context("failed to locate pcb-launcher")?;
    Command::new(&launcher)
        .arg(uri)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start {}", launcher.display()))?;
    Ok(())
}

fn launch_pcb_inner(uri: &str) -> Result<()> {
    validate_uri(uri)?;
    let pcb = sibling_pcb_executable()?;
    let mut log = open_launcher_log()?;
    writeln!(
        log,
        "\n--- pcb-launcher invocation at {:?} ---",
        std::time::SystemTime::now()
    )
    .context("failed to write the launcher log")?;
    let stdout = log
        .try_clone()
        .context("failed to clone the launcher log handle")?;
    let mut command = Command::new(&pcb);
    command
        .arg(PCB_TOOLCHAIN)
        .arg("open")
        .arg(uri)
        .env("PCB_URL_LAUNCHER", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(log));

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start {}", pcb.display()))?;
    // Keep the protocol handler alive while pcb opens KiCad. This preserves
    // browser-launched children on Windows and lets every platform surface a
    // non-zero exit instead of silently discarding it.
    let status = child
        .wait()
        .with_context(|| format!("failed to wait for {}", pcb.display()))?;
    if !status.success() {
        bail!("{} failed: {status}", pcb.display());
    }

    Ok(())
}

fn validate_uri(uri: &str) -> Result<()> {
    let url = Url::parse(uri).context("invalid diode URI")?;
    if url.scheme() != "diode" {
        bail!("expected a diode:// sandbox file URI");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("diode URI must not include user info");
    }
    if url.fragment().is_some() {
        bail!("diode URI must not include a fragment");
    }

    let host = url.host_str().context("diode URI must include a host")?;
    if !is_trusted_api_host(host, url.port()) {
        bail!("refusing untrusted diode API host: {host}");
    }

    let segments: Vec<_> = url.path().split('/').skip(1).collect();
    if segments.len() != 4
        || segments[0] != "sandboxes"
        || segments[1].is_empty()
        || segments[2] != "fs"
        || segments[3] != "read"
    {
        bail!("expected /sandboxes/{{sandboxId}}/fs/read");
    }

    let mut query = url.query_pairs();
    let Some((key, path)) = query.next() else {
        bail!("diode URI must include one path query parameter");
    };
    if key != "path" || query.next().is_some() {
        bail!("diode URI must include only one path query parameter");
    }
    if !path.starts_with('/')
        || path
            .split('/')
            .skip(1)
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        bail!("diode URI path must be a safe absolute sandbox path");
    }
    Ok(())
}

fn is_trusted_api_host(host: &str, port: Option<u16>) -> bool {
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host == "127.0.0.1" {
        return port == Some(3001);
    }
    port.is_none()
        && (host == "api.diode.computer"
            || host == "api.gov.diode.computer"
            || is_preview_api_host(&host))
}

fn is_preview_api_host(host: &str) -> bool {
    let Some(pr) = host.strip_suffix(".preview.api.diode.computer") else {
        return false;
    };
    pr.strip_prefix("pr-").is_some_and(|number| {
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn launcher_log_path() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".pcb/pcb-launcher.log"))
        .unwrap_or_else(|| std::env::temp_dir().join("pcb-launcher.log"))
}

fn open_launcher_log() -> Result<File> {
    let path = launcher_log_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    secure_launcher_log_permissions(&path)?;
    if path
        .metadata()
        .is_ok_and(|metadata| metadata.len() >= LAUNCHER_LOG_MAX_BYTES)
    {
        let rotated = path.with_extension("log.old");
        let _ = fs::remove_file(&rotated);
        let _ = fs::rename(&path, rotated);
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    secure_launcher_log_permissions(&path)?;
    Ok(file)
}

#[cfg(unix)]
fn secure_launcher_log_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if path.exists() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_launcher_log_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn append_launcher_error(error: &anyhow::Error) {
    if let Ok(mut log) = open_launcher_log() {
        let _ = writeln!(log, "pcb-launcher failed: {error:#}");
    }
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
        .replace('`', "\\\\`")
        .replace('$', "\\\\$")
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

    pub fn show_error(error: &anyhow::Error, log_path: &Path) {
        let message = format!("{error:#}\n\nDetails: {}", log_path.display());
        let _ = Command::new("osascript")
            .args([
                "-e",
                "on run argv",
                "-e",
                "display alert \"Open in KiCad failed\" message (item 1 of argv) as critical",
                "-e",
                "end run",
                "--",
            ])
            .arg(message)
            .status();
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
        let status = Command::new("xdg-mime")
            .args([
                "default",
                "diode-pcb-launcher.desktop",
                "x-scheme-handler/diode",
            ])
            .status()
            .context("failed to run xdg-mime to register the diode URL handler")?;
        if !status.success() {
            bail!("xdg-mime failed to register the diode URL handler: {status}");
        }
        Ok(())
    }

    pub fn show_error(error: &anyhow::Error, log_path: &Path) {
        let message = format!("{error:#}\n\nDetails: {}", log_path.display());
        let shown = Command::new("zenity")
            .args(["--error", "--title=Open in KiCad failed", "--text"])
            .arg(&message)
            .status()
            .is_ok_and(|status| status.success());
        if !shown {
            let _ = Command::new("kdialog")
                .args(["--title", "Open in KiCad failed", "--error"])
                .arg(message)
                .status();
        }
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
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

    pub fn show_error(error: &anyhow::Error, log_path: &Path) {
        let message = format!("{error:#}\n\nDetails: {}", log_path.display());
        let mut command = Command::new("powershell.exe");
        command.creation_flags(CREATE_NO_WINDOW);
        let _ = command
            .env("PCB_LAUNCHER_ERROR", &message)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Add-Type -AssemblyName PresentationFramework; [System.Windows.MessageBox]::Show($env:PCB_LAUNCHER_ERROR, 'Open in KiCad failed', 'OK', 'Error')",
            ])
            .status();
    }

    fn reg_add(key: &str, name: Option<&str>, value: &str) -> Result<()> {
        let mut command = Command::new("reg.exe");
        command.creation_flags(CREATE_NO_WINDOW);
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
    use std::sync::atomic::{AtomicBool, Ordering};

    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
    use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSURL};

    static RECEIVED_URL: AtomicBool = AtomicBool::new(false);

    define_class!(
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[ivars = ()]
        struct LauncherDelegate;

        unsafe impl NSObjectProtocol for LauncherDelegate {}

        unsafe impl NSApplicationDelegate for LauncherDelegate {
            #[unsafe(method(application:openURLs:))]
            fn application_open_urls(&self, application: &NSApplication, urls: &NSArray<NSURL>) {
                RECEIVED_URL.store(true, Ordering::Release);
                for url in urls {
                    if let Some(value) = url.absoluteString()
                        && let Err(error) = super::spawn_uri_worker(&value.to_string())
                    {
                        super::report_launch_error(&error);
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
        let mtm = MainThreadMarker::new().expect("pcb-launcher must run on the main thread");
        let application = NSApplication::sharedApplication(mtm);
        application.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        let delegate = LauncherDelegate::new(mtm);
        application.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

        // LaunchServices may start the app bundle without delivering an open-URL
        // event, so bound the lifetime after the application is ready to receive it.
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(10));
            if !RECEIVED_URL.load(Ordering::Acquire) {
                std::process::exit(0);
            }
        });

        application.run();
    }
}

#[cfg(test)]
mod tests {
    use super::{escape_desktop_exec_path, is_trusted_api_host, validate_uri};
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
    fn rejects_untrusted_api_hosts() {
        assert!(
            validate_uri(
                "diode://attacker.example/sandboxes/sandbox/fs/read?path=%2Fworkspace%2Flayout.kicad_pcb",
            )
            .is_err()
        );
    }

    #[test]
    fn accepts_diode_preview_and_loopback_hosts() {
        assert!(is_trusted_api_host(
            "pr-983.preview.api.diode.computer",
            None
        ));
        assert!(is_trusted_api_host("api.gov.diode.computer", None));
        assert!(is_trusted_api_host("localhost", Some(3001)));
        assert!(is_trusted_api_host("127.0.0.1", Some(3001)));
        assert!(!is_trusted_api_host("127.0.0.2", Some(3001)));
        assert!(!is_trusted_api_host("[::1]", Some(3001)));
        assert!(!is_trusted_api_host("localhost", Some(8080)));
        assert!(!is_trusted_api_host("preview.api.diode.computer", None));
        assert!(!is_trusted_api_host("evil.diode.computer", None));
        assert!(!is_trusted_api_host(
            "pr-main.preview.api.diode.computer",
            None
        ));
    }

    #[test]
    fn rejects_malformed_sandbox_file_uris() {
        assert!(
            validate_uri(
                "diode://api.diode.computer/sandboxes/sandbox/fs/write?path=%2Fworkspace%2Flayout.kicad_pcb",
            )
            .is_err()
        );
        assert!(
            validate_uri(
                "diode://api.diode.computer/sandboxes/sandbox/fs/read?path=%2Fworkspace%2F..%2Flayout.kicad_pcb",
            )
            .is_err()
        );
    }

    #[test]
    fn escapes_linux_desktop_exec_paths() {
        assert_eq!(
            escape_desktop_exec_path(Path::new("/tmp/a\\b\"c`d$e%f")),
            "/tmp/a\\\\\\\\b\\\\\"c\\\\`d\\\\$e%%f"
        );
    }

    #[cfg(unix)]
    #[test]
    fn secures_existing_launcher_log() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "pcb-launcher-permissions-{}-{nonce}.log",
            std::process::id()
        ));
        fs::write(&path, "sandbox output").expect("write log");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("set permissions");

        super::secure_launcher_log_permissions(&path).expect("secure log");

        let mode = fs::metadata(&path)
            .expect("log metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        fs::remove_file(path).expect("remove log");
    }
}
