use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use base64::Engine;
use clap::{Args, Subcommand};
use fslock::LockFile;
use rand::distr::{Alphanumeric, SampleString};
use reqwest::StatusCode;
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::WorkspaceContext;

mod service_account;

const NOT_AUTHENTICATED_MESSAGE: &str = "Not authenticated. Run `pcb auth login` to authenticate.";
const DIODE_API_AUTH_NONE: &str = "none";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredAuth {
    User(AuthTokens),
    ServiceAccount(service_account::ServiceAccountAuth),
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

pub(crate) fn get_auth_dir() -> Result<PathBuf> {
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

fn load_auth(ctx: &WorkspaceContext) -> Result<Option<StoredAuth>> {
    let path = get_auth_file_path(ctx)?;
    if !path.exists() {
        return Ok(None);
    }
    let contents = fs::read_to_string(&path)?;
    // TOML errors include the input. Never include credential contents in diagnostics.
    let value: toml::Value = toml::from_str(&contents)
        .map_err(|_| anyhow::anyhow!("Invalid authentication file: {}", path.display()))?;
    let auth = if value.get("kind").is_some() {
        value.try_into()
    } else {
        value.try_into().map(StoredAuth::User)
    }
    .map_err(|_| anyhow::anyhow!("Invalid authentication file: {}", path.display()))?;
    Ok(Some(auth))
}

fn load_tokens_with_context(ctx: &WorkspaceContext) -> Result<Option<AuthTokens>> {
    match load_auth(ctx)? {
        Some(StoredAuth::User(tokens)) => Ok(Some(tokens)),
        Some(StoredAuth::ServiceAccount(_)) => {
            anyhow::bail!("Service accounts use client credentials, not human refresh tokens")
        }
        None => Ok(None),
    }
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
    token_endpoint: Option<&str>,
    client_id: Option<&str>,
) -> Result<()> {
    let tokens = AuthTokens {
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        expires_at,
        email: email.map(|s| s.to_string()),
        token_endpoint: token_endpoint.map(str::to_string),
        client_id: client_id.map(str::to_string),
    };
    save_auth(ctx, &StoredAuth::User(tokens))
}

fn lock_auth(ctx: &WorkspaceContext) -> Result<LockFile> {
    let mut lock = LockFile::open(&get_auth_file_path(ctx)?.with_extension("toml.lock"))?;
    lock.lock()?;
    Ok(lock)
}

fn save_auth(ctx: &WorkspaceContext, auth: &StoredAuth) -> Result<()> {
    // Human files retain their legacy shape for older PCB toolchains.
    let contents = match auth {
        StoredAuth::User(tokens) => toml::to_string(tokens),
        StoredAuth::ServiceAccount(_) => toml::to_string(auth),
    }?;

    let auth_path = get_auth_file_path(ctx)?;
    AtomicFile::new(&auth_path, OverwriteBehavior::AllowOverwrite)
        .write(|f| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            f.write_all(contents.as_bytes())?;
            f.flush()
        })
        .map_err(|err| anyhow::anyhow!("Failed to write auth tokens: {err}"))?;

    Ok(())
}

fn clear_tokens_with_context(ctx: &WorkspaceContext) -> Result<()> {
    let _lock = lock_auth(ctx)?;
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

#[derive(Deserialize)]
struct OAuthRefreshResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: i64,
}

fn oauth_client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

fn refresh_tokens_with_context(ctx: &WorkspaceContext) -> Result<AuthTokens> {
    let _lock = lock_auth(ctx)?;

    let tokens = load_tokens_with_context(ctx)?.context("No tokens to refresh")?;
    if !tokens.is_expired() {
        return Ok(tokens);
    }

    let refreshed = match (&tokens.token_endpoint, &tokens.client_id) {
        (Some(token_endpoint), Some(client_id)) => {
            let body = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("grant_type", "refresh_token")
                .append_pair("refresh_token", &tokens.refresh_token)
                .append_pair("client_id", client_id)
                .finish();
            let response = oauth_client()?
                .post(token_endpoint)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    "application/x-www-form-urlencoded",
                )
                .body(body)
                .send()?;
            if !response.status().is_success() {
                anyhow::bail!("Token refresh failed: {}", response.status());
            }
            let response: OAuthRefreshResponse = response.json()?;
            if response.expires_in <= 0 {
                anyhow::bail!("Invalid token expiry");
            }
            AuthTokens {
                access_token: response.access_token,
                refresh_token: response
                    .refresh_token
                    .unwrap_or(tokens.refresh_token.clone()),
                expires_at: unix_now()?
                    .checked_add(response.expires_in)
                    .context("Token expiry is too large")?,
                email: tokens.email.clone(),
                token_endpoint: tokens.token_endpoint.clone(),
                client_id: tokens.client_id.clone(),
            }
        }
        (None, None) => {
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
            let response: RefreshResponse = response.json()?;
            AuthTokens {
                access_token: response.access_token,
                refresh_token: response.refresh_token,
                expires_at: response.expires_at,
                email: tokens.email.clone(),
                token_endpoint: None,
                client_id: None,
            }
        }
        _ => anyhow::bail!("Incomplete OAuth refresh configuration"),
    };

    save_tokens(
        ctx,
        &refreshed.access_token,
        &refreshed.refresh_token,
        refreshed.expires_at,
        refreshed.email.as_deref(),
        refreshed.token_endpoint.as_deref(),
        refreshed.client_id.as_deref(),
    )?;

    Ok(refreshed)
}

pub fn refresh_tokens() -> Result<AuthTokens> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    refresh_tokens_with_context(&ctx)
}

pub fn get_valid_token_with_context(ctx: &WorkspaceContext) -> Result<String> {
    get_valid_token_with_sources(ctx, refresh_tokens_with_context)
}

fn get_valid_token_with_sources(
    ctx: &WorkspaceContext,
    refresh_tokens: impl Fn(&WorkspaceContext) -> Result<AuthTokens>,
) -> Result<String> {
    let not_authenticated = || anyhow::anyhow!(NOT_AUTHENTICATED_MESSAGE);

    if let Some(token) = service_account::environment_token(ctx, false)? {
        return Ok(token.access_token);
    }

    let tokens = match load_auth(ctx)? {
        Some(StoredAuth::User(tokens)) => tokens,
        Some(StoredAuth::ServiceAccount(account)) => {
            return Ok(service_account::saved_token(ctx, account, false)?.access_token);
        }
        None => return Err(not_authenticated()),
    };

    if !tokens.is_expired() {
        return Ok(tokens.access_token);
    }

    match refresh_tokens(ctx) {
        Ok(new_tokens) => Ok(new_tokens.access_token),
        Err(_) => Err(not_authenticated()),
    }
}

pub fn get_valid_token() -> Result<String> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    get_valid_token_with_context(&ctx)
}

pub(crate) fn api_auth_disabled() -> bool {
    std::env::var("DIODE_API_AUTH")
        .is_ok_and(|value| value.eq_ignore_ascii_case(DIODE_API_AUTH_NONE))
}

pub fn get_api_token_with_context(ctx: &WorkspaceContext) -> Result<Option<String>> {
    if api_auth_disabled() {
        Ok(None)
    } else {
        get_valid_token_with_context(ctx).map(Some)
    }
}

pub fn get_api_token() -> Result<Option<String>> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    get_api_token_with_context(&ctx)
}

pub(crate) fn apply_bearer_auth(request: RequestBuilder, token: Option<&str>) -> RequestBuilder {
    if api_auth_disabled() {
        request
    } else if let Some(token) = token {
        request.bearer_auth(token)
    } else {
        request
    }
}

pub(crate) fn apply_api_auth_with_context(
    ctx: &WorkspaceContext,
    request: RequestBuilder,
) -> Result<RequestBuilder> {
    Ok(apply_bearer_auth(
        request,
        get_api_token_with_context(ctx)?.as_deref(),
    ))
}

struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    fn generate() -> Self {
        let verifier = Alphanumeric.sample_string(&mut rand::rng(), 64);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

#[derive(Serialize)]
struct DeviceStartRequest<'a> {
    code_challenge: &'a str,
    code_challenge_method: &'static str,
}

#[derive(Deserialize)]
struct DeviceStartResponse {
    device_code: String,
    user_code: String,
    verification_uri_complete: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Serialize)]
struct DevicePollRequest<'a> {
    device_code: &'a str,
}

#[derive(Deserialize)]
struct DeviceAuthorization {
    authorization_code: String,
    token_endpoint: String,
    client_id: String,
    redirect_uri: String,
}

#[derive(Deserialize)]
struct OAuthErrorResponse {
    error: String,
}

enum DevicePollResult {
    AuthorizationPending,
    SlowDown,
    AccessDenied,
    ExpiredToken,
    Authorized(DeviceAuthorization),
}

#[derive(Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
}

pub fn login_with_context(ctx: &WorkspaceContext) -> Result<()> {
    let client = oauth_client()?;
    let pkce = Pkce::generate();
    let api_base_url = ctx.api_base_url().trim_end_matches('/');
    let start_url = format!("{api_base_url}/api/auth/device/start");
    let started_at = Instant::now();
    let response = client
        .post(&start_url)
        .json(&DeviceStartRequest {
            code_challenge: &pkce.challenge,
            code_challenge_method: "S256",
        })
        .send()
        .context("Failed to start device authorization")?;
    if !response.status().is_success() {
        anyhow::bail!(
            "Failed to start device authorization: {}",
            response.status()
        );
    }
    let start: DeviceStartResponse = response
        .json()
        .context("Invalid device authorization response")?;
    if start.expires_in == 0 || start.interval == 0 {
        anyhow::bail!("Invalid device authorization polling configuration");
    }
    let deadline = started_at
        .checked_add(Duration::from_secs(start.expires_in))
        .context("Device authorization expiry is too large")?;

    println!("Code: {}", start.user_code);
    println!("Verify at: {}", start.verification_uri_complete);
    println!("Opening browser...");
    if let Err(error) = open::that(&start.verification_uri_complete) {
        eprintln!("Failed to open browser: {error}");
        eprintln!("Continue at: {}", start.verification_uri_complete);
    }

    let poll_url = format!("{api_base_url}/api/auth/device/poll");
    let authorization = wait_for_device_authorization(
        &client,
        &poll_url,
        &start.device_code,
        deadline,
        start.interval,
    )?;

    let token_exchange_body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("code", &authorization.authorization_code)
        .append_pair("client_id", &authorization.client_id)
        .append_pair("redirect_uri", &authorization.redirect_uri)
        .append_pair("code_verifier", &pkce.verifier)
        .finish();
    let response = client
        .post(&authorization.token_endpoint)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(token_exchange_body)
        .send()
        .context("Failed to exchange authorization code")?;
    if !response.status().is_success() {
        anyhow::bail!("Authorization code exchange failed: {}", response.status());
    }
    let tokens: TokenExchangeResponse = response.json().context("Invalid token response")?;
    if tokens.expires_in <= 0 {
        anyhow::bail!("Invalid token expiry");
    }
    let expires_at = unix_now()?
        .checked_add(tokens.expires_in)
        .context("Token expiry is too large")?;

    let _lock = lock_auth(ctx)?;
    save_tokens(
        ctx,
        &tokens.access_token,
        &tokens.refresh_token,
        expires_at,
        None,
        Some(&authorization.token_endpoint),
        Some(&authorization.client_id),
    )?;
    pcb_zen::git::clear_diodehub_credential_cache();

    println!("✓ Authentication successful!");

    Ok(())
}

fn wait_for_device_authorization(
    client: &Client,
    poll_url: &str,
    device_code: &str,
    deadline: Instant,
    mut interval: u64,
) -> Result<DeviceAuthorization> {
    sleep_before_poll(deadline, interval)?;
    loop {
        let result = poll_device_authorization(client, poll_url, device_code)?;
        match result {
            DevicePollResult::AuthorizationPending => {}
            DevicePollResult::SlowDown => interval = interval.saturating_add(5),
            DevicePollResult::AccessDenied => anyhow::bail!("Authorization was denied"),
            DevicePollResult::ExpiredToken => anyhow::bail!("Authorization expired"),
            DevicePollResult::Authorized(authorization) => return Ok(authorization),
        }
        sleep_before_poll(deadline, interval)?;
    }
}

fn poll_device_authorization(
    client: &Client,
    poll_url: &str,
    device_code: &str,
) -> Result<DevicePollResult> {
    let response = client
        .post(poll_url)
        .json(&DevicePollRequest { device_code })
        .send()
        .context("Failed to poll device authorization")?;
    let status = response.status();
    if status.is_success() {
        return response
            .json()
            .map(DevicePollResult::Authorized)
            .context("Invalid device authorization response");
    }
    if status != StatusCode::BAD_REQUEST {
        anyhow::bail!("Device authorization polling failed: {status}");
    }

    let error: OAuthErrorResponse = response
        .json()
        .context("Invalid device authorization error response")?;
    match error.error.as_str() {
        "authorization_pending" => Ok(DevicePollResult::AuthorizationPending),
        "slow_down" => Ok(DevicePollResult::SlowDown),
        "access_denied" => Ok(DevicePollResult::AccessDenied),
        "expired_token" => Ok(DevicePollResult::ExpiredToken),
        error => anyhow::bail!("Device authorization failed: {error}"),
    }
}

fn sleep_before_poll(deadline: Instant, interval: u64) -> Result<()> {
    let delay = Duration::from_secs(interval);
    if Instant::now()
        .checked_add(delay)
        .is_none_or(|next_poll| next_poll >= deadline)
    {
        anyhow::bail!("Authorization expired");
    }
    std::thread::sleep(delay);
    if Instant::now() >= deadline {
        anyhow::bail!("Authorization expired");
    }
    Ok(())
}

fn unix_now() -> Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("System clock is before the Unix epoch")?
        .as_secs();
    i64::try_from(seconds).context("System time is too large")
}

pub fn logout_with_context(ctx: &WorkspaceContext) -> Result<()> {
    pcb_zen::git::clear_diodehub_credential_cache();
    clear_tokens_with_context(ctx)?;
    // Bearer tokens left behind by the retired AWS credential exchange.
    let _ = fs::remove_dir_all(get_auth_dir()?.join("service-auth"));
    println!("✓ Logged out successfully");
    if service_account::environment_configured() {
        eprintln!(
            "Environment credentials remain active. Unset DIODE_CLIENT_ID and DIODE_CLIENT_SECRET to stop using them."
        );
    }
    Ok(())
}

pub fn status_with_context(ctx: &WorkspaceContext) -> Result<()> {
    println!("Authentication Status:");
    println!("  Endpoint: {}", ctx.api_base_url());
    if api_auth_disabled() {
        println!("  Status: API auth disabled");
        println!("  Method: DIODE_API_AUTH=none");
        return Ok(());
    }

    if let Some(account) = service_account::from_environment(ctx)? {
        account.print_status("environment");
        return Ok(());
    }
    match load_auth(ctx)? {
        Some(StoredAuth::ServiceAccount(account)) => {
            account.validate(ctx)?;
            account.print_status("saved login");
        }
        Some(StoredAuth::User(tokens)) => {
            println!("  Status: Logged in");
            println!("  Method: human login");
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
        None => {
            println!("  Status: Not logged in");
            println!("\nRun `pcb auth login` to authenticate.");
        }
    }
    Ok(())
}

pub fn refresh_with_context(ctx: &WorkspaceContext) -> Result<()> {
    if let Some(token) = service_account::environment_token(ctx, true)? {
        println!(
            "✓ Service-account token renewed (expires in {})",
            time_until_expiry(token.expires_at)
        );
        return Ok(());
    }
    if let Some(StoredAuth::ServiceAccount(account)) = load_auth(ctx)? {
        let token = service_account::saved_token(ctx, account, true)?;
        println!(
            "✓ Service-account token renewed (expires in {})",
            time_until_expiry(token.expires_at)
        );
        return Ok(());
    }
    let tokens = refresh_tokens_with_context(ctx)?;
    println!("✓ Token refreshed successfully");
    if let Some(email) = &tokens.email {
        println!("  Logged in as: {}", email);
    }
    println!("  Token expires in: {}", tokens.time_until_expiry());
    Ok(())
}

#[derive(Args)]
#[command(about = "Manage authentication")]
pub struct AuthArgs {
    #[command(subcommand)]
    command: Option<AuthCommand>,
}

#[derive(Subcommand)]
pub enum AuthCommand {
    Login(LoginArgs),
    Logout,
    Status,
    Refresh,
    /// Print a valid access token to stdout (refreshes if expired)
    Token,
    /// Configure or provide Git credentials using PCB authentication
    Git(crate::git_auth::GitAuthArgs),
}

#[derive(Args, Default)]
pub struct LoginArgs {
    /// Configure a service account with client credentials
    #[arg(long, conflicts_with = "setup_code")]
    service_account: bool,
    /// Read service-account credentials as JSON from stdin
    #[arg(long = "stdin", requires = "service_account")]
    from_stdin: bool,
    /// Redeem a one-time setup code from the service-account console
    #[arg(long, value_name = "CODE", conflicts_with = "service_account")]
    setup_code: Option<String>,
}

pub fn token_with_context(ctx: &WorkspaceContext) -> Result<()> {
    let token = get_valid_token_with_context(ctx)?;
    pcb_ui::write_stdout(|out| writeln!(out, "{token}"))?;
    Ok(())
}

pub fn execute(args: AuthArgs, ctx: &WorkspaceContext) -> Result<()> {
    match args.command {
        Some(AuthCommand::Login(args)) => {
            if let Some(code) = args.setup_code {
                service_account::setup(ctx, &code)
            } else if args.service_account {
                service_account::login(ctx, args.from_stdin)
            } else {
                login_with_context(ctx)
            }
        }
        None => login_with_context(ctx),
        Some(AuthCommand::Logout) => logout_with_context(ctx),
        Some(AuthCommand::Status) => status_with_context(ctx),
        Some(AuthCommand::Refresh) => refresh_with_context(ctx),
        Some(AuthCommand::Token) => token_with_context(ctx),
        Some(AuthCommand::Git(args)) => crate::git_auth::execute(args, ctx),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::cell::Cell;
    use std::ffi::OsString;

    pub(super) struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        pub(super) fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::remove_var(key);
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

    pub(super) fn isolated_context() -> (tempfile::TempDir, Vec<EnvGuard>, WorkspaceContext) {
        let tempdir = tempfile::tempdir().unwrap();
        let mut guards: Vec<_> = [
            "DIODE_CLIENT_ID",
            "DIODE_CLIENT_SECRET",
            "DIODE_API_URL",
            "DIODE_API_AUTH",
        ]
        .into_iter()
        .map(EnvGuard::unset)
        .collect();
        guards.push(EnvGuard::set("PCB_CONFIG_DIR", tempdir.path()));
        (tempdir, guards, WorkspaceContext::default())
    }

    #[test]
    #[serial]
    fn no_auth_file_returns_not_authenticated() {
        let (_tempdir, _guard, ctx) = isolated_context();
        let refresh_calls = Cell::new(0);

        let err = get_valid_token_with_sources(&ctx, |_| {
            refresh_calls.set(refresh_calls.get() + 1);
            anyhow::bail!("refresh should not be called")
        })
        .unwrap_err();

        assert_eq!(err.to_string(), NOT_AUTHENTICATED_MESSAGE);
        assert_eq!(refresh_calls.get(), 0);
    }

    #[test]
    #[serial]
    fn api_auth_none_returns_no_api_token() {
        let (_tempdir, _config_guard, ctx) = isolated_context();
        let _auth_guard = EnvGuard::set("DIODE_API_AUTH", "none");

        assert_eq!(get_api_token_with_context(&ctx).unwrap(), None);
    }

    #[test]
    #[serial]
    fn logout_purges_legacy_service_auth_tokens() {
        let (tempdir, _guard, ctx) = isolated_context();
        let stale = tempdir.path().join("service-auth/api.toml");
        fs::create_dir_all(stale.parent().unwrap()).unwrap();
        fs::write(&stale, "access_token = \"stale-bearer\"\n").unwrap();

        logout_with_context(&ctx).unwrap();

        assert!(!stale.exists());
    }

    #[test]
    #[serial]
    fn expired_auth_file_refresh_failure_returns_not_authenticated() {
        let (_tempdir, _guard, ctx) = isolated_context();
        save_tokens(
            &ctx,
            "expired-token",
            "refresh-token",
            unix_now() - 3600,
            Some("user@example.com"),
            None,
            None,
        )
        .unwrap();
        let refresh_calls = Cell::new(0);

        let err = get_valid_token_with_sources(&ctx, |_| {
            refresh_calls.set(refresh_calls.get() + 1);
            anyhow::bail!("refresh failed")
        })
        .unwrap_err();

        assert_eq!(err.to_string(), NOT_AUTHENTICATED_MESSAGE);
        assert_eq!(refresh_calls.get(), 1);
    }

    #[test]
    #[serial]
    fn expired_auth_file_refresh_success_returns_new_token() {
        let (_tempdir, _guard, ctx) = isolated_context();
        save_tokens(
            &ctx,
            "expired-token",
            "refresh-token",
            unix_now() - 3600,
            Some("user@example.com"),
            None,
            None,
        )
        .unwrap();
        let refresh_calls = Cell::new(0);

        let token = get_valid_token_with_sources(&ctx, |_| {
            refresh_calls.set(refresh_calls.get() + 1);
            Ok(AuthTokens {
                access_token: "refreshed-token".to_string(),
                refresh_token: "refresh-token".to_string(),
                expires_at: unix_now() + 3600,
                email: Some("user@example.com".to_string()),
                token_endpoint: None,
                client_id: None,
            })
        })
        .unwrap();

        assert_eq!(token, "refreshed-token");
        assert_eq!(refresh_calls.get(), 1);
    }

    #[test]
    #[serial]
    fn valid_auth_file_returns_local_token_without_refreshing() {
        let (_tempdir, _guard, ctx) = isolated_context();
        save_tokens(
            &ctx,
            "local-token",
            "refresh-token",
            unix_now() + 3600,
            Some("user@example.com"),
            None,
            None,
        )
        .unwrap();
        let refresh_calls = Cell::new(0);

        let token = get_valid_token_with_sources(&ctx, |_| {
            refresh_calls.set(refresh_calls.get() + 1);
            anyhow::bail!("refresh should not be called")
        })
        .unwrap();

        assert_eq!(token, "local-token");
        assert_eq!(refresh_calls.get(), 0);
    }
}
