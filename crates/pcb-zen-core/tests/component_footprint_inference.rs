mod common;

use common::InMemoryFileProvider;
use pcb_zen_core::EvalContext;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

fn eval_with_files(
    files: HashMap<String, String>,
    main_file: &str,
) -> pcb_zen_core::WithDiagnostics<pcb_zen_core::lang::eval::EvalOutput> {
    let file_provider: Arc<dyn pcb_zen_core::FileProvider> =
        Arc::new(InMemoryFileProvider::new(files));
    let mut resolution = pcb_zen_core::resolution::ResolutionResult::empty();
    resolution.workspace_info.root = PathBuf::from("/");
    resolution.workspace_info.packages.insert(
        "test".to_string(),
        pcb_zen_core::workspace::MemberPackage {
            rel_path: PathBuf::new(),
            config: Default::default(),
            version: None,
            dirty: false,
        },
    );
    resolution
        .package_resolutions
        .insert(PathBuf::from("/"), BTreeMap::default());

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
    pins = {"P": Net("N")},
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

#[test]
fn component_without_footprint_errors_when_inferred_file_missing() {
    let mut files = HashMap::new();
    files.insert("Part.kicad_sym".to_string(), single_pin_symbol("Part"));
    files.insert("test.zen".to_string(), component_zen_without_footprint());

    let result = eval_with_files(files, "test.zen");
    assert!(!result.is_success(), "expected eval failure");
    let rendered = result
        .diagnostics
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("Inferred footprint file not found"),
        "unexpected diagnostics: {rendered}"
    );
}

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
Component(
    name = "U1",
    footprint = "TEST:FP",
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
