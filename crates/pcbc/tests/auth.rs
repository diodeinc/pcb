use httpmock::prelude::*;
use serde_json::json;
use std::io::Write;
use std::process::{Command, Output, Stdio};

const CLIENT_ID: &str = "00000000-0000-4000-8000-000000000001";
const SECRET: &str = "dsc_integration-secret";

struct Fixture {
    dir: tempfile::TempDir,
    server: MockServer,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            server: MockServer::start(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pcbc"));
        command
            .args(args)
            .current_dir(self.dir.path())
            .env("PCB_CONFIG_DIR", self.dir.path().join("config"))
            .env("DIODE_API_URL", self.server.base_url())
            .env_remove("DIODE_CLIENT_ID")
            .env_remove("DIODE_CLIENT_SECRET")
            .env_remove("DIODE_API_AUTH")
            .env_remove("DIODE_APP_URL");
        command
    }

    fn token(&self, value: &str) -> httpmock::Mock<'_> {
        use base64::Engine;
        self.server.mock(|when, then| {
            when.method(POST).path("/api/auth/token")
                .header("authorization", format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT_ID}:{SECRET}"))))
                .body("grant_type=client_credentials");
            then.status(200).json_body(json!({"access_token":value, "token_type":"Bearer", "expires_in":900, "expires_at":4102444800_i64}));
        })
    }

    fn login(&self, mut command: Command) -> Output {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                json!({
                    "client_id":CLIENT_ID, "client_secret":SECRET,
                    "service_account_name":"ci-runner", "api_base_url":self.server.base_url(),
                })
                .to_string()
                .as_bytes(),
            )
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn service_account_import_token_refresh_and_logout() {
    let fixture = Fixture::new();
    let mut first = fixture.token("first-machine-token");
    success(fixture.login(fixture.command(&["auth", "login", "--service-account", "--stdin"])));
    assert_eq!(
        success(fixture.command(&["auth", "token"]).output().unwrap()),
        "first-machine-token\n"
    );
    let status = success(fixture.command(&["auth", "status"]).output().unwrap());
    assert!(status.contains("ci-runner") && status.contains("saved login"));
    assert!(!status.contains(SECRET) && !status.contains("first-machine-token"));
    let output = fixture
        .command(&["auth", "token"])
        .env("DIODE_CLIENT_ID", CLIENT_ID)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("DIODE_CLIENT_SECRET"));
    first.assert_calls(1);
    first.delete();
    let second = fixture.token("renewed-machine-token");
    success(fixture.command(&["auth", "refresh"]).output().unwrap());
    assert_eq!(
        success(fixture.command(&["auth", "token"]).output().unwrap()),
        "renewed-machine-token\n"
    );
    second.assert_calls(1);
    success(fixture.command(&["auth", "logout"]).output().unwrap());
    assert!(
        !fixture
            .command(&["auth", "token"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn component_command_uses_environment_credentials_without_a_login_file() {
    let fixture = Fixture::new();
    let token = fixture.token("ci-machine-token");
    let component = fixture.server.mock(|when, then| {
        when.method(POST)
            .path("/api/v2/components/search")
            .header("authorization", "Bearer ci-machine-token");
        then.status(200).json_body(json!([]));
    });
    success(
        fixture
            .command(&["component", "search", "STM32", "--format", "json"])
            .env("DIODE_CLIENT_ID", CLIENT_ID)
            .env("DIODE_CLIENT_SECRET", SECRET)
            .output()
            .unwrap(),
    );
    token.assert_calls(1);
    component.assert_calls(1);
    assert!(!fixture.dir.path().join("config").exists());
}

#[test]
fn credential_import_rejects_an_unconfigured_endpoint() {
    let fixture = Fixture::new();
    let token = fixture.token("imported-token");
    let mut command = fixture.command(&["auth", "login", "--service-account", "--stdin"]);
    command.env_remove("DIODE_API_URL");
    let output = fixture.login(command);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("active endpoint"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(SECRET));
    assert!(!fixture.dir.path().join("config").exists());
    token.assert_calls(0);
}
