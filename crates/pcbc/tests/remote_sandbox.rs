#![cfg(target_os = "linux")]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use httpmock::{HttpMockResponse, prelude::*};
use pcb_test_utils::sandbox::Sandbox;
use serde_json::json;

#[test]
fn open_syncs_kicad_saves_without_parallel_lifecycle_contention() {
    run_open_sync(false);
}

#[test]
fn interrupting_renewal_preserves_local_edits_and_leaves_editor_open() {
    run_open_sync(true);
}

fn run_open_sync(interrupt_during_renewal: bool) {
    const LOCK: &str = "/home/sandbox/.diode/sandbox-lock.json";
    const FILES: &[(&str, &str)] = &[
        ("board.kicad_pcb", "(kicad_pcb (version 20260101))\n"),
        ("board.kicad_pro", "{\"board\": {}}\n"),
        (
            "board.kicad_prl",
            "{\"board\": {\"visible_layers\": \"ffff\"}}\n",
        ),
        ("notes.txt", "layout notes\n"),
    ];
    let server = MockServer::start();
    // Overlapping mints while the sandbox is not ready receive SANDBOX_NOT_READY.
    // Sequential requests succeed once it is ready.
    let endpoint = server.url("/sandbox");
    let busy_until = Mutex::new(Instant::now());
    let mut mint = server.mock(|when, then| {
        when.method(POST)
            .path("/api/sandboxes/sbx_sync/access-token");
        then.delay(Duration::from_millis(100))
            .respond_with(move |_| {
                let mut busy_until = busy_until.lock().unwrap();
                let now = Instant::now();
                if now < *busy_until {
                    return HttpMockResponse::builder()
                        .status(503)
                        .body(
                            json!({
                                "error": "Sandbox lifecycle is busy; retry shortly.",
                                "code": "SANDBOX_NOT_READY",
                            })
                            .to_string(),
                        )
                        .build();
                }
                *busy_until = now + Duration::from_millis(100);
                HttpMockResponse::builder().status(200).body(json!({
                "http": {"endpoint": endpoint, "headers": {"x-connection": "sync-token"}},
                "expiresAt": 4102444800u64,
            }).to_string()).build()
            });
    });
    server.mock(|when, then| {
        when.method(GET)
            .path("/sandbox/fs/read")
            .query_param("path", LOCK);
        then.status(404);
    });
    let acquire = server.mock(|when, then| {
        when.method(PUT)
            .path("/sandbox/fs/write")
            .query_param("path", LOCK)
            .header("if-none-match", "*");
        then.status(200).header("etag", "lock-1");
    });
    let heartbeat = server.mock(|when, then| {
        when.method(PUT)
            .path("/sandbox/fs/write")
            .query_param("path", LOCK)
            .header("if-match", "lock-1");
        then.status(200).header("etag", "lock-1");
    });
    server.mock(|when, then| {
        when.method(GET)
            .path("/sandbox/fs/read")
            .query_param("path", "/layout");
        then.status(200)
            .json_body(json!({"path": "/layout", "entries":
                FILES.iter().map(|(name, content)| json!({
                    "name": name, "path": format!("/layout/{name}"), "type": "file",
                    "size": content.len(), "mode": "0644", "mtime": "2026-09-19T00:00:00Z",
                })).collect::<Vec<_>>()
            }));
    });
    for (name, content) in FILES {
        server.mock(|when, then| {
            when.method(GET)
                .path("/sandbox/fs/read")
                .query_param("path", format!("/layout/{name}"))
                .header("x-connection", "sync-token");
            then.status(200).body(*content);
        });
    }
    let mut uploads: Vec<_> = FILES
        .iter()
        .map(|(name, content)| {
            let expected = if *name == "board.kicad_pcb" {
                "(kicad_pcb (version 20260101) (general (thickness 1.2)))\n"
            } else {
                content
            };
            server.mock(|when, then| {
                when.method(PUT)
                    .path("/sandbox/fs/write")
                    .query_param("path", format!("/layout/{name}"))
                    .header("x-connection", "sync-token")
                    .body(expected);
                then.status(200).json_body(json!({}));
            })
        })
        .collect();
    let release = server.mock(|when, then| {
        when.method(POST)
            .path("/sandbox/exec")
            .body_includes("rm -f --")
            .body_includes(LOCK);
        then.status(201).header("location", "/exec/release");
    });
    server.mock(|when, then| {
        when.method(GET).path("/sandbox/exec/release");
        then.status(200).json_body(
            json!({"state": "exited", "exitCode": 0, "durationMs": 1, "timedOut": false}),
        );
    });
    server.mock(|when, then| {
        when.method(GET).path("/sandbox/fs/stat");
        then.status(200).json_body(json!({"size": 0}));
    });

    let mut sandbox = Sandbox::new();
    let editor = sandbox.root_path().join("pcbnew");
    // Exercise the real watcher with KiCad-style atomic save, but no GUI.
    fs::write(
        &editor,
        r#"#!/bin/sh
set -eu
trap 'touch "$SYNC_TEST_ROOT/editor-exited"' EXIT
printf '%s' "$1" > "$SYNC_TEST_ROOT/opened.tmp"
mv "$SYNC_TEST_ROOT/opened.tmp" "$SYNC_TEST_ROOT/opened"
for i in $(seq 1 300); do
    if [ -f "$SYNC_TEST_ROOT/save" ]; then break; fi
    sleep 0.05
done
printf '(kicad_pcb (version 20260101) (general (thickness 1.2)))\n' > "$1.tmp"
mv "$1.tmp" "$1"
for i in $(seq 1 300); do
    if [ -f "$SYNC_TEST_ROOT/close" ]; then exit 0; fi
    if [ -f "$SYNC_TEST_ROOT/interrupt" ] && [ ! -f "$SYNC_TEST_ROOT/interrupted" ]; then
        kill -INT "$PPID"
        touch "$SYNC_TEST_ROOT/interrupted"
    fi
    sleep 0.05
done
exit 1
"#,
    )
    .unwrap();
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).unwrap();
    let root = sandbox.root_path().to_owned();
    sandbox
        .env("DIODE_API_AUTH", "none")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("RAYON_NUM_THREADS", "4")
        .env("KICAD_PCBNEW", editor.to_string_lossy())
        .env("SYNC_TEST_ROOT", root.to_string_lossy());
    let uri = format!(
        "diode://{}/sandboxes/sbx_sync/fs/read?path=/layout/board.kicad_pcb",
        server.address()
    );
    let child = sandbox
        .run("pcbc", ["open", &uri])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .start()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !root.join("opened").exists() {
        if let Some(output) = child.try_wait().unwrap() {
            panic!(
                "pcb open failed before launching editor: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(Instant::now() < deadline, "editor did not start");
        thread::sleep(Duration::from_millis(10));
    }
    let pcb_path = fs::read_to_string(root.join("opened")).unwrap();
    let layout_dir = std::path::Path::new(&pcb_path).parent().unwrap();
    for (name, content) in FILES {
        assert_eq!(fs::read_to_string(layout_dir.join(name)).unwrap(), *content);
    }
    if interrupt_during_renewal {
        mint.delete();
        let busy = server.mock(|when, then| {
            when.method(POST)
                .path("/api/sandboxes/sbx_sync/access-token");
            then.status(503)
                .json_body(json!({"code": "SANDBOX_NOT_READY"}));
        });
        for upload in &mut uploads {
            upload.delete();
        }
        // Reject the old connection so saves and heartbeats share a blocked
        // renewal. No edited file can reach the sandbox during maintenance.
        server.mock(|when, then| {
            when.method(PUT)
                .path("/sandbox/fs/write")
                .query_param_matches("^path$", "^/layout/");
            then.status(503);
        });
        fs::write(root.join("save"), "").unwrap();
        while busy.calls() == 0 {
            assert!(child.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline, "renewal never started");
            thread::sleep(Duration::from_millis(10));
        }
        fs::write(root.join("interrupt"), "").unwrap();
        let stop_deadline = Instant::now() + Duration::from_secs(8);
        while child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < stop_deadline, "shutdown hung in renewal");
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Local recovery file"));
        assert_eq!(
            fs::read_to_string(&pcb_path).unwrap(),
            "(kicad_pcb (version 20260101) (general (thickness 1.2)))\n"
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(layout_dir.join(".pcb-sync-session.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["state"], "recoverable");
        assert!(
            !root.join("editor-exited").exists(),
            "sync closed the editor"
        );
        release.assert_calls(0);
        fs::write(root.join("close"), "").unwrap();
        while !root.join("editor-exited").exists() {
            assert!(Instant::now() < deadline, "test editor did not exit");
            thread::sleep(Duration::from_millis(10));
        }
        return;
    }
    fs::write(root.join("save"), "").unwrap();
    while uploads.iter().any(|upload| upload.calls() == 0) || heartbeat.calls() == 0 {
        assert!(
            child.try_wait().unwrap().is_none(),
            "sync exited while editor was open"
        );
        assert!(
            Instant::now() < deadline,
            "save or heartbeat did not reach sandbox"
        );
        thread::sleep(Duration::from_millis(10));
    }
    // Uploads must happen while the editor is open, not just at final sync.
    fs::write(root.join("close"), "").unwrap();
    let output = child.wait().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for upload in uploads {
        assert!(upload.calls() >= 2, "expected live and final uploads");
    }
    acquire.assert_calls(1);
    release.assert_calls(1);
    mint.assert_calls(1);
}
