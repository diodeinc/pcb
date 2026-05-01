use anyhow::{Context, Result};
use aws_config::BehaviorVersion;
use aws_config::meta::region::RegionProviderChain;
use aws_credential_types::provider::ProvideCredentials;
use aws_sigv4::http_request::{
    SignableBody, SignableRequest, SignatureLocation, SigningSettings, sign,
};
use aws_sigv4::sign::v4;
use aws_smithy_runtime_api::client::identity::Identity;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use std::time::SystemTime;

use crate::WorkspaceContext;

const SERVICE_TOKEN_EXPIRY_SKEW_SECONDS: i64 = 120;
const AWS_AUTH_AUDIENCE_HEADER: &str = "x-diode-audience";
const AWS_AUTH_AUDIENCE: &str = "diode-api/aws-auth/v1";

#[derive(Debug, Clone)]
pub(crate) struct AwsDiodeToken {
    pub access_token: String,
    pub expires_at: i64,
    pub aws_principal_arn: Option<String>,
}

impl AwsDiodeToken {
    pub fn is_valid_for_use(&self) -> bool {
        self.expires_at - unix_now() > SERVICE_TOKEN_EXPIRY_SKEW_SECONDS
    }
}

#[derive(Debug, Serialize)]
struct AwsIdentityProof {
    method: String,
    url: String,
    headers: HashMap<String, String>,
    body: String,
}

struct AwsIdentityProofWithCacheKey {
    proof: AwsIdentityProof,
    cache_discriminator: String,
}

#[derive(Debug, Deserialize)]
struct AwsExchangeResponse {
    access_token: String,
    expires_at: i64,
    aws_principal_arn: Option<String>,
    principal: Option<AwsExchangePrincipal>,
}

#[derive(Debug, Deserialize)]
struct AwsExchangePrincipal {
    arn: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedAwsDiodeToken {
    access_token: String,
    expires_at: i64,
    source: String,
    aws_principal_arn: Option<String>,
}

impl From<&AwsDiodeToken> for CachedAwsDiodeToken {
    fn from(token: &AwsDiodeToken) -> Self {
        Self {
            access_token: token.access_token.clone(),
            expires_at: token.expires_at,
            source: "aws".to_string(),
            aws_principal_arn: token.aws_principal_arn.clone(),
        }
    }
}

impl TryFrom<CachedAwsDiodeToken> for AwsDiodeToken {
    type Error = anyhow::Error;

    fn try_from(token: CachedAwsDiodeToken) -> Result<Self> {
        anyhow::ensure!(
            token.source == "aws",
            "service token cache source is not AWS"
        );
        Ok(Self {
            access_token: token.access_token,
            expires_at: token.expires_at,
            aws_principal_arn: token.aws_principal_arn,
        })
    }
}

static TOKEN_CACHE: OnceLock<Mutex<HashMap<String, AwsDiodeToken>>> = OnceLock::new();

pub(crate) fn get_service_token(ctx: &WorkspaceContext) -> Result<AwsDiodeToken> {
    let proof = build_identity_proof()?;
    let cache_key = format!("{}:{}", ctx.api_base_url(), proof.cache_discriminator);
    if let Some(token) = cached_token(&cache_key) {
        return Ok(token);
    }

    if let Some(token) = load_cached_service_token(ctx, &proof.cache_discriminator)? {
        cache_token(cache_key, token.clone());
        return Ok(token);
    }

    let url = format!("{}/api/auth/aws/exchange", ctx.api_base_url());
    let response = Client::new()
        .post(&url)
        .json(&proof.proof)
        .send()
        .context("Failed to exchange AWS identity proof")?;

    if !response.status().is_success() {
        anyhow::bail!("AWS identity exchange failed: {}", response.status());
    }

    let response: AwsExchangeResponse = response
        .json()
        .context("Failed to decode AWS identity exchange response")?;
    let token = AwsDiodeToken {
        access_token: response.access_token,
        expires_at: response.expires_at,
        aws_principal_arn: response
            .aws_principal_arn
            .or_else(|| response.principal.map(|principal| principal.arn)),
    };
    save_cached_service_token(ctx, &proof.cache_discriminator, &token)?;
    cache_token(cache_key, token.clone());
    Ok(token)
}

pub(crate) fn clear_service_token(ctx: &WorkspaceContext) -> Result<()> {
    let api_slug = crate::endpoint::auth_scope_slug(ctx.api_base_url());
    if let Ok(mut cache) = TOKEN_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        let cache_prefix = format!("{}:", ctx.api_base_url());
        cache.retain(|key, _| !key.starts_with(&cache_prefix));
    }

    let service_auth_dir = service_auth_dir_path()?;
    if !service_auth_dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(service_auth_dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name == format!("{api_slug}.toml")
            || (file_name.starts_with(&format!("{api_slug}-")) && file_name.ends_with(".toml"))
        {
            fs::remove_file(entry.path())?;
        }
    }

    Ok(())
}

fn cached_token(cache_key: &str) -> Option<AwsDiodeToken> {
    let cache = TOKEN_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let token = cache.lock().ok()?.get(cache_key).cloned()?;
    token.is_valid_for_use().then_some(token)
}

fn cache_token(cache_key: String, token: AwsDiodeToken) {
    if let Ok(mut cache) = TOKEN_CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        cache.insert(cache_key, token);
    }
}

fn service_auth_dir_path() -> Result<PathBuf> {
    let pcb_dir = if let Ok(config_dir) = std::env::var("PCB_CONFIG_DIR") {
        PathBuf::from(config_dir)
    } else {
        let home_dir = dirs::home_dir().context("Failed to get home directory")?;
        home_dir.join(".pcb")
    };
    Ok(pcb_dir.join("service-auth"))
}

fn service_auth_dir() -> Result<PathBuf> {
    let service_auth_dir = service_auth_dir_path()?;
    fs::create_dir_all(&service_auth_dir)?;
    Ok(service_auth_dir)
}

fn service_auth_file_path(ctx: &WorkspaceContext, cache_discriminator: &str) -> Result<PathBuf> {
    let slug = crate::endpoint::auth_scope_slug(ctx.api_base_url());
    Ok(service_auth_dir()?.join(format!("{slug}-{cache_discriminator}.toml")))
}

fn load_cached_service_token(
    ctx: &WorkspaceContext,
    cache_discriminator: &str,
) -> Result<Option<AwsDiodeToken>> {
    let path = service_auth_file_path(ctx, cache_discriminator)?;
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&path)?;
    let token = AwsDiodeToken::try_from(toml::from_str::<CachedAwsDiodeToken>(&contents)?)?;
    Ok(token.is_valid_for_use().then_some(token))
}

fn save_cached_service_token(
    ctx: &WorkspaceContext,
    cache_discriminator: &str,
    token: &AwsDiodeToken,
) -> Result<()> {
    let path = service_auth_file_path(ctx, cache_discriminator)?;
    let contents = toml::to_string(&CachedAwsDiodeToken::from(token))?;
    atomicwrites::AtomicFile::new(&path, atomicwrites::OverwriteBehavior::AllowOverwrite)
        .write(|f| {
            f.write_all(contents.as_bytes())?;
            f.flush()
        })
        .map_err(|err| anyhow::anyhow!("Failed to write AWS service auth token: {err}"))?;
    Ok(())
}

fn build_identity_proof() -> Result<AwsIdentityProofWithCacheKey> {
    let runtime = tokio::runtime::Runtime::new().context("Failed to create AWS auth runtime")?;
    runtime.block_on(async {
        let region_provider = RegionProviderChain::default_provider().or_else("us-west-2");
        let config = aws_config::defaults(BehaviorVersion::latest())
            .region(region_provider)
            .load()
            .await;
        let credentials = config
            .credentials_provider()
            .context("AWS credentials are unavailable")?
            .provide_credentials()
            .await
            .context("AWS credentials are unavailable")?;
        let cache_discriminator = aws_cache_discriminator(credentials.access_key_id());
        let identity: Identity = credentials.into();
        let region = config
            .region()
            .map(|region| region.as_ref())
            .unwrap_or("us-west-2");

        let mut signing_settings = SigningSettings::default();
        signing_settings.signature_location = SignatureLocation::QueryParams;
        signing_settings.expires_in = Some(Duration::from_secs(60));

        let signing_params = v4::SigningParams::builder()
            .identity(&identity)
            .region(region)
            .name("sts")
            .time(SystemTime::now())
            .settings(signing_settings)
            .build()
            .context("Failed to build STS signing parameters")?
            .into();

        let mut url = sts_get_caller_identity_url(region);
        let proof_headers = [(
            AWS_AUTH_AUDIENCE_HEADER.to_string(),
            AWS_AUTH_AUDIENCE.to_string(),
        )];
        let signable_request = SignableRequest::new(
            "GET",
            url.as_str(),
            proof_headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::empty(),
        )
        .context("Failed to build signable STS request")?;
        let (signing_instructions, _signature) = sign(signable_request, &signing_params)
            .context("Failed to create presigned STS GetCallerIdentity proof")?
            .into_parts();

        for (name, value) in signing_instructions.into_parts().1 {
            url.query_pairs_mut().append_pair(name, &value);
        }

        Ok(AwsIdentityProofWithCacheKey {
            proof: AwsIdentityProof {
                method: "GET".to_string(),
                url: url.to_string(),
                headers: proof_headers.into_iter().collect(),
                body: String::new(),
            },
            cache_discriminator,
        })
    })
}

fn aws_cache_discriminator(access_key_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(access_key_id.as_bytes());
    hex::encode(hasher.finalize())
}

fn sts_get_caller_identity_url(region: &str) -> url::Url {
    let mut url =
        url::Url::parse(&format!("https://sts.{region}.amazonaws.com/")).expect("STS URL is valid");
    url.query_pairs_mut()
        .append_pair("Action", "GetCallerIdentity")
        .append_pair("Version", "2011-06-15");
    url
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
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

    fn isolated_context() -> (tempfile::TempDir, EnvGuard, WorkspaceContext) {
        let tempdir = tempfile::tempdir().unwrap();
        let guard = EnvGuard::set("PCB_CONFIG_DIR", tempdir.path());
        (tempdir, guard, WorkspaceContext::default())
    }

    fn test_token(expires_at: i64) -> AwsDiodeToken {
        AwsDiodeToken {
            access_token: "cached-service-token".to_string(),
            expires_at,
            aws_principal_arn: Some(
                "arn:aws:sts::123456789012:assumed-role/example/session".to_string(),
            ),
        }
    }

    #[test]
    #[serial]
    fn service_token_cache_round_trips_under_service_auth_dir() {
        let (tempdir, _guard, ctx) = isolated_context();
        let token = test_token(unix_now() + 3600);
        let cache_discriminator = "aws-principal-a";

        save_cached_service_token(&ctx, cache_discriminator, &token).unwrap();
        let loaded = load_cached_service_token(&ctx, cache_discriminator)
            .unwrap()
            .unwrap();

        assert_eq!(loaded.access_token, token.access_token);
        assert_eq!(loaded.aws_principal_arn, token.aws_principal_arn);
        assert!(
            service_auth_file_path(&ctx, cache_discriminator)
                .unwrap()
                .starts_with(tempdir.path().join("service-auth"))
        );
    }

    #[test]
    #[serial]
    fn service_token_cache_ignores_tokens_near_expiry() {
        let (_tempdir, _guard, ctx) = isolated_context();
        let cache_discriminator = "aws-principal-a";
        save_cached_service_token(&ctx, cache_discriminator, &test_token(unix_now() + 60)).unwrap();

        assert!(
            load_cached_service_token(&ctx, cache_discriminator)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    #[serial]
    fn service_token_cache_is_scoped_by_aws_cache_discriminator() {
        let (_tempdir, _guard, ctx) = isolated_context();
        let token = test_token(unix_now() + 3600);

        save_cached_service_token(&ctx, "aws-principal-a", &token).unwrap();

        assert!(
            load_cached_service_token(&ctx, "aws-principal-b")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    #[serial]
    fn clear_service_token_removes_service_auth_cache_for_api_scope() {
        let (_tempdir, _guard, ctx) = isolated_context();
        let token = test_token(unix_now() + 3600);

        save_cached_service_token(&ctx, "aws-principal-a", &token).unwrap();
        clear_service_token(&ctx).unwrap();

        assert!(
            load_cached_service_token(&ctx, "aws-principal-a")
                .unwrap()
                .is_none()
        );
    }
}
