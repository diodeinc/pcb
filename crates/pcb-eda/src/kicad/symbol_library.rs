use crate::Symbol;
use anyhow::Result;
use sexp::{parse, Atom, Sexp};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::symbol::{parse_symbol, KicadSymbol};

/// A KiCad symbol library that can contain multiple symbols
pub struct KicadSymbolLibrary {
    symbols: Vec<KicadSymbol>,
}

impl KicadSymbolLibrary {
    /// Parse a KiCad symbol library from a string
    pub fn from_string(content: &str) -> Result<Self> {
        let sexp = parse(content)?;
        let mut symbols = Vec::new();

        match sexp {
            Sexp::List(kicad_symbol_lib) => {
                // Iterate through all items in the library
                for item in kicad_symbol_lib {
                    if let Sexp::List(ref symbol_list) = item {
                        if let Some(Sexp::Atom(Atom::S(ref sym))) = symbol_list.first() {
                            if sym == "symbol" {
                                // Parse this symbol
                                match parse_symbol(symbol_list) {
                                    Ok(symbol) => symbols.push(symbol),
                                    Err(e) => {
                                        // Log error but continue parsing other symbols
                                        eprintln!("Warning: Failed to parse symbol: {e}");
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => return Err(anyhow::anyhow!("Invalid KiCad symbol library format")),
        }

        // Resolve extends references
        resolve_extends(&mut symbols)?;

        Ok(KicadSymbolLibrary { symbols })
    }

    /// Parse a KiCad symbol library from a file
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        Self::from_string(&content)
    }

    /// Get all symbols in the library
    pub fn symbols(&self) -> &[KicadSymbol] {
        &self.symbols
    }

    /// Get a symbol by name
    pub fn get_symbol(&self, name: &str) -> Option<&KicadSymbol> {
        self.symbols.iter().find(|s| s.name() == name)
    }

    /// Get the names of all symbols in the library
    pub fn symbol_names(&self) -> Vec<&str> {
        self.symbols.iter().map(|s| s.name()).collect()
    }

    /// Convert all symbols to the generic Symbol type
    pub fn into_symbols(self) -> Vec<Symbol> {
        self.symbols.into_iter().map(|s| s.into()).collect()
    }
}

/// Resolve extends references by cloning parent symbols and applying child overrides
fn resolve_extends(symbols: &mut [KicadSymbol]) -> Result<()> {
    // Create a map for quick lookup
    let mut symbol_map: HashMap<String, usize> = HashMap::new();
    for (idx, symbol) in symbols.iter().enumerate() {
        symbol_map.insert(symbol.name().to_string(), idx);
    }

    // Collect symbols that need to be resolved (to avoid borrowing issues)
    let mut to_resolve: Vec<(usize, String)> = Vec::new();
    for (idx, symbol) in symbols.iter().enumerate() {
        if let Some(parent_name) = symbol.extends() {
            to_resolve.push((idx, parent_name.to_string()));
        }
    }

    // Apply inheritance by cloning parent and merging child properties
    for (child_idx, parent_name) in to_resolve {
        if let Some(&parent_idx) = symbol_map.get(&parent_name) {
            // Clone the parent as the base
            let mut merged = symbols[parent_idx].clone();
            let child = &symbols[child_idx];

            // Override with child's values
            merged.name = child.name.clone();
            merged.extends = child.extends.clone();

            // Override properties that are explicitly set in child
            if !child.footprint.is_empty() {
                merged.footprint = child.footprint.clone();
            }

            if !child.pins.is_empty() {
                merged.pins = child.pins.clone();
            }

            if child.mpn.is_some() {
                merged.mpn = child.mpn.clone();
            }

            if child.manufacturer.is_some() {
                merged.manufacturer = child.manufacturer.clone();
            }

            if child.datasheet_url.is_some() {
                merged.datasheet_url = child.datasheet_url.clone();
            }

            if child.description.is_some() {
                merged.description = child.description.clone();
            }

            // Merge properties - child properties override parent
            for (key, value) in &child.properties {
                merged.properties.insert(key.clone(), value.clone());
            }

            // Merge distributors - child distributors override parent
            // First, ensure parent distributors are preserved
            // Then override with child distributors
            for (dist, part) in &child.distributors {
                // If the distributor exists in parent, merge the properties
                if let Some(parent_part) = merged.distributors.get_mut(dist) {
                    // Override part number if child has it
                    if !part.part_number.is_empty() {
                        parent_part.part_number = part.part_number.clone();
                    }
                    // Override URL if child has it
                    if !part.url.is_empty() {
                        parent_part.url = part.url.clone();
                    }
                } else {
                    // New distributor, add it
                    merged.distributors.insert(dist.clone(), part.clone());
                }
            }

            // Override in_bom if explicitly set in child
            // Note: We can't easily tell if in_bom was explicitly set or is just the default false
            // So we'll use the child's value if it's true, otherwise keep parent's
            if child.in_bom {
                merged.in_bom = child.in_bom;
            }

            // Replace the child with the merged symbol
            symbols[child_idx] = merged;
        } else {
            eprintln!(
                "Warning: Symbol '{}' extends '{}' but parent not found",
                symbols[child_idx].name(),
                parent_name
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_multi_symbol_library() {
        let content = r#"(kicad_symbol_lib
            (symbol "Symbol1"
                (property "Reference" "U" (at 0 0 0))
                (symbol "Symbol1_0_1"
                    (pin input line (at 0 0 0) (length 2.54)
                        (name "A" (effects (font (size 1.27 1.27))))
                        (number "1" (effects (font (size 1.27 1.27))))
                    )
                )
            )
            (symbol "Symbol2"
                (property "Reference" "U" (at 0 0 0))
                (symbol "Symbol2_0_1"
                    (pin input line (at 0 0 0) (length 2.54)
                        (name "B" (effects (font (size 1.27 1.27))))
                        (number "2" (effects (font (size 1.27 1.27))))
                    )
                )
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        assert_eq!(lib.symbols.len(), 2);
        assert_eq!(lib.symbol_names(), vec!["Symbol1", "Symbol2"]);
    }

    #[test]
    fn test_extends_basic() {
        let content = r#"(kicad_symbol_lib
            (symbol "BaseSymbol"
                (property "Value" "Base" (at 0 0 0))
                (property "Footprint" "BaseFootprint" (at 0 0 0))
                (symbol "BaseSymbol_0_1"
                    (pin input line (at 0 0 0) (length 2.54)
                        (name "A" (effects (font (size 1.27 1.27))))
                        (number "1" (effects (font (size 1.27 1.27))))
                    )
                )
            )
            (symbol "ExtendedSymbol"
                (extends "BaseSymbol")
                (property "Value" "Extended" (at 0 0 0))
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        assert_eq!(lib.symbols.len(), 2);

        let extended = lib.get_symbol("ExtendedSymbol").unwrap();
        assert_eq!(extended.name(), "ExtendedSymbol");
        assert_eq!(
            extended.properties.get("Value"),
            Some(&"Extended".to_string())
        );
        assert_eq!(extended.footprint, "BaseFootprint"); // Inherited
        assert_eq!(extended.pins.len(), 1); // Inherited
    }

    #[test]
    fn test_extends_override_properties() {
        let content = r#"(kicad_symbol_lib
            (symbol "Base"
                (in_bom yes)
                (property "Value" "BaseValue" (at 0 0 0))
                (property "Footprint" "BaseFootprint" (at 0 0 0))
                (property "Manufacturer_Name" "BaseMfg" (at 0 0 0))
                (property "ki_description" "Base description" (at 0 0 0))
            )
            (symbol "Child"
                (extends "Base")
                (property "Footprint" "ChildFootprint" (at 0 0 0))
                (property "Manufacturer_Name" "ChildMfg" (at 0 0 0))
                (property "NewProperty" "NewValue" (at 0 0 0))
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        let child = lib.get_symbol("Child").unwrap();

        // Check overridden properties
        assert_eq!(child.footprint, "ChildFootprint");
        assert_eq!(child.manufacturer, Some("ChildMfg".to_string()));

        // Check inherited properties
        assert_eq!(
            child.properties.get("Value"),
            Some(&"BaseValue".to_string())
        );
        assert_eq!(child.description, Some("Base description".to_string()));
        assert!(child.in_bom);

        // Check new property
        assert_eq!(
            child.properties.get("NewProperty"),
            Some(&"NewValue".to_string())
        );
    }

    #[test]
    fn test_extends_override_pins() {
        let content = r#"(kicad_symbol_lib
            (symbol "Base"
                (symbol "Base_0_1"
                    (pin input line (at 0 0 0) (length 2.54)
                        (name "A" (effects (font (size 1.27 1.27))))
                        (number "1" (effects (font (size 1.27 1.27))))
                    )
                    (pin output line (at 0 0 0) (length 2.54)
                        (name "B" (effects (font (size 1.27 1.27))))
                        (number "2" (effects (font (size 1.27 1.27))))
                    )
                )
            )
            (symbol "Child"
                (extends "Base")
                (symbol "Child_0_1"
                    (pin bidirectional line (at 0 0 0) (length 2.54)
                        (name "X" (effects (font (size 1.27 1.27))))
                        (number "3" (effects (font (size 1.27 1.27))))
                    )
                )
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        let child = lib.get_symbol("Child").unwrap();

        // Child should have its own pins, not the base pins
        assert_eq!(child.pins.len(), 1);
        assert_eq!(child.pins[0].name, "X");
        assert_eq!(child.pins[0].number, "3");
    }

    #[test]
    fn test_extends_chain() {
        let content = r#"(kicad_symbol_lib
            (symbol "Base"
                (property "PropA" "ValueA" (at 0 0 0))
                (property "PropB" "ValueB" (at 0 0 0))
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

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        let final_symbol = lib.get_symbol("Final").unwrap();

        // Should have properties from entire chain
        assert_eq!(
            final_symbol.properties.get("PropA"),
            Some(&"ValueA".to_string())
        ); // From Base
        assert_eq!(
            final_symbol.properties.get("PropB"),
            Some(&"ValueB_Override".to_string())
        ); // From Middle
        assert_eq!(
            final_symbol.properties.get("PropC"),
            Some(&"ValueC_Override".to_string())
        ); // Overridden in Final
        assert_eq!(
            final_symbol.properties.get("PropD"),
            Some(&"ValueD".to_string())
        ); // New in Final
    }

    #[test]
    fn test_extends_missing_parent() {
        let content = r#"(kicad_symbol_lib
            (symbol "Orphan"
                (extends "MissingParent")
                (property "Value" "OrphanValue" (at 0 0 0))
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        let orphan = lib.get_symbol("Orphan").unwrap();

        // Should still have its own properties
        assert_eq!(orphan.name(), "Orphan");
        assert_eq!(
            orphan.properties.get("Value"),
            Some(&"OrphanValue".to_string())
        );
    }

    #[test]
    fn test_extends_distributors() {
        let content = r#"(kicad_symbol_lib
            (symbol "Base"
                (property "Mouser Part Number" "123-456" (at 0 0 0))
                (property "Mouser Price/Stock" "https://mouser.com/123-456" (at 0 0 0))
            )
            (symbol "Extended"
                (extends "Base")
                (property "Arrow Part Number" "ARR-789" (at 0 0 0))
                (property "Arrow Price/Stock" "https://arrow.com/arr-789" (at 0 0 0))
                (property "Mouser Part Number" "999-888" (at 0 0 0))
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        let extended = lib.get_symbol("Extended").unwrap();

        // Should have both distributors
        assert_eq!(extended.distributors.len(), 2);

        // Mouser should be overridden
        let mouser = extended.distributors.get("Mouser").unwrap();
        assert_eq!(mouser.part_number, "999-888");
        assert_eq!(mouser.url, "https://mouser.com/123-456"); // URL inherited

        // Arrow should be new
        let arrow = extended.distributors.get("Arrow").unwrap();
        assert_eq!(arrow.part_number, "ARR-789");
        assert_eq!(arrow.url, "https://arrow.com/arr-789");
    }
}
