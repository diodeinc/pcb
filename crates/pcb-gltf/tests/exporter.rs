//! End-to-end checks of the GLB writer, read back with `audit`.

mod audit;

use std::collections::HashMap;

use base64::Engine;
use pcb_step::{Board, Coverage, ModelResolver, Options, OutlinePolicy, ProxyMode, Report, Status};
use serde_json::Value;

const HEAD: &str = r#"(kicad_pcb (version 20241229) (generator "pcbnew")
  (general (thickness 1.6) (legacy_teardrops no))
  (layers (0 "F.Cu" signal) (2 "B.Cu" signal) (25 "Edge.Cuts" user))
  (setup (aux_axis_origin 0 0) (grid_origin 0 0))
"#;

const OUTLINE: &str = r#"(gr_line (start 0 0) (end 30 0) (layer "Edge.Cuts"))
(gr_line (start 30 0) (end 30 20) (layer "Edge.Cuts"))
(gr_line (start 30 20) (end 0 20) (layer "Edge.Cuts"))
(gr_line (start 0 20) (end 0 0) (layer "Edge.Cuts"))
"#;

/// A 2 x 2 x 1.6 mm block.
fn block_step() -> Vec<u8> {
    let text = format!(
        "{HEAD}{}\n(gr_line (start 0 0) (end 2 0) (layer \"Edge.Cuts\"))\n(gr_line (start 2 0) (end 2 2) (layer \"Edge.Cuts\"))\n(gr_line (start 2 2) (end 0 2) (layer \"Edge.Cuts\"))\n(gr_line (start 0 2) (end 0 0) (layer \"Edge.Cuts\")))",
        ""
    );
    let board = Board::parse(text.as_bytes()).unwrap();
    let options = Options {
        components: false,
        silkscreen: false,
        soldermask: false,
        ..Options::default()
    };
    let mut out = Vec::new();
    pcb_step::export(&board, &options, &mut out, &mut Report::default()).unwrap();
    out
}

const WRL_BOX: &str = "#VRML V2.0 utf8\nShape {\n appearance Appearance { material Material { diffuseColor 1 0 0 } }\n geometry IndexedFaceSet { coord Coordinate { point [0 0 0, 1 0 0, 1 2 0, 0 2 0, 0 0 3, 1 0 3, 1 2 3, 0 2 3] }\n coordIndex [0 3 2 1 -1, 4 5 6 7 -1, 0 1 5 4 -1, 1 2 6 5 -1, 2 3 7 6 -1, 3 0 4 7 -1] } }\n";

fn embedded(name: &str, data: &[u8]) -> String {
    let z = zstd::bulk::compress(data, 3).unwrap();
    let b64 = base64::engine::general_purpose::STANDARD.encode(z);
    format!(
        "(embedded_files (file (name \"{name}\") (type model) (data |{b64}|) (checksum \"00\")))\n"
    )
}

fn footprint(reference: &str, at: &str, extra: &str, models: &str) -> String {
    format!(
        "(footprint \"Lib:{reference}\" (layer \"{layer}\") (at {at}) (property \"Reference\" \"{reference}\" (at 0 0) (layer \"F.SilkS\")) (attr smd) {extra}\n (pad \"1\" smd rect (at -1 0) (size 1 1) (layers \"F.Cu\" \"F.Mask\")) (pad \"2\" smd rect (at 1 0) (size 1 1) (layers \"F.Cu\" \"F.Mask\")) {models})\n",
        layer = if extra.contains("BACK") {
            "B.Cu"
        } else {
            "F.Cu"
        },
        extra = extra.replace("BACK", "")
    )
}

fn model(reference: &str, extra: &str) -> String {
    format!(
        "(model \"{reference}\" (offset (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0)) {extra})"
    )
}

fn scaled(reference: &str, scale: &str, rotate: &str, offset: &str) -> String {
    format!(
        "(model \"{reference}\" (offset (xyz {offset})) (scale (xyz {scale})) (rotate (xyz {rotate})))"
    )
}

struct Run {
    report: Report,
    stats: Value,
}

impl Run {
    fn coverage(&self) -> &Coverage {
        &self.report.coverage
    }

    fn status(&self, reference: &str) -> Vec<Status> {
        self.coverage()
            .models
            .iter()
            .filter(|m| m.reference == reference)
            .map(|m| m.status)
            .collect()
    }

    fn model_things(&self, reference: &str) -> Vec<&Value> {
        self.stats["things"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["kind"] == "model" && t["reference"] == reference)
            .collect()
    }

    /// Bounds in mm in the scene's frame.
    fn bounds(&self, reference: &str) -> ([f64; 3], [f64; 3]) {
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for t in self.model_things(reference) {
            for p in t["primitives"].as_array().unwrap() {
                let v = |k: &str| -> [f64; 3] {
                    let a = &p[k];
                    [
                        a[0].as_f64().unwrap(),
                        a[1].as_f64().unwrap(),
                        a[2].as_f64().unwrap(),
                    ]
                };
                let (a, b) = (v("min"), v("max"));
                let (a, b) = ([a[0], -b[2], a[1]], [b[0], -a[2], b[1]]);
                for k in 0..3 {
                    lo[k] = lo[k].min(a[k] * 1e3);
                    hi[k] = hi[k].max(b[k] * 1e3);
                }
            }
        }
        (lo, hi)
    }

    fn extent(&self, reference: &str) -> [f64; 3] {
        let (lo, hi) = self.bounds(reference);
        [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]]
    }
}

fn export(text: &str, options: &Options) -> Result<Run, anyhow::Error> {
    let board = Board::parse(text.as_bytes())?;
    let mut report = Report::default();
    let mut glb = Vec::new();
    pcb_gltf::export(&board, options, &mut glb, &mut report)?;
    let stats = audit::stats_of(&glb)?;
    assert_eq!(stats["defects"], Value::Array(vec![]), "structural defects");
    Ok(Run { report, stats })
}

fn board(body: &str) -> String {
    format!("{HEAD}{OUTLINE}{body})")
}

fn copper() -> Options {
    Options {
        pads: true,
        tracks: true,
        zones: true,
        ..Options::default()
    }
}

fn visual() -> Options {
    Options {
        visual: true,
        ..copper()
    }
}

fn near(a: f64, b: f64, tolerance: f64) {
    assert!((a - b).abs() <= tolerance, "{a} vs {b}");
}

#[test]
fn non_uniform_scale_follows_rotation_and_the_bottom_side() {
    // Scale applies before the model's rotation, so the turn swaps x and y.
    let m = scaled("kicad-embed://b.step", "3 1 0.5", "0 0 90", "0 0 0");
    for (extra, name) in [("", "front"), ("BACK", "back")] {
        let text = board(&format!(
            "{}{}",
            footprint("U1", "10 10", extra, &m),
            embedded("b.step", &block_step())
        ));
        let run = export(&text, &Options::default()).unwrap();
        let e = run.extent("U1");
        near(e[0], 2.0, 0.01);
        near(e[1], 6.0, 0.01);
        near(e[2], 1.51 * 0.5, 0.01);
        let (lo, hi) = run.bounds("U1");
        if name == "back" {
            assert!(hi[2] < 0.0, "{lo:?} {hi:?}");
        } else {
            assert!(lo[2] > 1.5, "{lo:?}");
        }
    }
}

#[test]
fn wrl_models_are_drawn_in_tenths_of_an_inch() {
    let text = board(&format!(
        "{}{}",
        footprint("U1", "10 10", "", &model("kicad-embed://box.wrl", "")),
        embedded("box.wrl", WRL_BOX.as_bytes())
    ));
    let run = export(&text, &Options::default()).unwrap();
    assert_eq!(run.status("U1"), [Status::Rendered]);
    let e = run.extent("U1");
    near(e[0], 2.54, 0.01);
    near(e[1], 5.08, 0.01);
    near(e[2], 7.62, 0.01);
}

#[test]
fn the_default_resolver_reads_nothing_beside_the_board() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("secret.step"), block_step()).unwrap();
    let absolute = dir.path().join("secret.step");
    let text = board(&format!(
        "{}{}{}",
        footprint("U1", "10 10", "", &model(absolute.to_str().unwrap(), "")),
        footprint("U2", "20 10", "", &model("${KIPRJMOD}/secret.step", "")),
        footprint("U3", "25 10", "", &model("../secret.step", ""))
    ));
    let run = export(&text, &Options::default()).unwrap();
    for r in ["U1", "U2", "U3"] {
        assert!(run.model_things(r).is_empty());
        assert_eq!(run.status(r), [Status::Absent]);
    }
    assert_eq!(run.report.failed_models, 0);
}

#[test]
fn external_models_resolve_inside_allowed_roots_only() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(project.join("models")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(project.join("models/block.step"), block_step()).unwrap();
    std::fs::write(project.join("models/box.wrl"), WRL_BOX).unwrap();
    std::fs::write(outside.join("secret.step"), block_step()).unwrap();
    std::fs::write(project.join("models/notes.iges"), "x").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.join("secret.step"),
        project.join("models/link.step"),
    )
    .unwrap();
    let text = board(&format!(
        "{}{}{}{}{}{}{}{}{}{}",
        footprint(
            "OK1",
            "5 10",
            "",
            &model("${KIPRJMOD}/models/block.step", "")
        ),
        footprint("OK2", "10 10", "", &model("models/box.wrl", "")),
        footprint("OK3", "15 10", "", &model("${MYLIB}/block.step", "")),
        footprint(
            "ESC",
            "20 10",
            "",
            &model("${KIPRJMOD}/../outside/secret.step", "")
        ),
        footprint(
            "SYM",
            "25 10",
            "",
            &model("${KIPRJMOD}/models/link.step", "")
        ),
        footprint("UNK", "5 15", "", &model("${NOPE}/block.step", "")),
        footprint(
            "GONE",
            "10 15",
            "",
            &model("${KIPRJMOD}/models/missing.step", "")
        ),
        footprint(
            "FMT",
            "15 15",
            "",
            &model("${KIPRJMOD}/models/notes.iges", "")
        ),
        footprint("EMB", "20 15", "", &model("kicad-embed://b.step", "")),
        embedded("b.step", &block_step())
    ));
    let resolver = ModelResolver::default()
        .define("KIPRJMOD", &project)
        .unwrap()
        .define("MYLIB", &project.join("models"))
        .unwrap();
    let run = export(
        &text,
        &Options {
            models: resolver,
            ..Options::default()
        },
    )
    .unwrap();
    let s = |r: &str| run.status(r)[0];
    for r in ["OK1", "OK2", "OK3", "EMB"] {
        assert_eq!(s(r), Status::Rendered, "{r}");
    }
    assert_eq!(s("ESC"), Status::Absent);
    #[cfg(unix)]
    assert_eq!(s("SYM"), Status::Absent);
    assert_eq!(s("UNK"), Status::Absent);
    assert_eq!(s("GONE"), Status::Absent);
    assert_eq!(s("FMT"), Status::Unsupported);
    assert!(run.model_things("ESC").is_empty() && run.model_things("SYM").is_empty());
    assert!(!run.coverage().is_complete());
    assert_eq!(run.report.failed_models, 0);
}

#[test]
fn coverage_names_what_became_of_every_model() {
    let text = board(&format!(
        "{}{}{}{}{}{}{}{}",
        footprint("OK", "5 5", "", &model("kicad-embed://b.step", "")),
        footprint(
            "HID",
            "10 5",
            "",
            &model("kicad-embed://b.step", "(hide yes)")
        ),
        footprint(
            "DNP",
            "15 5",
            "(attr dnp)",
            &model("kicad-embed://b.step", "")
        ),
        footprint("NOM", "20 5", "", ""),
        footprint("MISS", "5 10", "", &model("kicad-embed://missing.step", "")),
        footprint("BAD", "10 10", "", &model("kicad-embed://bad.step", "")),
        footprint(
            "NEG",
            "15 10",
            "",
            &scaled("kicad-embed://b.step", "-1 1 1", "0 0 0", "0 0 0")
        ),
        [
            embedded("b.step", &block_step()),
            embedded("bad.step", b"not a step file")
        ]
        .concat()
    ));
    let options = Options {
        include_dnp: false,
        ..Options::default()
    };
    let run = export(&text, &options).unwrap();
    let s = |r: &str| run.status(r);
    assert_eq!(s("OK"), [Status::Rendered]);
    assert_eq!(s("HID"), [Status::Hidden]);
    assert_eq!(s("DNP"), [Status::Filtered]);
    assert_eq!(s("NOM"), []);
    assert_eq!(s("MISS"), [Status::Absent]);
    assert_eq!(s("BAD"), [Status::Failed]);
    assert_eq!(s("NEG"), [Status::Unsupported]);
    let incomplete: Vec<&str> = run
        .coverage()
        .incomplete()
        .map(|m| m.reference.as_str())
        .collect();
    assert_eq!(incomplete, ["MISS", "BAD", "NEG"]);
}

#[test]
fn missing_models_get_proxies_and_footprint_only_draws_no_models() {
    let text = board(&format!(
        "{}{}{}",
        footprint("OK", "5 10", "", &model("kicad-embed://b.step", "")),
        footprint(
            "MISS",
            "15 10",
            "",
            &model("kicad-embed://missing.step", "")
        ),
        embedded("b.step", &block_step())
    ));
    let run = export(
        &text,
        &Options {
            proxies: ProxyMode::Missing,
            ..Options::default()
        },
    )
    .unwrap();
    let proxies: Vec<&Value> = run.stats["things"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["name"] == "proxy")
        .collect();
    assert_eq!(proxies.len(), 1);
    assert_eq!(proxies[0]["reference"], "MISS");
    // Over the pads, 1 mm high.
    let e = run.extent("MISS");
    near(e[0], 3.0, 0.01);
    near(e[1], 1.0, 0.01);
    near(e[2], 1.0, 0.01);

    let run = export(
        &text,
        &Options {
            proxies: ProxyMode::All,
            ..Options::default()
        },
    )
    .unwrap();
    assert!(run.model_things("OK").iter().all(|t| t["name"] == "proxy"));
    assert_eq!(run.model_things("MISS").len(), 1);
}

#[test]
fn a_missing_outline_is_an_error_unless_the_caller_asks_for_a_visual_body() {
    let text = format!(
        "{HEAD}{}{})",
        footprint("U1", "10 10", "", &model("kicad-embed://b.step", "")),
        embedded("b.step", &block_step())
    );
    let err = export(&text, &Options::default()).err().unwrap();
    assert!(err.to_string().contains("no Edge.Cuts"), "{err}");
    let options = Options {
        outline: OutlinePolicy::Synthesize,
        ..Options::default()
    };
    let run = export(&text, &options).unwrap();
    assert_eq!(run.stats["pcb"]["board"]["syntheticOutline"], true);
    let (x0, x1) = layer_extent_x(&run, "body");
    assert!(x0 < 9.0 && x1 > 11.0, "{x0} {x1}");
    let real = export(&board(&footprint("U1", "10 10", "", "")), &options).unwrap();
    assert_eq!(real.stats["pcb"]["board"]["syntheticOutline"], false);
}

#[test]
fn the_manifest_ties_every_instance_to_its_footprint() {
    let text = board(&format!(
        "{}{}{}{}",
        footprint(
            "A1",
            "5 10",
            "(property \"Value\" \"10k\")",
            &model("kicad-embed://b.step", "")
        ),
        footprint("A2", "10 10", "BACK", &model("kicad-embed://b.step", "")),
        footprint("W1", "15 10", "", &model("kicad-embed://box.wrl", "")),
        [
            embedded("b.step", &block_step()),
            embedded("box.wrl", WRL_BOX.as_bytes())
        ]
        .concat()
    ));
    let run = export(&text, &visual()).unwrap();
    let manifest = &run.stats["pcb"];
    assert_eq!(manifest["schema"], "pcb-scene/1");
    let footprints = manifest["footprints"].as_array().unwrap();
    assert_eq!(footprints.len(), 3);
    assert_eq!(footprints[1]["side"], "back");
    for fp in footprints {
        let model = &fp["models"][0];
        let (node, index) = (
            model["instance"]["node"].as_u64().unwrap() as usize,
            model["instance"]["index"].as_u64().unwrap() as usize,
        );
        let extras = &run.stats["json"]["nodes"][node]["extras"];
        assert_eq!(extras["references"][index], fp["reference"]);
    }
}

#[test]
fn a_huge_board_keeps_mask_and_copper_apart_in_depth() {
    // One 16-bit step over 1.2 m is wider than the 0.015 mm mask gap.
    let text = format!(
        "{HEAD}(gr_line (start 0 0) (end 1200 0) (layer \"Edge.Cuts\"))(gr_line (start 1200 0) (end 1200 20) (layer \"Edge.Cuts\"))(gr_line (start 1200 20) (end 0 20) (layer \"Edge.Cuts\"))(gr_line (start 0 20) (end 0 0) (layer \"Edge.Cuts\"))\n{})",
        footprint("U1", "10 10", "", "")
    );
    let run = export(&text, &visual()).unwrap();
    let z = layer_z(&run);
    assert!(z["soldermask:front"].0 - z["pads:"].1 >= 0.0145, "{z:?}");
}

fn layer_z(run: &Run) -> HashMap<String, (f64, f64)> {
    run.stats["things"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] == "board")
        .flat_map(|t| t["primitives"].as_array().unwrap())
        .map(|p| {
            let side = p["extras"]["side"].as_str().unwrap_or("");
            (
                format!("{}:{side}", p["extras"]["layer"].as_str().unwrap()),
                (
                    p["min"][1].as_f64().unwrap() * 1e3,
                    p["max"][1].as_f64().unwrap() * 1e3,
                ),
            )
        })
        .collect()
}

fn area_up(run: &Run, layer: &str, side: Option<&str>) -> f64 {
    run.stats["things"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] == "board")
        .flat_map(|t| t["primitives"].as_array().unwrap())
        .filter(|p| p["extras"]["layer"] == layer && p["extras"]["side"].as_str() == side)
        .map(|p| p["areaUp"].as_f64().unwrap() * 1e6)
        .sum()
}

#[test]
fn paste_apertures_follow_kicads_margin_rules() {
    // Areas measured in KiCad 10's VRML export; the DNP footprint keeps its
    // paste.
    let pads = r#"(footprint "X:Y" (layer "F.Cu") (at 10 10) (property "Reference" "U1" (at 0 0) (layer "F.SilkS")) (attr smd dnp)
  (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu" "F.Mask" "F.Paste"))
  (pad "2" smd rect (at 3 0) (size 1 2) (layers "F.Cu" "F.Mask" "F.Paste") (solder_paste_margin_ratio -0.1))
  (pad "3" smd rect (at -3 0) (size 1 1) (layers "F.Cu" "F.Mask" "F.Paste") (solder_paste_margin 0.1))
  (pad "4" smd rect (at 0 3) (size 1 1) (layers "F.Cu" "F.Mask" "F.Paste") (solder_paste_margin -0.1) (solder_paste_margin_ratio 0.2))
  (pad "5" smd rect (at 0 -3) (size 1 1) (layers "F.Paste"))
  (pad "6" smd rect (at 5 5) (size 1 1) (layers "F.Cu" "F.Mask")))
"#;
    let run = export(&board(pads), &visual()).unwrap();
    near(area_up(&run, "paste", Some("front")), 6.143, 0.05);
    assert_eq!(area_up(&run, "paste", Some("back")), 0.0);
    let run = export(&board(pads), &copper()).unwrap();
    assert_eq!(area_up(&run, "paste", Some("front")), 0.0);
}

fn with(mut options: Options, f: impl FnOnce(&mut Options)) -> Options {
    f(&mut options);
    options
}

#[test]
fn vias_bore_through_the_body_and_mask_as_kicads_vrml_export_does() {
    let text = board(
        r#"(via (at 10 10) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (net 0) (tenting (front yes) (back yes)))
(via (at 15 10) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (net 0) (filling yes) (tenting (front yes) (back yes)))
"#,
    );
    let bore = std::f64::consts::PI * 0.04;
    let plain = export(&text, &copper()).unwrap();
    let cut = export(&text, &with(copper(), |o| o.cut_vias = true)).unwrap();
    let visual_cut = export(&text, &visual()).unwrap();
    let body = |r: &Run| area_up(r, "body", None);
    let mask = |r: &Run| area_up(r, "soldermask", Some("front"));
    // `cut_vias` leaves the filled via whole and the mask alone.
    near(body(&plain) - body(&cut), bore, 0.02);
    near(body(&plain) - body(&visual_cut), 2.0 * bore, 0.03);
    near(mask(&plain), mask(&cut), 1e-6);
    near(mask(&plain) - mask(&visual_cut), 2.0 * bore, 0.03);
}

#[test]
fn graphics_on_copper_layers_are_copper() {
    let text = board(
        r#"(gr_circle (center 15 10) (end 18 10) (stroke (width 0.1) (type default)) (fill yes) (layer "F.Cu"))
(gr_poly (pts (xy 2 2) (xy 6 2) (xy 6 6) (xy 2 6)) (stroke (width 0.1) (type default)) (fill yes) (layer "F.Cu"))
(gr_rect (start 20 5) (end 26 9) (stroke (width 0.5) (type default)) (fill no) (layer "B.Cu"))
"#,
    );
    let run = export(&text, &visual()).unwrap();
    let front = area_up(&run, "copper", None);
    near(front, 29.22 + 16.81, 0.6);
    let back: f64 = run.stats["things"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] == "board")
        .flat_map(|t| t["primitives"].as_array().unwrap())
        .filter(|p| p["extras"]["layer"] == "copper")
        .map(|p| p["areaDown"].as_f64().unwrap() * 1e6)
        .sum();
    near(back, 6.5 * 4.5 - 5.5 * 3.5, 0.3);
}

#[test]
fn copper_is_cut_to_the_board_outline_as_in_kicads_vrml_export() {
    // A pad centred on the board edge, and a track overhanging it.
    let text = board(
        r#"(footprint "X:Y" (layer "F.Cu") (at 30 10) (property "Reference" "J1" (at 0 0) (layer "F.SilkS")) (attr smd)
  (pad "1" smd rect (at 0 0) (size 2 2) (layers "F.Cu" "F.Mask")))
(segment (start 25 5) (end 33 5) (width 1) (layer "F.Cu") (net 0))
"#,
    );
    let pads = |r: &Run| area_up(r, "pads", None);
    let tracks = |r: &Run| area_up(r, "copper", None);
    let plain = export(&text, &copper()).unwrap();
    let clipped = export(&text, &visual()).unwrap();
    near(pads(&plain), 4.0, 0.05);
    near(pads(&clipped), 2.0, 0.05);
    near(
        tracks(&plain) - tracks(&clipped),
        3.0 + std::f64::consts::PI * 0.125,
        0.05,
    );
    let (lo, hi) = layer_extent_x(&clipped, "pads");
    assert!(hi <= 30.0 + 0.01 && lo >= 29.0 - 0.01, "{lo} {hi}");
}

fn layer_extent_x(run: &Run, layer: &str) -> (f64, f64) {
    let prims = run.stats["things"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["kind"] == "board")
        .flat_map(|t| t["primitives"].as_array().unwrap())
        .filter(|p| p["extras"]["layer"] == layer);
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for p in prims {
        lo = lo.min(p["min"][0].as_f64().unwrap() * 1e3);
        hi = hi.max(p["max"][0].as_f64().unwrap() * 1e3);
    }
    (lo, hi)
}
