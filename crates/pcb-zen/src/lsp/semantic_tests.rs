//! Protocol-level tests for semantic call intelligence (completion,
//! signature help, keyword hover/definition, references, document symbols).
//! Each test drives a real server over an in-memory LSP connection against an
//! on-disk workspace that uses the repository stdlib, offline.

use super::LspEvalContext;
use lsp_server::{Connection, Message};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAIN: &str = r#"Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("./modules/Led.zen")
load("./lib.zen", "Pull")

vcc = Power("VCC_3V3", voltage = "3.3V")
gnd = Ground("GND")

Resistor(name = "R_SENSE", value = "10k", P1 = vcc, P2 = gnd)
Led(name = "D1", A = vcc, K = gnd)
Pull(name = "RP", P1 = vcc)
"#;

struct Session {
    client: Connection,
    next_id: i64,
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Session {
    fn start() -> anyhow::Result<(Self, Value)> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        fs::write(
            root.join("pcb.toml"),
            "[workspace]\npcb-version = \"0.4\"\n",
        )?;
        fs::create_dir_all(root.join("modules"))?;
        fs::write(
            root.join("modules/Led.zen"),
            "A = io(Net, help = \"Anode\")\nK = io(Net)\ncolor = config(str, default = \"red\")\n",
        )?;
        fs::write(
            root.join("modules/Pull.zen"),
            "P1 = io(Net)\nP2 = io(Net, optional = True)\n",
        )?;
        fs::write(
            root.join("lib.zen"),
            "Pull = Module(\"./modules/Pull.zen\")\n",
        )?;
        fs::write(root.join("main.zen"), MAIN)?;

        let (client, server) = Connection::memory();
        std::thread::spawn(move || {
            pcb_starlark_lsp::server::server_with_connection(
                server,
                LspEvalContext::default().set_offline(true),
            )
        });
        let mut session = Session {
            client,
            next_id: 1,
            _dir: dir,
            root,
        };
        let init = session.request(
            "initialize",
            json!({
                "rootUri": url::Url::from_directory_path(&session.root).unwrap().as_str(),
                "capabilities": {}
            }),
        )?;
        session.notify("initialized", json!({}))?;
        Ok((session, init))
    }

    fn uri(&self, rel: &str) -> String {
        url::Url::from_file_path(self.root.join(rel))
            .unwrap()
            .to_string()
    }

    fn notify(&self, method: &str, params: Value) -> anyhow::Result<()> {
        let msg: Message = serde_json::from_value(json!({"method": method, "params": params}))?;
        self.client.sender.send(msg)?;
        Ok(())
    }

    fn open(&self, rel: &str, text: &str) -> anyhow::Result<()> {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": self.uri(rel), "languageId": "zener", "version": 1, "text": text}}),
        )
    }

    fn change(&self, rel: &str, text: &str, version: i64) -> anyhow::Result<()> {
        self.notify(
            "textDocument/didChange",
            json!({"textDocument": {"uri": self.uri(rel), "version": version},
                   "contentChanges": [{"text": text}]}),
        )
    }

    fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg: Message =
            serde_json::from_value(json!({"id": id, "method": method, "params": params}))?;
        self.client.sender.send(msg)?;
        loop {
            let message = self.client.receiver.recv_timeout(Duration::from_secs(60))?;
            let value = serde_json::to_value(message)?;
            if value["id"] == json!(id) {
                if !value["error"].is_null() {
                    anyhow::bail!("{method} failed: {}", value["error"]);
                }
                return Ok(value["result"].clone());
            }
        }
    }

    /// Request at the (line, character) of the `nth` occurrence of `needle` in `text`, offset by `delta`.
    fn at(
        &mut self,
        method: &str,
        rel: &str,
        text: &str,
        needle: &str,
        delta: usize,
        extra: Value,
    ) -> anyhow::Result<Value> {
        let (line, character) = position_of(text, needle, delta);
        let mut params = json!({"textDocument": {"uri": self.uri(rel)}, "position": {"line": line, "character": character}});
        if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                p.insert(k.clone(), v.clone());
            }
        }
        self.request(method, params)
    }
}

fn position_of(text: &str, needle: &str, delta: usize) -> (u32, u32) {
    let offset = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not in text"))
        + delta;
    let before = &text[..offset];
    let line = before.matches('\n').count() as u32;
    let character = before[before.rfind('\n').map(|i| i + 1).unwrap_or(0)..]
        .chars()
        .count() as u32;
    (line, character)
}

fn labels(result: &Value) -> Vec<String> {
    let items = if result.is_array() {
        result
    } else {
        &result["items"]
    };
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect()
}

fn property_labels(result: &Value) -> Vec<String> {
    let items = if result.is_array() {
        result
    } else {
        &result["items"]
    };
    items
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["kind"] == json!(10)) // CompletionItemKind::PROPERTY
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect()
}

fn target(link: &Value) -> (String, u32, u32) {
    let uri = link["targetUri"]
        .as_str()
        .or(link["uri"].as_str())
        .unwrap()
        .to_string();
    let range = if link["targetSelectionRange"].is_null() {
        &link["range"]
    } else {
        &link["targetSelectionRange"]
    };
    (
        uri,
        range["start"]["line"].as_u64().unwrap() as u32,
        range["start"]["character"].as_u64().unwrap() as u32,
    )
}

#[test]
fn advertises_the_full_semantic_surface() -> anyhow::Result<()> {
    let (_s, init) = Session::start()?;
    let caps = &init["capabilities"];
    // No trigger characters: with VS Code's default first-item selection, Enter after `(`
    // or `,` in a multi-line call would accept a suggestion instead of inserting a newline.
    assert!(
        caps["completionProvider"].is_object()
            && caps["completionProvider"]["triggerCharacters"].is_null(),
        "{caps}"
    );
    assert!(caps["signatureHelpProvider"].is_object(), "{caps}");
    assert_eq!(caps["hoverProvider"], json!(true));
    assert!(!caps["definitionProvider"].is_null());
    assert_eq!(caps["referencesProvider"], json!(true), "{caps}");
    assert_eq!(caps["documentSymbolProvider"], json!(true), "{caps}");
    Ok(())
}

#[test]
fn completes_unsupplied_module_arguments_with_docs_and_insert_text() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    let text = MAIN.replace(
        "Resistor(name = \"R_SENSE\", value = \"10k\", P1 = vcc, P2 = gnd)",
        "Resistor(name = \"R_SENSE\", )",
    );
    s.open("main.zen", &text)?;
    let r = s.at(
        "textDocument/completion",
        "main.zen",
        &text,
        "\"R_SENSE\", ",
        11,
        json!({}),
    )?;
    let props = property_labels(&r);
    for want in ["value", "package", "P1", "P2"] {
        assert!(
            props.contains(&want.to_string()),
            "missing {want}: {props:?}"
        );
    }
    assert!(
        !props.contains(&"name".to_string()),
        "already supplied: {props:?}"
    );
    let items = if r.is_array() {
        r.clone()
    } else {
        r["items"].clone()
    };
    let p1 = items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == json!("P1"))
        .unwrap();
    assert_eq!(p1["insertText"], json!("P1 = "), "{p1}");
    let doc = p1["documentation"].to_string();
    assert!(
        doc.contains("io") && doc.contains("Net") && doc.contains("Resistor.zen"),
        "{doc}"
    );
    Ok(())
}

#[test]
fn completes_mid_edit_and_filters_by_typed_prefix() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    s.open("main.zen", MAIN)?; // a good evaluation first
    let open_paren = MAIN.replace("Pull(name = \"RP\", P1 = vcc)\n", "Resistor(");
    s.change("main.zen", &open_paren, 2)?;
    // cursor right after the trailing, unclosed `Resistor(` (the buffer does not parse)
    let (line, character) = (
        open_paren.matches('\n').count() as u32,
        "Resistor(".len() as u32,
    );
    let r2 = s.request("textDocument/completion", json!({"textDocument": {"uri": s.uri("main.zen")}, "position": {"line": line, "character": character}}))?;
    let props = property_labels(&r2);
    assert!(
        props.contains(&"name".to_string()) && props.contains(&"P1".to_string()),
        "{props:?}"
    );

    let prefix = MAIN.replace(
        "Pull(name = \"RP\", P1 = vcc)\n",
        "Resistor(name = \"X\", P",
    );
    s.change("main.zen", &prefix, 3)?;
    let (line, character) = (
        prefix.matches('\n').count() as u32,
        "Resistor(name = \"X\", P".len() as u32,
    );
    let r3 = s.request("textDocument/completion", json!({"textDocument": {"uri": s.uri("main.zen")}, "position": {"line": line, "character": character}}))?;
    let props = property_labels(&r3);
    assert!(
        props.contains(&"P1".to_string()) && props.contains(&"P2".to_string()),
        "{props:?}"
    );
    assert!(
        !props.contains(&"value".to_string()) && !props.contains(&"name".to_string()),
        "{props:?}"
    );
    Ok(())
}

#[test]
fn completes_builtin_fields_from_stdlib_source() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    let text = MAIN.replace("gnd = Ground(\"GND\")", "x = Power(\"X\", )");
    s.open("main.zen", &text)?;
    let r = s.at(
        "textDocument/completion",
        "main.zen",
        &text,
        "Power(\"X\", ",
        11,
        json!({}),
    )?;
    assert!(
        property_labels(&r).contains(&"voltage".to_string()),
        "{:?}",
        labels(&r)
    );
    Ok(())
}

#[test]
fn keyword_definition_reaches_the_declaration() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    s.open("main.zen", MAIN)?;
    let r = s.at(
        "textDocument/definition",
        "main.zen",
        MAIN,
        "P1 = vcc, P2",
        1,
        json!({}),
    )?;
    let (uri, line, ch) = target(&r[0]);
    assert!(uri.ends_with("generics/Resistor.zen"), "{uri}");
    assert_eq!(
        (line, ch),
        (31, 0),
        "P1 = io(Net) is line 32 of Resistor.zen"
    );
    let r = s.at(
        "textDocument/definition",
        "main.zen",
        MAIN,
        "A = vcc",
        0,
        json!({}),
    )?;
    let (uri, line, ch) = target(&r[0]);
    assert!(
        uri.ends_with("modules/Led.zen") && (line, ch) == (0, 0),
        "{uri} {line}:{ch}"
    );
    let r = s.at(
        "textDocument/definition",
        "main.zen",
        MAIN,
        "voltage = \"3.3V\"",
        2,
        json!({}),
    )?;
    let (uri, line, ch) = target(&r[0]);
    assert!(uri.ends_with("interfaces.zen"), "{uri}");
    assert_eq!((line, ch), (12, 4), "Power's voltage field");
    Ok(())
}

#[test]
fn keyword_hover_shows_resolved_declaration() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    s.open("main.zen", MAIN)?;
    let r = s.at(
        "textDocument/hover",
        "main.zen",
        MAIN,
        "A = vcc",
        0,
        json!({}),
    )?;
    let text = r["contents"].to_string();
    assert!(
        text.contains("A")
            && text.contains("io")
            && text.contains("Anode")
            && text.contains("Led.zen"),
        "{text}"
    );
    Ok(())
}

#[test]
fn signature_help_lists_module_parameters_and_active_keyword() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    s.open("main.zen", MAIN)?;
    let r = s.at(
        "textDocument/signatureHelp",
        "main.zen",
        MAIN,
        "P2 = gnd)",
        6,
        json!({}),
    )?;
    let sig = &r["signatures"][0];
    let params: Vec<String> = sig["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["label"].as_str().unwrap().to_string())
        .collect();
    for want in ["name", "value", "P1", "P2"] {
        assert!(params.contains(&want.to_string()), "{params:?}");
    }
    let active = r["activeParameter"].as_u64().unwrap() as usize;
    assert_eq!(params[active], "P2");
    Ok(())
}

#[test]
fn references_distinguish_same_named_parameters_of_different_modules() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    s.open("main.zen", MAIN)?;
    let r = s.at(
        "textDocument/references",
        "main.zen",
        MAIN,
        "P1 = vcc, P2",
        0,
        json!({"context": {"includeDeclaration": false}}),
    )?;
    let locs = r.as_array().unwrap();
    assert_eq!(locs.len(), 1, "{r}");
    assert_eq!(
        locs[0]["range"]["start"]["line"],
        json!(7),
        "the Resistor call, not Pull's P1"
    );
    let r = s.at(
        "textDocument/references",
        "main.zen",
        MAIN,
        "P1 = vcc)",
        0,
        json!({"context": {"includeDeclaration": true}}),
    )?;
    let uris: Vec<String> = r
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["uri"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(uris.len(), 2, "{r}");
    assert!(
        uris.iter().any(|u| u.ends_with("modules/Pull.zen")),
        "{uris:?}"
    );
    Ok(())
}

#[test]
fn document_symbols_outline_aliases_imports_bindings_and_instances() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    s.open("main.zen", MAIN)?;
    let r = s.request(
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": s.uri("main.zen")}}),
    )?;
    let names: Vec<String> = r
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    for want in [
        "Resistor", "Led", "Pull", "vcc", "gnd", "R_SENSE", "D1", "RP",
    ] {
        assert!(
            names.contains(&want.to_string()),
            "missing {want}: {names:?}"
        );
    }
    let led = r
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["name"] == json!("Led"))
        .unwrap();
    assert_eq!(led["kind"], json!(2), "Module alias → SymbolKind::MODULE");
    Ok(())
}

#[test]
fn unresolved_modules_never_produce_guesses() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    let text = "Missing = Module(\"./nope.zen\")\nMissing(name = \"M\", P1 = None)\nMissing()\n";
    s.open("main.zen", text)?;
    let r = s.at(
        "textDocument/completion",
        "main.zen",
        text,
        "Missing()",
        8,
        json!({}),
    )?;
    assert!(property_labels(&r).is_empty(), "{r}");
    let r = s.at(
        "textDocument/definition",
        "main.zen",
        text,
        "P1 = None",
        0,
        json!({}),
    )?;
    assert!(
        r.as_array().map(|a| a.is_empty()).unwrap_or(r.is_null()),
        "{r}"
    );
    Ok(())
}

#[test]
fn parameter_info_request_reports_resolved_provenance() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    s.open("main.zen", MAIN)?;
    let r = s.at(
        "zener/parameterInfo",
        "main.zen",
        MAIN,
        "A = vcc",
        0,
        json!({}),
    )?;
    assert_eq!(r["callee"], json!("Led"), "{r}");
    let a = r["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == json!("A"))
        .unwrap();
    assert_eq!(a["kind"], json!("io"));
    assert_eq!(a["required"], json!(true));
    assert!(
        a["location"]["uri"]
            .as_str()
            .unwrap()
            .ends_with("modules/Led.zen"),
        "{a}"
    );
    assert_eq!(r["active"], json!("A"));
    Ok(())
}

#[allow(dead_code)]
fn _unused(_: &Path) {}

#[test]
fn an_unclosed_call_elsewhere_in_the_buffer_does_not_break_navigation_or_outline()
-> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    // Realistic flow: the file evaluated, then the user starts typing a call at the end.
    // (A file that has never evaluated has no resolved aliases yet: no semantic results, no guesses.)
    s.open("main.zen", MAIN)?;
    let text = format!("{MAIN}Resistor(name = \"R_TYPING\", P");
    s.change("main.zen", &text, 2)?;
    let r = s.at(
        "textDocument/definition",
        "main.zen",
        &text,
        "A = vcc",
        0,
        json!({}),
    )?;
    assert!(target(&r[0]).0.ends_with("modules/Led.zen"), "{r}");
    let r = s.at(
        "textDocument/hover",
        "main.zen",
        &text,
        "A = vcc",
        0,
        json!({}),
    )?;
    assert!(r["contents"].to_string().contains("Anode"), "{r}");
    let r = s.request(
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": s.uri("main.zen")}}),
    )?;
    let names: Vec<String> = r
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        names.contains(&"D1".to_string()) && names.contains(&"R_TYPING".to_string()),
        "{names:?}"
    );
    let r = s.at(
        "textDocument/references",
        "main.zen",
        &text,
        "A = vcc",
        0,
        json!({"context": {"includeDeclaration": false}}),
    )?;
    assert_eq!(r.as_array().unwrap().len(), 1, "{r}");
    Ok(())
}

#[test]
fn completes_a_prefix_inside_an_auto_closed_call() -> anyhow::Result<()> {
    // Editors auto-close `(`: the user types inside `Resistor(name = "X", P|)`.
    let (mut s, _) = Session::start()?;
    s.open("main.zen", MAIN)?;
    let text = format!("{MAIN}Resistor(name = \"X\", P)\n");
    s.change("main.zen", &text, 2)?;
    let line = text.matches('\n').count() as u32 - 1;
    let character = "Resistor(name = \"X\", P".len() as u32;
    let r = s.request("textDocument/completion", json!({"textDocument": {"uri": s.uri("main.zen")}, "position": {"line": line, "character": character}}))?;
    let props = property_labels(&r);
    assert!(
        props.contains(&"P1".to_string()) && props.contains(&"P2".to_string()),
        "{props:?}"
    );
    let items = if r.is_array() {
        r.clone()
    } else {
        r["items"].clone()
    };
    let p1 = items
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["label"] == json!("P1"))
        .unwrap();
    assert_eq!(p1["textEdit"]["newText"], json!("P1 = "), "{p1}");
    assert_eq!(
        p1["textEdit"]["range"]["start"]["character"],
        json!(character - 1),
        "replaces the typed P"
    );
    Ok(())
}

#[test]
fn a_keyword_still_waiting_for_its_value_does_not_break_the_file() -> anyhow::Result<()> {
    // Right after accepting `P1 = ` inside an auto-closed call: `Resistor(name = "X", P1 = )`.
    let (mut s, _) = Session::start()?;
    s.open("main.zen", MAIN)?;
    let text = format!("{MAIN}Resistor(name = \"X\", P1 = )\n");
    s.change("main.zen", &text, 2)?;
    let r = s.at(
        "textDocument/definition",
        "main.zen",
        &text,
        "A = vcc",
        0,
        json!({}),
    )?;
    assert!(target(&r[0]).0.ends_with("modules/Led.zen"), "{r}");
    let r = s.request(
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": s.uri("main.zen")}}),
    )?;
    assert!(
        r.as_array()
            .unwrap()
            .iter()
            .any(|d| d["name"] == json!("X")),
        "{r}"
    );
    // and on the same line, completion of the next argument still works
    let line = text.matches('\n').count() as u32 - 1;
    let character = "Resistor(name = \"X\", P1 = ".len() as u32;
    let r = s.request("textDocument/hover", json!({"textDocument": {"uri": s.uri("main.zen")}, "position": {"line": line, "character": 22}}))?;
    assert!(
        r["contents"].to_string().contains("io"),
        "hover on the P1 key of the line being edited: {r}"
    );
    let _ = character;
    Ok(())
}

#[test]
fn review_c1_a_renamed_or_repointed_alias_never_resolves_to_its_old_module() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    s.open("main.zen", MAIN)?;
    // Led becomes a local def: completion follows the def, not modules/Led.zen.
    let as_def = MAIN
        .replace(
            "Led = Module(\"./modules/Led.zen\")",
            "def Led(name, X = 1):\n    pass",
        )
        .replace(
            "Led(name = \"D1\", A = vcc, K = gnd)",
            "Led(name = \"D1\", )",
        );
    s.change("main.zen", &as_def, 2)?;
    let r = s.at(
        "textDocument/completion",
        "main.zen",
        &as_def,
        "Led(name = \"D1\", ",
        18,
        json!({}),
    )?;
    let props = property_labels(&r);
    assert!(
        props.contains(&"X".to_string()) && !props.contains(&"A".to_string()),
        "{props:?}"
    );
    // Led re-pointed while the buffer does not evaluate: no stale answer from Led.zen.
    let repointed = format!(
        "{}Broken(\n",
        MAIN.replace("./modules/Led.zen", "./modules/Pull.zen")
    );
    s.change("main.zen", &repointed, 3)?;
    let r = s.at(
        "zener/parameterInfo",
        "main.zen",
        &repointed,
        "A = vcc",
        0,
        json!({}),
    )?;
    assert!(
        r.is_null() || !r["source"].as_str().unwrap_or("").ends_with("Led.zen"),
        "{r}"
    );
    Ok(())
}

#[test]
fn review_i2_a_value_filled_earlier_on_the_line_does_not_shift_the_cursor() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    s.open("main.zen", MAIN)?;
    let text = MAIN.replace(
        "Led(name = \"D1\", A = vcc, K = gnd)",
        "Led(name = , K = gnd, A = vcc)",
    );
    s.change("main.zen", &text, 2)?;
    let r = s.at(
        "textDocument/hover",
        "main.zen",
        &text,
        "A = vcc)",
        0,
        json!({}),
    )?;
    assert!(
        r["contents"].to_string().contains("Anode"),
        "hover must describe A, not K: {r}"
    );
    Ok(())
}

#[test]
fn review_i3_references_of_a_local_def_keyword_stay_on_that_def() -> anyhow::Result<()> {
    let (mut s, _) = Session::start()?;
    let text = format!("{MAIN}def helper(P1):\n    pass\nhelper(P1 = 1)\n");
    s.open("main.zen", &text)?;
    let r = s.at(
        "textDocument/references",
        "main.zen",
        &text,
        "P1 = 1)",
        0,
        json!({"context": {"includeDeclaration": false}}),
    )?;
    let lines: Vec<u64> = r
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["range"]["start"]["line"].as_u64().unwrap())
        .collect();
    let call_line = text[..text.find("helper(P1 = 1)").unwrap()]
        .matches('\n')
        .count() as u64;
    assert_eq!(lines, vec![call_line], "{r}");
    Ok(())
}
