use anyhow::{Context, Result, bail};
use clap::Args;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use url::Url;

use crate::WorkspaceContext;

const AUTHTYPE_CAPABILITY: &str = "authtype";
const DEFAULT_DIODEHUB_HOST: &str = "code.diode.computer";
const MAX_CREDENTIAL_LINE_BYTES: usize = 65_535;

#[derive(Args, Debug)]
#[command(about = "Configure or provide Git credentials using PCB authentication")]
pub struct GitAuthArgs {
    /// DiodeHub host assigned to this credential helper
    #[arg(long, hide = true)]
    host: Option<String>,

    /// Git credential helper operation, `configure`, or `unconfigure`
    operation: String,

    /// DiodeHub HTTPS repository URL to configure
    repository_url: Option<String>,
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

    match args.operation.as_str() {
        "configure" => {
            let repository_url = args.repository_url.as_deref().context(
                "Missing DiodeHub repository URL; use `pcb auth git configure <repository-url>`",
            )?;
            pcb_zen::git::configure_diodehub_credentials_globally(repository_url)?;
        }
        "unconfigure" => pcb_zen::git::unconfigure_diodehub_credentials_globally()?,
        "capability" => {
            pcb_ui::write_stdout(|stdout| {
                writeln!(stdout, "version 0")?;
                writeln!(stdout, "capability {AUTHTYPE_CAPABILITY}")
            })?;
        }
        "get" => {
            let host = args.host.as_deref().unwrap_or(DEFAULT_DIODEHUB_HOST);
            let result = read_credential_request(stdin.lock())
                .and_then(|request| provide_credential(ctx, host, request));
            if let Err(error) = result {
                pcb_ui::write_stdout(|stdout| {
                    writeln!(stdout, "quit=true")?;
                    writeln!(stdout)
                })?;
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

fn provide_credential(
    ctx: &WorkspaceContext,
    configured_host: &str,
    request: CredentialRequest,
) -> Result<()> {
    if request.protocol.as_deref() != Some(b"https") {
        return Ok(());
    }
    let Some(request_host) = request.host.as_deref() else {
        return Ok(());
    };
    if credential_hostname(request_host)? != configured_host {
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
    let credential = exchange_credential(ctx, configured_host, path)?;

    pcb_ui::write_stdout(|output| {
        writeln!(output, "capability[]={AUTHTYPE_CAPABILITY}")?;
        writeln!(output, "authtype=Bearer")?;
        writeln!(output, "credential={}", credential.token)?;
        writeln!(output, "password_expiry_utc={}", credential.expires_at)?;
        writeln!(output)
    })?;

    Ok(())
}

fn credential_hostname(authority: &[u8]) -> Result<String> {
    let authority = std::str::from_utf8(authority).context("Git credential host is not UTF-8")?;
    let url =
        Url::parse(&format!("https://{authority}")).context("Git credential host is invalid")?;
    Ok(url
        .host_str()
        .context("Git credential host is invalid")?
        .to_owned())
}

fn exchange_credential(
    ctx: &WorkspaceContext,
    host: &str,
    path: &str,
) -> Result<MintedGitCredential> {
    let url = format!("{}/api/git/credentials", ctx.api_base_url());
    let client = Client::builder()
        .user_agent(format!("diode-pcb/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to create Git credential HTTP client")?;

    let request = client
        .post(url)
        .json(&GitCredentialExchangeRequest { host, path });

    let response = crate::auth::apply_api_auth_with_context(ctx, request)?
        .send()
        .context("Failed to exchange Diode authentication for a Git credential")?;

    if !response.status().is_success() {
        bail!("Git credential exchange failed: {}", response.status());
    }

    let response: GitCredentialExchangeResponse = response
        .json()
        .context("Failed to parse Git credential exchange response")?;
    let GitCredentialExchangeResponse {
        _provider: _,
        credential,
        expires_at,
    } = response;
    let GitCredential { _scheme: _, token } = credential;
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

    #[test]
    fn normalizes_credential_hostnames() {
        for (authority, hostname) in [
            ("code.example.com:443", "code.example.com"),
            ("code.example.com:8443", "code.example.com"),
            ("[2001:db8::1:2]", "[2001:db8::1:2]"),
            ("[2001:db8::1:2]:8443", "[2001:db8::1:2]"),
        ] {
            assert_eq!(credential_hostname(authority.as_bytes()).unwrap(), hostname);
        }
    }
}
