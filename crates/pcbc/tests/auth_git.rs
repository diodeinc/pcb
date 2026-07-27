use httpmock::Mock;
use httpmock::prelude::*;
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const GIT_HOST: &str = "code.diode.computer";
const REPOSITORY_PATH: &str = "acme/boards/widget.git";
const USER_ACCESS_TOKEN: &str = "user-access-token";
const REPOSITORY_TOKEN: &str = "repository-token";

struct TestContext {
    _tempdir: tempfile::TempDir,
    config_dir: PathBuf,
    api_url: String,
}

impl TestContext {
    fn new(api_url: String) -> Self {
        let tempdir = tempfile::tempdir().expect("create test directory");
        let config_dir = tempdir.path().join("pcb");
        let auth_dir = config_dir.join("auth");
        fs::create_dir_all(&auth_dir).expect("create auth directory");
        fs::write(
            auth_dir.join(format!("{}.toml", auth_scope_slug(&api_url))),
            format!(
                "access_token = \"{USER_ACCESS_TOKEN}\"\n\
                 refresh_token = \"refresh-token\"\n\
                 expires_at = 4102444800\n"
            ),
        )
        .expect("write auth tokens");

        Self {
            _tempdir: tempdir,
            config_dir,
            api_url,
        }
    }

    fn pcbc(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pcbc"));
        self.configure_environment(&mut command);
        command
    }

    fn git_credential(&self, action: &str) -> Command {
        let helper = format!(
            "!\"{}\" auth git",
            env!("CARGO_BIN_EXE_pcbc").replace('\\', "/")
        );
        let credential_context = format!("credential.https://{GIT_HOST}");

        let mut command = Command::new("git");
        command
            .arg("-c")
            .arg(format!("{credential_context}.helper="))
            .arg("-c")
            .arg(format!("{credential_context}.helper={helper}"))
            .arg("-c")
            .arg(format!("{credential_context}.useHttpPath=true"))
            .args(["credential", action]);
        self.configure_environment(&mut command);
        command
    }

    fn configure_environment(&self, command: &mut Command) {
        command
            .current_dir(self._tempdir.path())
            .env("PCB_CONFIG_DIR", &self.config_dir)
            .env("DIODE_API_URL", &self.api_url)
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost");
    }
}

fn auth_scope_slug(api_url: &str) -> String {
    let mut slug = String::with_capacity(api_url.len());
    let mut last_was_separator = false;

    for character in api_url.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            slug.push('_');
            last_was_separator = true;
        }
    }

    slug.trim_matches('_').to_string()
}

fn credential_request() -> String {
    format!(
        "capability[]=authtype\n\
         protocol=https\n\
         host={GIT_HOST}\n\
         path={REPOSITORY_PATH}\n\
         \n"
    )
}

fn run_with_input(mut command: Command, input: &str) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn command");
    child
        .stdin
        .take()
        .expect("open command stdin")
        .write_all(input.as_bytes())
        .expect("write command input");
    child.wait_with_output().expect("wait for command")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn mock_exchange<'a>(server: &'a MockServer, status: u16) -> Mock<'a> {
    server.mock(move |when, then| {
        when.method(POST)
            .path("/api/git/credentials")
            .header("authorization", format!("Bearer {USER_ACCESS_TOKEN}"))
            .json_body(json!({
                "host": GIT_HOST,
                "path": REPOSITORY_PATH,
            }));
        if status == 200 {
            then.status(200).json_body(json!({
                "repositoryId": "617882ea-14f6-40b4-bf52-cc1d3c7f67d4",
                "provider": "diodehub",
                "credential": {
                    "scheme": "Bearer",
                    "token": REPOSITORY_TOKEN,
                },
                "expiresAt": 4102444800u64,
            }));
        } else {
            then.status(status);
        }
    })
}

#[test]
fn advertises_authtype_and_ignores_unknown_operations() {
    let context = TestContext::new("http://127.0.0.1:1".to_string());

    let capability = context
        .pcbc()
        .args(["auth", "git", "capability"])
        .output()
        .expect("run helper capability operation");
    assert_success(&capability);
    assert_eq!(
        String::from_utf8(capability.stdout).unwrap(),
        "version 0\ncapability authtype\n"
    );
    assert!(capability.stderr.is_empty());

    let unknown = context
        .pcbc()
        .args(["auth", "git", "future-operation"])
        .output()
        .expect("run unknown helper operation");
    assert_success(&unknown);
    assert!(unknown.stdout.is_empty());
    assert!(unknown.stderr.is_empty());
}

#[test]
fn modern_git_fill_receives_an_ephemeral_bearer_credential() {
    let server = MockServer::start();
    let exchange = mock_exchange(&server, 200);
    let context = TestContext::new(server.base_url());

    let fill = run_with_input(context.git_credential("fill"), &credential_request());
    assert_success(&fill);
    assert!(fill.stderr.is_empty());

    let credential = String::from_utf8(fill.stdout).unwrap();
    let lines: Vec<&str> = credential.lines().collect();
    assert_eq!(lines.first(), Some(&"capability[]=authtype"));
    assert!(lines.contains(&"authtype=Bearer"));
    assert!(lines.contains(&format!("credential={REPOSITORY_TOKEN}").as_str()));
    assert!(lines.contains(&"ephemeral=1"));
    assert!(lines.contains(&"protocol=https"));
    assert!(lines.contains(&format!("host={GIT_HOST}").as_str()));
    assert!(lines.contains(&format!("path={REPOSITORY_PATH}").as_str()));
    assert!(!credential.contains("username="));
    assert!(!credential.contains("password="));
    exchange.assert_calls(1);

    let approve = run_with_input(context.git_credential("approve"), &credential);
    assert_success(&approve);
    assert!(approve.stdout.is_empty());
    assert!(approve.stderr.is_empty());

    let reject = run_with_input(context.git_credential("reject"), &credential);
    assert_success(&reject);
    assert!(reject.stdout.is_empty());
    assert!(reject.stderr.is_empty());
    exchange.assert_calls(1);
}

#[test]
fn modern_git_honors_quit_when_the_exchange_fails() {
    let server = MockServer::start();
    let exchange = mock_exchange(&server, 403);
    let context = TestContext::new(server.base_url());

    let fill = run_with_input(context.git_credential("fill"), &credential_request());
    assert!(!fill.status.success());
    assert!(!String::from_utf8_lossy(&fill.stdout).contains("credential="));

    let stderr = String::from_utf8_lossy(&fill.stderr);
    assert!(stderr.contains("pcb auth git: Git credential exchange failed: 403 Forbidden"));
    assert!(stderr.contains("credential helper"));
    assert!(stderr.contains("told us to quit"));
    exchange.assert_calls(1);
}

#[test]
fn store_and_erase_are_silent_without_exchanging_credentials() {
    let context = TestContext::new("http://127.0.0.1:1".to_string());

    for operation in ["store", "erase"] {
        let output = run_with_input(
            {
                let mut command = context.pcbc();
                command.args(["auth", "git", operation]);
                command
            },
            &credential_request(),
        );
        assert_success(&output);
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }
}
