use crate::Symbol;
use anyhow::Result;
use pcb_sexpr::{parse, Sexpr};
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use super::symbol::{parse_symbol, KicadSymbol};

/// A KiCad symbol library that can contain multiple symbols
pub struct KicadSymbolLibrary {
    symbols: Vec<KicadSymbol>,
}

impl KicadSymbolLibrary {
    /// Parse a KiCad symbol library from a string with lazy extends resolution
    pub fn from_string_lazy(content: &str) -> Result<Self> {
        // Parse symbols without resolving extends
        let symbol_pairs = parse_with_raw_sexprs(content)?;
        let symbols: Vec<KicadSymbol> = symbol_pairs.into_iter().map(|(s, _)| s).collect();

        Ok(KicadSymbolLibrary { symbols })
    }

    /// Parse a KiCad symbol library from a string (eager resolution for backwards compatibility)
    pub fn from_string(content: &str) -> Result<Self> {
        // Parse with raw s-expressions
        let mut symbol_pairs = parse_with_raw_sexprs(content)?;

        // Create a map for extends resolution
        let mut symbol_map: HashMap<String, (usize, Sexpr)> = HashMap::new();
        for (idx, (symbol, sexp)) in symbol_pairs.iter().enumerate() {
            symbol_map.insert(symbol.name().to_string(), (idx, sexp.clone()));
        }

        // Build dependency order for extends resolution
        let mut resolved: HashSet<String> = HashSet::new();
        let mut to_process: Vec<usize> = Vec::new();

        // First, add all symbols without extends
        for (idx, (symbol, _)) in symbol_pairs.iter().enumerate() {
            if symbol.extends().is_none() {
                resolved.insert(symbol.name().to_string());
                to_process.push(idx);
            }
        }

        // Then, iteratively add symbols whose parent has been resolved
        let mut made_progress = true;
        while made_progress {
            made_progress = false;
            for (idx, (symbol, _)) in symbol_pairs.iter().enumerate() {
                if let Some(parent_name) = symbol.extends() {
                    if resolved.contains(parent_name) && !resolved.contains(symbol.name()) {
                        resolved.insert(symbol.name().to_string());
                        to_process.push(idx);
                        made_progress = true;
                    }
                }
            }
        }

        // Process symbols in dependency order
        for &idx in &to_process {
            let (symbol, _) = &symbol_pairs[idx];
            if let Some(parent_name) = symbol.extends() {
                // Find the parent's already-merged sexp
                let parent_sexp = symbol_pairs
                    .iter()
                    .find(|(s, _)| s.name() == parent_name)
                    .map(|(_, sexp)| sexp.clone())
                    .unwrap_or_else(|| symbol_pairs[idx].1.clone());

                let (_, child_sexp) = &symbol_pairs[idx].clone();
                let merged_sexp = merge_symbol_sexprs(&parent_sexp, child_sexp);
                symbol_pairs[idx].1 = merged_sexp.clone();
                symbol_pairs[idx].0.raw_sexp = Some(merged_sexp);
            }
        }

        // Extract just the symbols
        let mut symbols: Vec<KicadSymbol> = symbol_pairs.into_iter().map(|(s, _)| s).collect();

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

    /// Get a symbol by name with lazy extends resolution
    pub fn get_symbol_lazy(&self, name: &str) -> Result<Option<KicadSymbol>> {
        // Find the base symbol
        let base_symbol = match self.symbols.iter().find(|s| s.name() == name) {
            Some(symbol) => symbol,
            None => return Ok(None),
        };

        // If no extends, return as-is
        if base_symbol.extends().is_none() {
            return Ok(Some(base_symbol.clone()));
        }

        // Otherwise, resolve the extends chain
        self.resolve_symbol_extends(base_symbol)
    }

    /// Resolve extends for a single symbol
    fn resolve_symbol_extends(&self, symbol: &KicadSymbol) -> Result<Option<KicadSymbol>> {
        let mut resolved = symbol.clone();
        let mut extends_chain = Vec::new();

        // Build the extends chain
        let mut current = symbol;
        while let Some(parent_name) = current.extends() {
            if extends_chain.contains(&parent_name) {
                // Circular dependency detected
                eprintln!(
                    "Warning: Circular extends dependency detected for symbol '{}'",
                    symbol.name()
                );
                break;
            }
            extends_chain.push(parent_name);

            // Find parent in current library
            if let Some(parent) = self.symbols.iter().find(|s| s.name() == parent_name) {
                current = parent;
            } else {
                // Parent not found in current library
                eprintln!(
                    "Warning: Symbol '{}' extends '{}' but parent not found",
                    symbol.name(),
                    parent_name
                );
                break;
            }
        }

        // Apply inheritance in reverse order (from base to derived)
        for parent_name in extends_chain.iter().rev() {
            if let Some(parent) = self.symbols.iter().find(|s| s.name() == *parent_name) {
                // Merge raw S-expressions if both have them
                if let (Some(parent_sexp), Some(child_sexp)) =
                    (&parent.raw_sexp, &resolved.raw_sexp)
                {
                    resolved.raw_sexp = Some(merge_symbol_sexprs(parent_sexp, child_sexp));
                }
                resolved = self.merge_symbols(parent, &resolved);
            }
        }

        Ok(Some(resolved))
    }

    /// Merge parent and child symbols
    fn merge_symbols(&self, parent: &KicadSymbol, child: &KicadSymbol) -> KicadSymbol {
        let mut merged = parent.clone();

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

        // Merge distributors
        for (dist, part) in &child.distributors {
            if let Some(parent_part) = merged.distributors.get_mut(dist) {
                if !part.part_number.is_empty() {
                    parent_part.part_number = part.part_number.clone();
                }
                if !part.url.is_empty() {
                    parent_part.url = part.url.clone();
                }
            } else {
                merged.distributors.insert(dist.clone(), part.clone());
            }
        }

        if child.in_bom {
            merged.in_bom = child.in_bom;
        }

        if child.raw_sexp.is_some() {
            merged.raw_sexp = child.raw_sexp.clone();
        }

        merged
    }

    /// Get the names of all symbols in the library
    pub fn symbol_names(&self) -> Vec<&str> {
        self.symbols.iter().map(|s| s.name()).collect()
    }

    /// Convert all symbols to the generic Symbol type
    pub fn into_symbols(self) -> Vec<Symbol> {
        self.symbols.into_iter().map(|s| s.into()).collect()
    }

    /// Convert all symbols to the generic Symbol type with lazy resolution
    pub fn into_symbols_lazy(self) -> Result<Vec<Symbol>> {
        let mut result = Vec::new();

        for symbol in &self.symbols {
            if let Some(resolved) = self.get_symbol_lazy(symbol.name())? {
                result.push(resolved.into());
            }
        }

        Ok(result)
    }

    /// Get a specific symbol with lazy resolution and convert to generic Symbol type
    pub fn get_symbol_lazy_as_eda(&self, name: &str) -> Result<Option<Symbol>> {
        Ok(self.get_symbol_lazy(name)?.map(|s| s.into()))
    }
}

/// Merge two symbol S-expressions, with child overriding parent
fn merge_symbol_sexprs(parent_sexp: &Sexpr, child_sexp: &Sexpr) -> Sexpr {
    // Both should be lists starting with "symbol"
    let parent_list = match parent_sexp {
        Sexpr::List(items) => items,
        _ => return child_sexp.clone(),
    };

    let child_list = match child_sexp {
        Sexpr::List(items) => items,
        _ => return child_sexp.clone(),
    };

    // Get the parent and child symbol names
    let parent_name = match parent_list.get(1) {
        Some(Sexpr::Symbol(name) | Sexpr::String(name)) => name.clone(),
        _ => "Unknown".to_string(),
    };

    let child_name = match child_list.get(1) {
        Some(Sexpr::Symbol(name) | Sexpr::String(name)) => name.clone(),
        _ => "Unknown".to_string(),
    };

    // Start with parent items, but skip the "symbol" and name
    let mut merged_items = vec![
        Sexpr::Symbol("symbol".to_string()),
        child_list
            .get(1)
            .cloned()
            .unwrap_or_else(|| Sexpr::Symbol("Unknown".to_string())),
    ];

    // Create a map of child properties for easy lookup
    let mut child_props: HashMap<String, Sexpr> = HashMap::new();
    let mut child_symbols: Vec<Sexpr> = Vec::new();
    let mut has_child_in_bom = false;

    for item in child_list.iter().skip(2) {
        if let Sexpr::List(prop_items) = item {
            if let Some(Sexpr::Symbol(prop_type)) = prop_items.first() {
                match prop_type.as_str() {
                    "extends" => continue, // Skip extends in merged output
                    "property" => {
                        if let Some(Sexpr::Symbol(key) | Sexpr::String(key)) = prop_items.get(1) {
                            child_props.insert(key.clone(), item.clone());
                        }
                    }
                    "in_bom" => {
                        has_child_in_bom = true;
                        child_props.insert("in_bom".to_string(), item.clone());
                    }
                    s if s.starts_with("symbol") => {
                        // This is a symbol section (like "symbol_0_1")
                        child_symbols.push(item.clone());
                    }
                    _ => {
                        // Other properties
                        child_props.insert(prop_type.clone(), item.clone());
                    }
                }
            }
        }
    }

    // Add parent properties that aren't overridden by child
    for item in parent_list.iter().skip(2) {
        if let Sexpr::List(prop_items) = item {
            if let Some(Sexpr::Symbol(prop_type)) = prop_items.first() {
                match prop_type.as_str() {
                    "property" => {
                        if let Some(Sexpr::Symbol(key) | Sexpr::String(key)) = prop_items.get(1) {
                            if !child_props.contains_key(key) {
                                merged_items.push(item.clone());
                            }
                        }
                    }
                    "in_bom" => {
                        if !has_child_in_bom {
                            merged_items.push(item.clone());
                        }
                    }
                    s if s.starts_with("symbol") => {
                        // Skip parent symbol sections if child has any
                        if child_symbols.is_empty() {
                            // Rename parent sub-symbol to match child symbol name
                            if let Sexpr::List(mut symbol_items) = item.clone() {
                                if let Some(symbol_name_expr) = symbol_items.get_mut(1) {
                                    match symbol_name_expr {
                                        Sexpr::Symbol(symbol_name) => {
                                            // Replace parent name with child name in sub-symbol name
                                            if symbol_name.starts_with(&parent_name) {
                                                let suffix = &symbol_name[parent_name.len()..];
                                                *symbol_name = format!("{child_name}{suffix}");
                                            }
                                        }
                                        Sexpr::String(symbol_name) => {
                                            // Replace parent name with child name in sub-symbol name
                                            if symbol_name.starts_with(&parent_name) {
                                                let suffix = &symbol_name[parent_name.len()..];
                                                *symbol_name = format!("{child_name}{suffix}");
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                merged_items.push(Sexpr::List(symbol_items));
                            } else {
                                merged_items.push(item.clone());
                            }
                        }
                    }
                    _ => {
                        if !child_props.contains_key(prop_type) {
                            merged_items.push(item.clone());
                        }
                    }
                }
            }
        }
    }

    // Add all child properties
    for (_, prop) in child_props {
        merged_items.push(prop);
    }

    // Add child symbol sections
    for sym in child_symbols {
        merged_items.push(sym);
    }

    Sexpr::List(merged_items)
}

/// Parse a KiCad symbol library from a string, keeping raw S-expressions
pub fn parse_with_raw_sexprs(content: &str) -> Result<Vec<(KicadSymbol, Sexpr)>> {
    let sexp = parse(content)?;
    let mut symbol_pairs = Vec::new();

    match sexp {
        Sexpr::List(kicad_symbol_lib) => {
            // Iterate through all items in the library
            for item in kicad_symbol_lib {
                if let Sexpr::List(ref symbol_list) = item {
                    if let Some(Sexpr::Symbol(ref sym)) = symbol_list.first() {
                        if sym == "symbol" {
                            // Parse this symbol
                            match parse_symbol(symbol_list) {
                                Ok(mut symbol) => {
                                    // Store the raw s-expression with the symbol
                                    symbol.raw_sexp = Some(item.clone());
                                    symbol_pairs.push((symbol, item.clone()));
                                }
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

    Ok(symbol_pairs)
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

            // For raw_sexp, use the child's if it exists, otherwise keep parent's
            if child.raw_sexp.is_some() {
                merged.raw_sexp = child.raw_sexp.clone();
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

    #[test]
    fn test_extends_renames_sub_symbols() {
        let content = r#"(kicad_symbol_lib
            (symbol "BaseIC"
                (property "Reference" "U" (at 0 0 0))
                (symbol "BaseIC_0_1"
                    (rectangle (start -5.08 5.08) (end 5.08 -5.08))
                )
                (symbol "BaseIC_1_1"
                    (pin input line (at -7.62 2.54 0) (length 2.54)
                        (name "IN" (effects (font (size 1.27 1.27))))
                        (number "1" (effects (font (size 1.27 1.27))))
                    )
                )
            )
            (symbol "CustomIC"
                (extends "BaseIC")
                (property "Value" "CustomIC" (at 0 0 0))
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        let custom = lib.get_symbol("CustomIC").unwrap();

        // Check that the raw S-expression has renamed sub-symbols
        if let Some(raw_sexp) = &custom.raw_sexp {
            let sexp_str = format!("{raw_sexp:?}");

            // Should contain CustomIC_0_1 and CustomIC_1_1, not BaseIC_0_1 and BaseIC_1_1
            assert!(
                sexp_str.contains("CustomIC_0_1"),
                "Should contain CustomIC_0_1"
            );
            assert!(
                sexp_str.contains("CustomIC_1_1"),
                "Should contain CustomIC_1_1"
            );
            assert!(
                !sexp_str.contains("BaseIC_0_1"),
                "Should not contain BaseIC_0_1"
            );
            assert!(
                !sexp_str.contains("BaseIC_1_1"),
                "Should not contain BaseIC_1_1"
            );
        } else {
            panic!("CustomIC should have raw_sexp after extends resolution");
        }
    }
}
