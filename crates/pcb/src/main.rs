#[cfg(all(feature = "mimalloc", not(target_family = "wasm")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::Colorize;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RELEASE_LIST_URL: &str = "https://pcb.api.diode.computer/";
const RELEASE_BASE_URL: &str = "https://pcb.api.diode.computer/pcb";
const USER_AGENT: &str = "pcb";
const METADATA_TIMEOUT: Duration = Duration::from_secs(10);
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const RELEASE_LIST_CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const STANDALONE_INSTALL_REQUIRED: &str = "Self-update is only available for pcb installed via the standalone installer.\nIf you installed pcb via a package manager, please update using that tool.";

#[derive(Parser)]
#[command(name = "pcb")]
#[command(about = "PCB toolchain shim and version manager", long_about = None)]
#[command(version)]
struct ShimCli {
    #[command(subcommand)]
    command: ShimCommands,
}

#[derive(Subcommand)]
enum ShimCommands {
    /// Update the pcb shim and managed pcbc toolchains
    #[command(name = "self")]
    SelfUpdate(SelfUpdateArgs),

    /// Manage installed pcbc toolchains
    Toolchain(ToolchainArgs),
}

#[derive(Parser)]
struct SelfUpdateArgs {
    #[command(subcommand)]
    command: SelfUpdateCommands,
}

#[derive(Subcommand)]
enum SelfUpdateCommands {
    /// Update the pcb shim and managed pcbc toolchains
    Update,
}

#[derive(Parser)]
struct ToolchainArgs {
    #[command(subcommand)]
    command: ToolchainCommands,
}

#[derive(Subcommand)]
enum ToolchainCommands {
    /// List installed pcbc toolchains
    List,

    /// Show the active pcbc toolchain for the current directory
    Show,

    /// Install a pcbc toolchain request such as 0.3, 0.3.83, or latest
    Install { request: String },

    /// Uninstall an exact pcbc toolchain version such as 0.3.83
    Uninstall { version: String },
}

#[derive(Debug, Clone)]
enum ToolchainRequest {
    Lane { major: u64, minor: u64 },
    Exact(Version),
    Latest,
}

#[derive(Debug, Clone)]
struct ResolvedToolchain {
    version: Version,
    binary: PathBuf,
    reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReleaseListCache {
    fetched_at: u64,
    versions: Vec<Version>,
}

#[derive(Debug, Serialize, Deserialize)]
struct InstallReceipt {
    version: Version,
    target: String,
    url: String,
    sha256: String,
    installed_at: String,
}

#[derive(Debug, Deserialize)]
struct LatestRelease {
    version: Version,
    tag: String,
}

#[derive(Debug, Deserialize)]
struct StandaloneInstallReceipt {
    install_prefix: PathBuf,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{} {e}", "Error:".red());
        for cause in e.chain().skip(1) {
            eprintln!("  {cause}");
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();

    if args.first().and_then(|arg| arg.to_str()) == Some("--version")
        || args.first().and_then(|arg| arg.to_str()) == Some("-V")
    {
        println!("pcb {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if is_shim_command(&args) {
        let cli = ShimCli::parse_from(std::iter::once(OsString::from("pcb")).chain(args));
        return execute_shim(cli);
    }

    let override_request = take_cli_override(&mut args)?;
    let selection = select_toolchain(override_request)?;
    exec_toolchain(&selection.binary, &args)
}

fn is_shim_command(args: &[OsString]) -> bool {
    matches!(
        args.first().and_then(|arg| arg.to_str()),
        Some("self" | "toolchain")
    )
}

fn execute_shim(cli: ShimCli) -> Result<()> {
    match cli.command {
        ShimCommands::SelfUpdate(args) => match args.command {
            SelfUpdateCommands::Update => self_update(),
        },
        ShimCommands::Toolchain(args) => match args.command {
            ToolchainCommands::List => toolchain_list(),
            ToolchainCommands::Show => toolchain_show(),
            ToolchainCommands::Install { request } => toolchain_install(&request),
            ToolchainCommands::Uninstall { version } => toolchain_uninstall(&version),
        },
    }
}

fn take_cli_override(args: &mut Vec<OsString>) -> Result<Option<ToolchainRequest>> {
    let Some(first) = args.first().and_then(|arg| arg.to_str()) else {
        return Ok(None);
    };
    let Some(raw) = first.strip_prefix('+') else {
        return Ok(None);
    };
    if raw.is_empty() {
        anyhow::bail!("empty toolchain override; expected +0.3, +0.3.83, or +latest");
    }
    let request = parse_request(raw)?;
    args.remove(0);
    Ok(Some(request))
}

fn parse_request(raw: &str) -> Result<ToolchainRequest> {
    if raw == "latest" {
        return Ok(ToolchainRequest::Latest);
    }

    let parts: Vec<&str> = raw.split('.').collect();
    match parts.as_slice() {
        [major, minor] => Ok(ToolchainRequest::Lane {
            major: major
                .parse()
                .with_context(|| format!("invalid pcb toolchain request `{raw}`"))?,
            minor: minor
                .parse()
                .with_context(|| format!("invalid pcb toolchain request `{raw}`"))?,
        }),
        [_, _, _] => Ok(ToolchainRequest::Exact(
            Version::parse(raw)
                .with_context(|| format!("invalid pcb toolchain request `{raw}`"))?,
        )),
        _ => {
            anyhow::bail!("invalid pcb toolchain request `{raw}`; expected 0.3, 0.3.83, or latest")
        }
    }
}

fn select_toolchain(override_request: Option<ToolchainRequest>) -> Result<ResolvedToolchain> {
    let (request, reason) = if let Some(request) = override_request {
        (request, "command-line override".to_string())
    } else if let Some((path, lane)) = find_workspace_pcb_version()? {
        (
            parse_request(&lane)?,
            format!("{} requires {lane}", path.display()),
        )
    } else {
        (
            ToolchainRequest::Latest,
            "no pcb.toml found; using latest".to_string(),
        )
    };

    resolve_request(&request, reason)
}

fn resolve_request(request: &ToolchainRequest, reason: String) -> Result<ResolvedToolchain> {
    if let Some(local) = best_local_toolchain(request)? {
        return Ok(ResolvedToolchain {
            version: local.0,
            binary: local.1,
            reason,
        });
    }

    let version = resolve_remote_version(request)?;
    let binary = ensure_installed(&version)?;
    Ok(ResolvedToolchain {
        version,
        binary,
        reason,
    })
}

fn best_local_toolchain(request: &ToolchainRequest) -> Result<Option<(Version, PathBuf)>> {
    let mut candidates = installed_toolchains()?;

    if let Some((version, binary)) = sibling_pcbc() {
        candidates.insert(version, binary);
    }

    Ok(candidates
        .into_iter()
        .filter(|(version, _)| request_matches(request, version))
        .next_back())
}

fn installed_toolchains() -> Result<BTreeMap<Version, PathBuf>> {
    let mut installed = BTreeMap::new();
    let root = toolchains_dir();
    let Ok(entries) = fs::read_dir(&root) else {
        return Ok(installed);
    };

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let Ok(version) = Version::parse(&name) else {
            continue;
        };
        let binary = entry.path().join(target_triple()).join(pcbc_binary_name());
        if binary.is_file() {
            installed.insert(version, binary);
        }
    }

    Ok(installed)
}

fn request_matches(request: &ToolchainRequest, version: &Version) -> bool {
    match request {
        ToolchainRequest::Lane { major, minor } => {
            version.major == *major && version.minor == *minor && version.pre.is_empty()
        }
        ToolchainRequest::Exact(exact) => version == exact,
        ToolchainRequest::Latest => version.pre.is_empty(),
    }
}

fn resolve_remote_version(request: &ToolchainRequest) -> Result<Version> {
    let releases = fetch_release_versions(false)?;
    releases
        .into_iter()
        .filter(|version| request_matches(request, version))
        .max()
        .ok_or_else(|| anyhow::anyhow!("no pcbc release found for `{}`", format_request(request)))
}

fn format_request(request: &ToolchainRequest) -> String {
    match request {
        ToolchainRequest::Lane { major, minor } => format!("{major}.{minor}"),
        ToolchainRequest::Exact(version) => version.to_string(),
        ToolchainRequest::Latest => "latest".to_string(),
    }
}

fn fetch_release_versions(force_refresh: bool) -> Result<Vec<Version>> {
    if !force_refresh
        && let Some(cache) = read_release_cache()?
        && cache_is_fresh(cache.fetched_at)
    {
        return Ok(cache.versions);
    }

    let url = format!(
        "{RELEASE_LIST_URL}?list-type=2&prefix=pcb/&delimiter=/&_pcb_cache_bust={}",
        unix_timestamp()
    );
    let body = reqwest::blocking::Client::builder()
        .timeout(METADATA_TIMEOUT)
        .build()?
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()?
        .error_for_status()?
        .text()?;

    let versions = parse_release_versions(&body);
    write_release_cache(&versions)?;
    Ok(versions)
}

fn parse_release_versions(xml: &str) -> Vec<Version> {
    let mut versions = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<Prefix>") {
        rest = &rest[start + "<Prefix>".len()..];
        let Some(end) = rest.find("</Prefix>") else {
            break;
        };
        let prefix = &rest[..end];
        rest = &rest[end + "</Prefix>".len()..];

        let Some(raw) = prefix
            .strip_prefix("pcb/v")
            .and_then(|value| value.strip_suffix('/'))
        else {
            continue;
        };
        if let Ok(version) = Version::parse(raw) {
            versions.push(version);
        }
    }
    versions.sort();
    versions.dedup();
    versions
}

fn read_release_cache() -> Result<Option<ReleaseListCache>> {
    let path = release_list_cache_path();
    let Ok(content) = fs::read_to_string(path) else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_str(&content)?))
}

fn write_release_cache(versions: &[Version]) -> Result<()> {
    let path = release_list_cache_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let cache = ReleaseListCache {
        fetched_at: unix_timestamp(),
        versions: versions.to_vec(),
    };
    fs::write(path, serde_json::to_vec_pretty(&cache)?)?;
    Ok(())
}

fn cache_is_fresh(fetched_at: u64) -> bool {
    unix_timestamp().saturating_sub(fetched_at) < RELEASE_LIST_CACHE_TTL.as_secs()
}

fn find_workspace_pcb_version() -> Result<Option<(PathBuf, String)>> {
    let mut dir = std::env::current_dir()?;
    loop {
        let path = dir.join("pcb.toml");
        if path.is_file()
            && let Some(version) = read_workspace_pcb_version(&path)?
        {
            return Ok(Some((path, version)));
        }
        if !dir.pop() {
            return Ok(None);
        }
    }
}

fn read_workspace_pcb_version(path: &Path) -> Result<Option<String>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value: toml::Value =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(value
        .get("workspace")
        .and_then(|workspace| workspace.get("pcb-version"))
        .and_then(|version| version.as_str())
        .map(ToOwned::to_owned))
}

fn ensure_installed(version: &Version) -> Result<PathBuf> {
    ensure_supported_target()?;

    let binary = installed_binary_path(version);
    if binary.is_file() {
        return Ok(binary);
    }

    let lock_path = locks_dir().join(format!("install-{}-{}.lock", version, target_triple()));
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut lock = fslock::LockFile::open(&lock_path)?;
    lock.lock()?;
    let result = if binary.is_file() {
        Ok(binary)
    } else {
        install_toolchain(version)
    };
    lock.unlock()?;
    result
}

fn install_toolchain(version: &Version) -> Result<PathBuf> {
    let archive_name = toolchain_archive_name("pcb");
    let archive_url = format!("{RELEASE_BASE_URL}/v{version}/{archive_name}");

    eprintln!("Installing pcbc {version} ({})...", target_triple());

    let archive =
        download_optional(&http_client(ARCHIVE_TIMEOUT)?, &archive_url)?.ok_or_else(|| {
            anyhow::anyhow!(
                "no pcb archive found for v{} on {}",
                version,
                target_triple()
            )
        })?;
    let actual_sha256 = verify_archive_checksum(&archive_url, &archive)?;

    fs::create_dir_all(downloads_dir())?;
    let download_path = downloads_dir().join(format!("{}-v{}", archive_name, version));
    fs::write(&download_path, &archive)?;

    let temp = tempfile::tempdir()?;
    let archive_path = temp.path().join(&archive_name);
    fs::write(&archive_path, &archive)?;

    let extract_dir = temp.path().join("extract");
    fs::create_dir_all(&extract_dir)?;
    extract_archive(&archive_path, &extract_dir)?;

    let src_binary = find_extracted_binary(&extract_dir)?;
    let install_dir = installed_dir(version);
    let staging_dir = install_dir.with_extension("tmp");
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)?;
    }
    fs::create_dir_all(&staging_dir)?;
    let dst_binary = staging_dir.join(pcbc_binary_name());
    fs::copy(&src_binary, &dst_binary)?;
    copy_executable_permissions(&src_binary, &dst_binary)?;

    let receipt = InstallReceipt {
        version: version.clone(),
        target: target_triple().to_string(),
        url: archive_url,
        sha256: actual_sha256,
        installed_at: isoish_timestamp(),
    };
    fs::write(
        staging_dir.join("receipt.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )?;

    if install_dir.exists() {
        fs::remove_dir_all(&install_dir)?;
    }
    if let Some(parent) = install_dir.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&staging_dir, &install_dir)?;

    Ok(install_dir.join(pcbc_binary_name()))
}

fn download_optional(client: &reqwest::blocking::Client, url: &str) -> Result<Option<Vec<u8>>> {
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(response.error_for_status()?.bytes()?.to_vec()))
}

fn http_client(timeout: Duration) -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()?)
}

fn verify_archive_checksum(url: &str, archive: &[u8]) -> Result<String> {
    let checksum = download_text(&http_client(METADATA_TIMEOUT)?, &format!("{url}.sha256"))?;
    let expected_sha256 = parse_sha256(&checksum)?;
    let actual_sha256 = sha256_hex(archive);
    anyhow::ensure!(
        actual_sha256 == expected_sha256,
        "checksum mismatch for {url}: expected {expected_sha256}, got {actual_sha256}"
    );
    Ok(actual_sha256)
}

fn download_text(client: &reqwest::blocking::Client, url: &str) -> Result<String> {
    Ok(client
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()?
        .error_for_status()?
        .text()?)
}

fn parse_sha256(content: &str) -> Result<String> {
    content
        .split_whitespace()
        .next()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("empty checksum file"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn extract_archive(archive_path: &Path, extract_dir: &Path) -> Result<()> {
    let archive = archive_path.to_string_lossy();
    if archive.ends_with(".zip") {
        let mut zip = zip::ZipArchive::new(fs::File::open(archive_path)?)?;
        zip.extract(extract_dir)?;
    } else {
        let status = Command::new("tar")
            .arg("xf")
            .arg(archive_path)
            .arg("-C")
            .arg(extract_dir)
            .status()?;
        anyhow::ensure!(
            status.success(),
            "failed to extract pcbc archive {}",
            archive_path.display()
        );
    }
    Ok(())
}

fn find_extracted_binary(extract_dir: &Path) -> Result<PathBuf> {
    find_file_named(extract_dir, pcbc_binary_name())
        .or_else(|| find_file_named(extract_dir, legacy_pcb_binary_name()))
        .ok_or_else(|| anyhow::anyhow!("archive did not contain {}", pcbc_binary_name()))
}

fn find_file_named(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.file_name().and_then(|value| value.to_str()) == Some(name) && path.is_file() {
            return Some(path);
        }
        if path.is_dir()
            && let Some(found) = find_file_named(&path, name)
        {
            return Some(found);
        }
    }
    None
}

#[cfg(unix)]
fn copy_executable_permissions(src: &Path, dst: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(src)?.permissions().mode();
    fs::set_permissions(dst, fs::Permissions::from_mode(mode | 0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_executable_permissions(_src: &Path, _dst: &Path) -> Result<()> {
    Ok(())
}

fn toolchain_list() -> Result<()> {
    let installed = installed_toolchains()?;
    if installed.is_empty() {
        println!("No pcbc toolchains installed.");
        return Ok(());
    }

    for (version, binary) in installed {
        println!("{version}\t{}", binary.display());
    }
    Ok(())
}

fn toolchain_show() -> Result<()> {
    let selection = select_toolchain(None)?;
    println!("active: {}", selection.version);
    println!("reason: {}", selection.reason);
    println!("binary: {}", selection.binary.display());
    Ok(())
}

fn toolchain_install(raw: &str) -> Result<()> {
    let request = parse_request(raw)?;
    let version = resolve_remote_version_force(&request)?;
    let binary = ensure_installed(&version)?;
    println!("installed pcbc {version}: {}", binary.display());
    Ok(())
}

fn resolve_remote_version_force(request: &ToolchainRequest) -> Result<Version> {
    let releases = fetch_release_versions(true)?;
    releases
        .into_iter()
        .filter(|version| request_matches(request, version))
        .max()
        .ok_or_else(|| anyhow::anyhow!("no pcbc release found for `{}`", format_request(request)))
}

fn toolchain_uninstall(raw: &str) -> Result<()> {
    let version = Version::parse(raw).with_context(|| format!("invalid exact version `{raw}`"))?;
    let dir = toolchains_dir().join(version.to_string());
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
        println!("uninstalled pcbc {version}");
    } else {
        println!("pcbc {version} is not installed");
    }
    Ok(())
}

fn self_update() -> Result<()> {
    ensure_standalone_install()?;

    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let latest = fetch_latest_release()?;

    if latest.version > current_version {
        install_shim_update(&latest)?;
    }

    let mut requests = BTreeSet::new();
    requests.insert((latest.version.major, latest.version.minor));
    for version in installed_toolchains()?.keys() {
        requests.insert((version.major, version.minor));
    }

    for (major, minor) in requests {
        let request = ToolchainRequest::Lane { major, minor };
        let version = resolve_remote_version_force(&request)?;
        let _ = ensure_installed(&version)?;
    }

    if latest.version > current_version {
        println!(
            "Updated pcb {} → {}",
            current_version.to_string().dimmed(),
            latest.version.to_string().green()
        );
    } else {
        println!("pcb is already up to date.");
    }
    Ok(())
}

fn fetch_latest_release() -> Result<LatestRelease> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(METADATA_TIMEOUT)
        .build()?
        .get(format!("{RELEASE_BASE_URL}/latest.json"))
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()?
        .error_for_status()?
        .json()?)
}

fn install_shim_update(latest: &LatestRelease) -> Result<()> {
    ensure_supported_target()?;

    let archive_name = toolchain_archive_name("pcb");
    let archive_url = format!("{RELEASE_BASE_URL}/{}/{}", latest.tag, archive_name);
    let archive = http_client(ARCHIVE_TIMEOUT)?
        .get(&archive_url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()?
        .error_for_status()?
        .bytes()?;
    verify_archive_checksum(&archive_url, &archive)?;

    let temp = tempfile::tempdir()?;
    let archive_path = temp.path().join(archive_name);
    fs::write(&archive_path, archive)?;
    let extract_dir = temp.path().join("extract");
    fs::create_dir_all(&extract_dir)?;
    extract_archive(&archive_path, &extract_dir)?;
    let binary = find_file_named(&extract_dir, legacy_pcb_binary_name())
        .ok_or_else(|| anyhow::anyhow!("archive did not contain {}", legacy_pcb_binary_name()))?;
    if let Some(pcbc) = find_file_named(&extract_dir, pcbc_binary_name()) {
        let sibling = std::env::current_exe()?
            .parent()
            .ok_or_else(|| anyhow::anyhow!("failed to find current executable directory"))?
            .join(pcbc_binary_name());
        fs::copy(&pcbc, &sibling)?;
        copy_executable_permissions(&pcbc, &sibling)?;
    }
    self_replace::self_replace(binary)?;
    Ok(())
}

fn ensure_standalone_install() -> Result<()> {
    let receipt = fs::read_to_string(standalone_receipt_path())
        .ok()
        .and_then(|content| serde_json::from_str::<StandaloneInstallReceipt>(&content).ok())
        .ok_or_else(|| anyhow::anyhow!(STANDALONE_INSTALL_REQUIRED))?;
    let install_prefix = receipt
        .install_prefix
        .canonicalize()
        .map_err(|_| anyhow::anyhow!(STANDALONE_INSTALL_REQUIRED))?;
    let current_exe = std::env::current_exe()?.canonicalize()?;
    anyhow::ensure!(
        current_exe.starts_with(&install_prefix),
        STANDALONE_INSTALL_REQUIRED
    );

    Ok(())
}

fn exec_toolchain(binary: &Path, args: &[OsString]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(binary).args(args).exec();
        Err(err).with_context(|| format!("failed to exec {}", binary.display()))
    }

    #[cfg(not(unix))]
    {
        let status = Command::new(binary).args(args).status()?;
        if !status.success() {
            match status.code() {
                Some(code) => std::process::exit(code),
                None => anyhow::bail!("{} terminated by signal", binary.display()),
            }
        }
        Ok(())
    }
}

fn sibling_pcbc() -> Option<(Version, PathBuf)> {
    let current = std::env::current_exe().ok()?;
    let sibling = current.parent()?.join(pcbc_binary_name());
    if sibling == current || !sibling.is_file() {
        return None;
    }
    let version = sibling_pcbc_version(&sibling)?;
    Some((version, sibling))
}

fn sibling_pcbc_version(binary: &Path) -> Option<Version> {
    let output = Command::new(binary).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let version = stdout.split_whitespace().last()?;
    Version::parse(version).ok()
}

fn installed_binary_path(version: &Version) -> PathBuf {
    installed_dir(version).join(pcbc_binary_name())
}

fn installed_dir(version: &Version) -> PathBuf {
    toolchains_dir()
        .join(version.to_string())
        .join(target_triple())
}

fn data_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_default()
                    .join("AppData")
                    .join("Local")
            })
            .join("pcb")
    } else {
        dirs::data_local_dir()
            .unwrap_or_else(|| {
                dirs::home_dir()
                    .unwrap_or_default()
                    .join(".local")
                    .join("share")
            })
            .join("pcb")
    }
}

fn toolchains_dir() -> PathBuf {
    data_dir().join("toolchains")
}

fn downloads_dir() -> PathBuf {
    data_dir().join("downloads")
}

fn locks_dir() -> PathBuf {
    data_dir().join("locks")
}

fn release_list_cache_path() -> PathBuf {
    data_dir().join("release-list-cache.json")
}

fn standalone_receipt_path() -> PathBuf {
    let config_dir = if cfg!(windows) {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config"))
    };

    config_dir.join("pcb").join("pcb-receipt.json")
}

fn toolchain_archive_name(binary: &str) -> String {
    let ext = if cfg!(windows) { "zip" } else { "tar.xz" };
    format!("{}-{}.{}", binary, target_triple(), ext)
}

fn pcbc_binary_name() -> &'static str {
    if cfg!(windows) { "pcbc.exe" } else { "pcbc" }
}

fn legacy_pcb_binary_name() -> &'static str {
    if cfg!(windows) { "pcb.exe" } else { "pcb" }
}

fn target_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => "unsupported",
    }
}

fn ensure_supported_target() -> Result<()> {
    anyhow::ensure!(
        target_triple() != "unsupported",
        "unsupported platform: {}-{}",
        std::env::consts::ARCH,
        std::env::consts::OS
    );
    Ok(())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn isoish_timestamp() -> String {
    unix_timestamp().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request_supports_mvp_forms() {
        assert!(matches!(
            parse_request("0.3").unwrap(),
            ToolchainRequest::Lane { major: 0, minor: 3 }
        ));
        assert!(matches!(
            parse_request("0.3.83").unwrap(),
            ToolchainRequest::Exact(version) if version == Version::new(0, 3, 83)
        ));
        assert!(matches!(
            parse_request("latest").unwrap(),
            ToolchainRequest::Latest
        ));
    }

    #[test]
    fn release_listing_parser_extracts_only_version_prefixes() {
        let xml = r#"
            <ListBucketResult>
              <CommonPrefixes><Prefix>pcb/latest/</Prefix></CommonPrefixes>
              <CommonPrefixes><Prefix>pcb/main/</Prefix></CommonPrefixes>
              <CommonPrefixes><Prefix>pcb/v0.3.82/</Prefix></CommonPrefixes>
              <CommonPrefixes><Prefix>pcb/v0.3.83/</Prefix></CommonPrefixes>
              <CommonPrefixes><Prefix>pcb/v0.4.0-beta.1/</Prefix></CommonPrefixes>
            </ListBucketResult>
        "#;

        assert_eq!(
            parse_release_versions(xml),
            vec![
                Version::parse("0.3.82").unwrap(),
                Version::parse("0.3.83").unwrap(),
                Version::parse("0.4.0-beta.1").unwrap(),
            ]
        );
    }

    #[test]
    fn lane_requests_do_not_match_other_lanes_or_prereleases() {
        let request = ToolchainRequest::Lane { major: 0, minor: 3 };

        assert!(request_matches(
            &request,
            &Version::parse("0.3.83").unwrap()
        ));
        assert!(!request_matches(
            &request,
            &Version::parse("0.4.0").unwrap()
        ));
        assert!(!request_matches(
            &request,
            &Version::parse("0.3.84-beta.1").unwrap()
        ));
    }
}
