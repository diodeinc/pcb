#![cfg(target_os = "linux")]

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use httpmock::{HttpMockResponse, prelude::*};
use pcb_test_utils::sandbox::Sandbox;
use serde_json::json;

const LOCK: &str = "/home/sandbox/.diode/sandbox-lock.json";
const DIRECTORIES: &[(&str, &[&str])] = &[
    (
        "",
        &[
            "board.kicad_pcb",
            "main.kicad_sch",
            "main.kicad_pro",
            "notes.txt",
            "sheets/",
            "main-backups/",
        ],
    ),
    (
        "sheets/",
        &[
            "analog/",
            "obsolete.kicad_sch",
            "power.kicad_sch.tmp",
            "~power.kicad_sch.lck",
        ],
    ),
    ("sheets/analog/", &["power.kicad_sch", "local.kicad_sym"]),
    ("main-backups/", &["old.kicad_sch"]),
];

struct OpenCase {
    target: &'static str,
    editor: &'static str,
    downloaded: &'static [&'static str],
    saved: &'static [&'static str],
    removed: &'static [&'static str],
}

const BOARD: OpenCase = OpenCase {
    target: "board.kicad_pcb",
    editor: "KICAD_PCBNEW",
    downloaded: &[
        "board.kicad_pcb",
        "main.kicad_sch",
        "main.kicad_pro",
        "notes.txt",
    ],
    saved: &[
        "board.kicad_pcb",
        "main.kicad_sch",
        "main.kicad_pro",
        "notes.txt",
    ],
    removed: &[],
};
const SCHEMATIC: OpenCase = OpenCase {
    target: "main.kicad_sch",
    editor: "KICAD_EESCHEMA",
    downloaded: &[
        "main.kicad_sch",
        "main.kicad_pro",
        "sheets/analog/power.kicad_sch",
        "sheets/analog/local.kicad_sym",
        "sheets/obsolete.kicad_sch",
    ],
    saved: &[
        "main.kicad_sch",
        "main.kicad_pro",
        "sheets/analog/power.kicad_sch",
        "sheets/analog/local.kicad_sym",
        "new sheets/new.kicad_sch",
    ],
    removed: &["sheets/obsolete.kicad_sch"],
};

#[test]
fn open_syncs_kicad_saves_without_parallel_lifecycle_contention() {
    run_open_sync(&BOARD, false);
}

#[test]
fn interrupting_renewal_preserves_local_edits_and_leaves_editor_open() {
    run_open_sync(&BOARD, true);
}

#[test]
fn schematic_syncs_nested_saves_additions_and_deletions() {
    run_open_sync(&SCHEMATIC, false);
}

#[test]
fn schematic_resumes_interrupted_sync_without_relaunching_editor() {
    run_open_sync(&SCHEMATIC, true);
}

fn run_open_sync(case: &OpenCase, interrupt: bool) {
    let server = MockServer::start();
    let maintenance = Arc::new(AtomicBool::new(false));
    let busy = server.mock(|when, then| {
        let maintenance = maintenance.clone();
        when.method(POST)
            .path("/api/sandboxes/sbx_sync/access-token")
            .is_true(move |_| maintenance.load(Ordering::SeqCst));
        then.status(503)
            .json_body(json!({"code": "SANDBOX_NOT_READY"}));
    });
    let mint = server.mock(|when, then| {
        let endpoint = server.url("/sandbox");
        let busy_until = Mutex::new(Instant::now());
        when.method(POST)
            .path("/api/sandboxes/sbx_sync/access-token");
        // Concurrent mints contend for the sandbox lifecycle lock.
        then.delay(Duration::from_millis(100))
            .respond_with(move |_| {
                let mut busy_until = busy_until.lock().unwrap();
                if Instant::now() < *busy_until {
                    return HttpMockResponse::builder()
                        .status(503)
                        .body(json!({"code": "SANDBOX_NOT_READY"}).to_string())
                        .build();
                }
                *busy_until = Instant::now() + Duration::from_millis(100);
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
    // This precedes the successful uploads, so failed attempts don't count as saves.
    server.mock(|when, then| {
        let maintenance = maintenance.clone();
        when.method(PUT)
            .path("/sandbox/fs/write")
            .query_param_matches("^path$", "^/layout/")
            .is_true(move |_| maintenance.load(Ordering::SeqCst));
        then.status(503);
    });
    let mut downloads = Vec::new();
    for (dir, names) in DIRECTORIES {
        let path = format!("/layout/{dir}").trim_end_matches('/').to_string();
        server.mock(|when, then| {
            when.method(GET)
                .path("/sandbox/fs/read")
                .query_param("path", &path);
            then.status(200).json_body(
                json!({"path": path, "entries": names.iter().map(|name| json!({
                "name": name.trim_end_matches('/'),
                "path": format!("/layout/{dir}{}", name.trim_end_matches('/')),
                "type": if name.ends_with('/') { "directory" } else { "file" }, "mode": "0644",
            })).collect::<Vec<_>>()}),
            );
        });
        for name in names.iter().filter(|name| !name.ends_with('/')) {
            let relative = format!("{dir}{name}");
            let download = server.mock(|when, then| {
                when.method(GET)
                    .path("/sandbox/fs/read")
                    .query_param("path", format!("/layout/{relative}"));
                then.status(200).body(&relative);
            });
            downloads.push((relative, download));
        }
    }
    let uploads: Vec<_> = case
        .saved
        .iter()
        .map(|name| {
            server.mock(|when, then| {
                when.method(PUT)
                    .path("/sandbox/fs/write")
                    .query_param("path", format!("/layout/{name}"))
                    .header("x-connection", "sync-token")
                    .body(format!("edited {name}"));
                then.status(200).json_body(json!({}));
            })
        })
        .collect();
    let removals: Vec<_> = case
        .removed
        .iter()
        .map(|name| {
            server.mock(|when, then| {
                when.method(POST)
                    .path("/sandbox/exec")
                    .body_includes("rm -f --")
                    .body_includes(format!("/layout/{name}"));
                then.status(201).header("location", "/exec/done");
            })
        })
        .collect();
    let release = server.mock(|when, then| {
        when.method(POST)
            .path("/sandbox/exec")
            .body_includes("rm -f --")
            .body_includes(LOCK);
        then.status(201).header("location", "/exec/done");
    });
    server.mock(|when, then| {
        when.method(GET).path("/sandbox/exec/done");
        then.status(200).json_body(
            json!({"state": "exited", "exitCode": 0, "durationMs": 1, "timedOut": false}),
        );
    });
    server.mock(|when, then| {
        when.method(GET).path("/sandbox/fs/stat");
        then.status(200).json_body(json!({"size": 0}));
    });

    let mut sandbox = Sandbox::new();
    let root = sandbox.root_path().to_owned();
    let editor = root.join(if case.editor == "KICAD_EESCHEMA" {
        "eeschema"
    } else {
        "pcbnew"
    });
    write_script(
        &editor,
        r#"#!/bin/sh
set -eu
trap 'touch "$SYNC_TEST_ROOT/editor-exited"' EXIT
[ "$#" -eq 1 ]
printf 'launch\n' >> "$SYNC_TEST_ROOT/launches"
printf '%s' "$1" > "$SYNC_TEST_ROOT/opened.tmp"
mv "$SYNC_TEST_ROOT/opened.tmp" "$SYNC_TEST_ROOT/opened"
for i in $(seq 1 600); do
    if [ -f "$SYNC_TEST_ROOT/close" ]; then exit 0; fi
    if [ -f "$SYNC_TEST_ROOT/interrupt" ] && [ ! -f "$SYNC_TEST_ROOT/interrupted" ]; then
        kill -INT "$PPID"
        touch "$SYNC_TEST_ROOT/interrupted"
    fi
    sleep 0.05
done
exit 1
"#,
    );
    write_script(
        &root.join("zenity"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$SYNC_TEST_ROOT/dialog\"\n",
    );
    sandbox
        .env("DIODE_API_AUTH", "none")
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("RAYON_NUM_THREADS", "4")
        .env("PCB_URL_LAUNCHER", "1")
        .env("KICAD_PCBNEW", "/nonexistent/pcbnew")
        .env("KICAD_EESCHEMA", "/nonexistent/eeschema")
        .env(case.editor, editor.to_string_lossy())
        .env("SYNC_TEST_ROOT", root.to_string_lossy())
        .env(
            "PATH",
            format!("{}:{}", root.display(), std::env::var("PATH").unwrap()),
        );
    let uri = format!(
        "diode://{}/sandboxes/sbx_sync/fs/read?path=/layout/{}",
        server.address(),
        case.target
    );
    let command = sandbox
        .run("pcbc", ["open", &uri])
        .stdout_capture()
        .stderr_capture()
        .unchecked();
    let mut child = command.start().unwrap();
    wait_for("editor launch", || root.join("opened").exists());
    let opened = fs::read_to_string(root.join("opened")).unwrap();
    let local = Path::new(&opened).parent().unwrap();
    assert_eq!(Path::new(&opened).file_name().unwrap(), case.target);
    for (name, download) in downloads {
        if case.downloaded.contains(&name.as_str()) {
            assert_eq!(fs::read_to_string(local.join(&name)).unwrap(), name);
        } else {
            assert!(!local.join(name).exists());
            download.assert_calls(0);
        }
    }

    maintenance.store(interrupt, Ordering::SeqCst);
    // Perform KiCad-style atomic saves while the scripted editor stays open.
    for name in case.saved {
        let path = local.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path.with_extension("tmp"), format!("edited {name}")).unwrap();
        fs::rename(path.with_extension("tmp"), path).unwrap();
    }
    for name in case.removed {
        fs::remove_file(local.join(name)).unwrap();
    }
    fs::create_dir_all(local.join("main-backups")).unwrap();
    fs::write(local.join("main-backups/saved.kicad_sch"), "ignored").unwrap();

    if interrupt {
        wait_for("blocked renewal", || busy.calls() > 0);
        fs::write(root.join("interrupt"), "").unwrap();
        wait_for("interrupted CLI exit", || {
            child.try_wait().unwrap().is_some()
        });
        let output = child.wait().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Local recovery file"));
        assert_eq!(session_state(local), "recoverable");
        assert!(
            !root.join("editor-exited").exists(),
            "sync closed the editor"
        );
        release.assert_calls(0);
        for name in case.saved {
            assert_eq!(
                fs::read_to_string(local.join(name)).unwrap(),
                format!("edited {name}")
            );
        }
        maintenance.store(false, Ordering::SeqCst);
        child = command.start().unwrap();
    }
    wait_for("live sync and heartbeat", || {
        heartbeat.calls() > 0 && uploads.iter().chain(&removals).all(|mock| mock.calls() > 0)
    });
    assert!(
        child.try_wait().unwrap().is_none(),
        "sync exited while editor was open"
    );
    assert_eq!(
        fs::read_to_string(root.join("launches")).unwrap(),
        "launch\n"
    );
    if interrupt {
        assert!(
            fs::read_to_string(root.join("dialog"))
                .unwrap()
                .contains("Resume Sync")
        );
    }
    fs::write(root.join("close"), "").unwrap();
    wait_for("final sync", || child.try_wait().unwrap().is_some());
    let output = child.wait().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(session_state(local), "complete");
    for mock in uploads.iter().chain(&removals) {
        assert!(mock.calls() >= 2, "expected live and final sync");
    }
    acquire.assert_calls(1 + usize::from(interrupt));
    release.assert_calls(1);
    mint.assert_calls(1 + usize::from(interrupt));
}

#[test]
fn local_open_launches_the_selected_schematic_not_its_companion_project() {
    let mut sandbox = Sandbox::new();
    let root = sandbox.root_path().to_owned();
    let target = root.join("selected sheet.kicad_sch");
    let project = target.with_extension("kicad_pro");
    fs::write(&target, "schematic").unwrap();
    fs::write(&project, "{}").unwrap();
    let editor = root.join("eeschema");
    write_script(
        &editor,
        "#!/bin/sh\n[ \"$#\" -eq 1 ] || exit 1\nprintf '%s' \"$1\" > \"$SYNC_TEST_ROOT/opened\"\n",
    );
    sandbox
        .env("KICAD_EESCHEMA", editor.to_string_lossy())
        .env("KICAD_PCBNEW", "/nonexistent/pcbnew")
        .env("SYNC_TEST_ROOT", root.to_string_lossy());
    sandbox
        .run("pcbc", ["open", target.to_str().unwrap()])
        .run()
        .unwrap();
    wait_for("local editor", || root.join("opened").exists());
    assert_eq!(
        fs::read_to_string(root.join("opened")).unwrap(),
        target.to_str().unwrap()
    );
    for unsupported in [
        project.to_str().unwrap(),
        "diode://localhost/sandboxes/test/fs/read?path=/layout/selected.kicad_pro",
    ] {
        let output = sandbox
            .run("pcbc", ["open", unsupported])
            .stderr_capture()
            .unchecked()
            .run()
            .unwrap();
        assert!(
            !output.status.success(),
            ".kicad_pro must not be an open target"
        );
    }
}

fn write_script(path: &Path, script: &str) {
    fs::write(path, script).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn wait_for(description: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn session_state(local: &Path) -> String {
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(local.join(".pcb-sync-session.json")).unwrap()).unwrap();
    manifest["state"].as_str().unwrap().to_owned()
}
