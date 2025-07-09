use insta::{assert_yaml_snapshot, with_settings};
use pcb_eda::kicad::symbol_library::KicadSymbolLibrary;
use std::path::PathBuf;

fn get_test_library_path() -> PathBuf {
    let resources_dir = if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        PathBuf::from(manifest_dir).join("tests/resources")
    } else {
        PathBuf::from("tests/resources")
    };
    resources_dir.join("kicad/extends_test/extended_symbols.kicad_sym")
}

#[test]
fn test_extends_library_symbols() {
    let lib_path = get_test_library_path();
    let library = KicadSymbolLibrary::from_file(&lib_path).unwrap();

    // Snapshot all symbols to verify extends resolution
    let symbols: Vec<_> = library
        .symbol_names()
        .into_iter()
        .map(|name| (name, library.get_symbol(name).unwrap()))
        .collect();

    with_settings!({sort_maps => true}, {
        assert_yaml_snapshot!(symbols);
    });
}

#[test]
fn test_extends_base_amplifier() {
    let lib_path = get_test_library_path();
    let library = KicadSymbolLibrary::from_file(&lib_path).unwrap();
    let symbol = library.get_symbol("BaseAmplifier").unwrap();

    with_settings!({sort_maps => true}, {
        assert_yaml_snapshot!(symbol);
    });
}

#[test]
fn test_extends_specific_amplifier() {
    let lib_path = get_test_library_path();
    let library = KicadSymbolLibrary::from_file(&lib_path).unwrap();
    let symbol = library.get_symbol("SpecificAmplifier").unwrap();

    with_settings!({sort_maps => true}, {
        assert_yaml_snapshot!(symbol);
    });
}

#[test]
fn test_extends_custom_pin_amplifier() {
    let lib_path = get_test_library_path();
    let library = KicadSymbolLibrary::from_file(&lib_path).unwrap();
    let symbol = library.get_symbol("CustomPinAmplifier").unwrap();

    with_settings!({sort_maps => true}, {
        assert_yaml_snapshot!(symbol);
    });
}

#[test]
fn test_extends_minimal() {
    let lib_path = get_test_library_path();
    let library = KicadSymbolLibrary::from_file(&lib_path).unwrap();
    let symbol = library.get_symbol("MinimalExtends").unwrap();

    with_settings!({sort_maps => true}, {
        assert_yaml_snapshot!(symbol);
    });
}

#[test]
fn test_extends_nonexistent_parent() {
    let content = r#"(kicad_symbol_lib
        (symbol "OrphanSymbol"
            (extends "NonExistentParent")
            (property "Value" "Orphan" (at 0 0 0))
            (property "Footprint" "OrphanFootprint" (at 0 0 0))
        )
    )"#;

    let library = KicadSymbolLibrary::from_string(content).unwrap();
    let orphan = library.get_symbol("OrphanSymbol").unwrap();

    with_settings!({sort_maps => true}, {
        assert_yaml_snapshot!(orphan);
    });
}

#[test]
fn test_extends_chain() {
    let content = r#"(kicad_symbol_lib
        (symbol "Base"
            (property "PropA" "ValueA" (at 0 0 0))
            (property "PropB" "ValueB" (at 0 0 0))
            (property "Footprint" "BaseFootprint" (at 0 0 0))
        )
        (symbol "Middle"
            (extends "Base")
            (property "PropB" "ValueB_Override" (at 0 0 0))
            (property "PropC" "ValueC" (at 0 0 0))
        )
        (symbol "Final"
            (extends "Middle")
            (property "PropC" "ValueC_Override" (at 0 0 0))
            (property "PropD" "ValueD" (at 0 0 0))
        )
    )"#;

    let library = KicadSymbolLibrary::from_string(content).unwrap();
    let final_symbol = library.get_symbol("Final").unwrap();

    with_settings!({sort_maps => true}, {
        assert_yaml_snapshot!(final_symbol);
    });
}

#[test]
fn test_extends_multiple_inheritance() {
    let content = r#"(kicad_symbol_lib
        (symbol "BaseComponent"
            (in_bom yes)
            (property "Footprint" "SOIC-8" (at 0 0 0))
            (property "Manufacturer_Name" "Generic Corp" (at 0 0 0))
            (property "Mouser Part Number" "123-456" (at 0 0 0))
            (property "Mouser Price/Stock" "https://mouser.com/123-456" (at 0 0 0))
            (symbol "BaseComponent_0_1"
                (pin power_in line (at 0 0 0) (length 2.54)
                    (name "VCC" (effects (font (size 1.27 1.27))))
                    (number "1" (effects (font (size 1.27 1.27))))
                )
                (pin power_in line (at 0 0 0) (length 2.54)
                    (name "GND" (effects (font (size 1.27 1.27))))
                    (number "2" (effects (font (size 1.27 1.27))))
                )
            )
        )
        (symbol "ExtendedWithNewDistributor"
            (extends "BaseComponent")
            (property "Arrow Part Number" "ARR-789" (at 0 0 0))
            (property "Arrow Price/Stock" "https://arrow.com/arr-789" (at 0 0 0))
        )
        (symbol "ExtendedWithOverride"
            (extends "BaseComponent")
            (property "Mouser Part Number" "999-888" (at 0 0 0))
            (property "Footprint" "TQFP-32" (at 0 0 0))
        )
    )"#;

    let library = KicadSymbolLibrary::from_string(content).unwrap();

    // Snapshot all three symbols to see inheritance behavior
    let symbols: Vec<_> = vec![
        (
            "BaseComponent",
            library.get_symbol("BaseComponent").unwrap(),
        ),
        (
            "ExtendedWithNewDistributor",
            library.get_symbol("ExtendedWithNewDistributor").unwrap(),
        ),
        (
            "ExtendedWithOverride",
            library.get_symbol("ExtendedWithOverride").unwrap(),
        ),
    ];

    with_settings!({sort_maps => true}, {
        assert_yaml_snapshot!(symbols);
    });
}

#[test]
fn test_symbol_conversion_to_generic() {
    let lib_path = get_test_library_path();
    let library = KicadSymbolLibrary::from_file(&lib_path).unwrap();

    // Convert all symbols to generic Symbol type
    let symbols = library.into_symbols();

    with_settings!({sort_maps => true}, {
        assert_yaml_snapshot!(symbols);
    });
}
