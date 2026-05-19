use clap::{Args, Subcommand};
use colored::Colorize;
use rand::seq::SliceRandom;
use semver::Version;
use serde::Deserialize;
use std::{env, fs, io, path::PathBuf, process::Command, time::Duration};

const LATEST_RELEASE_URL: &str = "https://pcb.api.diode.computer/pcb/latest.json";
const UPDATE_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const METADATA_TIMEOUT: Duration = Duration::from_secs(10);
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const STANDALONE_INSTALL_REQUIRED: &str = "Self-update is only available for pcb installed via the standalone installer.\nIf you installed pcb via a package manager, please update using that tool.";

const FORTUNES: &str = include_str!("fortune.txt");

#[derive(Args)]
pub struct SelfUpdateArgs {
    #[command(subcommand)]
    command: SelfUpdateCommands,
}

#[derive(Subcommand)]
enum SelfUpdateCommands {
    /// Update the pcb tool to the latest version
    Update,
}

pub fn execute(args: SelfUpdateArgs) -> anyhow::Result<()> {
    match args.command {
        SelfUpdateCommands::Update => {
            ensure_standalone_install()?;
            let current_version = Version::parse(env!("CARGO_PKG_VERSION"))?;
            let latest = fetch_latest_release()?;
            if latest.version <= current_version {
                println!("Already up to date.");
                return Ok(());
            }

            install_update(&latest)?;

            // Update was performed - print changelog for the version range.
            println!();
            let selector = format!("{}..{}", current_version, latest.version);
            let _ = crate::changelog::execute(crate::changelog::ChangelogArgs { selector });

            // Print a random fortune
            let fortunes: Vec<&str> = FORTUNES.lines().filter(|l| !l.is_empty()).collect();
            if let Some(fortune) = fortunes.choose(&mut rand::thread_rng()) {
                println!();
                println!("{}", format!("> {}", fortune).truecolor(90, 90, 90));
            }

            println!();
            println!(
                "Updated {} → {}",
                current_version.to_string().dimmed(),
                latest.version.to_string().green()
            );

            Ok(())
        }
    }
}

pub fn is_update_available() -> anyhow::Result<bool> {
    ensure_standalone_install()?;
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))?;
    Ok(fetch_latest_release()?.version > current_version)
}

pub fn update_check_due() -> bool {
    let Some(path) = update_check_stamp_path() else {
        return true;
    };
    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return true;
    };
    modified
        .elapsed()
        .map_or(true, |elapsed| elapsed >= UPDATE_CHECK_INTERVAL)
}

pub fn mark_update_checked() {
    let Some(path) = update_check_stamp_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, b"");
}

#[derive(Debug, Deserialize)]
struct LatestRelease {
    version: Version,
    tag: String,
}

#[derive(Debug, Deserialize)]
struct InstallReceipt {
    install_prefix: PathBuf,
}

fn ensure_standalone_install() -> anyhow::Result<()> {
    let receipt = fs::read_to_string(receipt_path())
        .ok()
        .and_then(|content| serde_json::from_str::<InstallReceipt>(&content).ok())
        .ok_or_else(|| anyhow::anyhow!(STANDALONE_INSTALL_REQUIRED))?;
    let install_prefix = receipt
        .install_prefix
        .canonicalize()
        .map_err(|_| anyhow::anyhow!(STANDALONE_INSTALL_REQUIRED))?;
    let current_exe = env::current_exe()?.canonicalize()?;
    anyhow::ensure!(
        current_exe.starts_with(&install_prefix),
        STANDALONE_INSTALL_REQUIRED
    );

    Ok(())
}

fn receipt_path() -> PathBuf {
    let config_dir = if cfg!(windows) {
        PathBuf::from(env::var_os("LOCALAPPDATA").unwrap_or_default())
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
    };

    config_dir.join("pcb").join("pcb-receipt.json")
}

fn fetch_latest_release() -> anyhow::Result<LatestRelease> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(METADATA_TIMEOUT)
        .build()?
        .get(LATEST_RELEASE_URL)
        .header(reqwest::header::USER_AGENT, "pcb")
        .send()?
        .error_for_status()?
        .json()?)
}

fn update_check_stamp_path() -> Option<PathBuf> {
    dirs::cache_dir().map(|dir| dir.join("pcb").join("update-check"))
}

fn install_update(latest: &LatestRelease) -> anyhow::Result<()> {
    let archive_name = archive_name();
    let archive_url = format!(
        "https://pcb.api.diode.computer/pcb/{}/{}",
        latest.tag, archive_name
    );
    let archive = reqwest::blocking::Client::builder()
        .timeout(ARCHIVE_TIMEOUT)
        .build()?
        .get(archive_url)
        .header(reqwest::header::USER_AGENT, "pcb")
        .send()?
        .error_for_status()?
        .bytes()?;

    let temp = tempfile::tempdir()?;
    let archive_path = temp.path().join(archive_name);
    fs::write(&archive_path, archive)?;

    if archive_name.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(fs::File::open(&archive_path)?)?;
        let mut src = zip.by_name(binary_name())?;
        let mut dst = fs::File::create(temp.path().join(binary_name()))?;
        io::copy(&mut src, &mut dst)?;
    } else {
        let status = Command::new("tar")
            .args([
                "xf",
                archive_path.to_str().unwrap(),
                "--strip-components",
                "1",
                "-C",
            ])
            .arg(temp.path())
            .status()?;
        anyhow::ensure!(status.success(), "failed to extract pcb archive");
    }

    self_replace::self_replace(temp.path().join(binary_name()))?;

    Ok(())
}

fn archive_name() -> &'static str {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => "pcb-aarch64-apple-darwin.tar.xz",
        ("macos", "x86_64") => "pcb-x86_64-apple-darwin.tar.xz",
        ("linux", "aarch64") => "pcb-aarch64-unknown-linux-gnu.tar.xz",
        ("linux", "x86_64") => "pcb-x86_64-unknown-linux-gnu.tar.xz",
        ("windows", "x86_64") => "pcb-x86_64-pc-windows-msvc.zip",
        _ => panic!("unsupported self-update platform"),
    }
}

fn binary_name() -> &'static str {
    if cfg!(windows) { "pcb.exe" } else { "pcb" }
}
