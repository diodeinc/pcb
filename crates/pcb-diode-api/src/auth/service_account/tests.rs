use super::*;
use crate::auth::tests::{EnvGuard, isolated_context};
use crate::auth::{get_auth_file_path, get_valid_token_with_context};
use base64::Engine;
use httpmock::prelude::*;
use serde_json::json;
use serial_test::serial;
use std::fs;

const CLIENT_ID: &str = "00000000-0000-4000-8000-000000000001";
const SECRET: &str = "dsc_test-secret";

fn account(endpoint: &str) -> ServiceAccountAuth {
    ServiceAccountAuth {
        api_base_url: endpoint.to_string(),
        credentials: Credentials {
            client_id: CLIENT_ID.to_string(),
            client_secret: SECRET.to_string(),
            service_account_name: Some("test-runner".to_string()),
        },
        token: None,
        setup_id: None,
    }
}

fn token_mock<'a>(server: &'a MockServer, secret: &str, token: &str) -> httpmock::Mock<'a> {
    server.mock(|when, then| {
        when.method(POST)
            .path("/api/auth/token")
            .header(
                "authorization",
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{CLIENT_ID}:{secret}"))
                ),
            )
            .header("content-type", "application/x-www-form-urlencoded")
            .body("grant_type=client_credentials");
        then.status(200).json_body(json!({
            "access_token": token, "token_type": "Bearer", "expires_in": 900,
            "expires_at": unix_now().unwrap() + 900,
        }));
    })
}

#[test]
#[serial]
fn saved_credentials_renew_once_across_concurrent_requests() {
    let (_dir, _env, _) = isolated_context();
    let server = MockServer::start();
    let ctx = WorkspaceContext::from_api_base_url(server.base_url());
    let token = token_mock(&server, SECRET, "machine-token");
    let mut saved = account(ctx.api_base_url());
    saved.token = Some(AccessToken {
        access_token: "expiring".into(),
        expires_at: unix_now().unwrap() + 30,
    });
    save_auth(&ctx, &StoredAuth::ServiceAccount(saved)).unwrap();

    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    get_valid_token_with_context(&ctx).unwrap()
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), "machine-token");
        }
    });
    assert_eq!(get_valid_token_with_context(&ctx).unwrap(), "machine-token");
    token.assert_calls(1);
    let path = get_auth_file_path(&ctx).unwrap();
    let contents = fs::read_to_string(&path).unwrap();
    assert!(!contents.contains("refresh_token"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
#[serial]
fn environment_overrides_saved_user_and_cache_tracks_secret_rotation() {
    let (_dir, _env, _) = isolated_context();
    let server = MockServer::start();
    let ctx = WorkspaceContext::from_api_base_url(server.base_url());
    crate::auth::save_tokens(
        &ctx,
        "human",
        "refresh",
        unix_now().unwrap() + 3600,
        None,
        None,
        None,
    )
    .unwrap();
    let path = get_auth_file_path(&ctx).unwrap();
    let before = fs::read(&path).unwrap();
    let _url = EnvGuard::set("DIODE_API_URL", ctx.api_base_url());
    let _id = EnvGuard::set("DIODE_CLIENT_ID", CLIENT_ID);
    let _secret = EnvGuard::set("DIODE_CLIENT_SECRET", SECRET);
    let first = token_mock(&server, SECRET, "first-token");
    assert_eq!(get_valid_token_with_context(&ctx).unwrap(), "first-token");
    assert_eq!(get_valid_token_with_context(&ctx).unwrap(), "first-token");
    first.assert_calls(1);
    let _rotated = EnvGuard::set("DIODE_CLIENT_SECRET", "rotated-secret");
    let second = token_mock(&server, "rotated-secret", "second-token");
    assert_eq!(get_valid_token_with_context(&ctx).unwrap(), "second-token");
    second.assert_calls(1);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
#[serial]
fn mismatched_endpoint_and_malformed_credentials_fail_without_exposing_secrets() {
    let (_dir, _env, _) = isolated_context();
    let ctx = WorkspaceContext::from_api_base_url("https://api.other.example");
    let mut stored = account("https://api.example");
    stored.token = Some(AccessToken {
        access_token: "cached-token".into(),
        expires_at: unix_now().unwrap() + 900,
    });
    save_auth(&ctx, &StoredAuth::ServiceAccount(stored)).unwrap();
    assert!(
        get_valid_token_with_context(&ctx)
            .unwrap_err()
            .to_string()
            .contains("different API endpoint")
    );
    fs::write(
        get_auth_file_path(&ctx).unwrap(),
        format!("kind = \"service_account\"\nclient_secret = \"{SECRET}\"\n"),
    )
    .unwrap();
    let error = get_valid_token_with_context(&ctx).unwrap_err();
    assert!(!format!("{error:#}").contains(SECRET));
    assert!(error.to_string().contains("Invalid authentication file"));
}

#[test]
fn credential_ids_and_oauth_basic_encoding() {
    let client = reqwest::blocking::Client::new();
    for (id, secret, encoded) in [
        (CLIENT_ID, SECRET, format!("{CLIENT_ID}:{SECRET}")),
        (
            "sandbox:test-sandbox",
            SECRET,
            format!("sandbox%3Atest-sandbox:{SECRET}"),
        ),
        (
            "sandbox:id%+ café",
            "dsc_secret:%+ café",
            "sandbox%3Aid%25%2B+caf%C3%A9:dsc_secret%3A%25%2B+caf%C3%A9".into(),
        ),
    ] {
        let import: CredentialImport = serde_json::from_value(json!({
            "client_id": id, "client_secret": secret,
        }))
        .unwrap();
        assert_eq!(import.credentials.client_id, id);
        let request = import
            .credentials
            .authenticate(client.post("https://api.example/api/auth/token"))
            .build()
            .unwrap();
        assert_eq!(
            request.headers()[reqwest::header::AUTHORIZATION],
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(encoded)
            )
        );
    }
}

#[test]
fn credentials_reject_missing_or_empty_ids() {
    let ctx = WorkspaceContext::from_api_base_url("https://api.example");
    assert!(
        serde_json::from_value::<CredentialImport>(json!({
            "client_secret": SECRET,
        }))
        .is_err()
    );
    let import: CredentialImport = serde_json::from_value(json!({
        "client_id": "", "client_secret": SECRET,
    }))
    .unwrap();
    let mut invalid = account(ctx.api_base_url());
    invalid.credentials = import.credentials;
    assert!(
        invalid
            .validate(&ctx)
            .unwrap_err()
            .to_string()
            .contains("must not be empty")
    );
}
