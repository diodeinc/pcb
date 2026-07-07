#![cfg(not(target_os = "windows"))]
//! Integration tests for `pcb datasheet <QUERY>`, one per resolution tier.
//!
//! - workspace tier: a workspace fixture with a vendored component package
//! - refdes tier: board evaluation preferring the design's own resolved symbol
//! - registry-index tier: a minimal local registry SQLite index
//! - kicad-index tier: a fixture KiCad symbol library (via `PCB_KICAD_SYMBOL_PATH`)
//! - download tier: mocked `POST /api/component/download`
//! - web-search tier: mocked `POST /api/component/search`

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;

use base64::Engine;
use pcb_test_utils::sandbox::Sandbox;

const WORKSPACE_TOML: &str = r#"[workspace]
pcb-version = "0.3"
members = ["components/**"]
"#;

const LM358_SYMBOL: &str = r#"(kicad_symbol_lib
  (symbol "LM358"
    (property "Datasheet" "https://symbol.example.com/lm358.pdf" (at 0 0 0))
    (symbol "LM358_1_1"
      (pin passive line (at 0 0 0) (length 2.54) (name "A") (number "1"))
      (pin passive line (at 0 0 0) (length 2.54) (name "B") (number "2"))
    )
  )
)
"#;

/// Run `pcb datasheet <args>` in the sandbox with extra environment overrides.
/// Returns `(success, stdout, stderr)`.
fn run_datasheet(
    sb: &mut Sandbox,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> (bool, String, String) {
    // Skip the self-update check in tests, plus any per-test overrides. These must be part of the
    // injected env map (duct's `full_env` would otherwise clobber chained `.env()` calls).
    sb.env("CI", "1");
    for (k, v) in extra_env {
        sb.env(*k, *v);
    }

    let mut full_args = vec!["datasheet"];
    full_args.extend_from_slice(args);

    let output = sb
        .run("pcb", full_args)
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("pcb datasheet should execute");

    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    )
}

/// Absolute path to the sandbox auth.toml (under the isolated `PCB_CONFIG_DIR`).
fn write_auth_token(sb: &Sandbox) {
    let auth_path = sb.root_path().join("home/.pcb/auth.toml");
    std::fs::create_dir_all(auth_path.parent().unwrap()).unwrap();
    // expires_at far in the future so no refresh (network) is attempted.
    let toml = "access_token = \"test-token\"\nrefresh_token = \"test-refresh\"\nexpires_at = 4102444800\nemail = \"test@example.com\"\n";
    std::fs::write(&auth_path, toml).unwrap();
}

/// Spawn a tiny mock HTTP server. Each route is `(path_substring, json_body)`.
/// Returns the base URL (e.g. `http://127.0.0.1:PORT`).
fn spawn_mock_api(routes: Vec<(&'static str, String)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let port = listener.local_addr().unwrap().port();

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());

            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }

            // Read headers, capturing Content-Length.
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    break;
                }
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(rest) = lower.strip_prefix("content-length:") {
                    content_length = rest.trim().parse().unwrap_or(0);
                }
            }
            if content_length > 0 {
                let mut body = vec![0u8; content_length];
                let _ = reader.read_exact(&mut body);
            }

            let matched = routes
                .iter()
                .find(|(path, _)| request_line.contains(path))
                .map(|(_, body)| body.clone());

            let (status, payload) = match matched {
                Some(body) => (200, body),
                None => (404, "{}".to_string()),
            };

            let response = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    format!("http://127.0.0.1:{port}")
}

/// Environment overrides that force direct connections to the loopback mock server
/// (the sandbox otherwise routes HTTP through a dead proxy to block egress).
fn direct_network_env(api_url: &str) -> Vec<(&str, &str)> {
    vec![
        ("DIODE_API_URL", api_url),
        ("HTTP_PROXY", ""),
        ("HTTPS_PROXY", ""),
        ("http_proxy", ""),
        ("https_proxy", ""),
        ("NO_PROXY", "127.0.0.1,localhost"),
        ("no_proxy", "127.0.0.1,localhost"),
    ]
}

fn create_registry_db(path: &Path, rows: &[(&str, &str, &str, Option<&str>)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE packages (
            id INTEGER PRIMARY KEY,
            url TEXT,
            mpn TEXT,
            manufacturer TEXT,
            digikey TEXT,
            edatasheet TEXT
        );",
    )
    .unwrap();
    for (i, (url, mpn, manufacturer, digikey)) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO packages (id, url, mpn, manufacturer, digikey, edatasheet) VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
            rusqlite::params![i as i64 + 1, url, mpn, manufacturer, digikey],
        )
        .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Workspace tier
// ---------------------------------------------------------------------------

#[test]
fn datasheet_workspace_tier_prefers_local_pdf() {
    let mut sb = Sandbox::new();
    sb.write("pcb.toml", WORKSPACE_TOML)
        .write("components/TestMfr/LM358/LM358.kicad_sym", LM358_SYMBOL)
        .write("components/TestMfr/LM358/LM358.pdf", b"%PDF-1.4 local");

    let (ok, stdout, stderr) = run_datasheet(&mut sb, &["LM358", "--offline"], &[]);
    assert!(ok, "expected success. stderr: {stderr}");
    assert!(
        stdout.ends_with("components/TestMfr/LM358/LM358.pdf"),
        "expected local PDF path, got: {stdout}"
    );

    let (ok, stdout, _) = run_datasheet(&mut sb, &["LM358", "--offline", "-f", "json"], &[]);
    assert!(ok);
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["interpretation"], "mpn");
    assert_eq!(json["source"], "workspace");
    assert_eq!(json["mpn"], "LM358");
}

#[test]
fn datasheet_workspace_tier_falls_back_to_symbol_property() {
    let mut sb = Sandbox::new();
    sb.write("pcb.toml", WORKSPACE_TOML)
        .write("components/TestMfr/LM358/LM358.kicad_sym", LM358_SYMBOL);
    // No sibling PDF -> use the symbol's Datasheet property.

    let (ok, stdout, stderr) = run_datasheet(&mut sb, &["LM358", "--offline", "-f", "json"], &[]);
    assert!(ok, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["source"], "workspace");
    assert_eq!(json["url"], "https://symbol.example.com/lm358.pdf");
}

#[test]
fn datasheet_workspace_tier_manufacturer_disambiguation() {
    // Two component packages share the MPN; --manufacturer must pick the matching one
    // (canonical layout: components/<manufacturer>/<mpn>/).
    let symbol_for = |url: &str| {
        format!(
            "(kicad_symbol_lib (symbol \"LM358\" (property \"Datasheet\" \"{url}\" (at 0 0 0))))"
        )
    };
    let mut sb = Sandbox::new();
    sb.write("pcb.toml", WORKSPACE_TOML)
        .write(
            "components/AcmeA/LM358/LM358.kicad_sym",
            symbol_for("https://acme-a.example.com/lm358.pdf"),
        )
        .write(
            "components/AcmeB/LM358/LM358.kicad_sym",
            symbol_for("https://acme-b.example.com/lm358.pdf"),
        );

    for (mfr, url) in [
        ("AcmeA", "https://acme-a.example.com/lm358.pdf"),
        ("AcmeB", "https://acme-b.example.com/lm358.pdf"),
    ] {
        let (ok, stdout, stderr) = run_datasheet(
            &mut sb,
            &["LM358", "--manufacturer", mfr, "--offline", "-f", "json"],
            &[],
        );
        assert!(ok, "stderr: {stderr}");
        let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(json["source"], "workspace");
        assert_eq!(json["url"], url, "wrong datasheet for manufacturer {mfr}");
    }
}

// ---------------------------------------------------------------------------
// Reference-designator tier
// ---------------------------------------------------------------------------

#[test]
fn datasheet_refdes_tier_uses_design_symbol() {
    let board = r#"
sym = Symbol(library = "./components/TestMfr/LM358/LM358.kicad_sym")
n1 = Net("N1")
n2 = Net("N2")
Component(
    name = "U1",
    symbol = sym,
    footprint = "Package_SO:SOIC-8_3.9x4.9mm_P1.27mm",
    mpn = "LM358",
    manufacturer = "TestMfr",
    pins = {"A": n1, "B": n2},
)
"#;

    let mut sb = Sandbox::new();
    sb.write("pcb.toml", WORKSPACE_TOML)
        .write("components/TestMfr/LM358/LM358.kicad_sym", LM358_SYMBOL)
        .write("components/TestMfr/LM358/LM358.pdf", b"%PDF-1.4 local")
        .write("board.zen", board);

    let (ok, stdout, stderr) = run_datasheet(
        &mut sb,
        &[
            "U1",
            "--refdes",
            "--board",
            "board.zen",
            "--offline",
            "-f",
            "json",
        ],
        &[],
    );
    assert!(ok, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["interpretation"], "refdes");
    assert_eq!(json["source"], "workspace");
    assert_eq!(json["mpn"], "LM358");
    assert_eq!(json["manufacturer"], "TestMfr");
    assert!(
        json["url"].as_str().unwrap().ends_with("LM358.pdf"),
        "expected local PDF, got {}",
        json["url"]
    );
}

#[test]
fn datasheet_refdes_outside_workspace_fails() {
    // Run in a temp dir that is not a workspace.
    let mut sb = Sandbox::new();
    let (ok, _stdout, stderr) = run_datasheet(&mut sb, &["U1", "--refdes"], &[]);
    assert!(!ok, "expected failure outside a workspace");
    assert!(
        stderr.contains("workspace"),
        "expected a workspace error, got: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// Registry-index tier
// ---------------------------------------------------------------------------

#[test]
fn datasheet_registry_index_tier() {
    let mut sb = Sandbox::new();
    sb.write("pcb.toml", WORKSPACE_TOML);

    let db_path = sb.root_path().join("registry/packages.db");
    create_registry_db(
        &db_path,
        &[(
            "github.com/diodeinc/registry/components/TI/OPA333",
            "OPA333",
            "Texas Instruments",
            Some(r#"{"datasheetUrl":"https://registry.example.com/opa333.pdf"}"#),
        )],
    );
    let db_str = db_path.to_string_lossy().into_owned();

    let (ok, stdout, stderr) = run_datasheet(
        &mut sb,
        &["OPA333", "--offline", "-f", "json"],
        &[("PCB_REGISTRY_DB", &db_str)],
    );
    assert!(ok, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["source"], "registry_index");
    assert_eq!(json["url"], "https://registry.example.com/opa333.pdf");
}

// ---------------------------------------------------------------------------
// KiCad-index tier
// ---------------------------------------------------------------------------

#[test]
fn datasheet_kicad_index_tier() {
    let mut sb = Sandbox::new();
    sb.write("pcb.toml", WORKSPACE_TOML);

    let kicad_dir = sb.root_path().join("kicad-symbols");
    std::fs::create_dir_all(&kicad_dir).unwrap();
    std::fs::write(
        kicad_dir.join("Amplifier.kicad_sym"),
        "(kicad_symbol_lib (symbol \"TL072\" (property \"Datasheet\" \"https://kicad.example.com/tl072.pdf\" (at 0 0 0))))",
    )
    .unwrap();
    let kicad_str = kicad_dir.to_string_lossy().into_owned();

    let (ok, stdout, stderr) = run_datasheet(
        &mut sb,
        &["TL072", "--offline", "-f", "json"],
        &[("PCB_KICAD_SYMBOL_PATH", &kicad_str)],
    );
    assert!(ok, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["source"], "kicad_index");
    assert_eq!(json["url"], "https://kicad.example.com/tl072.pdf");
}

// ---------------------------------------------------------------------------
// Download tier (encoded component id)
// ---------------------------------------------------------------------------

#[test]
fn datasheet_download_tier_component_id() {
    let mut sb = Sandbox::new();
    sb.write("pcb.toml", WORKSPACE_TOML);
    write_auth_token(&sb);

    let component_id = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({
            "source": "web",
            "mpn": "LM358",
            "manufacturer": "Texas Instruments",
            "backendId": 1234,
        }))
        .unwrap(),
    );

    let download_body = serde_json::json!({
        "datasheetUrl": "https://signed.example.com/lm358-download.pdf",
        "metadata": {"mpn": "LM358", "timestamp": "2024-01-01", "source": "web"}
    })
    .to_string();

    let api_url = spawn_mock_api(vec![("/api/component/download", download_body)]);
    let env = direct_network_env(&api_url);

    let (ok, stdout, stderr) = run_datasheet(&mut sb, &[&component_id, "--id", "-f", "json"], &env);
    assert!(ok, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["interpretation"], "component_id");
    assert_eq!(json["source"], "download_cache");
    assert_eq!(json["url"], "https://signed.example.com/lm358-download.pdf");
    assert_eq!(json["mpn"], "LM358");
}

// ---------------------------------------------------------------------------
// Web-search tier
// ---------------------------------------------------------------------------

#[test]
fn datasheet_web_search_tier() {
    let mut sb = Sandbox::new();
    sb.write("pcb.toml", WORKSPACE_TOML);
    write_auth_token(&sb);

    // Empty registry index + empty KiCad symbol dir so those tiers deterministically miss.
    let db_path = sb.root_path().join("registry/packages.db");
    create_registry_db(&db_path, &[]);
    let db_str = db_path.to_string_lossy().into_owned();
    let kicad_dir = sb.root_path().join("empty-symbols");
    std::fs::create_dir_all(&kicad_dir).unwrap();
    let kicad_str = kicad_dir.to_string_lossy().into_owned();

    let search_body = serde_json::json!([
        {
            "part_number": "LM358",
            "manufacturer": "Texas Instruments",
            "component_id": "abc",
            "datasheets": ["https://web.example.com/ti-lm358.pdf"],
            "score": 9.9
        },
        {
            "part_number": "LM358",
            "manufacturer": "ON Semiconductor",
            "component_id": "def",
            "datasheets": ["https://web.example.com/on-lm358.pdf"],
            "score": 3.0
        }
    ])
    .to_string();

    let api_url = spawn_mock_api(vec![("/api/component/search", search_body)]);
    let mut env = direct_network_env(&api_url);
    env.push(("PCB_REGISTRY_DB", &db_str));
    env.push(("PCB_KICAD_SYMBOL_PATH", &kicad_str));

    // Without --manufacturer, the higher-scored TI result wins.
    let (ok, stdout, stderr) = run_datasheet(&mut sb, &["LM358", "-f", "json"], &env);
    assert!(ok, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["source"], "web_search");
    assert_eq!(json["url"], "https://web.example.com/ti-lm358.pdf");
    assert_eq!(json["mpn"], "LM358");

    // --manufacturer selects the ON Semiconductor result even though it scores lower.
    let (ok, stdout, stderr) = run_datasheet(
        &mut sb,
        &["LM358", "--manufacturer", "ON Semiconductor", "-f", "json"],
        &env,
    );
    assert!(ok, "stderr: {stderr}");
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["url"], "https://web.example.com/on-lm358.pdf");
}
