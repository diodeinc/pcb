mod common;

use common::InMemoryFileProvider;
use pcb_zen_core::EvalContext;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn eval_with_files(
    files: HashMap<String, String>,
    main_file: &str,
) -> pcb_zen_core::WithDiagnostics<pcb_zen_core::lang::eval::EvalOutput> {
    let mut all_files = common::stdlib_test_files();
    all_files.extend(files);
    let file_provider: Arc<dyn pcb_zen_core::FileProvider> =
        Arc::new(InMemoryFileProvider::new(all_files));
    let resolution = common::test_resolution();
    let ctx = EvalContext::new(file_provider, resolution).set_source_path(PathBuf::from(main_file));
    ctx.eval()
}

fn single_pin_symbol(footprint_prop: &str) -> String {
    format!(
        r#"(kicad_symbol_lib (version 20211014) (generator kicad_symbol_editor)
  (symbol "Part" (pin_names (offset 1.016)) (in_bom yes) (on_board yes)
    (property "Reference" "U" (id 0) (at 0 0 0))
    (property "Footprint" "{footprint_prop}" (id 1) (at 0 0 0))
    (symbol "Part_1_1"
      (pin passive line (at 0 0 0) (length 2.54)
        (name "P" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27))))
      )
    )
  )
)"#
    )
}

fn component_zen_without_footprint() -> String {
    r#"

Component(
    name = "U1",
    symbol = Symbol(library = "Part.kicad_sym", name = "Part"),
    pins = {"P": builtin.net_type("Net")("N")},
)
"#
    .to_string()
}

#[test]
fn component_infers_footprint_from_symbol_bare_stem() {
    let mut files = HashMap::new();
    files.insert("Part.kicad_sym".to_string(), single_pin_symbol("Part"));
    files.insert(
        "Part.kicad_mod".to_string(),
        "(footprint \"Part\")".to_string(),
    );
    files.insert("test.zen".to_string(), component_zen_without_footprint());

    let result = eval_with_files(files, "test.zen");
    assert!(
        result.is_success(),
        "{}",
        result
            .diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );

    let output = result.output.expect("expected eval output");
    let module_tree = output.module_tree();
    let root_module = module_tree
        .values()
        .find(|m| m.path().is_root())
        .expect("expected root module");
    let component = root_module
        .components()
        .find(|c| c.name() == "U1")
        .expect("expected U1 component");
    assert!(
        component.footprint().ends_with("Part.kicad_mod"),
        "expected inferred footprint path, got {}",
        component.footprint()
    );
}

#[test]
fn component_infers_footprint_from_symbol_legacy_stem_pair() {
    let mut files = HashMap::new();
    files.insert("Part.kicad_sym".to_string(), single_pin_symbol("Part:Part"));
    files.insert(
        "Part.kicad_mod".to_string(),
        "(footprint \"Part\")".to_string(),
    );
    files.insert("test.zen".to_string(), component_zen_without_footprint());

    let result = eval_with_files(files, "test.zen");
    assert!(
        result.is_success(),
        "{}",
        result
            .diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

snapshot_eval!(missing_local_inferred_footprint, {
    "Part.kicad_sym" => single_pin_symbol("Part"),
    "test.zen" => component_zen_without_footprint(),
});

#[test]
fn explicit_footprint_takes_precedence_over_symbol_footprint_property() {
    let mut files = HashMap::new();
    files.insert(
        "Part.kicad_sym".to_string(),
        single_pin_symbol("Package_SO:SOIC-8_3.9x4.9mm_P1.27mm"),
    );
    files.insert(
        "test.zen".to_string(),
        r#"
Net = builtin.net_type("Net")
Component(
    name = "U1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    symbol = Symbol(library = "Part.kicad_sym", name = "Part"),
    pins = {"P": Net("N")},
)
"#
        .to_string(),
    );

    let result = eval_with_files(files, "test.zen");
    assert!(
        result.is_success(),
        "{}",
        result
            .diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}
