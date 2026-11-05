#![cfg(not(target_os = "windows"))]

use std::io::{BufRead, Read};

use httpmock::MockServer;
use pcb_test_utils::sandbox::Sandbox;
use serde_json::{Value, json};

#[test]
fn schematic_evaluation_does_not_request_bom_matches() {
    for args in [&["lsp"][..], &["lsp", "--offline"][..]] {
        let server = MockServer::start();
        let network = server.mock(|when, then| {
            when.any_request();
            then.status(500);
        });
        let source = r#"
Resistor = Module("@stdlib/generics/Resistor.zen")
Resistor(name = "R1", value = "10kOhm", package = "0603", P1 = Net("A"), P2 = Net("B"))
"#;
        let mut sandbox = Sandbox::new().with_workspace();
        sandbox
            .env("DIODE_API_URL", server.base_url())
            .env("DIODE_API_AUTH", "none")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .write("main.zen", source)
            .sync();
        let uri = format!("file://{}", sandbox.root_path().join("main.zen").display());
        let root_uri = format!("file://{}", sandbox.root_path().display());
        let changed_source = source.replace("10kOhm", "22kOhm");
        let messages = [
            json!({"id": 1, "method": "initialize", "params": {
                "capabilities": {}, "rootUri": root_uri
            }}),
            json!({"method": "initialized", "params": {}}),
            // Exercise the uncached viewer-state fallback before didOpen.
            json!({"id": 2, "method": "viewer/getState", "params": {"uri": uri}}),
            json!({"method": "textDocument/didOpen", "params": {"textDocument": {
                "uri": uri, "languageId": "starlark", "version": 1, "text": source
            }}}),
            json!({"id": 3, "method": "zener/evaluate", "params": {"uri": uri, "inputs": {}}}),
            // didOpen populates the cached viewer state.
            json!({"id": 4, "method": "viewer/getState", "params": {"uri": uri}}),
            json!({"method": "textDocument/didChange", "params": {
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": changed_source}]
            }}),
            json!({"id": 5, "method": "zener/evaluate", "params": {"uri": uri, "inputs": {}}}),
            json!({"id": 6, "method": "shutdown", "params": null}),
            json!({"method": "exit", "params": null}),
        ];
        let responses = run_lsp(&mut sandbox, args, messages);
        for id in [2, 3, 4, 5] {
            let response = responses.iter().find(|r| r["id"] == id).unwrap();
            assert!(response.get("error").is_none(), "{response}");
            let schematic = if id == 2 || id == 4 {
                &response["result"]["state"]
            } else {
                assert_eq!(response["result"]["success"], true, "{response}");
                &response["result"]["schematic"]
            };
            assert!(!schematic["instances"].as_object().unwrap().is_empty());
        }
        network.assert_calls(0);
    }
}

/// A component with no part information raises `bom.unspecified` (an error),
/// but evaluation still produces a schematic.
const UNSPECIFIED_PART_ZEN: &str = r#"
Component(
    name = "U1",
    prefix = "U",
    footprint = File("test.kicad_mod"),
    pin_defs = {"A": "1"},
    pins = {"A": Net("A")},
)
"#;

const ONE_PAD_KICAD_MOD: &str = r#"(footprint "test"
  (layer "F.Cu")
  (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu"))
)
"#;

#[test]
fn evaluation_with_errors_still_returns_schematic() {
    let mut sandbox = Sandbox::new().with_workspace();
    sandbox
        .write("main.zen", UNSPECIFIED_PART_ZEN)
        .write("test.kicad_mod", ONE_PAD_KICAD_MOD);
    let uri = format!("file://{}", sandbox.root_path().join("main.zen").display());
    let root_uri = format!("file://{}", sandbox.root_path().display());
    let responses = run_lsp(
        &mut sandbox,
        &["lsp", "--offline"],
        [
            json!({"id": 1, "method": "initialize", "params": {
                "capabilities": {}, "rootUri": root_uri
            }}),
            json!({"method": "initialized", "params": {}}),
            json!({"id": 2, "method": "zener/evaluate", "params": {"uri": uri, "inputs": {}}}),
            json!({"id": 3, "method": "shutdown", "params": null}),
            json!({"method": "exit", "params": null}),
        ],
    );
    let result = &responses.iter().find(|r| r["id"] == 2).unwrap()["result"];
    assert_eq!(result["success"], false, "{result}");
    assert!(
        result["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["level"] == "error"
                && d["message"]
                    .as_str()
                    .unwrap()
                    .contains("missing part information")),
        "{result}"
    );
    assert!(
        !result["schematic"]["instances"]
            .as_object()
            .unwrap()
            .is_empty(),
        "{result}"
    );
}

fn run_lsp(
    sandbox: &mut Sandbox,
    args: &[&str],
    messages: impl IntoIterator<Item = Value>,
) -> Vec<Value> {
    let mut input = String::new();
    for mut message in messages {
        message["jsonrpc"] = json!("2.0");
        let body = message.to_string();
        input.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    }
    let output = sandbox
        .run("pcbc", args)
        .stdin_bytes(input)
        .stdout_capture()
        .stderr_capture()
        .run()
        .expect("LSP session should complete");

    let mut stdout = std::io::BufReader::new(output.stdout.as_slice());
    let mut responses = Vec::new();
    loop {
        let mut header = String::new();
        if stdout.read_line(&mut header).unwrap() == 0 {
            break;
        }
        let length: usize = header
            .strip_prefix("Content-Length: ")
            .expect("LSP frame header")
            .trim()
            .parse()
            .unwrap();
        let mut separator = [0; 2];
        stdout.read_exact(&mut separator).unwrap();
        assert_eq!(&separator, b"\r\n");
        let mut body = vec![0; length];
        stdout.read_exact(&mut body).unwrap();
        responses.push(serde_json::from_slice::<Value>(&body).unwrap());
    }
    responses
}
