//! Integration tests for the sandbox data-plane client: minting sandbox
//! access tokens from the API and using them against the orchestrator data
//! plane (fs read/write/list, exec over SSE).

use std::sync::Once;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use httpmock::Mock;
use httpmock::prelude::*;
use pcb_diode_api::{ExecSyncRequest, SandboxClient, WorkspaceContext};

const SANDBOX: &str = "sbx_1";
const EXEC_STATUS_OK: &str = r#"{"exitCode":0,"durationMs":12,"timedOut":false,"canceled":false}"#;

fn client_for(server: &MockServer) -> SandboxClient {
    static DISABLE_AUTH: Once = Once::new();
    DISABLE_AUTH.call_once(|| unsafe { std::env::set_var("DIODE_API_AUTH", "none") });
    SandboxClient::new(WorkspaceContext::from_api_base_url(server.base_url()))
        .expect("create sandbox client")
}

/// Mint mock pointing the data plane back at the mock server itself.
fn mock_mint<'a>(server: &'a MockServer, token: &str, ttl_secs: i64) -> Mock<'a> {
    let body = serde_json::json!({
        "token": token,
        "expiresAt": chrono::Utc::now().timestamp() + ttl_secs,
        "dataPlaneUrl": server.base_url(),
    });
    server.mock(move |when, then| {
        when.method(POST).path("/api/sandboxes/sbx_1/access-token");
        then.status(200).json_body(body);
    })
}

fn sse_body(events: &[(u64, &str, &str)]) -> String {
    events
        .iter()
        .map(|(id, event, data)| format!("id: {id}\nevent: {event}\ndata: {data}\n\n"))
        .collect()
}

#[test]
fn reads_files_with_minted_token_and_caches_it() {
    let server = MockServer::start();
    let mint = mock_mint(&server, "minted-token", 20 * 60);
    let read = server.mock(|when, then| {
        when.method(GET)
            .path("/sandboxes/sbx_1/fs/read")
            .query_param("path", "/home/sandbox/main.zen")
            .header("authorization", "Bearer minted-token");
        then.status(200).body("zen-content");
    });

    let client = client_for(&server);
    for _ in 0..2 {
        let bytes = client.read_file(SANDBOX, "/home/sandbox/main.zen").unwrap();
        assert_eq!(bytes, b"zen-content");
    }

    read.assert_calls(2);
    mint.assert_calls(1);
}

#[test]
fn re_mints_token_within_expiry_margin() {
    let server = MockServer::start();
    // Expires within the client's 2-minute refresh margin, so the cached
    // token must not be reused.
    let mint = mock_mint(&server, "short-token", 60);
    let read = server.mock(|when, then| {
        when.method(GET).path("/sandboxes/sbx_1/fs/read");
        then.status(200).body("ok");
    });

    let client = client_for(&server);
    for _ in 0..2 {
        client.read_file(SANDBOX, "/home/sandbox/main.zen").unwrap();
    }

    read.assert_calls(2);
    mint.assert_calls(2);
}

#[test]
fn falls_back_to_cached_token_when_refresh_fails() {
    let server = MockServer::start();
    // Valid for another 90s — inside the 2-minute refresh margin, so every
    // call attempts a refresh, but the token itself is still usable.
    let mut mint = mock_mint(&server, "stale-token", 90);
    let read = server.mock(|when, then| {
        when.method(GET)
            .path("/sandboxes/sbx_1/fs/read")
            .header("authorization", "Bearer stale-token");
        then.status(200).body("ok");
    });

    let client = client_for(&server);
    client.read_file(SANDBOX, "/home/sandbox/main.zen").unwrap();
    mint.assert_calls(1);
    mint.delete();
    let failing_mint = server.mock(|when, then| {
        when.method(POST).path("/api/sandboxes/sbx_1/access-token");
        then.status(500);
    });

    client.read_file(SANDBOX, "/home/sandbox/main.zen").unwrap();
    read.assert_calls(2);
    failing_mint.assert_calls(1);
}

#[test]
fn re_mints_once_when_data_plane_rejects_the_token() {
    let server = MockServer::start();
    let mint = mock_mint(&server, "rejected-token", 20 * 60);
    let read = server.mock(|when, then| {
        when.method(GET).path("/sandboxes/sbx_1/fs/read");
        then.status(401);
    });

    let client = client_for(&server);
    let err = client
        .read_file(SANDBOX, "/home/sandbox/main.zen")
        .unwrap_err();

    assert!(
        format!("{err:#}").contains("401"),
        "unexpected error: {err:#}"
    );
    read.assert_calls(2); // the original request plus exactly one retry
    mint.assert_calls(2); // re-minted before the retry
}

#[test]
fn exec_streams_events_and_returns_output() {
    let server = MockServer::start();
    let _mint = mock_mint(&server, "minted-token", 20 * 60);
    let create = server.mock(|when, then| {
        when.method(POST)
            .path("/sandboxes/sbx_1/exec")
            .json_body(serde_json::json!({"cmd": "echo hi", "timeoutMs": 30_000}));
        then.status(201).header("Location", "/exec/exec-7");
    });
    let events = server.mock(|when, then| {
        when.method(GET)
            .path("/sandboxes/sbx_1/exec/exec-7/events")
            .query_param("encoding", "base64")
            .header("authorization", "Bearer minted-token");
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(format!(
                ":keep-alive\n\n{}",
                sse_body(&[
                    (1, "stdout", &BASE64.encode(b"hi\n")),
                    (2, "stderr", &BASE64.encode(b"warn")),
                    (3, "status", EXEC_STATUS_OK),
                ])
            ));
    });

    let client = client_for(&server);
    let output = client
        .exec_sync(
            SANDBOX,
            ExecSyncRequest::command("echo hi").timeout(Duration::from_secs(30)),
        )
        .unwrap();

    assert_eq!(output.stdout, "hi\n");
    assert_eq!(output.stderr, "warn");
    assert_eq!(output.exit_code, Some(0));
    assert!(!output.timed_out);
    create.assert();
    events.assert();
}

#[test]
fn exec_reconnects_after_stream_drop_and_resumes_after_last_event() {
    let server = MockServer::start();
    let _mint = mock_mint(&server, "minted-token", 20 * 60);
    let _create = server.mock(|when, then| {
        when.method(POST).path("/sandboxes/sbx_1/exec");
        then.status(201).header("Location", "/exec/exec-7");
    });
    // First connection: one stdout event, then the stream ends without a
    // status event (dropped connection).
    let initial = server.mock(|when, then| {
        when.method(GET)
            .path("/sandboxes/sbx_1/exec/exec-7/events")
            .query_param_missing("after");
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(sse_body(&[(1, "stdout", &BASE64.encode(b"partial "))]));
    });
    let resumed = server.mock(|when, then| {
        when.method(GET)
            .path("/sandboxes/sbx_1/exec/exec-7/events")
            .query_param("after", "1");
        then.status(200)
            .header("content-type", "text/event-stream")
            .body(sse_body(&[
                (2, "stdout", &BASE64.encode(b"done")),
                (3, "status", EXEC_STATUS_OK),
            ]));
    });

    let client = client_for(&server);
    let output = client
        .exec_sync(SANDBOX, ExecSyncRequest::command("echo hi"))
        .unwrap();

    assert_eq!(output.stdout, "partial done");
    assert_eq!(output.exit_code, Some(0));
    initial.assert();
    resumed.assert();
}

#[test]
fn writes_files_through_the_data_plane() {
    let server = MockServer::start();
    let _mint = mock_mint(&server, "minted-token", 20 * 60);
    let write = server.mock(|when, then| {
        when.method(PUT)
            .path("/sandboxes/sbx_1/fs/write")
            .query_param("path", "/home/sandbox/My Board/board.kicad_pcb")
            .header("content-type", "application/octet-stream")
            .body("pcb-bytes");
        then.status(200).json_body(serde_json::json!({
            "bytesWritten": 9,
            "path": "/home/sandbox/My Board/board.kicad_pcb",
            "type": "file",
            "size": 9,
            "mode": "0644",
        }));
    });

    let client = client_for(&server);
    client
        .write_file(
            SANDBOX,
            "/home/sandbox/My Board/board.kicad_pcb",
            b"pcb-bytes",
        )
        .unwrap();
    write.assert();
}

#[test]
fn lists_directories_via_fs_read() {
    let server = MockServer::start();
    let _mint = mock_mint(&server, "minted-token", 20 * 60);
    server.mock(|when, then| {
        when.method(GET)
            .path("/sandboxes/sbx_1/fs/read")
            .query_param("path", "/home/sandbox/layout");
        then.status(200).json_body(serde_json::json!({
            "path": "/home/sandbox/layout",
            "type": "directory",
            "entries": [{
                "name": "board.kicad_pcb",
                "path": "/home/sandbox/layout/board.kicad_pcb",
                "type": "file",
                "size": 10,
                "mode": "0644",
                "mtime": "2026-07-04T00:00:00Z",
            }],
        }));
    });

    let client = client_for(&server);
    let listing = client.list(SANDBOX, "/home/sandbox/layout").unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].kind, "file");
    assert_eq!(listing.entries[0].name, "board.kicad_pcb");
}

#[test]
fn mint_404_reports_missing_access() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/api/sandboxes/sbx_1/access-token");
        then.status(404);
    });

    let client = client_for(&server);
    let err = client
        .read_file(SANDBOX, "/home/sandbox/main.zen")
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("not found or you do not have access"),
        "unexpected error: {err:#}"
    );
}
