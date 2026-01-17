use crate::Symbol;
use anyhow::{anyhow, Result};
use pcb_sexpr::{parse, Sexpr, SexprKind};
use regex::Regex;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::ops::Range;
use std::path::Path;
use std::sync::{LazyLock, RwLock};
use tracing::{instrument, warn};

use super::symbol::{parse_symbol, KicadSymbol};

/// Location of a symbol in the source file
#[derive(Debug, Clone)]
struct SymbolLocation {
    /// Byte range in the source content
    range: Range<usize>,
    /// Name of parent symbol if this symbol uses `extends`
    extends: Option<String>,
}

/// A KiCad symbol library that can contain multiple symbols.
///
/// Uses lazy parsing: on construction, only scans for symbol names and byte ranges.
/// Actual S-expr parsing happens on-demand when symbols are requested.
pub struct KicadSymbolLibrary {
    /// Raw file content (for lazy parsing)
    content: String,
    /// Map from symbol name to its location in the file (BTreeMap for deterministic iteration order)
    symbol_locations: BTreeMap<String, SymbolLocation>,
    /// Cache of already-parsed and resolved symbols
    resolved_cache: RwLock<HashMap<String, KicadSymbol>>,
}

impl KicadSymbolLibrary {
    /// Parse a KiCad symbol library from a string with lazy parsing.
    ///
    /// This only scans for symbol names and byte ranges - no S-expr parsing.
    /// Actual parsing happens on-demand when symbols are requested via `get_symbol_lazy`.
    pub fn from_string_lazy(content: impl Into<String>) -> Result<Self> {
        let content = content.into();
        let symbol_locations = scan_symbol_locations(&content)?;

        Ok(KicadSymbolLibrary {
            content,
            symbol_locations,
            resolved_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Parse a KiCad symbol library from a string (same as from_string_lazy).
    pub fn from_string(content: impl Into<String>) -> Result<Self> {
        Self::from_string_lazy(content)
    }

    /// Parse a KiCad symbol library from a file
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)?;
        Self::from_string_lazy(content)
    }

    /// Check if a symbol exists in this library
    pub fn has_symbol(&self, name: &str) -> bool {
        self.symbol_locations.contains_key(name)
    }

    /// Get the names of all symbols in the library
    pub fn symbol_names(&self) -> Vec<&str> {
        self.symbol_locations.keys().map(|s| s.as_str()).collect()
    }

    /// Get a symbol by name with lazy parsing and extends resolution.
    ///
    /// This is the primary way to retrieve symbols. It parses the symbol
    /// on-demand from the raw content and resolves any extends chain.
    #[instrument(name = "get_symbol", skip(self), fields(symbol = %name))]
    pub fn get_symbol_lazy(&self, name: &str) -> Result<Option<KicadSymbol>> {
        self.get_symbol_with_chain(name, &mut std::collections::HashSet::new())
    }

    /// Internal helper that tracks the extends chain to detect cycles.
    fn get_symbol_with_chain(
        &self,
        name: &str,
        chain: &mut std::collections::HashSet<String>,
    ) -> Result<Option<KicadSymbol>> {
        // Check cache first (read lock)
        {
            let cache = self
                .resolved_cache
                .read()
                .map_err(|e| anyhow!("Cache read lock poisoned: {}", e))?;
            if let Some(cached) = cache.get(name) {
                return Ok(Some(cached.clone()));
            }
        }

        // Check if symbol exists
        let location = match self.symbol_locations.get(name) {
            Some(loc) => loc.clone(),
            None => return Ok(None),
        };

        // Check for circular extends
        if chain.contains(name) {
            // Break cycle by returning symbol without parent resolution
            let symbol_str = &self.content[location.range.clone()];
            return Ok(Some(parse_symbol_from_substring(symbol_str)?));
        }

        // Add to chain before resolving extends
        chain.insert(name.to_string());

        // Parse just this symbol's substring
        let symbol_str = &self.content[location.range.clone()];
        let base_symbol = parse_symbol_from_substring(symbol_str)?;

        // Resolve extends chain if needed
        let resolved = if let Some(parent_name) = &location.extends {
            self.resolve_extends_with_chain(&base_symbol, parent_name, chain)?
        } else {
            base_symbol
        };

        // Cache and return (write lock)
        {
            let mut cache = self
                .resolved_cache
                .write()
                .map_err(|e| anyhow!("Cache write lock poisoned: {}", e))?;
            cache.insert(name.to_string(), resolved.clone());
        }
        Ok(Some(resolved))
    }

    /// Resolve the extends chain for a symbol, passing the chain for cycle detection.
    fn resolve_extends_with_chain(
        &self,
        child: &KicadSymbol,
        parent_name: &str,
        chain: &mut std::collections::HashSet<String>,
    ) -> Result<KicadSymbol> {
        // Get parent (recursively resolving its extends chain)
        let parent = match self.get_symbol_with_chain(parent_name, chain)? {
            Some(p) => p,
            None => {
                // Parent not found - return child as-is
                return Ok(child.clone());
            }
        };

        // Merge parent and child
        Ok(merge_symbols(&parent, child))
    }

    /// Convert all symbols to the generic Symbol type with lazy resolution
    pub fn into_symbols_lazy(self) -> Result<Vec<Symbol>> {
        let names: Vec<String> = self.symbol_locations.keys().cloned().collect();
        let mut result = Vec::with_capacity(names.len());

        for name in names {
            if let Some(resolved) = self.get_symbol_lazy(&name)? {
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
    let parent_list = match &parent_sexp.kind {
        SexprKind::List(items) => items,
        _ => return child_sexp.clone(),
    };

    let child_list = match &child_sexp.kind {
        SexprKind::List(items) => items,
        _ => return child_sexp.clone(),
    };

    // Get the parent and child symbol names
    let parent_name = match parent_list.get(1) {
        Some(s) => match &s.kind {
            SexprKind::Symbol(name) | SexprKind::String(name) => name.clone(),
            _ => "Unknown".to_string(),
        },
        _ => "Unknown".to_string(),
    };

    let child_name = match child_list.get(1) {
        Some(s) => match &s.kind {
            SexprKind::Symbol(name) | SexprKind::String(name) => name.clone(),
            _ => "Unknown".to_string(),
        },
        _ => "Unknown".to_string(),
    };

    // Start with parent items, but skip the "symbol" and name
    let mut merged_items = vec![
        Sexpr::symbol("symbol"),
        child_list
            .get(1)
            .cloned()
            .unwrap_or_else(|| Sexpr::symbol("Unknown")),
    ];

    // Create a map of child properties for easy lookup
    let mut child_props: HashMap<String, Sexpr> = HashMap::new();
    let mut child_symbols: Vec<Sexpr> = Vec::new();
    let mut has_child_in_bom = false;

    for item in child_list.iter().skip(2) {
        if let SexprKind::List(prop_items) = &item.kind {
            if let Some(first) = prop_items.first() {
                if let SexprKind::Symbol(prop_type) = &first.kind {
                    match prop_type.as_str() {
                        "extends" => continue, // Skip extends in merged output
                        "property" => {
                            if let Some(second) = prop_items.get(1) {
                                if let SexprKind::Symbol(key) | SexprKind::String(key) =
                                    &second.kind
                                {
                                    child_props.insert(key.clone(), item.clone());
                                }
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
    }

    // Add parent properties that aren't overridden by child
    for item in parent_list.iter().skip(2) {
        if let SexprKind::List(prop_items) = &item.kind {
            if let Some(first) = prop_items.first() {
                if let SexprKind::Symbol(prop_type) = &first.kind {
                    match prop_type.as_str() {
                        "property" => {
                            if let Some(second) = prop_items.get(1) {
                                if let SexprKind::Symbol(key) | SexprKind::String(key) =
                                    &second.kind
                                {
                                    if !child_props.contains_key(key) {
                                        merged_items.push(item.clone());
                                    }
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
                                if let SexprKind::List(symbol_items) = &item.kind {
                                    let mut symbol_items = symbol_items.clone();
                                    if let Some(symbol_name_expr) = symbol_items.get_mut(1) {
                                        match &symbol_name_expr.kind {
                                            SexprKind::Symbol(symbol_name) => {
                                                // Replace parent name with child name in sub-symbol name
                                                if symbol_name.starts_with(&parent_name) {
                                                    let suffix = &symbol_name[parent_name.len()..];
                                                    *symbol_name_expr = Sexpr::symbol(format!(
                                                        "{child_name}{suffix}"
                                                    ));
                                                }
                                            }
                                            SexprKind::String(symbol_name) => {
                                                // Replace parent name with child name in sub-symbol name
                                                if symbol_name.starts_with(&parent_name) {
                                                    let suffix = &symbol_name[parent_name.len()..];
                                                    *symbol_name_expr = Sexpr::string(format!(
                                                        "{child_name}{suffix}"
                                                    ));
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                    merged_items.push(Sexpr::list(symbol_items));
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
    }

    // Add all child properties
    for (_, prop) in child_props {
        merged_items.push(prop);
    }

    // Add child symbol sections
    for sym in child_symbols {
        merged_items.push(sym);
    }

    Sexpr::list(merged_items)
}

/// Parse a KiCad symbol library from a string, keeping raw S-expressions
pub fn parse_with_raw_sexprs(content: &str) -> Result<Vec<(KicadSymbol, Sexpr)>> {
    let sexp = parse(content)?;
    let mut symbol_pairs = Vec::new();

    match &sexp.kind {
        SexprKind::List(kicad_symbol_lib) => {
            // Iterate through all items in the library
            for item in kicad_symbol_lib {
                if let SexprKind::List(symbol_list) = &item.kind {
                    if let Some(SexprKind::Symbol(sym)) = symbol_list.first().map(|s| &s.kind) {
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

/// Regex to find `(symbol` followed by whitespace (handles newlines after keyword)
static SYMBOL_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(symbol\s").expect("Invalid regex"));

/// Scan content for symbol locations without parsing S-expressions.
///
/// Returns a map from symbol name to its byte range and extends info.
/// Uses containment-based filtering: symbols whose range is fully contained
/// within another symbol's range are considered sub-symbols and excluded.
#[instrument(name = "scan_symbols", skip(content), fields(content_len = content.len()))]
fn scan_symbol_locations(content: &str) -> Result<BTreeMap<String, SymbolLocation>> {
    let bytes = content.as_bytes();

    let mut locations = BTreeMap::new();
    let mut current_top_end = 0;

    for mat in SYMBOL_REGEX.find_iter(content) {
        let symbol_start = mat.start();
        let after_keyword = mat.end(); // Position right after "(symbol" + whitespace char

        if symbol_start < current_top_end {
            continue;
        }

        if after_keyword >= bytes.len() {
            continue;
        }

        // Skip any additional whitespace/newlines after the match
        let mut name_start = after_keyword;
        while name_start < bytes.len() && bytes[name_start].is_ascii_whitespace() {
            name_start += 1;
        }

        if name_start >= bytes.len() {
            continue;
        }

        // Parse the symbol name (either quoted "Name" or unquoted Name)
        let (name, _name_end) = if bytes.get(name_start) == Some(&b'"') {
            // Quoted name
            let start = name_start + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'"' {
                if bytes[end] == b'\\' && end + 1 < bytes.len() {
                    end += 2; // Skip escaped char
                } else {
                    end += 1;
                }
            }
            let name = String::from_utf8_lossy(&bytes[start..end]).to_string();
            (name, end + 1)
        } else {
            // Unquoted name
            let start = name_start;
            let mut end = start;
            while end < bytes.len() && !bytes[end].is_ascii_whitespace() && bytes[end] != b')' {
                end += 1;
            }
            let name = String::from_utf8_lossy(&bytes[start..end]).to_string();
            (name, end)
        };

        if name.is_empty() {
            continue;
        }

        // Find the end of this symbol by counting parentheses
        let symbol_end = match find_matching_paren(bytes, symbol_start) {
            Ok(end) => end,
            Err(_) => continue, // Skip malformed symbols
        };

        // Look for extends within this symbol's content
        let symbol_content = &content[symbol_start..symbol_end];
        let extends = extract_extends(symbol_content);

        current_top_end = symbol_end;
        locations.insert(
            name,
            SymbolLocation {
                range: symbol_start..symbol_end,
                extends,
            },
        );
    }

    Ok(locations)
}

/// Find the matching closing paren for the opening paren at `start`
fn find_matching_paren(bytes: &[u8], start: usize) -> Result<usize> {
    let mut depth = 0;
    let mut pos = start;
    let mut in_string = false;

    while pos < bytes.len() {
        let b = bytes[pos];

        if in_string {
            if b == b'\\' && pos + 1 < bytes.len() {
                pos += 2; // Skip escaped char
                continue;
            }
            if b == b'"' {
                in_string = false;
            }
        } else {
            match b {
                b'"' => in_string = true,
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(pos + 1);
                    }
                }
                _ => {}
            }
        }
        pos += 1;
    }

    Err(anyhow!("Unmatched parenthesis at position {}", start))
}

/// Extract the extends target from a symbol's content
fn extract_extends(content: &str) -> Option<String> {
    // Look for (extends "ParentName") pattern
    let pattern = "(extends ";
    let start = content.find(pattern)?;
    let after = &content[start + pattern.len()..];

    // Skip whitespace
    let after = after.trim_start();

    // Extract the name (quoted or unquoted)
    if let Some(inner) = after.strip_prefix('"') {
        // Find the closing quote
        let end = inner.find('"')?;
        Some(inner[..end].to_string())
    } else {
        let end = after.find(|c: char| c.is_whitespace() || c == ')')?;
        Some(after[..end].to_string())
    }
}

/// Parse a single symbol from its raw S-expression substring
fn parse_symbol_from_substring(content: &str) -> Result<KicadSymbol> {
    let sexp = parse(content)?;

    match &sexp.kind {
        SexprKind::List(items) => {
            let mut symbol = parse_symbol(items)?;
            symbol.raw_sexp = Some(sexp);
            Ok(symbol)
        }
        _ => Err(anyhow!("Expected symbol S-expression list")),
    }
}

/// Merge parent and child symbols (child overrides parent)
fn merge_symbols(parent: &KicadSymbol, child: &KicadSymbol) -> KicadSymbol {
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

    // Merge raw S-expressions if both have them
    if let (Some(parent_sexp), Some(child_sexp)) = (&parent.raw_sexp, &child.raw_sexp) {
        merged.raw_sexp = Some(merge_symbol_sexprs(parent_sexp, child_sexp));
    } else if child.raw_sexp.is_some() {
        merged.raw_sexp = child.raw_sexp.clone();
    }

    merged
}

/// Resolve extends references by cloning parent symbols and applying child overrides
#[allow(dead_code)]
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
        assert_eq!(lib.symbol_names().len(), 2);
        assert!(lib.has_symbol("Symbol1"));
        assert!(lib.has_symbol("Symbol2"));
    }

    #[test]
    fn test_symbol_name_with_trailing_underscore() {
        // Regression test: symbol names ending with underscore should not be
        // filtered out as sub-symbols (e.g., "BM04B-SRSS-TB_LF__SN_")
        let content = r#"(kicad_symbol_lib
            (symbol "BM04B-SRSS-TB_LF__SN_"
                (property "Reference" "J" (at 0 0 0))
                (property "Value" "BM04B-SRSS-TB_LF__SN_" (at 0 0 0))
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        assert_eq!(lib.symbol_names().len(), 1);
        assert!(lib.has_symbol("BM04B-SRSS-TB_LF__SN_"));

        let symbol = lib
            .get_symbol_lazy("BM04B-SRSS-TB_LF__SN_")
            .unwrap()
            .unwrap();
        assert_eq!(symbol.name(), "BM04B-SRSS-TB_LF__SN_");
    }

    #[test]
    fn test_symbol_name_with_underscores_not_subsymbol() {
        // Symbol names with underscores but not ending in digits should be kept
        let content = r#"(kicad_symbol_lib
            (symbol "My_Component_V2"
                (property "Reference" "U" (at 0 0 0))
            )
            (symbol "Another_Part_"
                (property "Reference" "U" (at 0 0 0))
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        assert_eq!(lib.symbol_names().len(), 2);
        assert!(lib.has_symbol("My_Component_V2"));
        assert!(lib.has_symbol("Another_Part_"));
    }

    #[test]
    fn test_symbol_name_on_separate_line() {
        // Regression test: symbol name can be on a separate line after "(symbol"
        let content = r#"(kicad_symbol_lib
            (symbol
                "TYPE-C24PQT"
                (property "Reference" "J" (at 0 0 0))
                (property "Value" "TYPE-C24PQT" (at 0 0 0))
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        assert_eq!(lib.symbol_names().len(), 1);
        assert!(lib.has_symbol("TYPE-C24PQT"));

        let symbol = lib.get_symbol_lazy("TYPE-C24PQT").unwrap().unwrap();
        assert_eq!(symbol.name(), "TYPE-C24PQT");
    }

    #[test]
    fn test_subsymbol_filtering_by_containment() {
        // Sub-symbols should be filtered out based on containment, not name pattern
        let content = r#"(kicad_symbol_lib
            (symbol "MyComponent"
                (property "Reference" "U" (at 0 0 0))
                (symbol "MyComponent_0_1"
                    (pin input line (at 0 0 0) (length 2.54)
                        (name "A" (effects (font (size 1.27 1.27))))
                        (number "1" (effects (font (size 1.27 1.27))))
                    )
                )
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        // Should only have the top-level symbol, not the sub-symbol
        assert_eq!(lib.symbol_names().len(), 1);
        assert!(lib.has_symbol("MyComponent"));
        assert!(!lib.has_symbol("MyComponent_0_1"));
    }

    #[test]
    fn test_extract_extends_quoted() {
        // Test the extract_extends helper function with quoted parent name
        let content = r#"(extends "BaseSymbol")"#;
        let result = extract_extends(content);
        assert_eq!(result, Some("BaseSymbol".to_string()));
    }

    #[test]
    fn test_extract_extends_unquoted() {
        // Test extract_extends with unquoted parent name
        let content = r#"(extends BaseSymbol)"#;
        let result = extract_extends(content);
        assert_eq!(result, Some("BaseSymbol".to_string()));
    }

    #[test]
    fn test_extract_extends_in_symbol() {
        // Test extract_extends within a full symbol definition
        let content = r#"(symbol "Child"
            (extends "ParentSymbol")
            (property "Value" "ChildValue" (at 0 0 0))
        )"#;
        let result = extract_extends(content);
        assert_eq!(result, Some("ParentSymbol".to_string()));
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
        assert_eq!(lib.symbol_names().len(), 2);

        let extended = lib.get_symbol_lazy("ExtendedSymbol").unwrap().unwrap();
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
        let child = lib.get_symbol_lazy("Child").unwrap().unwrap();

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
        let child = lib.get_symbol_lazy("Child").unwrap().unwrap();

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
        let final_symbol = lib.get_symbol_lazy("Final").unwrap().unwrap();

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
        let orphan = lib.get_symbol_lazy("Orphan").unwrap().unwrap();

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
        let extended = lib.get_symbol_lazy("Extended").unwrap().unwrap();

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
    fn test_extends_reference() {
        let content = r#"(kicad_symbol_lib
            (symbol "Base"
                (property "Reference" "J" (at 0 0 0))
            )
            (symbol "Extended"
                (extends "Base")
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        let extended = lib.get_symbol_lazy("Extended").unwrap().unwrap();
        assert_eq!(extended.reference, "J");
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
        let custom = lib.get_symbol_lazy("CustomIC").unwrap().unwrap();

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
