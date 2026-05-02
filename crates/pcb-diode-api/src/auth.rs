use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use clap::{Args, Subcommand};
use fslock::LockFile;
use rand::Rng;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;

use crate::WorkspaceContext;
use crate::aws_auth::{self, AwsDiodeToken};

const NOT_AUTHENTICATED_MESSAGE: &str = "Not authenticated. Run `pcb auth login` to authenticate.";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub email: Option<String>,
}

impl AuthTokens {
    pub fn is_expired(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        self.expires_at - now < 300
    }

    pub fn time_until_expiry(&self) -> String {
        time_until_expiry(self.expires_at)
    }
}

fn time_until_expiry(expires_at: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let remaining = expires_at - now;

    if remaining <= 0 {
        "expired".to_string()
    } else if remaining < 3600 {
        format!("{} minutes", remaining / 60)
    } else if remaining < 86400 {
        format!("{} hours", remaining / 3600)
    } else {
        format!("{} days", remaining / 86400)
    }
}

impl WorkspaceContext {
    pub fn token(&self) -> Result<String> {
        get_valid_token_with_context(self)
    }
}

fn get_auth_dir() -> Result<PathBuf> {
    let pcb_dir = if let Ok(config_dir) = std::env::var("PCB_CONFIG_DIR") {
        PathBuf::from(config_dir)
    } else {
        let home_dir = dirs::home_dir().context("Failed to get home directory")?;
        home_dir.join(".pcb")
    };
    fs::create_dir_all(&pcb_dir)?;
    Ok(pcb_dir)
}

fn get_auth_file_path(ctx: &WorkspaceContext) -> Result<PathBuf> {
    let auth_dir = get_auth_dir()?;
    if ctx.use_legacy_auth_file() {
        return Ok(auth_dir.join("auth.toml"));
    }

    let scoped_dir = auth_dir.join("auth");
    fs::create_dir_all(&scoped_dir)?;
    let slug = crate::endpoint::auth_scope_slug(ctx.api_base_url());
    Ok(scoped_dir.join(format!("{slug}.toml")))
}

fn load_tokens_with_context(ctx: &WorkspaceContext) -> Result<Option<AuthTokens>> {
    let path = get_auth_file_path(ctx)?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path)?;
    Ok(Some(toml::from_str(&contents)?))
}

pub fn load_tokens() -> Result<Option<AuthTokens>> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    load_tokens_with_context(&ctx)
}

fn save_tokens(
    ctx: &WorkspaceContext,
    access_token: &str,
    refresh_token: &str,
    expires_at: i64,
    email: Option<&str>,
) -> Result<()> {
    let tokens = AuthTokens {
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        expires_at,
        email: email.map(|s| s.to_string()),
    };
    let contents = toml::to_string(&tokens)?;

    let auth_path = get_auth_file_path(ctx)?;
    AtomicFile::new(&auth_path, OverwriteBehavior::AllowOverwrite)
        .write(|f| {
            f.write_all(contents.as_bytes())?;
            f.flush()
        })
        .map_err(|err| anyhow::anyhow!("Failed to write auth tokens: {err}"))?;

    Ok(())
}

fn clear_tokens_with_context(ctx: &WorkspaceContext) -> Result<()> {
    let path = get_auth_file_path(ctx)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct RefreshRequest {
    refresh_token: String,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
}

fn refresh_tokens_with_context(ctx: &WorkspaceContext) -> Result<AuthTokens> {
    let lock_path = get_auth_file_path(ctx)?.with_extension("toml.lock");
    let mut lock = LockFile::open(&lock_path)?;
    lock.lock()?;

    let tokens = load_tokens_with_context(ctx)?.context("No tokens to refresh")?;
    if !tokens.is_expired() {
        return Ok(tokens);
    }

    let url = format!("{}/api/auth/refresh", ctx.api_base_url());
    let response = Client::new()
        .post(&url)
        .json(&RefreshRequest {
            refresh_token: tokens.refresh_token.clone(),
        })
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("Token refresh failed: {}", response.status());
    }

    let refresh_response: RefreshResponse = response.json()?;

    save_tokens(
        ctx,
        &refresh_response.access_token,
        &refresh_response.refresh_token,
        refresh_response.expires_at,
        tokens.email.as_deref(),
    )?;

    Ok(AuthTokens {
        access_token: refresh_response.access_token,
        refresh_token: refresh_response.refresh_token,
        expires_at: refresh_response.expires_at,
        email: tokens.email,
    })
}

pub fn refresh_tokens() -> Result<AuthTokens> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    refresh_tokens_with_context(&ctx)
}

pub fn get_valid_token_with_context(ctx: &WorkspaceContext) -> Result<String> {
    get_valid_token_with_sources(
        ctx,
        aws_auth::get_service_token,
        refresh_tokens_with_context,
    )
}

fn get_valid_token_with_sources(
    ctx: &WorkspaceContext,
    aws_service_token: impl Fn(&WorkspaceContext) -> Result<AwsDiodeToken>,
    refresh_tokens: impl Fn(&WorkspaceContext) -> Result<AuthTokens>,
) -> Result<String> {
    let aws_token = || {
        aws_service_token(ctx)
            .map(|token| token.access_token)
            .map_err(|_| anyhow::anyhow!(NOT_AUTHENTICATED_MESSAGE))
    };

    let Some(tokens) = load_tokens_with_context(ctx)? else {
        return aws_token();
    };

    if !tokens.is_expired() {
        return Ok(tokens.access_token);
    }

    match refresh_tokens(ctx) {
        Ok(new_tokens) => Ok(new_tokens.access_token),
        Err(_) => aws_token(),
    }
}

pub fn get_valid_token() -> Result<String> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    get_valid_token_with_context(&ctx)
}

pub fn login_with_context(ctx: &WorkspaceContext) -> Result<()> {
    let code: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(6)
        .map(char::from)
        .collect::<String>()
        .to_uppercase();

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{}/callback", port);

    let auth_url = format!(
        "{}/cli-auth?code={}&redirect_uri={}",
        ctx.web_base_url(),
        code,
        urlencoding::encode(&redirect_uri)
    );

    println!("Code: {}", code);
    println!("Opening browser...");

    if let Err(e) = open::that(&auth_url) {
        eprintln!("Failed to open browser: {}", e);
        eprintln!("Please manually open: {}", auth_url);
    }

    let (mut stream, _) = listener.accept()?;

    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let tokens = parse_tokens_from_request(&request_line)?;

    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\n\r\n",
        ctx.web_base_url()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()?;

    save_tokens(
        ctx,
        &tokens.access_token,
        &tokens.refresh_token,
        tokens.expires_at,
        tokens.email.as_deref(),
    )?;

    println!("✓ Authentication successful!");
    if let Some(email) = &tokens.email {
        println!("  Logged in as: {}", email);
    }

    Ok(())
}

pub fn login() -> Result<()> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    login_with_context(&ctx)
}

pub fn logout_with_context(ctx: &WorkspaceContext) -> Result<()> {
    clear_tokens_with_context(ctx)?;
    aws_auth::clear_service_token(ctx)?;
    println!("✓ Logged out successfully");
    Ok(())
}

pub fn logout() -> Result<()> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    logout_with_context(&ctx)
}

pub fn status_with_context(ctx: &WorkspaceContext) -> Result<()> {
    println!("Authentication Status:");
    match load_tokens_with_context(ctx)? {
        Some(tokens) => {
            println!("  Status: Logged in");
            if let Some(email) = &tokens.email {
                println!("  Email: {}", email);
            }
            if tokens.is_expired() {
                println!("  Token: expired");
                println!("\nRun `pcb auth login` to re-authenticate.");
            } else {
                println!("  Token expires in: {}", tokens.time_until_expiry());
            }
        }
        None => match aws_auth::get_service_token(ctx) {
            Ok(token) => {
                println!("  Status: Logged in");
                println!("  Method: AWS credentials");
                if let Some(aws_principal_arn) = &token.aws_principal_arn {
                    println!("  AWS principal: {}", aws_principal_arn);
                }
                println!(
                    "  Token expires in: {}",
                    time_until_expiry(token.expires_at)
                );
            }
            Err(_) => {
                println!("  Status: Not logged in");
                println!("\nRun `pcb auth login` to authenticate.");
            }
        },
    }
    Ok(())
}

pub fn status() -> Result<()> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    status_with_context(&ctx)
}

pub fn refresh_with_context(ctx: &WorkspaceContext) -> Result<()> {
    let tokens = refresh_tokens_with_context(ctx)?;
    println!("✓ Token refreshed successfully");
    if let Some(email) = &tokens.email {
        println!("  Logged in as: {}", email);
    }
    println!("  Token expires in: {}", tokens.time_until_expiry());
    Ok(())
}

pub fn refresh() -> Result<()> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    refresh_with_context(&ctx)
}

struct CallbackTokens {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
    email: Option<String>,
}

fn parse_tokens_from_request(request_line: &str) -> Result<CallbackTokens> {
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        anyhow::bail!("Invalid HTTP request format");
    }

    let query_string = parts[1].split('?').nth(1).context("No query string")?;

    let mut access_token = None;
    let mut refresh_token = None;
    let mut expires_at = None;

    for param in query_string.split('&') {
        let mut kv = param.split('=');
        let key = kv.next().context("Invalid query parameter")?;
        let value = kv.next().context("Invalid query parameter")?;
        let decoded_value = urlencoding::decode(value)?.into_owned();

        match key {
            "access_token" => access_token = Some(decoded_value),
            "refresh_token" => refresh_token = Some(decoded_value),
            "expires_at" => expires_at = Some(decoded_value),
            _ => {}
        }
    }

    Ok(CallbackTokens {
        access_token: access_token.context("Missing access_token")?,
        refresh_token: refresh_token.context("Missing refresh_token")?,
        expires_at: expires_at.context("Missing expires_at")?.parse()?,
        email: None,
    })
}

#[derive(Args, Debug)]
#[command(about = "Manage authentication")]
pub struct AuthArgs {
    #[command(subcommand)]
    command: Option<AuthCommand>,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    Login,
    Logout,
    Status,
    Refresh,
    /// Print a valid access token to stdout (refreshes if expired)
    Token,
}

pub fn token_with_context(ctx: &WorkspaceContext) -> Result<()> {
    let token = get_valid_token_with_context(ctx)?;
    println!("{}", token);
    Ok(())
}

pub fn token() -> Result<()> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    token_with_context(&ctx)
}

pub fn execute(args: AuthArgs, ctx: &WorkspaceContext) -> Result<()> {
    match args.command {
        Some(AuthCommand::Login) | None => login_with_context(ctx),
        Some(AuthCommand::Logout) => logout_with_context(ctx),
        Some(AuthCommand::Status) => status_with_context(ctx),
        Some(AuthCommand::Refresh) => refresh_with_context(ctx),
        Some(AuthCommand::Token) => token_with_context(ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::cell::Cell;
    use std::ffi::OsString;

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(previous) = &self.previous {
                    std::env::set_var(self.key, previous);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn unix_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    fn isolated_context() -> (tempfile::TempDir, EnvGuard, WorkspaceContext) {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = EnvGuard::set("PCB_CONFIG_DIR", tempdir.path());
        (tempdir, guard, WorkspaceContext::default())
    }

    fn service_token(access_token: &str) -> AwsDiodeToken {
        AwsDiodeToken {
            access_token: access_token.to_string(),
            expires_at: unix_now() + 3600,
            aws_principal_arn: None,
        }
    }

    #[test]
    #[serial]
    fn no_auth_file_and_aws_exchange_success_returns_service_token() {
        let (_tempdir, _guard, ctx) = isolated_context();
        let aws_calls = Cell::new(0);
        let refresh_calls = Cell::new(0);

        let token = get_valid_token_with_sources(
            &ctx,
            |_| {
                aws_calls.set(aws_calls.get() + 1);
                Ok(service_token("aws-service-token"))
            },
            |_| {
                refresh_calls.set(refresh_calls.get() + 1);
                anyhow::bail!("refresh should not be called")
            },
        )
        .unwrap();

        assert_eq!(token, "aws-service-token");
        assert_eq!(refresh_calls.get(), 0);
        assert_eq!(aws_calls.get(), 1);
    }

    #[test]
    #[serial]
    fn expired_auth_file_refresh_failure_falls_back_to_aws_exchange() {
        let (_tempdir, _guard, ctx) = isolated_context();
        save_tokens(
            &ctx,
            "expired-token",
            "refresh-token",
            unix_now() - 3600,
            Some("user@example.com"),
        )
        .unwrap();
        let aws_calls = Cell::new(0);
        let refresh_calls = Cell::new(0);

        let token = get_valid_token_with_sources(
            &ctx,
            |_| {
                aws_calls.set(aws_calls.get() + 1);
                Ok(service_token("aws-service-token"))
            },
            |_| {
                refresh_calls.set(refresh_calls.get() + 1);
                anyhow::bail!("refresh failed")
            },
        )
        .unwrap();

        assert_eq!(token, "aws-service-token");
        assert_eq!(refresh_calls.get(), 1);
        assert_eq!(aws_calls.get(), 1);
    }

    #[test]
    #[serial]
    fn no_auth_file_and_aws_unavailable_returns_existing_error() {
        let (_tempdir, _guard, ctx) = isolated_context();
        let aws_calls = Cell::new(0);
        let refresh_calls = Cell::new(0);

        let err = get_valid_token_with_sources(
            &ctx,
            |_| {
                aws_calls.set(aws_calls.get() + 1);
                anyhow::bail!("aws unavailable")
            },
            |_| {
                refresh_calls.set(refresh_calls.get() + 1);
                anyhow::bail!("refresh should not be called")
            },
        )
        .unwrap_err();

        assert_eq!(err.to_string(), NOT_AUTHENTICATED_MESSAGE);
        assert_eq!(refresh_calls.get(), 0);
        assert_eq!(aws_calls.get(), 1);
    }

    #[test]
    #[serial]
    fn valid_auth_file_does_not_call_aws_exchange() {
        let (_tempdir, _guard, ctx) = isolated_context();
        save_tokens(
            &ctx,
            "local-token",
            "refresh-token",
            unix_now() + 3600,
            Some("user@example.com"),
        )
        .unwrap();
        let aws_calls = Cell::new(0);
        let refresh_calls = Cell::new(0);

        let token = get_valid_token_with_sources(
            &ctx,
            |_| {
                aws_calls.set(aws_calls.get() + 1);
                anyhow::bail!("aws should not be called")
            },
            |_| {
                refresh_calls.set(refresh_calls.get() + 1);
                anyhow::bail!("refresh should not be called")
            },
        )
        .unwrap();

        assert_eq!(token, "local-token");
        assert_eq!(refresh_calls.get(), 0);
        assert_eq!(aws_calls.get(), 0);
    }
}
