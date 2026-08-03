use httpmock::Mock;
use httpmock::prelude::*;
use serde_json::json;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard};

const GIT_ORIGIN: &str = "https://git.preview.diode.localhost:8443";
const GIT_HELPER_CONFIG: &str = "credential.https://git.preview.diode.localhost:8443.helper";
const GIT_HOST: &str = "git.preview.diode.localhost:8443";
const GIT_USE_HTTP_PATH_CONFIG: &str =
    "credential.https://git.preview.diode.localhost:8443.useHttpPath";
const LEGACY_GIT_HELPER_CONFIG: &str = "credential.https://code.diode.computer.helper";
const LEGACY_GIT_USE_HTTP_PATH_CONFIG: &str = "credential.https://code.diode.computer.useHttpPath";
const REPOSITORY_PATH: &str = "acme/boards/widget.git";
const USER_ACCESS_TOKEN: &str = "user-access-token";
const REPOSITORY_TOKEN: &str = "repository-token";
const REPOSITORY_TOKEN_EXPIRES_AT: u64 = 4_102_444_800;
static TEST_LOCK: Mutex<()> = Mutex::new(());

struct TestContext {
    _test_lock: MutexGuard<'static, ()>,
    _tempdir: tempfile::TempDir,
    config_dir: PathBuf,
    git_config: PathBuf,
    path: OsString,
    api_url: String,
}

impl TestContext {
    fn new(api_url: String) -> Self {
        let test_lock = TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let tempdir = tempfile::tempdir().expect("create test directory");
        let config_dir = tempdir.path().join("pcb");
        let auth_dir = config_dir.join("auth");
        fs::create_dir_all(&auth_dir).expect("create auth directory");
        let bin_dir = tempdir.path().join("bin");
        fs::create_dir_all(&bin_dir).expect("create bin directory");
        let pcb_path = bin_dir.join(format!("pcb{}", env::consts::EXE_SUFFIX));
        if fs::hard_link(env!("CARGO_BIN_EXE_pcbc"), &pcb_path).is_err() {
            fs::copy(env!("CARGO_BIN_EXE_pcbc"), pcb_path).expect("copy pcbc test binary as pcb");
        }
        fs::write(
            auth_dir.join(format!("{}.toml", auth_scope_slug(&api_url))),
            format!(
                "access_token = \"{USER_ACCESS_TOKEN}\"\n\
                 refresh_token = \"refresh-token\"\n\
                 expires_at = 4102444800\n"
            ),
        )
        .expect("write auth tokens");

        let path = env::join_paths(
            std::iter::once(bin_dir)
                .chain(env::split_paths(&env::var_os("PATH").unwrap_or_default())),
        )
        .expect("construct test PATH");

        Self {
            _test_lock: test_lock,
            git_config: tempdir.path().join("gitconfig"),
            _tempdir: tempdir,
            config_dir,
            path,
            api_url,
        }
    }

    fn pcbc(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pcbc"));
        self.configure_environment(&mut command);
        command
    }

    fn git_credential(&self, action: &str) -> Command {
        let mut command = Command::new("git");
        command.args(["credential", action]);
        self.configure_environment(&mut command);
        command
    }

    fn git(&self) -> Command {
        let mut command = Command::new("git");
        self.configure_environment(&mut command);
        command
    }

    fn git_credential_cache(&self, action: &str) -> Command {
        let mut command = Command::new("git");
        command
            .arg("credential-cache")
            .arg(format!("--socket={}", self.cache_socket().display()))
            .arg("--timeout=3300")
            .arg(action);
        self.configure_environment(&mut command);
        command
    }

    fn cache_socket(&self) -> PathBuf {
        self.config_dir.join("git-credential-cache/socket")
    }

    fn managed_git_config(&self) -> PathBuf {
        self.config_dir.join("gitconfig")
    }

    fn run_git_config(&self, args: &[&str]) -> Output {
        self.git()
            .args(["config", "--global"])
            .args(args)
            .output()
            .expect("run git config")
    }

    fn git_config_values(&self, key: &str) -> Vec<String> {
        let output = self.run_git_config(&["--get-all", key]);
        if output.status.code() == Some(1) {
            return Vec::new();
        }
        assert_success(&output);
        String::from_utf8(output.stdout)
            .expect("git config output is UTF-8")
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn managed_git_config_values(&self, key: &str) -> Vec<String> {
        let output = self
            .git()
            .args(["config", "--file"])
            .arg(self.managed_git_config())
            .args(["--get-all", key])
            .output()
            .expect("read PCB-managed Git config");
        if output.status.code() == Some(1) {
            return Vec::new();
        }
        assert_success(&output);
        String::from_utf8(output.stdout)
            .expect("PCB-managed Git config output is UTF-8")
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn run_config_command(&self, operation: &str) -> Output {
        let mut command = self.pcbc();
        command.args(["auth", "git", operation]);
        if operation == "configure" {
            command.arg(format!("{GIT_ORIGIN}/{REPOSITORY_PATH}"));
        }
        command
            .output()
            .expect("run pcb auth git configuration command")
    }

    fn run_config_command_with_url(&self, repository_url: &str) -> Output {
        self.pcbc()
            .args(["auth", "git", "configure", repository_url])
            .output()
            .expect("run pcb auth git configuration command")
    }

    fn configure_environment(&self, command: &mut Command) {
        command
            .current_dir(self._tempdir.path())
            .env("PCB_CONFIG_DIR", &self.config_dir)
            .env("DIODE_API_URL", &self.api_url)
            .env("GIT_CONFIG_GLOBAL", &self.git_config)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env("PATH", &self.path)
            .env_remove("DIODE_API_AUTH")
            .env_remove("RUST_LOG");
    }
}

impl Drop for TestContext {
    fn drop(&mut self) {
        let _ = self.git_credential_cache("exit").output();
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
        "command failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_clean_success(output: &Output) {
    assert_success(output);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

fn mock_exchange<'a>(
    server: &'a MockServer,
    host: &'a str,
    status: u16,
    authorization: Option<&str>,
) -> Mock<'a> {
    server.mock(move |when, then| {
        let when = when
            .method(POST)
            .path("/api/git/credentials")
            .json_body(json!({
                "host": host,
                "path": REPOSITORY_PATH,
            }));
        if let Some(authorization) = authorization {
            when.header("authorization", authorization);
        } else {
            when.header_missing("authorization");
        }
        if status == 200 {
            then.status(200).json_body(json!({
                "provider": "diodehub",
                "credential": {
                    "scheme": "Bearer",
                    "token": REPOSITORY_TOKEN,
                },
                "expiresAt": REPOSITORY_TOKEN_EXPIRES_AT,
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
fn configure_is_idempotent_and_preserves_unrelated_global_config() {
    let context = TestContext::new("http://127.0.0.1:1".to_string());
    let unrelated_include = context._tempdir.path().join("unrelated.gitconfig");
    fs::write(&unrelated_include, "[alias]\n\tco = checkout\n").unwrap();
    assert_clean_success(&context.run_git_config(&["--add", "user.email", "dev@example.com"]));
    assert_clean_success(&context.run_git_config(&["--add", "credential.helper", "store"]));
    assert_clean_success(&context.run_git_config(&[
        "--add",
        "include.path",
        unrelated_include.to_str().unwrap(),
    ]));
    assert_clean_success(&context.run_git_config(&[
        "--add",
        "credential.https://git.preview.diode.localhost:8443.username",
        "developer",
    ]));
    assert_clean_success(&context.run_git_config(&[
        "--add",
        LEGACY_GIT_HELPER_CONFIG,
        "!developer-helper",
    ]));
    assert_clean_success(&context.run_git_config(&[
        "--add",
        LEGACY_GIT_HELPER_CONFIG,
        "!pcb auth git",
    ]));
    assert_clean_success(&context.run_git_config(&[
        "--add",
        LEGACY_GIT_USE_HTTP_PATH_CONFIG,
        "false",
    ]));
    assert_clean_success(&context.run_git_config(&[
        "--add",
        LEGACY_GIT_USE_HTTP_PATH_CONFIG,
        "true",
    ]));

    assert_clean_success(&context.run_config_command("configure"));
    assert_clean_success(&context.run_config_command("configure"));

    let helpers = context.managed_git_config_values(GIT_HELPER_CONFIG);
    assert_eq!(helpers.len(), 3);
    assert_eq!(helpers[0], "");
    assert!(helpers[1].starts_with("cache --timeout=3300 --socket="));
    assert_eq!(helpers[2], format!("!pcb auth git --host={GIT_HOST}"));
    assert_eq!(
        context.managed_git_config_values(GIT_USE_HTTP_PATH_CONFIG),
        ["true"]
    );
    let includes = context.git_config_values("include.path");
    assert_eq!(
        includes
            .iter()
            .filter(|value| *value == context.managed_git_config().to_str().unwrap())
            .count(),
        1
    );
    assert!(includes.contains(&unrelated_include.to_string_lossy().into_owned()));
    assert_eq!(
        context.git_config_values(LEGACY_GIT_HELPER_CONFIG),
        ["!developer-helper"]
    );
    assert_eq!(
        context.git_config_values(LEGACY_GIT_USE_HTTP_PATH_CONFIG),
        ["false"]
    );
    assert_eq!(context.git_config_values("user.email"), ["dev@example.com"]);
    assert_eq!(context.git_config_values("credential.helper"), ["store"]);
    assert_eq!(
        context.git_config_values("credential.https://git.preview.diode.localhost:8443.username"),
        ["developer"]
    );
}

#[test]
fn configure_replaces_the_managed_credential_origin() {
    let context = TestContext::new("http://127.0.0.1:1".to_string());
    assert_clean_success(&context.run_config_command("configure"));

    let other_origin = "https://code.gov.diode.computer";
    assert_clean_success(
        &context.run_config_command_with_url(&format!("{other_origin}/acme/boards/widget.git")),
    );

    assert!(
        context
            .managed_git_config_values(GIT_HELPER_CONFIG)
            .is_empty()
    );
    let helpers = context.managed_git_config_values(&format!("credential.{other_origin}.helper"));
    assert_eq!(helpers.len(), 3);
    assert_eq!(helpers[0], "");
    assert!(helpers[1].starts_with("cache --timeout=3300 --socket="));
    assert_eq!(helpers[2], "!pcb auth git --host=code.gov.diode.computer");
}

#[test]
fn configure_requires_an_https_repository_url() {
    let context = TestContext::new("http://127.0.0.1:1".to_string());

    let missing = context
        .pcbc()
        .args(["auth", "git", "configure"])
        .output()
        .expect("run configure without a repository URL");
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains(
        "Missing DiodeHub repository URL; use `pcb auth git configure <repository-url>`"
    ));

    let http = context.run_config_command_with_url("http://code.example.com/acme/widget.git");
    assert!(!http.status.success());
    assert!(
        String::from_utf8_lossy(&http.stderr).contains("DiodeHub repository URL must use HTTPS")
    );
    assert!(!context.managed_git_config().exists());
}

#[test]
fn unconfigure_is_idempotent_and_preserves_unrelated_global_config() {
    let context = TestContext::new("http://127.0.0.1:1".to_string());
    let unrelated_include = context._tempdir.path().join("unrelated.gitconfig");
    fs::write(&unrelated_include, "[alias]\n\tco = checkout\n").unwrap();
    assert_clean_success(&context.run_git_config(&[
        "--add",
        "include.path",
        unrelated_include.to_str().unwrap(),
    ]));
    assert_clean_success(&context.run_git_config(&[
        "--add",
        "credential.https://github.com.helper",
        "!github-helper",
    ]));
    assert_clean_success(&context.run_git_config(&[
        "--add",
        "credential.https://git.preview.diode.localhost:8443.username",
        "developer",
    ]));
    assert_clean_success(&context.run_config_command("configure"));
    let cached_credential = format!(
        "capability[]=authtype\n\
         protocol=https\n\
         host={GIT_HOST}\n\
         path={REPOSITORY_PATH}\n\
         authtype=Bearer\n\
         credential={REPOSITORY_TOKEN}\n\
         password_expiry_utc={REPOSITORY_TOKEN_EXPIRES_AT}\n\
         \n"
    );
    assert_clean_success(&run_with_input(
        context.git_credential_cache("store"),
        &cached_credential,
    ));
    assert!(context.cache_socket().exists());

    assert_clean_success(&context.run_config_command("unconfigure"));
    assert_clean_success(&context.run_config_command("unconfigure"));

    assert!(!context.cache_socket().exists());
    assert!(!context.managed_git_config().exists());
    assert!(context.git_config_values(GIT_HELPER_CONFIG).is_empty());
    assert!(
        context
            .git_config_values(GIT_USE_HTTP_PATH_CONFIG)
            .is_empty()
    );
    assert_eq!(
        context.git_config_values("include.path"),
        [unrelated_include.to_string_lossy().into_owned()]
    );
    assert_eq!(
        context.git_config_values("credential.https://github.com.helper"),
        ["!github-helper"]
    );
    assert_eq!(
        context.git_config_values("credential.https://git.preview.diode.localhost:8443.username"),
        ["developer"]
    );
}

#[test]
fn modern_git_fill_caches_the_bearer_credential_until_rejected() {
    let server = MockServer::start();
    let exchange = mock_exchange(&server, GIT_HOST, 200, Some("Bearer user-access-token"));
    let context = TestContext::new(server.base_url());
    assert_clean_success(&context.run_config_command("configure"));

    let fill = run_with_input(context.git_credential("fill"), &credential_request());
    assert_success(&fill);
    assert!(fill.stderr.is_empty());

    let credential = String::from_utf8(fill.stdout).unwrap();
    let lines: Vec<&str> = credential.lines().collect();
    assert_eq!(lines.first(), Some(&"capability[]=authtype"));
    assert!(lines.contains(&"authtype=Bearer"));
    assert!(lines.contains(&format!("credential={REPOSITORY_TOKEN}").as_str()));
    assert!(lines.contains(&format!("password_expiry_utc={REPOSITORY_TOKEN_EXPIRES_AT}").as_str()));
    assert!(lines.contains(&"protocol=https"));
    assert!(lines.contains(&format!("host={GIT_HOST}").as_str()));
    assert!(lines.contains(&format!("path={REPOSITORY_PATH}").as_str()));
    assert!(!credential.contains("ephemeral="));
    assert!(!credential.contains("username="));
    assert!(!credential.contains("password="));
    exchange.assert_calls(1);

    let approve = run_with_input(context.git_credential("approve"), &credential);
    assert_success(&approve);
    assert!(approve.stdout.is_empty());
    assert!(approve.stderr.is_empty());

    let cached_fill = run_with_input(context.git_credential("fill"), &credential_request());
    assert_success(&cached_fill);
    assert!(cached_fill.stderr.is_empty());
    let cached_credential = String::from_utf8(cached_fill.stdout).unwrap();
    assert_eq!(cached_credential, credential);
    exchange.assert_calls(1);

    let reject = run_with_input(context.git_credential("reject"), &cached_credential);
    assert_success(&reject);
    assert!(reject.stdout.is_empty());
    assert!(reject.stderr.is_empty());

    let refreshed_fill = run_with_input(context.git_credential("fill"), &credential_request());
    assert_success(&refreshed_fill);
    assert!(refreshed_fill.stderr.is_empty());
    exchange.assert_calls(2);
}

#[test]
fn ambient_api_auth_without_auth_file_returns_bearer_credential_to_git() {
    let server = MockServer::start();
    let exchange = mock_exchange(&server, GIT_HOST, 200, None);
    let context = TestContext::new(server.base_url());
    fs::remove_dir_all(context.config_dir.join("auth")).expect("remove PCB auth directory");
    assert!(!context.config_dir.join("auth").exists());
    assert_clean_success(&context.run_config_command("configure"));

    let fill = run_with_input(
        {
            let mut command = context.git_credential("fill");
            command.env("DIODE_API_AUTH", "none");
            command
        },
        &credential_request(),
    );
    assert_success(&fill);
    assert!(fill.stderr.is_empty());

    let credential = String::from_utf8(fill.stdout).unwrap();
    assert!(credential.contains("authtype=Bearer"));
    assert!(credential.contains(&format!("credential={REPOSITORY_TOKEN}")));
    assert!(credential.contains(&format!(
        "password_expiry_utc={REPOSITORY_TOKEN_EXPIRES_AT}"
    )));
    exchange.assert_calls(1);
}

#[test]
fn modern_git_ignores_an_expired_cached_bearer_credential() {
    let server = MockServer::start();
    let exchange = mock_exchange(&server, GIT_HOST, 200, Some("Bearer user-access-token"));
    let context = TestContext::new(server.base_url());
    assert_clean_success(&context.run_config_command("configure"));
    let expired_credential = format!(
        "capability[]=authtype\n\
         protocol=https\n\
         host={GIT_HOST}\n\
         path={REPOSITORY_PATH}\n\
         authtype=Bearer\n\
         credential=expired-repository-token\n\
         password_expiry_utc=1\n\
         \n"
    );
    assert_clean_success(&run_with_input(
        context.git_credential_cache("store"),
        &expired_credential,
    ));

    let fill = run_with_input(context.git_credential("fill"), &credential_request());
    assert_success(&fill);
    assert!(fill.stderr.is_empty());
    let credential = String::from_utf8(fill.stdout).unwrap();
    assert!(credential.contains(&format!("credential={REPOSITORY_TOKEN}")));
    assert!(!credential.contains("expired-repository-token"));
    exchange.assert_calls(1);
}

#[test]
fn auth_logout_stops_the_git_credential_cache() {
    let server = MockServer::start();
    let exchange = mock_exchange(&server, GIT_HOST, 200, Some("Bearer user-access-token"));
    let context = TestContext::new(server.base_url());
    assert_clean_success(&context.run_config_command("configure"));

    let fill = run_with_input(context.git_credential("fill"), &credential_request());
    assert_success(&fill);
    assert_clean_success(&run_with_input(
        context.git_credential("approve"),
        &String::from_utf8(fill.stdout).unwrap(),
    ));
    assert!(context.cache_socket().exists());

    let logout = context
        .pcbc()
        .args(["auth", "logout"])
        .output()
        .expect("run pcb auth logout");
    assert_success(&logout);
    assert!(!context.cache_socket().exists());
    exchange.assert_calls(1);
}

#[test]
fn modern_git_honors_quit_when_the_exchange_fails() {
    let server = MockServer::start();
    let exchange = mock_exchange(&server, GIT_HOST, 403, Some("Bearer user-access-token"));
    let context = TestContext::new(server.base_url());
    assert_clean_success(&context.run_config_command("configure"));

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
fn credential_helpers_ignore_unrelated_hosts() {
    let context = TestContext::new("http://127.0.0.1:1".to_string());
    let github_request = "capability[]=authtype\n\
                          protocol=https\n\
                          host=github.com\n\
                          path=acme/widget.git\n\
                          \n";
    let scoped_helper = run_with_input(
        {
            let mut command = context.pcbc();
            command.args(["auth", "git", "--host", GIT_HOST, "get"]);
            command
        },
        github_request,
    );
    assert_clean_success(&scoped_helper);

    let credential_file = context._tempdir.path().join("credentials");
    fs::write(
        &credential_file,
        "https://fallback-user:fallback-password@github.com/acme/widget.git\n",
    )
    .expect("write fallback credentials");
    assert_clean_success(&context.run_git_config(&["--add", "credential.helper", "!pcb auth git"]));
    assert_clean_success(&context.run_git_config(&[
        "--add",
        "credential.helper",
        &format!(
            "store --file={}",
            credential_file.display().to_string().replace('\\', "/")
        ),
    ]));
    assert_clean_success(&context.run_config_command("configure"));

    let fill = run_with_input(context.git_credential("fill"), github_request);
    assert_success(&fill);
    assert!(fill.stderr.is_empty());

    let credential = String::from_utf8(fill.stdout).unwrap();
    assert!(credential.contains("username=fallback-user"));
    assert!(credential.contains("password=fallback-password"));
}

#[test]
fn legacy_helper_defaults_to_the_commercial_diodehub_host() {
    let server = MockServer::start();
    let host = "code.diode.computer";
    let exchange = mock_exchange(&server, host, 200, Some("Bearer user-access-token"));
    let context = TestContext::new(server.base_url());
    let output = run_with_input(
        {
            let mut command = context.pcbc();
            command.args(["auth", "git", "get"]);
            command
        },
        &format!(
            "capability[]=authtype\n\
             protocol=https\n\
             host={host}\n\
             path={REPOSITORY_PATH}\n\
             \n"
        ),
    );

    assert_success(&output);
    assert!(output.stderr.is_empty());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains(&format!("credential={REPOSITORY_TOKEN}"))
    );
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
