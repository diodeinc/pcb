#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;

const BOARD_ZEN: &str = r#"
Component(
    name = "U1",
    symbol = Symbol(library = "Part.kicad_sym"),
    footprint = File("Part.kicad_mod"),
    pins = {"P1": Net("A"), "P2": Net("B")},
    part = Part(mpn = "TEST", manufacturer = "TEST"),
)
"#;

const FOOTPRINT: &str = r#"(footprint "Part"
  (layer "F.Cu")
  (pad "1" smd rect (at -1 0) (size 1 1) (layers "F.Cu"))
  (pad "2" smd rect (at 1 0) (size 1 1) (layers "F.Cu"))
)
"#;

const SYMBOL: &str = r#"(kicad_symbol_lib
  (version 20251024)
  (symbol "Part"
    (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Value" "Part" (at 0 -2.54 0) (effects (font (size 1.27 1.27))))
    (symbol "Part_0_1"
      (rectangle (start -2.54 2.54) (end 2.54 -2.54) (stroke (width 0.254)) (fill (type background))))
    (symbol "Part_1_1"
      (pin input line (at -5.08 0 0) (length 2.54) (name "P1" (effects (font (size 1.27 1.27)))) (number "1" (effects (font (size 1.27 1.27)))))
      (pin input line (at 5.08 0 180) (length 2.54) (name "P2" (effects (font (size 1.27 1.27)))) (number "2" (effects (font (size 1.27 1.27)))))
    )
  )
)
"#;

#[test]
fn fix_applies_what_build_points_to() {
    // A comment hides the fill behind it until the comment is gone.
    let broken = SYMBOL
        .replacen(
            "(symbol \"Part_0_1\"",
            ";; body\n    (symbol \"Part_0_1\"",
            1,
        )
        .replacen("(type background)", "(type solid)", 1)
        .replacen("(at -5.08 0 0)", "(at -5.08 -0.0 0)", 1);
    let fixed = SYMBOL.replacen("(type background)", "(type outline)", 1);

    let mut sandbox = Sandbox::new().with_workspace();
    sandbox
        .write("board.zen", BOARD_ZEN)
        .write("Part.kicad_mod", FOOTPRINT)
        .write("Part.kicad_sym", &broken);
    let symbol = sandbox.default_cwd().join("Part.kicad_sym");

    let build = sandbox.snapshot_run("pcbc", ["build", "board.zen"]);
    assert!(build.contains("Exit Code: 1"), "{build}");
    assert!(build.contains("`pcb fix` does this"), "{build}");
    assert!(build.contains("1 fixable with `pcb fix`"), "{build}");

    let diff = sandbox.snapshot_run("pcbc", ["fix", "--diff"]);
    assert!(
        diff.contains("+      (rectangle") && diff.contains("(type outline)"),
        "{diff}"
    );
    assert_eq!(std::fs::read_to_string(&symbol).unwrap(), broken);

    let fix = sandbox.snapshot_run("pcbc", ["fix"]);
    assert!(fix.contains("Fixed Part.kicad_sym"), "{fix}");
    assert_eq!(std::fs::read_to_string(&symbol).unwrap(), fixed);

    let build = sandbox.snapshot_run("pcbc", ["build", "board.zen"]);
    assert!(build.contains("Exit Code: 0"), "{build}");
    assert!(!build.contains("pcb fix"), "{build}");
}
