use anyhow::{Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RELEASE_LIST_URL: &str = "https://pcb.api.diode.computer/";
const RELEASE_BASE_URL: &str = "https://pcb.api.diode.computer/pcb";
const SHIM_LATEST_RELEASE_URL: &str = "https://pcb.api.diode.computer/pcb/pcb-latest.json";
const NIGHTLY_LATEST_RELEASE_URL: &str = "https://pcb.api.diode.computer/pcb/nightly/latest.json";
const USER_AGENT: &str = "pcb";
const METADATA_TIMEOUT: Duration = Duration::from_secs(10);
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const RELEASE_LIST_CACHE_TTL: Duration = Duration::from_secs(60 * 60);

enum ShimCommand {
    SelfUpdate,
    ToolchainList,
    ToolchainShow,
    ToolchainInstall(String),
    ToolchainUninstall(String),
}

#[derive(Debug, Clone)]
enum ToolchainRequest {
    Lane { major: u64, minor: u64 },
    Exact(Version),
    Latest,
    Nightly,
}

#[derive(Debug, Clone)]
struct ResolvedToolchain {
    label: String,
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

#[derive(Debug, Clone, Deserialize)]
struct NightlyRelease {
    date: String,
    sha: String,
    base_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NightlyReceipt {
    channel: String,
    date: String,
    sha: String,
    target: String,
    url: String,
    sha256: String,
    installed_at: String,
}

enum DownloadKind {
    Binary,
    Archive,
}

struct Download {
    name: String,
    url: String,
    bytes: Vec<u8>,
    kind: DownloadKind,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        for cause in e.chain().skip(1) {
            eprintln!("  {cause}");
        }
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();

    if is_shim_command(&args) {
        return execute_shim(parse_shim_command(&args)?);
    }

    let override_request = take_cli_override(&mut args)?;
    let selection = select_toolchain(override_request, is_help_request(&args))?;
    exec_toolchain(&selection.binary, &args)
}

fn is_help_request(args: &[OsString]) -> bool {
    matches!(
        args.first().and_then(|arg| arg.to_str()),
        None | Some("--help" | "-h" | "help")
    )
}

fn is_shim_command(args: &[OsString]) -> bool {
    matches!(
        args.first().and_then(|arg| arg.to_str()),
        Some("self" | "toolchain")
    )
}

fn parse_shim_command(args: &[OsString]) -> Result<ShimCommand> {
    let strings: Vec<&str> = args
        .iter()
        .map(|arg| {
            arg.to_str()
                .ok_or_else(|| anyhow::anyhow!("shim commands must be valid UTF-8"))
        })
        .collect::<Result<_>>()?;

    match strings.as_slice() {
        ["self", "update"] => Ok(ShimCommand::SelfUpdate),
        ["toolchain", "list"] => Ok(ShimCommand::ToolchainList),
        ["toolchain", "show"] => Ok(ShimCommand::ToolchainShow),
        ["toolchain", "install", request] => Ok(ShimCommand::ToolchainInstall((*request).into())),
        ["toolchain", "uninstall", version] => {
            Ok(ShimCommand::ToolchainUninstall((*version).into()))
        }
        ["self", "--help" | "-h" | "help"] => {
            println!("Usage: pcb self update");
            std::process::exit(0);
        }
        ["toolchain", "--help" | "-h" | "help"] => {
            println!(
                "Usage:\n  pcb toolchain list\n  pcb toolchain show\n  pcb toolchain install <request>\n  pcb toolchain uninstall <version>"
            );
            std::process::exit(0);
        }
        ["self", ..] => anyhow::bail!("usage: pcb self update"),
        ["toolchain", ..] => {
            anyhow::bail!("usage: pcb toolchain <list|show|install|uninstall> [request|version]")
        }
        _ => anyhow::bail!("unknown shim command"),
    }
}

fn execute_shim(command: ShimCommand) -> Result<()> {
    match command {
        ShimCommand::SelfUpdate => self_update(),
        ShimCommand::ToolchainList => toolchain_list(),
        ShimCommand::ToolchainShow => toolchain_show(),
        ShimCommand::ToolchainInstall(request) => toolchain_install(&request),
        ShimCommand::ToolchainUninstall(version) => toolchain_uninstall(&version),
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
        anyhow::bail!("empty toolchain override; expected +0.3, +0.3.83, +latest, or +nightly");
    }
    let request = parse_request(raw)?;
    args.remove(0);
    Ok(Some(request))
}

fn parse_request(raw: &str) -> Result<ToolchainRequest> {
    if raw == "latest" {
        return Ok(ToolchainRequest::Latest);
    }
    if raw == "nightly" {
        return Ok(ToolchainRequest::Nightly);
    }

    if let Ok(version) = Version::parse(raw) {
        return Ok(ToolchainRequest::Exact(version));
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
        _ => {
            anyhow::bail!(
                "invalid pcb toolchain request `{raw}`; expected 0.3, 0.3.83, latest, or nightly"
            )
        }
    }
}

fn select_toolchain(
    override_request: Option<ToolchainRequest>,
    prefer_local: bool,
) -> Result<ResolvedToolchain> {
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

    resolve_request(&request, reason, prefer_local)
}

fn resolve_request(
    request: &ToolchainRequest,
    reason: String,
    prefer_local: bool,
) -> Result<ResolvedToolchain> {
    if matches!(request, ToolchainRequest::Nightly) {
        return resolve_nightly(reason);
    }

    if matches!(request, ToolchainRequest::Latest) {
        if prefer_local && let Some(local) = best_local_toolchain(request)? {
            return Ok(ResolvedToolchain {
                label: local.0.to_string(),
                binary: local.1,
                reason,
            });
        }

        match resolve_remote_version(request, false).and_then(|version| {
            let binary = ensure_installed(&version)?;
            Ok((version, binary))
        }) {
            Ok((version, binary)) => {
                return Ok(ResolvedToolchain {
                    label: version.to_string(),
                    binary,
                    reason,
                });
            }
            Err(remote_error) => {
                if let Some(local) = best_local_toolchain(request)? {
                    eprintln!(
                        "Warning: failed to check latest release ({remote_error}); using installed pcbc {}",
                        local.0
                    );
                    return Ok(ResolvedToolchain {
                        label: local.0.to_string(),
                        binary: local.1,
                        reason,
                    });
                }
                return Err(remote_error);
            }
        }
    }

    if let Some(local) = best_local_toolchain(request)? {
        return Ok(ResolvedToolchain {
            label: local.0.to_string(),
            binary: local.1,
            reason,
        });
    }

    let version = resolve_remote_version(request, false)?;
    let binary = ensure_installed(&version)?;
    Ok(ResolvedToolchain {
        label: version.to_string(),
        binary,
        reason,
    })
}

fn resolve_nightly(reason: String) -> Result<ResolvedToolchain> {
    match fetch_nightly_release().and_then(|release| ensure_nightly_installed(&release)) {
        Ok((receipt, binary)) => Ok(ResolvedToolchain {
            label: format!("nightly {} ({})", receipt.date, short_sha(&receipt.sha)),
            binary,
            reason,
        }),
        Err(remote_error) => {
            if let Some((receipt, binary)) = installed_nightly_toolchain()? {
                eprintln!(
                    "Warning: failed to check nightly release ({remote_error}); using installed pcbc nightly {} ({})",
                    receipt.date,
                    short_sha(&receipt.sha)
                );
                return Ok(ResolvedToolchain {
                    label: format!("nightly {} ({})", receipt.date, short_sha(&receipt.sha)),
                    binary,
                    reason,
                });
            }
            Err(remote_error)
        }
    }
}

fn best_local_toolchain(request: &ToolchainRequest) -> Result<Option<(Version, PathBuf)>> {
    let mut candidates = installed_toolchains()?;

    if let Some((version, binary)) = sibling_pcbc() {
        candidates.insert(version, binary);
    }

    Ok(candidates
        .into_iter()
        .rfind(|(version, _)| request_matches(request, version)))
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
        ToolchainRequest::Nightly => false,
    }
}

fn format_request(request: &ToolchainRequest) -> String {
    match request {
        ToolchainRequest::Lane { major, minor } => format!("{major}.{minor}"),
        ToolchainRequest::Exact(version) => version.to_string(),
        ToolchainRequest::Latest => "latest".to_string(),
        ToolchainRequest::Nightly => "nightly".to_string(),
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
    let body = download_text(&http_client(METADATA_TIMEOUT)?, &url)?;

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

        let raw = prefix
            .strip_prefix("pcb/v")
            .and_then(|value| value.strip_suffix('/'));
        let Some(raw) = raw else { continue };
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
    Ok(serde_json::from_str(&content).ok())
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

fn ensure_nightly_installed(release: &NightlyRelease) -> Result<(NightlyReceipt, PathBuf)> {
    ensure_supported_target()?;

    if let Some((receipt, binary)) = installed_nightly_toolchain()?
        && receipt.sha == release.sha
    {
        return Ok((receipt, binary));
    }

    let lock_path = locks_dir().join(format!("install-nightly-{}.lock", target_triple()));
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut lock = fslock::LockFile::open(&lock_path)?;
    lock.lock()?;
    let result = if let Some((receipt, binary)) = installed_nightly_toolchain()?
        && receipt.sha == release.sha
    {
        Ok((receipt, binary))
    } else {
        install_nightly(release)
    };
    lock.unlock()?;
    result
}

fn install_nightly(release: &NightlyRelease) -> Result<(NightlyReceipt, PathBuf)> {
    eprintln!(
        "Installing pcbc nightly {} ({}, {})...",
        release.date,
        short_sha(&release.sha),
        target_triple()
    );

    let name = binary_artifact_name("pcbc");
    let url = format!("{}/{}", release.base_url.trim_end_matches('/'), name);
    let bytes = download_artifact_bytes(&http_client(ARCHIVE_TIMEOUT)?, &url)?;
    let actual_sha256 = verify_checksum(&url, &bytes)?;

    fs::create_dir_all(downloads_dir())?;
    fs::write(
        downloads_dir().join(format!("{}-nightly-{}", name, release.sha)),
        &bytes,
    )?;

    let install_dir = nightly_dir();
    let staging_dir = install_dir.with_extension("tmp");
    if staging_dir.exists() {
        fs::remove_dir_all(&staging_dir)?;
    }
    fs::create_dir_all(&staging_dir)?;
    let binary = staging_dir.join(pcbc_binary_name());
    fs::write(&binary, bytes)?;
    copy_executable_permissions(&binary, &binary)?;

    let receipt = NightlyReceipt {
        channel: "nightly".to_string(),
        date: release.date.clone(),
        sha: release.sha.clone(),
        target: target_triple().to_string(),
        url,
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

    Ok((receipt, install_dir.join(pcbc_binary_name())))
}

fn installed_nightly_toolchain() -> Result<Option<(NightlyReceipt, PathBuf)>> {
    let binary = nightly_dir().join(pcbc_binary_name());
    if !binary.is_file() {
        return Ok(None);
    }
    let receipt_path = nightly_dir().join("receipt.json");
    let Ok(content) = fs::read_to_string(receipt_path) else {
        return Ok(None);
    };
    let receipt: NightlyReceipt = serde_json::from_str(&content)?;
    Ok(Some((receipt, binary)))
}

fn install_toolchain(version: &Version) -> Result<PathBuf> {
    eprintln!("Installing pcbc {version} ({})...", target_triple());

    let download = download_toolchain(version)?;
    let actual_sha256 = verify_checksum(&download.url, &download.bytes)?;

    fs::create_dir_all(downloads_dir())?;
    let download_path = downloads_dir().join(format!("{}-v{}", download.name, version));
    fs::write(&download_path, &download.bytes)?;

    let temp = tempfile::tempdir()?;
    let src_binary = match download.kind {
        DownloadKind::Binary => {
            let path = temp.path().join(pcbc_binary_name());
            fs::write(&path, &download.bytes)?;
            path
        }
        DownloadKind::Archive => {
            let archive_path = temp.path().join(&download.name);
            fs::write(&archive_path, &download.bytes)?;
            let extract_dir = temp.path().join("extract");
            fs::create_dir_all(&extract_dir)?;
            extract_archive(&archive_path, &extract_dir)?;
            find_extracted_binary(&extract_dir)?
        }
    };
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
        url: download.url,
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

fn download_toolchain(version: &Version) -> Result<Download> {
    let client = http_client(ARCHIVE_TIMEOUT)?;

    for binary in ["pcbc", "pcb"] {
        let name = binary_artifact_name(binary);
        let url = format!("{RELEASE_BASE_URL}/v{version}/{name}");
        if let Some(bytes) = download_optional_artifact(&client, &url)? {
            return Ok(Download {
                name,
                url,
                bytes,
                kind: DownloadKind::Binary,
            });
        }
    }

    for binary in ["pcbc", "pcb"] {
        let name = toolchain_archive_name(binary);
        let url = format!("{RELEASE_BASE_URL}/v{version}/{name}");
        if let Some(bytes) = download_optional(&client, &url)? {
            return Ok(Download {
                name,
                url,
                bytes,
                kind: DownloadKind::Archive,
            });
        }
    }

    anyhow::bail!(
        "no pcbc or legacy pcb binary found for v{} on {}",
        version,
        target_triple()
    )
}

fn download_optional(client: &ureq::Agent, url: &str) -> Result<Option<Vec<u8>>> {
    match client.get(url).header("User-Agent", USER_AGENT).call() {
        Ok(mut response) => Ok(Some(read_download_bytes(response.body_mut())?)),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn download_optional_artifact(client: &ureq::Agent, url: &str) -> Result<Option<Vec<u8>>> {
    let compressed_url = format!("{url}.zst");
    if let Some(compressed) = download_optional(client, &compressed_url)? {
        return Ok(Some(decompress_zstd(&compressed_url, compressed)?));
    }
    download_optional(client, url)
}

fn download_artifact_bytes(client: &ureq::Agent, url: &str) -> Result<Vec<u8>> {
    download_optional_artifact(client, url)?.ok_or_else(|| anyhow::anyhow!("not found: {url}"))
}

fn read_download_bytes(body: &mut ureq::Body) -> Result<Vec<u8>> {
    Ok(body.with_config().limit(MAX_DOWNLOAD_BYTES).read_to_vec()?)
}

fn decompress_zstd(url: &str, bytes: Vec<u8>) -> Result<Vec<u8>> {
    let decoder = zstd::stream::read::Decoder::new(Cursor::new(bytes))
        .with_context(|| format!("failed to decompress {url}"))?;
    let mut limited = decoder.take(MAX_DOWNLOAD_BYTES + 1);
    let mut decompressed = Vec::new();
    limited
        .read_to_end(&mut decompressed)
        .with_context(|| format!("failed to decompress {url}"))?;
    anyhow::ensure!(
        decompressed.len() <= MAX_DOWNLOAD_BYTES as usize,
        "decompressed artifact exceeds maximum size: {url}"
    );
    Ok(decompressed)
}

fn http_client(timeout: Duration) -> Result<ureq::Agent> {
    Ok(ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .build(),
        )
        .timeout_global(Some(timeout))
        .build()
        .into())
}

fn verify_checksum(url: &str, bytes: &[u8]) -> Result<String> {
    let checksum = download_text(&http_client(METADATA_TIMEOUT)?, &format!("{url}.sha256"))?;
    let expected_sha256 = parse_sha256(&checksum)?;
    let actual_sha256 = sha256_hex(bytes);
    anyhow::ensure!(
        actual_sha256 == expected_sha256,
        "checksum mismatch for {url}: expected {expected_sha256}, got {actual_sha256}"
    );
    Ok(actual_sha256)
}

fn download_text(client: &ureq::Agent, url: &str) -> Result<String> {
    Ok(client
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()?
        .body_mut()
        .read_to_string()?)
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
    let nightly = installed_nightly_toolchain()?;
    if installed.is_empty() && nightly.is_none() {
        println!("No pcbc toolchains installed.");
        return Ok(());
    }

    for (version, binary) in installed {
        println!("{version}\t{}", binary.display());
    }
    if let Some((receipt, binary)) = nightly {
        println!(
            "nightly\t{} ({})\t{}",
            receipt.date,
            short_sha(&receipt.sha),
            binary.display()
        );
    }
    Ok(())
}

fn toolchain_show() -> Result<()> {
    let selection = select_toolchain(None, false)?;
    println!("shim: {}", env!("CARGO_PKG_VERSION"));
    println!("active: {}", selection.label);
    println!("reason: {}", selection.reason);
    println!("binary: {}", selection.binary.display());
    Ok(())
}

fn toolchain_install(raw: &str) -> Result<()> {
    let request = parse_request(raw)?;
    if matches!(request, ToolchainRequest::Nightly) {
        let release = fetch_nightly_release()?;
        let (receipt, binary) = ensure_nightly_installed(&release)?;
        println!(
            "installed pcbc nightly {} ({}): {}",
            receipt.date,
            short_sha(&receipt.sha),
            binary.display()
        );
        return Ok(());
    }
    let version = resolve_remote_version(&request, true)?;
    let binary = ensure_installed(&version)?;
    println!("installed pcbc {version}: {}", binary.display());
    Ok(())
}

fn resolve_remote_version(request: &ToolchainRequest, force_refresh: bool) -> Result<Version> {
    let releases = fetch_release_versions(force_refresh)?;
    releases
        .into_iter()
        .filter(|version| request_matches(request, version))
        .max()
        .ok_or_else(|| anyhow::anyhow!("no pcbc release found for `{}`", format_request(request)))
}

fn toolchain_uninstall(raw: &str) -> Result<()> {
    if raw == "nightly" {
        let dir = toolchains_dir().join("nightly");
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
            println!("uninstalled pcbc nightly");
        } else {
            println!("pcbc nightly is not installed");
        }
        return Ok(());
    }

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
    let current_version = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let mut updated_shim = None;
    match fetch_latest_release() {
        Ok(latest) if latest.version > current_version => {
            let version = latest.version.clone();
            install_shim_update(&latest)?;
            updated_shim = Some(version);
        }
        Ok(_) => {}
        Err(err) => eprintln!(
            "Warning: failed to check latest pcb shim release ({err}); continuing with pcbc updates"
        ),
    }

    let mut requests = BTreeSet::new();
    let stable_result: Result<Vec<(Version, Version, PathBuf)>> = (|| {
        let releases = fetch_release_versions(true)?;
        let installed = installed_toolchains()?;
        let latest_toolchain = releases
            .iter()
            .filter(|version| version.pre.is_empty())
            .max()
            .ok_or_else(|| anyhow::anyhow!("no pcbc releases found"))?;
        requests.insert((latest_toolchain.major, latest_toolchain.minor));
        for version in installed.keys().filter(|version| version.pre.is_empty()) {
            requests.insert((version.major, version.minor));
        }

        let mut changelogs = Vec::new();
        for (major, minor) in requests {
            let request = ToolchainRequest::Lane { major, minor };
            let version = releases
                .iter()
                .filter(|version| request_matches(&request, version))
                .max()
                .ok_or_else(|| {
                    anyhow::anyhow!("no pcbc release found for `{}`", format_request(&request))
                })?;
            let previous = installed
                .keys()
                .filter(|installed| request_matches(&request, installed))
                .max()
                .cloned();
            let binary = ensure_installed(version)?;
            if let Some(previous) = previous
                && previous < *version
            {
                changelogs.push((previous, version.clone(), binary));
            }
        }
        Ok(changelogs)
    })();
    match stable_result {
        Ok(changelogs) => {
            for (from, to, binary) in changelogs {
                let selector = format!("{from}..{to}");
                match Command::new(binary).args(["changelog", &selector]).status() {
                    Ok(status) if status.success() => {}
                    Ok(status) => eprintln!("Warning: failed to print pcbc changelog ({status})"),
                    Err(err) => eprintln!("Warning: failed to print pcbc changelog ({err})"),
                }
            }
        }
        Err(err) => eprintln!("Warning: failed to update managed pcbc toolchains ({err})"),
    }

    if installed_nightly_toolchain()?.is_some()
        && let Err(err) =
            fetch_nightly_release().and_then(|nightly| ensure_nightly_installed(&nightly))
    {
        eprintln!("Warning: failed to update installed nightly toolchain ({err})");
    }

    if let Some(version) = updated_shim {
        println!("Updated pcb {current_version} → {version}");
    }
    Ok(())
}

fn fetch_latest_release() -> Result<LatestRelease> {
    let content = download_text(&http_client(METADATA_TIMEOUT)?, SHIM_LATEST_RELEASE_URL)?;
    Ok(serde_json::from_str(&content)?)
}

fn fetch_nightly_release() -> Result<NightlyRelease> {
    let content = download_text(&http_client(METADATA_TIMEOUT)?, NIGHTLY_LATEST_RELEASE_URL)?;
    Ok(serde_json::from_str(&content)?)
}

fn install_shim_update(latest: &LatestRelease) -> Result<()> {
    ensure_supported_target()?;

    let client = http_client(ARCHIVE_TIMEOUT)?;
    let temp = tempfile::tempdir()?;
    let binary_name = binary_artifact_name("pcb");
    let binary_url = format!("{RELEASE_BASE_URL}/{}/{}", latest.tag, binary_name);
    let bytes = download_artifact_bytes(&client, &binary_url)?;
    verify_checksum(&binary_url, &bytes)?;
    let binary = temp.path().join(legacy_pcb_binary_name());
    fs::write(&binary, bytes)?;
    copy_executable_permissions(&binary, &binary)?;
    self_replace::self_replace(binary)?;
    Ok(())
}

fn exec_toolchain(binary: &Path, args: &[OsString]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = Command::new(binary).arg0("pcb").args(args).exec();
        Err(err).with_context(|| format!("failed to exec {}", binary.display()))
    }

    #[cfg(not(unix))]
    {
        let status = Command::new(binary)
            .env("PCB_SHIM_ARG0", "pcb")
            .args(args)
            .status()?;
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

fn nightly_dir() -> PathBuf {
    toolchains_dir().join("nightly").join(target_triple())
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
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

fn binary_artifact_name(binary: &str) -> String {
    let ext = if cfg!(windows) { ".exe" } else { "" };
    format!("{}-{}{}", binary, target_triple(), ext)
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
    fn release_listing_parser_extracts_only_version_prefixes() {
        let xml = r#"
            <ListBucketResult>
              <CommonPrefixes><Prefix>pcb/latest/</Prefix></CommonPrefixes>
              <CommonPrefixes><Prefix>pcb/nightly/</Prefix></CommonPrefixes>
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
}
