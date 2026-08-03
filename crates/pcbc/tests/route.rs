#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

const BOARD_NO_LAYOUT_ZEN: &str = r#"
p1 = Net("P1")
"#;

/// Kills the wrapped child on drop, including on an early panic (e.g. from
/// `assert!`/`.expect()`) — without this, a spawned FreeRouting process
/// leaks for the rest of the test run whenever a later assertion fails,
/// which is exactly the kind of orphaned-process bug production code (see
/// `FreeroutingServer` in freerouting.rs) uses the same pattern to prevent.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A .zen file that declares a layout path but whose board files have not
/// been generated yet. resolve_board() will fail with a "run pcb layout"
/// error.
const BOARD_WITH_LAYOUT_ZEN: &str = r#"
LoadCap = Module("@stdlib/generics/Capacitor.zen")
vcc = Net("VCC")
gnd = Net("GND")
LoadCap(name = "C1", value = "100nF", package = "0402", P1 = vcc, P2 = gnd)
Layout(name="TestBoard", path="build/TestBoard", bom_profile=None)
"#;

/// Two components connected by a shared net, placed apart by auto-placement
/// so the net actually needs a routed trace between them — unlike
/// `BOARD_WITH_LAYOUT_ZEN`'s single capacitor, whose two pads are close
/// enough that FreeRouting needs zero real routing work and never produces
/// an output object at all. Needed for tests that assert on *retrieving*
/// output, not just on a clean DRC pass.
const BOARD_WITH_ROUTABLE_NETS_ZEN: &str = r#"
Resistor = Module("@stdlib/generics/Resistor.zen")

n1 = Net("N1")
n2 = Net("N2")
n3 = Net("N3")

Resistor(name = "R1", value = "1k", package = "0402", P1 = n1, P2 = n2)
Resistor(name = "R2", value = "1k", package = "0402", P1 = n2, P2 = n3)

Layout(name="TestBoard", path="build/TestBoard", bom_profile=None)
"#;

/// Helper: create a minimal KiCad layout directory (kicad_pro + kicad_pcb)
/// inside the sandbox so that resolve_board() passes and routing can proceed
/// until the Java/JAR prerequisite check.
fn scaffold_layout(sandbox: &mut Sandbox) {
    sandbox.write(
        "build/TestBoard/test.kicad_pro",
        "(kicad_pro (version 20231010))\n",
    );
    sandbox.write(
        "build/TestBoard/test.kicad_pcb",
        "(kicad_pcb (version 20231010))\n",
    );
}

/// Run `pcbc route --engine freerouting <extra_args...> board.zen` in
/// `sandbox` and return (exit_code, stdout, stderr).
///
/// We deliberately don't snapshot this output: it embeds absolute paths,
/// timings, network errors, and upstream FreeRouting log lines, none of
/// which are stable across machines or runs. Assert on exit status and
/// specific substrings instead.
fn run_route_freerouting(sandbox: &mut Sandbox, extra_args: &[&str]) -> (i32, String, String) {
    let mut args = vec!["route", "--engine", "freerouting"];
    args.extend_from_slice(extra_args);
    args.push("board.zen");

    let output = sandbox
        .run("pcbc", args)
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("failed to run pcbc route --engine freerouting");

    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

// ---------------------------------------------------------------------------
// Error-path tests
// ---------------------------------------------------------------------------

#[test]
fn test_freerouting_no_layout() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_NO_LAYOUT_ZEN);
    let (code, stdout, stderr) = run_route_freerouting(&mut sandbox, &[]);
    assert_ne!(code, 0, "expected failure with no layout defined");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("No layout path defined") || combined.contains("layout"),
        "expected a layout-related error, got:\n{combined}"
    );
}

#[test]
fn test_freerouting_java_not_found() {
    // Sandbox passes through the host PATH; if Java isn't on the host, this
    // naturally covers the error.
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_WITH_LAYOUT_ZEN);
    scaffold_layout(&mut sandbox);

    let (code, stdout, stderr) = run_route_freerouting(&mut sandbox, &[]);
    let combined = format!("{stdout}{stderr}");
    if combined.contains("Java 21+ not found") {
        assert_ne!(code, 0);
    } else {
        // Host has Java 21+: we'll hit the JAR lookup instead, which is
        // covered by test_freerouting_bad_jar_path. Nothing to assert here.
    }
}

#[test]
fn test_freerouting_bad_jar_path() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_WITH_LAYOUT_ZEN);
    scaffold_layout(&mut sandbox);

    let (code, stdout, stderr) =
        run_route_freerouting(&mut sandbox, &["--fr-jar", "/nonexistent/freerouting.jar"]);
    assert_ne!(code, 0, "expected failure for a nonexistent --fr-jar path");
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("FreeRouting JAR not found"),
        "expected a FreeRouting JAR error, got:\n{combined}"
    );
}

// ---------------------------------------------------------------------------
// Integration tests — need Java + FreeRouting JAR on the host
// ---------------------------------------------------------------------------

/// Resolve the FreeRouting JAR path for integration tests.
///
/// Priority:
/// 1. `FREEROUTING_TEST_JAR` env var (test-specific override)
/// 2. `FREEROUTING_JAR` env var
/// 3. Cached download (`~/.cache/pcb/test-cache/freerouting-cli.jar`)
/// 4. Download to cache from GitHub releases
///
/// Returns `None` (and prints a diagnostic) so the calling test can skip.
fn resolve_freerouting_jar() -> Option<PathBuf> {
    for var in &["FREEROUTING_TEST_JAR", "FREEROUTING_JAR"] {
        if let Ok(path) = std::env::var(var) {
            let p = PathBuf::from(&path);
            if p.exists() {
                return Some(p);
            }
        }
    }

    let cache_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("pcb")
        .join("test-cache");
    let cached = cache_dir.join("freerouting-2.2.4.jar");

    if cached.exists() {
        return Some(cached);
    }

    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        eprintln!("[route test] Skipping: failed to create cache dir: {e}");
        return None;
    }

    let urls = [
        "https://github.com/freerouting/freerouting/releases/download/v2.2.4/freerouting-2.2.4.jar",
    ];

    for url in &urls {
        eprintln!("[route test] Downloading FreeRouting JAR from {url} ...");
        let downloaded = reqwest::blocking::get(*url)
            .and_then(|resp| resp.error_for_status())
            .and_then(|resp| resp.bytes())
            .ok()
            .filter(|bytes| !bytes.is_empty())
            .and_then(|bytes| std::fs::write(&cached, &bytes).ok());
        if downloaded.is_some() {
            eprintln!("[route test] Downloaded to {}", cached.display());
            return Some(cached);
        }
    }

    eprintln!(
        "[route test] Skipping: could not download FreeRouting JAR. \
         Set FREEROUTING_TEST_JAR to a pre-downloaded jar."
    );
    None
}

fn java_compatible() -> bool {
    let output = match Command::new("java").arg("-version").output() {
        Ok(o) => o,
        Err(_) => return false,
    };
    if !output.status.success() {
        return false;
    }
    // java -version writes to stderr, not stdout
    let stderr = String::from_utf8_lossy(&output.stderr);
    let major = stderr
        .lines()
        .find(|l| l.contains("version"))
        .and_then(|l| l.split('"').nth(1))
        .and_then(|v| v.split('.').next())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    major >= 21
}

/// Poll `url` until it returns HTTP 200 or `deadline` elapses.
fn wait_for_http_ok(url: &str, deadline: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        let ok = reqwest::blocking::get(url)
            .map(|resp| resp.status().is_success())
            .unwrap_or(false);
        if ok {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Spawn a FreeRouting API server on a free loopback port and wait for it to
/// report ready. Mirrors the exact flags `FreeroutingServer::spawn` uses in
/// production (headless, GUI disabled, API server on a private loopback
/// port, no auth since there's a single local caller, analytics off) so this
/// stays a faithful smoke test of the same invocation path.
///
/// Panics (via `expect`/`assert!`) on failure to spawn or become ready — this
/// is test setup, not something callers are expected to recover from.
fn spawn_freerouting_server(jar_path: &PathBuf) -> (ChildGuard, u16) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let child = ChildGuard(
        Command::new("java")
            .arg("-Djava.awt.headless=true")
            .arg("-jar")
            .arg(jar_path)
            .arg("--gui.enabled=false")
            .arg("--api_server.enabled=true")
            .arg("--api_server.authentication.enabled=false")
            .arg(format!("--api_server-endpoints=http://127.0.0.1:{port}"))
            .arg("--usage_and_diagnostic_data.disable_analytics=true")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to launch FreeRouting API server"),
    );

    assert!(
        wait_for_http_ok(
            &format!("http://127.0.0.1:{port}/v1/system/status"),
            Duration::from_secs(15),
        ),
        "FreeRouting API server never became ready at http://127.0.0.1:{port}/v1/system/status"
    );

    (child, port)
}

#[test]
fn test_freerouting_api_server_boots() {
    // Smoke test that the resolved JAR is a genuine FreeRouting build
    // capable of running in local API-server mode — the mode production
    // code depends on exclusively (see freerouting.rs: driven through
    // FreeRouting's REST API rather than one-shot CLI mode specifically so
    // Ctrl+C/timeout can recover partial output). Protects against e.g.
    // FREEROUTING_JAR pointing at a stale or incompatible build.
    if !java_compatible() {
        eprintln!("[route test] Skipping: Java 21+ not available");
        return;
    }

    let jar_path = match resolve_freerouting_jar() {
        Some(j) => j,
        None => return,
    };

    let (_child, _port) = spawn_freerouting_server(&jar_path);
}

/// Regression guard for the specific capability CLI mode structurally
/// cannot provide: retrieving real routed output after cancelling a job.
/// FreeRouting's CLI mode (`-de`/`-do`) only writes its `.ses` output once,
/// gated on an internal `COMPLETED` state that a confirmed upstream bug
/// prevents an interrupted job from ever reaching — so Ctrl+C/timeout in CLI
/// mode never yields a partial result. The API's `GET /jobs/{id}/output`
/// explicitly supports returning output for a cancelled job.
///
/// This drives the API directly (session -> job -> settings -> input ->
/// start -> cancel -> output) rather than through `pcbc route`, so we
/// control the exact cancel timing. We don't try to catch the job strictly
/// mid-route before cancelling — on a small test board FreeRouting can
/// finish before our cancel call lands, and that's fine: the assertion is
/// "output is valid and non-empty after a cancel call", regardless of
/// whether cancel landed pre- or post-completion. Both are meaningful
/// confirmations of the capability under test.
#[test]
fn test_freerouting_cancel_returns_output() {
    if !java_compatible() {
        eprintln!("[route test] Skipping: Java 21+ not available");
        return;
    }
    let jar_path = match resolve_freerouting_jar() {
        Some(j) => j,
        None => return,
    };
    let kicad_missing = Command::new("kicad-cli")
        .arg("version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true);
    if kicad_missing {
        eprintln!("[route test] Skipping: KiCad not installed");
        return;
    }

    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_WITH_ROUTABLE_NETS_ZEN);
    let build_output = sandbox
        .run("pcbc", ["layout", "--no-open", "board.zen"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("layout command failed");
    if !build_output.status.success() {
        eprintln!("[route test] Skipping: layout generation failed (KiCad Python not available?)");
        return;
    }

    let board_path = sandbox
        .default_cwd()
        .join("build/TestBoard/layout.kicad_pcb");
    let dsn_path = sandbox.default_cwd().join("board.dsn");

    // Same export used in production (freerouting.rs::export_dsn).
    let dsn_script = r#"
import pcbnew
import sys
brd = pcbnew.LoadBoard(sys.argv[1])
pcbnew.ExportSpecctraDSN(brd, sys.argv[2])
"#;
    if pcb_kicad::PythonScriptBuilder::new(dsn_script)
        .arg(board_path.to_string_lossy())
        .arg(dsn_path.to_string_lossy())
        .run()
        .is_err()
        || !dsn_path.exists()
    {
        eprintln!("[route test] Skipping: DSN export failed (KiCad Python not available?)");
        return;
    }

    let (_child, port) = spawn_freerouting_server(&jar_path);

    let base = format!("http://127.0.0.1:{port}");
    let h_env = "Freerouting-Environment-Host: pcb-test/1.0";
    // Freerouting requires this to be a well-formed UUID even with
    // authentication disabled — an arbitrary string is rejected with a 500.
    let h_profile = format!("Freerouting-Profile-ID: {}", uuid::Uuid::new_v4());

    let curl_json = |args: &[&str]| -> serde_json::Value {
        let output = Command::new("curl").args(args).output().expect("curl failed");
        serde_json::from_slice(&output.stdout).unwrap_or(serde_json::Value::Null)
    };

    let session = curl_json(&[
        "-s",
        "-X",
        "POST",
        &format!("{base}/v1/sessions/create"),
        "-H",
        h_env,
        "-H",
        &h_profile,
    ]);
    let session_id = session["id"]
        .as_str()
        .expect("session response missing id")
        .to_string();

    let job = curl_json(&[
        "-s",
        "-X",
        "POST",
        &format!("{base}/v1/jobs/enqueue"),
        "-H",
        h_env,
        "-H",
        &h_profile,
        "-H",
        "Content-Type: application/json",
        "-d",
        &format!(r#"{{"session_id":"{session_id}","name":"cancel-test","priority":"NORMAL"}}"#),
    ]);
    let job_id = job["id"]
        .as_str()
        .expect("enqueue response missing id")
        .to_string();

    let dsn_bytes = std::fs::read(&dsn_path).expect("failed to read exported DSN");
    let dsn_b64 = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&dsn_bytes)
    };
    let input_payload_path = sandbox.default_cwd().join("input_payload.json");
    std::fs::write(
        &input_payload_path,
        serde_json::json!({"filename": "board.dsn", "data": dsn_b64}).to_string(),
    )
    .unwrap();

    curl_json(&[
        "-s",
        "-X",
        "POST",
        &format!("{base}/v1/jobs/{job_id}/input"),
        "-H",
        h_env,
        "-H",
        &h_profile,
        "-H",
        "Content-Type: application/json",
        "--data",
        &format!("@{}", input_payload_path.display()),
    ]);

    curl_json(&[
        "-s",
        "-X",
        "PUT",
        &format!("{base}/v1/jobs/{job_id}/start"),
        "-H",
        h_env,
        "-H",
        &h_profile,
    ]);

    // Give the router a brief window to complete at least one pass before
    // cancelling — cancelling with truly zero elapsed progress is a real,
    // separate scenario (verified to correctly report "no output" rather
    // than crash, matching production's NothingToRoute handling) but isn't
    // what this test is checking. This is still "cancel while probably
    // still running or just finished" on these tiny fixtures, not a
    // guarantee we caught it mid-route — see the doc comment above.
    std::thread::sleep(Duration::from_millis(300));

    curl_json(&[
        "-s",
        "-X",
        "PUT",
        &format!("{base}/v1/jobs/{job_id}/cancel"),
        "-H",
        h_env,
        "-H",
        &h_profile,
    ]);

    let output_resp = Command::new("curl")
        .args([
            "-s",
            "-w",
            "\n%{http_code}",
            &format!("{base}/v1/jobs/{job_id}/output"),
            "-H",
            h_env,
            "-H",
            &h_profile,
        ])
        .output()
        .expect("curl failed");

    let text = String::from_utf8_lossy(&output_resp.stdout);
    let (body, code) = text
        .rsplit_once('\n')
        .expect("curl -w output missing status code");

    assert!(
        code == "200" || code == "202",
        "expected a successful output response after cancel, got {code}:\n{body}"
    );
    let output_json: serde_json::Value =
        serde_json::from_str(body).expect("output response was not valid JSON");
    let data = output_json["data"].as_str().unwrap_or("");
    assert!(
        !data.is_empty(),
        "expected non-empty routed SES data after cancel, got:\n{body}"
    );
    let decoded = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .expect("output data was not valid base64")
    };
    assert!(!decoded.is_empty(), "decoded SES output was empty");
}

#[test]
fn test_freerouting_cli() {
    // Full integration test: requires Java, FreeRouting JAR, and KiCad
    // (for DSN export, SES import, and a post-route DRC check).
    if !java_compatible() {
        eprintln!("[route test] Skipping: Java 21+ not available");
        return;
    }

    let jar_path = match resolve_freerouting_jar() {
        Some(j) => j,
        None => return,
    };

    // KiCad is needed for DSN export and SES import
    let kicad_missing = Command::new("kicad-cli")
        .arg("version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true);

    if kicad_missing {
        eprintln!("[route test] Skipping full integration: KiCad not installed");
        eprintln!("  (JAR downloaded to {})", jar_path.display());
        return;
    }

    // Build a board then route it locally
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox.write("board.zen", BOARD_WITH_LAYOUT_ZEN);

    let build_output = sandbox
        .run("pcbc", ["layout", "--no-open", "board.zen"])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("layout command failed");

    if !build_output.status.success() {
        let stderr = String::from_utf8_lossy(&build_output.stderr);
        if stderr.contains("Python") || stderr.contains("kicad") || stderr.contains("KiCad") {
            eprintln!("[route test] Skipping: KiCad Python not available");
            return;
        }
        panic!(
            "layout generation failed:\nstdout:{}\nstderr:{}",
            String::from_utf8_lossy(&build_output.stdout),
            stderr,
        );
    }

    let (code, stdout, stderr) = run_route_freerouting(
        &mut sandbox,
        &[
            "--no-open",
            "--fr-jar",
            &jar_path.to_string_lossy(),
            "--fr-timeout",
            "60",
        ],
    );

    assert!(
        code == 0,
        "route --engine freerouting failed:\nstdout:{stdout}\nstderr:{stderr}"
    );

    // The board should now be routed with no unresolved airwires. We only
    // assert on unconnected items (routing completeness), not on overall DRC
    // cleanliness: this minimal fixture has no board outline, which trips an
    // unrelated `invalid_outline` violation regardless of routing quality
    // (the same reason `pcbc publish` suppresses it for bare test boards via
    // `-S layout.drc.invalid_outline`).
    let board_path = sandbox
        .default_cwd()
        .join("build/TestBoard/layout.kicad_pcb");
    // Explicit `-o` keeps the report inside the sandbox instead of defaulting
    // to a `layout-drc.rpt` file in the test process's cwd (the repo itself).
    let drc_report_path = sandbox.default_cwd().join("layout-drc.rpt");
    let drc_output = Command::new("kicad-cli")
        .args(["pcb", "drc", "-o"])
        .arg(&drc_report_path)
        .arg(&board_path)
        .output()
        .expect("failed to run kicad-cli pcb drc");
    let drc_stdout = String::from_utf8_lossy(&drc_output.stdout);

    assert!(
        drc_stdout.contains("Found 0 unconnected items"),
        "expected no unconnected items after routing:\nstdout:{}\nstderr:{}",
        drc_stdout,
        String::from_utf8_lossy(&drc_output.stderr)
    );
}
