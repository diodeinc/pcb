use anyhow::{Context, Result, bail};
use clap::Args;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::WorkspaceContext;

const AUTHTYPE_CAPABILITY: &str = "authtype";
const DIODEHUB_CREDENTIAL_HELPER_CONFIG: &str = "credential.https://code.diode.computer.helper";
const DIODEHUB_CREDENTIAL_USE_HTTP_PATH_CONFIG: &str =
    "credential.https://code.diode.computer.useHttpPath";
const DIODEHUB_CREDENTIAL_HELPER: &str = "!pcb auth git";
const DIODEHUB_CREDENTIAL_CACHE_TIMEOUT_SECONDS: u64 = 55 * 60;
const DIODEHUB_HOST: &str = "code.diode.computer";
const GIT_CONFIG_NOT_FOUND: i32 = 5;
const MAX_CREDENTIAL_LINE_BYTES: usize = 65_535;

#[derive(Args, Debug)]
#[command(about = "Configure or provide Git credentials using PCB authentication")]
pub struct GitAuthArgs {
    /// Git credential helper operation, `configure`, or `unconfigure`
    operation: String,
}

#[derive(Debug, Default)]
struct CredentialRequest {
    protocol: Option<Vec<u8>>,
    host: Option<Vec<u8>>,
    path: Option<Vec<u8>>,
    capabilities: Vec<Vec<u8>>,
}

#[derive(Serialize)]
struct GitCredentialExchangeRequest<'a> {
    host: &'a str,
    path: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitCredentialExchangeResponse {
    repository_id: String,
    #[serde(rename = "provider")]
    _provider: GitProvider,
    credential: GitCredential,
    expires_at: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum GitProvider {
    Diodehub,
}

#[derive(Deserialize)]
struct GitCredential {
    #[serde(rename = "scheme")]
    _scheme: GitCredentialScheme,
    token: String,
}

#[derive(Deserialize)]
enum GitCredentialScheme {
    Bearer,
}

struct MintedGitCredential {
    token: String,
    expires_at: u64,
}

pub fn execute(args: GitAuthArgs, ctx: &WorkspaceContext) -> Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();

    match args.operation.as_str() {
        "configure" => configure()?,
        "unconfigure" => unconfigure()?,
        "capability" => {
            writeln!(stdout, "version 0")?;
            writeln!(stdout, "capability {AUTHTYPE_CAPABILITY}")?;
        }
        "get" => {
            let result = read_credential_request(stdin.lock())
                .and_then(|request| provide_credential(ctx, request, &mut stdout));
            if let Err(error) = result {
                writeln!(stdout, "quit=true")?;
                writeln!(stdout)?;
                stdout.flush()?;
                eprintln!("pcb auth git: {error:#}");
            }
        }
        "store" | "erase" => {
            read_credential_request(stdin.lock())?;
        }
        _ => {}
    }

    Ok(())
}

fn configure() -> Result<()> {
    let cache_helper = credential_cache_helper()?;
    run_git_config(&["--replace-all", DIODEHUB_CREDENTIAL_HELPER_CONFIG, ""])?;
    run_git_config(&["--add", DIODEHUB_CREDENTIAL_HELPER_CONFIG, &cache_helper])?;
    run_git_config(&[
        "--add",
        DIODEHUB_CREDENTIAL_HELPER_CONFIG,
        DIODEHUB_CREDENTIAL_HELPER,
    ])?;
    run_git_config(&[
        "--replace-all",
        DIODEHUB_CREDENTIAL_USE_HTTP_PATH_CONFIG,
        "true",
    ])
}

fn unconfigure() -> Result<()> {
    clear_credential_cache();
    unset_git_config(DIODEHUB_CREDENTIAL_HELPER_CONFIG)?;
    unset_git_config(DIODEHUB_CREDENTIAL_USE_HTTP_PATH_CONFIG)
}

fn credential_cache_socket() -> Result<PathBuf> {
    let config_dir = crate::auth::get_auth_dir()?;
    let config_dir = if config_dir.is_absolute() {
        config_dir
    } else {
        std::env::current_dir()
            .context("Failed to resolve PCB config directory")?
            .join(config_dir)
    };
    Ok(config_dir.join("git-credential-cache").join("socket"))
}

fn credential_cache_helper() -> Result<String> {
    let socket = credential_cache_socket()?;
    let socket = socket
        .to_str()
        .context("PCB config directory is not valid UTF-8")?;
    Ok(format!(
        "cache --timeout={DIODEHUB_CREDENTIAL_CACHE_TIMEOUT_SECONDS} --socket={}",
        shell_quote(socket)
    ))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn stop_credential_cache() -> Result<()> {
    let socket = credential_cache_socket()?;
    let mut socket_argument = OsString::from("--socket=");
    socket_argument.push(socket);
    let output = Command::new("git")
        .arg("credential-cache")
        .arg(socket_argument)
        .arg("exit")
        .output()
        .context("Failed to stop Git credential cache")?;
    if !output.status.success() {
        bail!("`git credential-cache exit` failed with {}", output.status);
    }
    Ok(())
}

pub(crate) fn clear_credential_cache() {
    let _ = stop_credential_cache();
}

fn run_git_config(args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(["config", "--global"])
        .args(args)
        .status()
        .context("Failed to run `git config`")?;
    if !status.success() {
        bail!(
            "`git config --global {}` failed with {status}",
            args.join(" ")
        );
    }
    Ok(())
}

fn unset_git_config(key: &str) -> Result<()> {
    let status = Command::new("git")
        .args(["config", "--global", "--unset-all", key])
        .status()
        .context("Failed to run `git config`")?;
    if !status.success() && status.code() != Some(GIT_CONFIG_NOT_FOUND) {
        bail!("`git config --global --unset-all {key}` failed with {status}");
    }
    Ok(())
}

fn provide_credential(
    ctx: &WorkspaceContext,
    request: CredentialRequest,
    output: &mut impl Write,
) -> Result<()> {
    if request.protocol.as_deref() != Some(b"https")
        || request.host.as_deref() != Some(DIODEHUB_HOST.as_bytes())
    {
        return Ok(());
    }

    let Some(path) = request.path.as_deref().filter(|path| !path.is_empty()) else {
        return Ok(());
    };

    if !request
        .capabilities
        .iter()
        .any(|capability| capability == AUTHTYPE_CAPABILITY.as_bytes())
    {
        bail!("Git did not advertise the `authtype` credential capability");
    }

    let path = std::str::from_utf8(path).context("Git credential path is not UTF-8")?;
    let credential = exchange_credential(ctx, DIODEHUB_HOST, path)?;

    writeln!(output, "capability[]={AUTHTYPE_CAPABILITY}")?;
    writeln!(output, "authtype=Bearer")?;
    writeln!(output, "credential={}", credential.token)?;
    writeln!(output, "password_expiry_utc={}", credential.expires_at)?;
    writeln!(output)?;
    output.flush()?;

    Ok(())
}

fn exchange_credential(
    ctx: &WorkspaceContext,
    host: &str,
    path: &str,
) -> Result<MintedGitCredential> {
    let api_token = ctx
        .token()
        .context("Failed to get a valid Diode API token")?;
    let url = format!("{}/api/git/credentials", ctx.api_base_url());
    let client = Client::builder()
        .user_agent(format!("diode-pcb/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to create Git credential HTTP client")?;

    let response = client
        .post(url)
        .bearer_auth(api_token)
        .json(&GitCredentialExchangeRequest { host, path })
        .send()
        .context("Failed to exchange Diode authentication for a Git credential")?;

    if !response.status().is_success() {
        bail!("Git credential exchange failed: {}", response.status());
    }

    let response: GitCredentialExchangeResponse = response
        .json()
        .context("Failed to parse Git credential exchange response")?;
    let GitCredentialExchangeResponse {
        repository_id,
        _provider: _,
        credential,
        expires_at,
    } = response;
    let GitCredential { _scheme: _, token } = credential;
    Uuid::parse_str(&repository_id)
        .context("Git credential exchange returned an invalid repository ID")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is before the Unix epoch")?
        .as_secs();

    if expires_at <= now {
        bail!("Git credential exchange returned an expired credential");
    }
    if token.is_empty() || token.contains(['\r', '\n', '\0']) {
        bail!("Git credential exchange returned an invalid credential");
    }

    Ok(MintedGitCredential { token, expires_at })
}

fn read_credential_request(mut input: impl BufRead) -> Result<CredentialRequest> {
    let mut request = CredentialRequest::default();
    let mut line = Vec::new();

    loop {
        line.clear();
        let bytes_read = input
            .read_until(b'\n', &mut line)
            .context("Failed to read Git credential request")?;
        if bytes_read == 0 {
            break;
        }
        if line.len() > MAX_CREDENTIAL_LINE_BYTES {
            bail!("Git credential request line exceeds 65535 bytes");
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.is_empty() {
            break;
        }
        if line.contains(&b'\0') {
            bail!("Git credential request contains a NUL byte");
        }

        let Some(separator) = line.iter().position(|byte| *byte == b'=') else {
            bail!("Invalid Git credential request line");
        };
        let (key, value) = (&line[..separator], &line[separator + 1..]);

        match key {
            b"protocol" => request.protocol = Some(value.to_vec()),
            b"host" => request.host = Some(value.to_vec()),
            b"path" => request.path = Some(value.to_vec()),
            b"capability[]" if value.is_empty() => request.capabilities.clear(),
            b"capability[]" => request.capabilities.push(value.to_vec()),
            _ => {}
        }
    }

    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_known_attributes_and_ignores_extensions() {
        let request = read_credential_request(Cursor::new(
            b"capability[]=authtype\nprotocol=https\nhost=code.diode.computer\npath=acme/widget.git\nwwwauth[]=Bearer\n\n",
        ))
        .unwrap();

        assert_eq!(request.protocol.as_deref(), Some(b"https".as_slice()));
        assert_eq!(
            request.host.as_deref(),
            Some(b"code.diode.computer".as_slice())
        );
        assert_eq!(request.path.as_deref(), Some(b"acme/widget.git".as_slice()));
        assert_eq!(request.capabilities, [b"authtype".to_vec()]);
    }

    #[test]
    fn empty_capability_resets_the_capability_list() {
        let request = read_credential_request(Cursor::new(
            b"capability[]=authtype\ncapability[]=\ncapability[]=state\n\n",
        ))
        .unwrap();

        assert_eq!(request.capabilities, [b"state".to_vec()]);
    }

    #[test]
    fn quotes_credential_cache_socket_for_the_shell() {
        assert_eq!(
            shell_quote("/tmp/PCB's cache/socket"),
            "'/tmp/PCB'\\''s cache/socket'"
        );
    }

    #[test]
    fn accepts_a_line_at_the_protocol_limit() {
        let mut input = b"extension=".to_vec();
        input.resize(MAX_CREDENTIAL_LINE_BYTES - 1, b'x');
        input.push(b'\n');

        let request = read_credential_request(Cursor::new(input)).unwrap();

        assert!(request.protocol.is_none());
    }

    #[test]
    fn rejects_lines_longer_than_the_protocol_limit() {
        let input = vec![b'x'; MAX_CREDENTIAL_LINE_BYTES + 1];
        let error = read_credential_request(Cursor::new(input)).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Git credential request line exceeds 65535 bytes")
        );
    }
}
