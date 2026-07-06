//! Integration tests for the sandbox data-plane client: minting sandbox
//! access tokens from the API and using them against the orchestrator data
//! plane (fs read/write/list, job-shaped exec, CAS locks).

use std::sync::Once;
use std::time::Duration;

use httpmock::Mock;
use httpmock::prelude::*;
use pcb_diode_api::{ExecSyncRequest, SandboxClient, SandboxLockOptions, WorkspaceContext};

const SANDBOX: &str = "sbx_1";
const LOCK_QUERY: &str = "/home/sandbox/.diode/sandbox-lock.json";

fn client_for(server: &MockServer) -> SandboxClient {
    static DISABLE_AUTH: Once = Once::new();
    DISABLE_AUTH.call_once(|| unsafe { std::env::set_var("DIODE_API_AUTH", "none") });
    SandboxClient::new(WorkspaceContext::from_api_base_url(server.base_url()))
        .expect("create sandbox client")
}

/// Mint mock pointing the data plane back at the mock server itself.
fn mock_mint<'a>(server: &'a MockServer, token: &str) -> Mock<'a> {
    let body = serde_json::json!({
        "token": token,
        "expiresAt": 4102444800u64,
        "dataPlaneUrl": server.base_url(),
    });
    server.mock(move |when, then| {
        when.method(POST).path("/api/sandboxes/sbx_1/access-token");
        then.status(200).json_body(body);
    })
}

fn exec_info_json(state: &str, exit_code: Option<i32>) -> serde_json::Value {
    serde_json::json!({
        "id": "exec-7",
        "state": state,
        "durationMs": 12,
        "exitCode": exit_code,
        "timedOut": false,
        "canceled": false,
    })
}

/// Mocks for one wrapped exec that finishes immediately: create (matching
/// the given body substrings), poll, and stat of both output files
/// (`out_size`/`err_size`), plus reads when the sizes are small but nonzero.
fn mock_exec<'a>(
    server: &'a MockServer,
    body_includes: &[&str],
    out: (u64, &str),
    err: (u64, &str),
) -> (Mock<'a>, Mock<'a>) {
    let body_includes: Vec<String> = body_includes.iter().map(ToString::to_string).collect();
    let create = server.mock(move |mut when, then| {
        when = when.method(POST).path("/sandboxes/sbx_1/exec");
        for substring in body_includes {
            when = when.body_includes(substring);
        }
        then.status(201).header("Location", "/exec/exec-7");
    });
    let poll = server.mock(|when, then| {
        when.method(GET).path("/sandboxes/sbx_1/exec/exec-7");
        then.status(200)
            .json_body(exec_info_json("exited", Some(0)));
    });
    for (kind, (size, content)) in [("out", out), ("err", err)] {
        server.mock(move |when, then| {
            when.method(GET)
                .path("/sandboxes/sbx_1/fs/stat")
                .query_param_matches("^path$", format!("\\.{kind}$"));
            then.status(200).json_body(serde_json::json!({
                "path": format!("/tmp/.pcb-exec/x.{kind}"),
                "type": "file",
                "size": size,
                "mode": "0644",
            }));
        });
        server.mock(move |when, then| {
            when.method(GET)
                .path("/sandboxes/sbx_1/fs/read")
                .query_param_matches("^path$", format!("\\.{kind}$"));
            then.status(200).body(content);
        });
    }
    (create, poll)
}

#[test]
fn reads_files_with_minted_token_and_caches_it() {
    let server = MockServer::start();
    let mint = mock_mint(&server, "minted-token");
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
fn re_mints_once_when_data_plane_rejects_the_token() {
    let server = MockServer::start();
    let mint = mock_mint(&server, "rejected-token");
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
fn exec_runs_job_shaped_and_returns_output() {
    let server = MockServer::start();
    let _mint = mock_mint(&server, "minted-token");
    // The create request must carry the wrapped command (output redirected
    // to files) with the caller's command and timeout intact.
    let (create, poll) = mock_exec(
        &server,
        &["exec >/tmp/.pcb-exec/", "; echo hi", "\"timeoutMs\":30000"],
        (3, "hi\n"),
        (4, "warn"),
    );

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
    poll.assert();
    create.assert();
}

#[test]
fn exec_skips_empty_and_oversized_output_downloads() {
    let server = MockServer::start();
    let _mint = mock_mint(&server, "minted-token");
    // stdout is empty, stderr is far over the download cap; neither should
    // be fetched with fs/read.
    let _ = mock_exec(
        &server,
        &[],
        (0, "unused"),
        (10 * 1024 * 1024 * 1024, "unused"),
    );

    let client = client_for(&server);
    let output = client
        .exec_sync(SANDBOX, ExecSyncRequest::command("true"))
        .unwrap();

    assert_eq!(output.stdout, "");
    assert!(
        output.stderr.contains("not downloaded"),
        "stderr: {}",
        output.stderr
    );
}

#[test]
fn exec_gives_up_after_repeated_poll_failures_and_cancels() {
    let server = MockServer::start();
    let _mint = mock_mint(&server, "minted-token");
    let _create = server.mock(|when, then| {
        when.method(POST).path("/sandboxes/sbx_1/exec");
        then.status(201).header("Location", "/exec/exec-7");
    });
    let poll = server.mock(|when, then| {
        when.method(GET).path("/sandboxes/sbx_1/exec/exec-7");
        then.status(500);
    });
    let cancel = server.mock(|when, then| {
        when.method(DELETE).path("/sandboxes/sbx_1/exec/exec-7");
        then.status(202);
    });

    let client = client_for(&server);
    let err = client
        .exec_sync(SANDBOX, ExecSyncRequest::command("echo hi"))
        .unwrap_err();

    assert!(
        format!("{err:#}").contains("500"),
        "unexpected error: {err:#}"
    );
    poll.assert_calls(3);
    cancel.assert_calls(1);
}

#[test]
fn writes_files_through_the_data_plane() {
    let server = MockServer::start();
    let _mint = mock_mint(&server, "minted-token");
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
    let _mint = mock_mint(&server, "minted-token");
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

fn lock_file_json(lease_id: &str, expires_at: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "local-edit",
        "holder": "someone",
        "leaseId": lease_id,
        "startedAt": "2020-01-01T00:00:00Z",
        "updatedAt": "2020-01-01T00:00:00Z",
        "expiresAt": expires_at,
        "ttlSeconds": 90,
    })
}

#[test]
fn refuses_to_take_an_active_lock() {
    let server = MockServer::start();
    let _mint = mock_mint(&server, "minted-token");
    server.mock(|when, then| {
        when.method(GET)
            .path("/sandboxes/sbx_1/fs/read")
            .query_param("path", LOCK_QUERY);
        then.status(200)
            .header("etag", "\"lock-v1\"")
            .json_body(lock_file_json("other-lease", "2099-01-01T00:00:00Z"));
    });

    let client = client_for(&server);
    let err = client
        .acquire_lock(SANDBOX, SandboxLockOptions::local_edit("pcb open"))
        .map(|_| ())
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("already locked"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn reclaims_stale_lock_with_compare_and_swap() {
    let server = MockServer::start();
    let _mint = mock_mint(&server, "minted-token");
    // A stale lock held by someone else; acquire must overwrite it with an
    // If-Match on exactly the etag it read.
    let read = server.mock(|when, then| {
        when.method(GET)
            .path("/sandboxes/sbx_1/fs/read")
            .query_param("path", LOCK_QUERY);
        then.status(200)
            .header("etag", "\"stale-lock\"")
            .json_body(lock_file_json("other-lease", "2020-01-02T00:00:00Z"));
    });
    let reclaim = server.mock(|when, then| {
        when.method(PUT)
            .path("/sandboxes/sbx_1/fs/write")
            .query_param("path", LOCK_QUERY)
            .header("if-match", "\"stale-lock\"");
        then.status(200).header("etag", "\"our-lock\"").body("{}");
    });

    let client = client_for(&server);
    let guard = client
        .acquire_lock(SANDBOX, SandboxLockOptions::local_edit("pcb open"))
        .unwrap();
    assert!(guard.is_active());
    reclaim.assert_calls(1);

    // Release re-reads the lock; the (static) mock still returns the other
    // lease, so release concludes the lock is no longer ours and leaves it.
    guard.release().unwrap();
    read.assert_calls(2);
}

#[test]
fn lock_lifecycle_heartbeats_with_cas_and_releases() {
    let server = MockServer::start();
    let _mint = mock_mint(&server, "minted-token");
    // No lock exists: acquire must be a create-only write.
    server.mock(|when, then| {
        when.method(GET)
            .path("/sandboxes/sbx_1/fs/read")
            .query_param("path", LOCK_QUERY);
        then.status(404);
    });
    let acquire = server.mock(|when, then| {
        when.method(PUT)
            .path("/sandboxes/sbx_1/fs/write")
            .query_param("path", LOCK_QUERY)
            .header("if-none-match", "*");
        then.status(200).header("etag", "\"lock-v1\"").body("{}");
    });
    // Heartbeats CAS against the previous write's etag; keep returning the
    // same etag so every heartbeat matches.
    let heartbeat = server.mock(|when, then| {
        when.method(PUT)
            .path("/sandboxes/sbx_1/fs/write")
            .query_param("path", LOCK_QUERY)
            .header("if-match", "\"lock-v1\"");
        then.status(200).header("etag", "\"lock-v1\"").body("{}");
    });
    // Release deletes the lock file via a wrapped `rm` exec.
    let (rm_create, _) = mock_exec(&server, &["; rm -f -- "], (0, ""), (0, ""));

    let client = client_for(&server);
    let mut options = SandboxLockOptions::local_edit("pcb open");
    options.ttl = Duration::from_secs(2);
    options.heartbeat_interval = Duration::from_secs(1);
    let guard = client.acquire_lock(SANDBOX, options).unwrap();
    acquire.assert_calls(1);

    std::thread::sleep(Duration::from_millis(1400));
    assert!(guard.is_active());
    guard.release().unwrap();

    assert!(heartbeat.calls() >= 1, "expected at least one heartbeat");
    rm_create.assert_calls(1);
}
