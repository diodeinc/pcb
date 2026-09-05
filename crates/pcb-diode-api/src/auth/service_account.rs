use super::{
    StoredAuth, load_auth, lock_auth, oauth_client, save_auth, time_until_expiry, unix_now,
};
use crate::WorkspaceContext;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, IsTerminal};
use std::sync::Mutex;
use url::{Host, Url};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
struct Credentials {
    client_id: String,
    client_secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service_account_name: Option<String>,
}

impl Credentials {
    fn authenticate(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        // OAuth client_secret_basic encodes each component before Basic joins them
        // with a colon (RFC 6749 section 2.3.1).
        let client_id: String =
            url::form_urlencoded::byte_serialize(self.client_id.as_bytes()).collect();
        let client_secret: String =
            url::form_urlencoded::byte_serialize(self.client_secret.as_bytes()).collect();
        request.basic_auth(client_id, Some(client_secret))
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct AccessToken {
    pub access_token: String,
    pub expires_at: i64,
}

impl AccessToken {
    fn is_valid(&self) -> bool {
        unix_now().is_ok_and(|now| self.expires_at.saturating_sub(now) > 60)
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct ServiceAccountAuth {
    api_base_url: String,
    #[serde(flatten)]
    credentials: Credentials,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<AccessToken>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    setup_id: Option<Uuid>,
}

impl ServiceAccountAuth {
    pub(super) fn validate(&self, ctx: &WorkspaceContext) -> Result<()> {
        if api_url(&self.api_base_url)? != api_url(ctx.api_base_url())? {
            bail!("Service-account credentials belong to a different API endpoint");
        }
        if self.credentials.client_id.is_empty() {
            bail!("Invalid service-account client ID: must not be empty");
        }
        if self.credentials.client_secret.is_empty() {
            bail!("Invalid service-account client secret");
        }
        Ok(())
    }

    pub(super) fn print_status(&self, source: &str) {
        println!("  Status: Service account configured");
        println!("  Source: {source}");
        if let Some(name) = &self.credentials.service_account_name {
            println!("  Service account: {name}");
        }
        println!("  Client ID: {}", self.credentials.client_id);
        match &self.token {
            Some(token) if token.is_valid() => {
                println!(
                    "  Token expires in: {}",
                    time_until_expiry(token.expires_at)
                );
            }
            _ => println!("  Token: obtained automatically on the next request"),
        }
        if self.setup_id.is_some() {
            println!("  Console confirmation pending; run `pcb auth refresh` to retry");
        }
    }
}

fn api_url(value: &str) -> Result<String> {
    let url = Url::parse(value).map_err(|_| anyhow::anyhow!("Invalid service-account API URL"))?;
    let loopback = match url.host() {
        Some(Host::Domain("localhost")) => true,
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host().is_none()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    {
        bail!(
            "Service-account API URL must use HTTPS (or HTTP on loopback), without user info, query, or fragment"
        );
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub(super) fn environment_configured() -> bool {
    std::env::var_os("DIODE_CLIENT_ID").is_some()
        || std::env::var_os("DIODE_CLIENT_SECRET").is_some()
}

pub(super) fn from_environment(ctx: &WorkspaceContext) -> Result<Option<ServiceAccountAuth>> {
    if !environment_configured() {
        return Ok(None);
    }
    let client_id = std::env::var("DIODE_CLIENT_ID")
        .map_err(|_| anyhow::anyhow!("Set DIODE_CLIENT_ID to the service-account client ID"))?;
    let client_secret = std::env::var("DIODE_CLIENT_SECRET")
        .map_err(|_| anyhow::anyhow!("Set DIODE_CLIENT_SECRET with DIODE_CLIENT_ID"))?;
    // An explicit URL keeps repository configuration from redirecting CI secrets.
    let api_base_url = std::env::var("DIODE_API_URL").map_err(|_| {
        anyhow::anyhow!("Set DIODE_API_URL when using environment client credentials")
    })?;
    let account = ServiceAccountAuth {
        api_base_url: api_url(&api_base_url)?,
        credentials: Credentials {
            client_id,
            client_secret,
            service_account_name: None,
        },
        token: None,
        setup_id: None,
    };
    account.validate(ctx)?;
    Ok(Some(account))
}

struct EnvironmentToken {
    key: [u8; 32],
    token: AccessToken,
}

static ENVIRONMENT_TOKEN: Mutex<Option<EnvironmentToken>> = Mutex::new(None);

pub(super) fn environment_token(
    ctx: &WorkspaceContext,
    force: bool,
) -> Result<Option<AccessToken>> {
    let Some(account) = from_environment(ctx)? else {
        return Ok(None);
    };
    let key = Sha256::digest(format!(
        "{}\0{}\0{}",
        account.api_base_url, account.credentials.client_id, account.credentials.client_secret
    ))
    .into();
    let mut cache = ENVIRONMENT_TOKEN.lock().unwrap();
    if !force
        && let Some(cached) = cache.as_ref()
        && cached.key == key
        && cached.token.is_valid()
    {
        return Ok(Some(cached.token.clone()));
    }
    let token = exchange(&account)?;
    *cache = Some(EnvironmentToken {
        key,
        token: token.clone(),
    });
    Ok(Some(token))
}

pub(super) fn saved_token(
    ctx: &WorkspaceContext,
    account: ServiceAccountAuth,
    force: bool,
) -> Result<AccessToken> {
    account.validate(ctx)?;
    if !force && let Some(token) = account.token.filter(AccessToken::is_valid) {
        return Ok(token);
    }
    let _lock = lock_auth(ctx)?;
    // Re-read after locking: another process may have renewed or replaced it.
    let Some(StoredAuth::ServiceAccount(mut account)) = load_auth(ctx)? else {
        bail!("Authentication changed while renewing the token; retry the command");
    };
    account.validate(ctx)?;
    if !force && let Some(token) = account.token.as_ref().filter(|token| token.is_valid()) {
        return Ok(token.clone());
    }
    let token = exchange(&account)?;
    account.token = Some(token.clone());
    save_auth(ctx, &StoredAuth::ServiceAccount(account.clone()))?;
    finish_setup(ctx, &mut account);
    Ok(token)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
}

fn exchange(account: &ServiceAccountAuth) -> Result<AccessToken> {
    let request = oauth_client()?.post(format!("{}/api/auth/token", account.api_base_url));
    let response = account
        .credentials
        .authenticate(request)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body("grant_type=client_credentials")
        .send()
        .context("Service-account token exchange failed")?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        bail!(
            "Service-account credentials were rejected. Replace the configured client ID and secret."
        );
    }
    if !response.status().is_success() {
        bail!(
            "Service-account token exchange failed: {}",
            response.status()
        );
    }
    let response: TokenResponse = response
        .json()
        .map_err(|_| anyhow::anyhow!("Invalid service-account token response"))?;
    let now = unix_now()?;
    if response.access_token.is_empty() || response.expires_in <= 0 {
        bail!("Invalid service-account token response");
    }
    let expires_at = now
        .checked_add(response.expires_in)
        .context("Invalid service-account token expiry")?;
    Ok(AccessToken {
        access_token: response.access_token,
        expires_at,
    })
}

#[derive(Deserialize)]
struct CredentialImport {
    api_base_url: Option<String>,
    #[serde(flatten)]
    credentials: Credentials,
}

fn require_direct_auth() -> Result<()> {
    if super::api_auth_disabled() {
        bail!("Unset DIODE_API_AUTH=none before configuring service-account credentials");
    }
    Ok(())
}

pub(super) fn login(ctx: &WorkspaceContext, from_stdin: bool) -> Result<()> {
    require_direct_auth()?;
    let credentials = if from_stdin {
        let import: CredentialImport =
            serde_json::from_reader(io::stdin().lock()).map_err(|_| {
                anyhow::anyhow!("Invalid credential JSON; expected client_id and client_secret")
            })?;
        if let Some(url) = &import.api_base_url
            && api_url(url)? != api_url(ctx.api_base_url())?
        {
            bail!(
                "Credential JSON endpoint differs from the active endpoint. Set DIODE_API_URL to its api_base_url before importing."
            );
        }
        import.credentials
    } else {
        if !io::stdin().is_terminal() {
            bail!(
                "Use `pcb auth login --service-account --stdin` to import credential JSON non-interactively"
            );
        }
        println!("Endpoint: {}", api_url(ctx.api_base_url())?);
        let client_id = inquire::Text::new("Client ID:").prompt()?;
        let client_id = client_id.trim().to_string();
        let client_secret = inquire::Password::new("Client secret:")
            .without_confirmation()
            .prompt()?;
        Credentials {
            client_id,
            client_secret: client_secret.trim().to_string(),
            service_account_name: None,
        }
    };
    configure(
        ctx,
        ServiceAccountAuth {
            api_base_url: api_url(ctx.api_base_url())?,
            credentials,
            token: None,
            setup_id: None,
        },
    )
}

fn configure(ctx: &WorkspaceContext, mut account: ServiceAccountAuth) -> Result<()> {
    account.validate(ctx)?;
    account.token = Some(exchange(&account)?);
    let _lock = lock_auth(ctx)?;
    save_auth(ctx, &StoredAuth::ServiceAccount(account.clone()))?;
    pcb_zen::git::clear_diodehub_credential_cache();
    println!(
        "✓ Service-account credentials saved for {}",
        account.api_base_url
    );
    if environment_configured() {
        eprintln!("DIODE_CLIENT_ID and DIODE_CLIENT_SECRET override this saved login while set.");
    }
    finish_setup(ctx, &mut account);
    Ok(())
}

#[derive(Deserialize)]
struct SetupResponse {
    setup_id: Uuid,
    #[serde(flatten)]
    credentials: Credentials,
}

pub(super) fn setup(ctx: &WorkspaceContext, code: &str) -> Result<()> {
    require_direct_auth()?;
    let api_base_url = api_url(ctx.api_base_url())?;
    let response = oauth_client()?
        .post(format!("{api_base_url}/api/auth/service-account-setup/redeem"))
        .json(&serde_json::json!({ "setup_code": code }))
        .send()
        .context("Setup redemption failed; the code may have been consumed. Check the console before generating a new code.")?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        bail!(
            "This API does not support setup codes yet. Use `pcb auth login --service-account --stdin` to import credentials."
        );
    }
    if !status.is_success() {
        let error = response.json::<super::OAuthErrorResponse>().ok();
        match error.as_ref().map(|error| error.error.as_str()) {
            Some("expired_setup_code") => {
                bail!("Setup code expired. Generate a new command in the console.")
            }
            Some("setup_code_used") => bail!(
                "Setup code was already used. Check the console before generating a new command."
            ),
            Some("invalid_setup_code") => {
                bail!("Invalid setup code. Copy the command again from the console.")
            }
            _ => bail!("Setup redemption failed: {status}"),
        }
    }
    let response: SetupResponse = response.json().map_err(|_| {
        anyhow::anyhow!("Invalid setup response; check the console before generating a new code")
    })?;
    configure(
        ctx,
        ServiceAccountAuth {
            api_base_url,
            credentials: response.credentials,
            token: None,
            setup_id: Some(response.setup_id),
        },
    )
}

// Caller holds the auth-file lock; keep setup_id until confirmation succeeds.
fn finish_setup(ctx: &WorkspaceContext, account: &mut ServiceAccountAuth) {
    let Some(setup_id) = account.setup_id else {
        return;
    };
    let confirm = (|| -> Result<()> {
        let request = oauth_client()?.post(format!(
            "{}/api/auth/service-account-setup/{setup_id}/confirm",
            account.api_base_url
        ));
        let response = account.credentials.authenticate(request).send()?;
        if response.status() != reqwest::StatusCode::NO_CONTENT {
            bail!("Console confirmation failed: {}", response.status());
        }
        account.setup_id = None;
        save_auth(ctx, &StoredAuth::ServiceAccount(account.clone()))
    })();
    if let Err(error) = confirm {
        eprintln!(
            "Credentials are saved and usable, but setup confirmation failed: {error}. Run `pcb auth refresh` to retry."
        );
    }
}

#[cfg(test)]
mod tests;
