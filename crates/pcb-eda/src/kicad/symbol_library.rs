use crate::Symbol;
use anyhow::{Context, Result, anyhow, bail, ensure};
use pcb_sexpr::{Sexpr, SexprKind, parse};
use regex::Regex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};
use tracing::{instrument, warn};

use super::symbol::{KicadSymbol, description_from_properties, parse_symbol};

/// Location of a symbol in the source file
#[derive(Debug, Clone)]
struct SymbolLocation {
    /// Index into `KicadSymbolLibrary.sources`
    source_idx: usize,
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
    /// Raw source contents (one entry for a flat library file, many for `.kicad_symdir`)
    sources: Vec<String>,
    /// Map from symbol name to its location in the file (BTreeMap for deterministic iteration order)
    symbol_locations: BTreeMap<String, SymbolLocation>,
    /// Cache of already-parsed and resolved symbols
    resolved_cache: RwLock<HashMap<String, Arc<KicadSymbol>>>,
    /// Symbol definitions as written, before `extends` resolution. Spans are
    /// relative to the start of the definition.
    definition_cache: RwLock<HashMap<String, Arc<Sexpr>>>,
    /// The `(version ...)` stamp from the library header, if declared.
    format_version: Option<i32>,
}

/// Read the `(version NNNN)` stamp from the header of a `(kicad_symbol_lib …)`
/// file without parsing it: scan only the text before the first symbol.
pub(super) fn scan_format_version(source: &str) -> Option<i32> {
    let head = source.trim_start().strip_prefix("(kicad_symbol_lib")?;
    let head = &head[..head.find("(symbol").unwrap_or(head.len())];
    let rest = head.split_once("(version")?.1;
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

impl KicadSymbolLibrary {
    /// Parse a KiCad symbol library from one or more source strings.
    ///
    /// A flat `.kicad_sym` library provides one source string; a split
    /// `.kicad_symdir` library provides one source string per symbol file.
    pub fn from_sources(sources: Vec<String>) -> Result<Self> {
        if sources.is_empty() {
            return Err(anyhow!("No symbol library sources provided"));
        }

        let mut symbol_locations = BTreeMap::new();
        for (source_idx, content) in sources.iter().enumerate() {
            for (name, location) in scan_symbol_locations(content, source_idx)? {
                symbol_locations.insert(name, location);
            }
        }

        let format_version = sources
            .first()
            .and_then(|source| scan_format_version(source));
        Ok(KicadSymbolLibrary {
            sources,
            symbol_locations,
            resolved_cache: RwLock::new(HashMap::new()),
            definition_cache: RwLock::new(HashMap::new()),
            format_version,
        })
    }

    /// The KiCad symbol-library format version declared in the file header.
    pub fn format_version(&self) -> Option<i32> {
        self.format_version
    }

    /// Parse a KiCad symbol library from a string with lazy parsing.
    ///
    /// This only scans for symbol names and byte ranges - no S-expr parsing.
    /// Actual parsing happens on-demand when symbols are requested via `get_symbol_lazy`.
    pub fn from_string_lazy(content: impl Into<String>) -> Result<Self> {
        Self::from_sources(vec![content.into()])
    }

    /// Parse a KiCad symbol library from a string (same as from_string_lazy).
    pub fn from_string(content: impl Into<String>) -> Result<Self> {
        Self::from_string_lazy(content)
    }

    /// Parse a KiCad symbol library from a file
    pub fn from_file(path: &Path) -> Result<Self> {
        if path.is_dir() {
            return Self::from_directory(path);
        }

        let content = fs::read_to_string(path)?;
        Self::from_string_lazy(content)
    }

    /// Load a complete library for inspection without the lazy loader's fallbacks.
    /// Each source is parsed once; definitions are cached for the usual resolver.
    /// Reject ambiguous metadata and invalid inheritance before resolving any symbol.
    pub fn from_file_strict(path: &Path) -> Result<Self> {
        let paths = if path.is_dir() {
            symbol_library_paths(path)?
        } else {
            vec![path.to_path_buf()]
        };
        ensure!(
            !paths.is_empty(),
            "No .kicad_sym files in {}",
            path.display()
        );
        let mut library = Self {
            sources: Vec::new(),
            symbol_locations: BTreeMap::new(),
            resolved_cache: RwLock::new(HashMap::new()),
            definition_cache: RwLock::new(HashMap::new()),
            format_version: None,
        };
        for path in &paths {
            let source = fs::read_to_string(path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            library
                .add_source_strict(source)
                .with_context(|| format!("Invalid symbol library {}", path.display()))?;
        }

        // Validate independently of the resolved cache, so no fallback can hide a
        // missing parent or cycle. Completed chains are visited only once.
        let mut checked = HashSet::new();
        for name in library.symbol_locations.keys() {
            let mut chain = Vec::new();
            let mut visiting = HashSet::new();
            let mut current = name.as_str();
            while !checked.contains(current) {
                if !visiting.insert(current) {
                    chain.push(current);
                    bail!(
                        "{}: inheritance cycle: {}",
                        path.display(),
                        chain.join(" -> ")
                    );
                }
                chain.push(current);
                let location = &library.symbol_locations[current];
                let Some(parent) = location.extends.as_deref() else {
                    break;
                };
                ensure!(
                    library.has_symbol(parent),
                    "{}: symbol {current:?} extends missing parent {parent:?}",
                    paths[location.source_idx].display()
                );
                current = parent;
            }
            checked.extend(chain);
        }
        Ok(library)
    }

    /// Validate and cache one source, retaining symbol-relative definition spans.
    fn add_source_strict(&mut self, source: String) -> Result<()> {
        if let Some((offset, fault)) = pcb_sexpr::scan::malformed(&source) {
            bail!("at byte {offset}: {fault}");
        }
        let mut roots = pcb_sexpr::parse_all(&source)?;
        ensure!(roots.len() == 1, "Expected exactly one library root");
        let mut root = roots.pop().unwrap();
        ensure!(
            pcb_sexpr::kicad::symbol::kicad_symbol_lib_items(&root).is_some(),
            "Expected a kicad_symbol_lib root"
        );
        let version = root
            .find_list("version")
            .and_then(|items| items.get(1))
            .and_then(Sexpr::as_int)
            .and_then(|version| i32::try_from(version).ok())
            .ok_or_else(|| anyhow!("Missing or invalid library version"))?;
        self.format_version.get_or_insert(version);
        for mut node in root.as_list_mut().unwrap().drain(1..) {
            let items = node
                .as_list()
                .ok_or_else(|| anyhow!("expected a library field at byte {}", node.span.start))?;
            if items.first().and_then(Sexpr::as_sym) != Some("symbol") {
                continue;
            }
            let name = items
                .get(1)
                .and_then(Sexpr::as_atom)
                .filter(|name| !name.is_empty())
                .ok_or_else(|| anyhow!("missing symbol name at byte {}", node.span.start))?
                .to_string();
            ensure!(!self.has_symbol(&name), "duplicate symbol {name:?}");
            let mut extends = None;
            let mut properties = HashSet::new();
            for field in &items[2..] {
                let field = field
                    .as_list()
                    .ok_or_else(|| anyhow!("symbol {name:?}: expected a field"))?;
                match field.first().and_then(Sexpr::as_sym) {
                    Some("extends") => {
                        ensure!(
                            extends.is_none() && field.len() == 2,
                            "symbol {name:?}: invalid extends"
                        );
                        extends = Some(
                            field[1]
                                .as_atom()
                                .filter(|name| !name.is_empty())
                                .ok_or_else(|| anyhow!("symbol {name:?}: invalid extends target"))?
                                .to_string(),
                        );
                    }
                    Some("property") => {
                        let (Some(key), Some(_)) = (
                            field.get(1).and_then(Sexpr::as_atom),
                            field.get(2).and_then(Sexpr::as_atom),
                        ) else {
                            bail!("symbol {name:?}: property requires a name and value");
                        };
                        ensure!(
                            properties.insert(key),
                            "symbol {name:?}: duplicate property {key:?}"
                        );
                    }
                    _ => {}
                }
            }
            let range = node.span.start..node.span.end;
            relativize_spans(&mut node, range.start);
            self.definition_cache
                .get_mut()
                .unwrap()
                .insert(name.clone(), Arc::new(node));
            self.symbol_locations.insert(
                name,
                SymbolLocation {
                    source_idx: self.sources.len(),
                    range,
                    extends,
                },
            );
        }
        self.sources.push(source);
        Ok(())
    }

    /// Parse a split KiCad symbol library from a `.kicad_symdir` directory.
    pub fn from_directory(path: &Path) -> Result<Self> {
        let sources: Vec<String> = symbol_library_paths(path)?
            .into_iter()
            .map(fs::read_to_string)
            .collect::<std::result::Result<_, _>>()?;

        Self::from_sources(sources).map_err(|err| {
            anyhow!(
                "Failed to parse symbol library directory {}: {}",
                path.display(),
                err
            )
        })
    }

    /// The library's source texts, in the order given to [`Self::from_sources`].
    pub fn sources(&self) -> &[String] {
        &self.sources
    }

    /// Symbol `name` as written: its source index, its byte offset in that
    /// source, and its parse, which symbol loading and checking share.
    pub(super) fn definition(&self, name: &str) -> Result<Option<(usize, usize, Arc<Sexpr>)>> {
        let Some(location) = self.symbol_locations.get(name) else {
            return Ok(None);
        };
        let at = |node| Some((location.source_idx, location.range.start, node));
        let cached = self
            .definition_cache
            .read()
            .map_err(|e| anyhow!("Cache read lock poisoned: {}", e))?
            .get(name)
            .cloned();
        if let Some(node) = cached {
            return Ok(at(node));
        }
        let node = Arc::new(parse(
            &self.sources[location.source_idx][location.range.clone()],
        )?);
        self.definition_cache
            .write()
            .map_err(|e| anyhow!("Cache write lock poisoned: {}", e))?
            .insert(name.to_string(), Arc::clone(&node));
        Ok(at(node))
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
        Ok(self.resolved(name)?.map(|symbol| (*symbol).clone()))
    }

    /// Symbol `name` as loading resolves it, shared with the cache.
    pub(super) fn resolved(&self, name: &str) -> Result<Option<Arc<KicadSymbol>>> {
        self.get_symbol_with_chain(name, &mut std::collections::HashSet::new())
    }

    /// Internal helper that tracks the extends chain to detect cycles.
    fn get_symbol_with_chain(
        &self,
        name: &str,
        chain: &mut std::collections::HashSet<String>,
    ) -> Result<Option<Arc<KicadSymbol>>> {
        // Check cache first (read lock)
        {
            let cache = self
                .resolved_cache
                .read()
                .map_err(|e| anyhow!("Cache read lock poisoned: {}", e))?;
            if let Some(cached) = cache.get(name) {
                return Ok(Some(Arc::clone(cached)));
            }
        }

        // Check if symbol exists
        let location = match self.symbol_locations.get(name) {
            Some(loc) => loc.clone(),
            None => return Ok(None),
        };

        // Parse just this symbol's substring
        let Some((_, _, definition)) = self.definition(name)? else {
            return Ok(None);
        };
        let base_symbol = parse_symbol(&definition)?;

        // Check for circular extends
        if chain.contains(name) {
            // Break cycle by returning symbol without parent resolution
            return Ok(Some(Arc::new(base_symbol)));
        }

        // Add to chain before resolving extends
        chain.insert(name.to_string());

        // Merge into the parent, itself resolved through its own extends
        // chain; a parent that is not found leaves the child as it is.
        let parent = match &location.extends {
            Some(parent_name) => self.get_symbol_with_chain(parent_name, chain)?,
            None => None,
        };
        let resolved = Arc::new(match parent {
            Some(parent) => merge_symbols(&parent, &base_symbol),
            None => base_symbol,
        });

        // Cache and return (write lock)
        {
            let mut cache = self
                .resolved_cache
                .write()
                .map_err(|e| anyhow!("Cache write lock poisoned: {}", e))?;
            cache.insert(name.to_string(), Arc::clone(&resolved));
        }
        Ok(Some(resolved))
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

    // Child items keyed for override lookup but kept in source order: a hash
    // map here reorders the flattened symbol on every run, which downstream
    // writers see as a real change to the schematic.
    let mut child_props: Vec<(String, Sexpr)> = Vec::new();
    let mut child_symbols: Vec<Sexpr> = Vec::new();
    let mut has_child_in_bom = false;

    for item in child_list.iter().skip(2) {
        if let SexprKind::List(prop_items) = &item.kind
            && let Some(first) = prop_items.first()
            && let SexprKind::Symbol(prop_type) = &first.kind
        {
            match prop_type.as_str() {
                "extends" => continue, // Skip extends in merged output
                "property" => {
                    if let Some(second) = prop_items.get(1)
                        && let SexprKind::Symbol(key) | SexprKind::String(key) = &second.kind
                    {
                        push_child_item(&mut child_props, key.clone(), item.clone());
                    }
                }
                "in_bom" => {
                    has_child_in_bom = true;
                    push_child_item(&mut child_props, "in_bom".to_string(), item.clone());
                }
                s if s.starts_with("symbol") => {
                    // This is a symbol section (like "symbol_0_1")
                    child_symbols.push(item.clone());
                }
                _ => {
                    // Other properties
                    push_child_item(&mut child_props, prop_type.clone(), item.clone());
                }
            }
        }
    }

    // Add parent properties that aren't overridden by child
    for item in parent_list.iter().skip(2) {
        if let SexprKind::List(prop_items) = &item.kind
            && let Some(first) = prop_items.first()
            && let SexprKind::Symbol(prop_type) = &first.kind
        {
            match prop_type.as_str() {
                "property" => {
                    if let Some(second) = prop_items.get(1)
                        && let SexprKind::Symbol(key) | SexprKind::String(key) = &second.kind
                        && !child_props.iter().any(|(existing, _)| existing == key)
                    {
                        merged_items.push(item.clone());
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
                                    SexprKind::Symbol(symbol_name)
                                        if symbol_name.starts_with(&parent_name) =>
                                    {
                                        // Replace parent name with child name in sub-symbol name
                                        let suffix = &symbol_name[parent_name.len()..];
                                        *symbol_name_expr =
                                            Sexpr::symbol(format!("{child_name}{suffix}"));
                                    }
                                    SexprKind::String(symbol_name)
                                        if symbol_name.starts_with(&parent_name) =>
                                    {
                                        // Replace parent name with child name in sub-symbol name
                                        let suffix = &symbol_name[parent_name.len()..];
                                        *symbol_name_expr =
                                            Sexpr::string(format!("{child_name}{suffix}"));
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
                    if !child_props
                        .iter()
                        .any(|(existing, _)| existing == prop_type)
                    {
                        merged_items.push(item.clone());
                    }
                }
            }
        }
    }

    // Add all child properties, in the child's own order
    for (_, prop) in child_props {
        merged_items.push(prop);
    }

    // Add child symbol sections
    for sym in child_symbols {
        merged_items.push(sym);
    }

    Sexpr::list(merged_items)
}

/// Record a child item under its key, replacing an earlier item with the same
/// key in place so the child's first mention decides the position.
fn push_child_item(items: &mut Vec<(String, Sexpr)>, key: String, item: Sexpr) {
    match items.iter_mut().find(|(existing, _)| *existing == key) {
        Some(entry) => entry.1 = item,
        None => items.push((key, item)),
    }
}

/// Regex to find `(symbol` followed by whitespace (handles newlines after keyword)
static SYMBOL_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(symbol\s").expect("Invalid regex"));

fn symbol_library_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("kicad_sym") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn relativize_spans(node: &mut Sexpr, offset: usize) {
    node.span.start -= offset;
    node.span.end -= offset;
    if let Some(items) = node.as_list_mut() {
        for item in items {
            relativize_spans(item, offset);
        }
    }
}

/// Scan content for symbol locations without parsing S-expressions.
///
/// Returns a map from symbol name to its byte range and extends info.
/// Uses containment-based filtering: symbols whose range is fully contained
/// within another symbol's range are considered sub-symbols and excluded.
#[instrument(name = "scan_symbols", skip(content), fields(content_len = content.len()))]
fn scan_symbol_locations(
    content: &str,
    source_idx: usize,
) -> Result<BTreeMap<String, SymbolLocation>> {
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
                source_idx,
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

/// Merge parent and child symbols (child overrides parent)
fn merge_symbols(parent: &KicadSymbol, child: &KicadSymbol) -> KicadSymbol {
    let mut merged = parent.clone();

    // Override with child's values
    merged.name = child.name.clone();
    merged.extends = child.extends.clone();

    // Jumper metadata always comes from the parent: KiCad's LIB_SYMBOL::Flatten()
    // never transfers a derived symbol's own jumper fields, so any declared on a
    // derived symbol are ignored.

    // Override properties that are explicitly set in child
    if !child.footprint.is_empty() {
        merged.footprint = child.footprint.clone();
    }

    if !child.pins.is_empty() {
        merged.pins = child.pins.clone();
        merged.native_stack_groups = child.native_stack_groups.clone();
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

    // Merge properties - child properties override parent
    for (key, value) in &child.properties {
        merged.properties.insert(key.clone(), value.clone());
    }
    merged.description = description_from_properties(&merged.properties);

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

    // Presence, not the defaulted boolean value, decides whether to override.
    if child
        .raw_sexp
        .as_ref()
        .is_some_and(|sexp| sexp.find_list("in_bom").is_some())
    {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_definitions_keep_symbol_relative_spans_and_share_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("parts.kicad_sym");
        let source = r#"(kicad_symbol_lib (version 20241209)
            (symbol "Base" (property "Value" "(symbol not-a-definition)"))
            (symbol "Child" (extends "Base")))"#;
        fs::write(&path, source).unwrap();
        let library = KicadSymbolLibrary::from_file_strict(&path).unwrap();
        assert_eq!(library.symbol_names(), ["Base", "Child"]);
        let (source_idx, offset, definition) = library.definition("Child").unwrap().unwrap();
        assert_eq!(source_idx, 0);
        assert_eq!(offset, source.find("(symbol \"Child\"").unwrap());
        assert_eq!(definition.span.start, 0);
        let parent = &definition.find_list("extends").unwrap()[1];
        assert_eq!(
            &source[offset + parent.span.start..offset + parent.span.end],
            "\"Base\""
        );
        assert!(Arc::ptr_eq(
            &definition,
            &library.definition("Child").unwrap().unwrap().2
        ));
        let child = library.get_symbol_lazy("Child").unwrap().unwrap();
        assert_eq!(
            child.metadata().primary.value.as_deref(),
            Some("(symbol not-a-definition)")
        );
    }

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
    fn test_parse_split_symbol_library_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("VCC.kicad_sym"),
            r##"(kicad_symbol_lib
                (symbol "VCC"
                    (property "Reference" "#PWR" (at 0 0 0))
                )
            )"##,
        )
        .unwrap();
        fs::write(
            dir.path().join("GND.kicad_sym"),
            r##"(kicad_symbol_lib
                (symbol "GND"
                    (property "Reference" "#PWR" (at 0 0 0))
                )
            )"##,
        )
        .unwrap();

        let lib = KicadSymbolLibrary::from_directory(dir.path()).unwrap();
        assert_eq!(lib.symbol_names(), vec!["GND", "VCC"]);
        assert!(lib.has_symbol("GND"));
        assert!(lib.has_symbol("VCC"));
    }

    #[test]
    fn test_resolve_extends_across_split_symbol_library_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("Base.kicad_sym"),
            r#"(kicad_symbol_lib
                (symbol "Base"
                    (property "Reference" "U" (at 0 0 0))
                    (property "Footprint" "Test:Base" (at 0 0 0))
                    (symbol "Base_0_1"
                        (pin input line (at 0 0 0) (length 2.54)
                            (name "IN")
                            (number "1")
                        )
                    )
                )
            )"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("Child.kicad_sym"),
            r#"(kicad_symbol_lib
                (symbol "Child"
                    (extends "Base")
                    (property "Reference" "U" (at 0 0 0))
                )
            )"#,
        )
        .unwrap();

        let lib = KicadSymbolLibrary::from_directory(dir.path()).unwrap();
        let child = lib.get_symbol_lazy("Child").unwrap().unwrap();
        assert_eq!(child.pins().len(), 1);
        assert_eq!(child.pins()[0].number, "1");
        assert_eq!(
            child.properties().get("Footprint").map(String::as_str),
            Some("Test:Base")
        );
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
    fn test_extends_merge_keeps_child_items_in_source_order() {
        let content = r#"(kicad_symbol_lib
            (symbol "Base"
                (pin_numbers (hide yes))
                (in_bom yes)
                (property "Reference" "L" (at 0 0 0))
                (property "Value" "Base" (at 0 0 0))
                (symbol "Base_1_1" (pin passive line (at 0 0 0) (length 2.54) (name "1") (number "1")))
                (embedded_fonts no)
            )
            (symbol "Child"
                (extends "Base")
                (property "Value" "Child" (at 0 0 0))
                (property "Description" "child" (at 0 0 0))
                (property "Manufacturer_Part_Number" "C-1" (at 0 0 0))
                (property "ki_keywords" "child" (at 0 0 0))
                (embedded_fonts no)
            )
        )"#;
        let expected = vec![
            "pin_numbers",
            "in_bom",
            "property:Reference",
            "symbol",
            "property:Value",
            "property:Description",
            "property:Manufacturer_Part_Number",
            "property:ki_keywords",
            "embedded_fonts",
        ];

        // The child's items follow the child's own order, run after run.
        for _ in 0..16 {
            let lib = KicadSymbolLibrary::from_string(content).unwrap();
            let child = lib.get_symbol_lazy("Child").unwrap().unwrap();
            assert_eq!(merged_item_tags(child.raw_sexp.as_ref().unwrap()), expected);
        }
    }

    fn merged_item_tags(sexpr: &Sexpr) -> Vec<String> {
        let SexprKind::List(items) = &sexpr.kind else {
            panic!("symbol is a list");
        };
        items
            .iter()
            .skip(2)
            .filter_map(|item| {
                let SexprKind::List(parts) = &item.kind else {
                    return None;
                };
                let SexprKind::Symbol(tag) = &parts.first()?.kind else {
                    return None;
                };
                if tag != "property" {
                    return Some(tag.clone());
                }
                match &parts.get(1)?.kind {
                    SexprKind::Symbol(name) | SexprKind::String(name) => {
                        Some(format!("property:{name}"))
                    }
                    _ => None,
                }
            })
            .collect()
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
    fn native_stack_groups_follow_effective_pins_through_extends() {
        for (child_pins, stack) in [
            ("", vec!["1", "2"]),
            (
                r#"(pin passive line (name "A") (number "1"))
                (pin passive line (name "B") (number "2"))"#,
                vec![],
            ),
            (
                r#"(pin passive line (name "C") (number "[5,6]"))"#,
                vec!["5", "6"],
            ),
        ] {
            let content = format!(
                r#"(kicad_symbol_lib
                (symbol "Base"
                    (jumper_pin_groups ("3" "4"))
                    (pin passive line (name "P") (number "[1,2]")))
                (symbol "Child" (extends "Base")
                    (jumper_pin_groups ("7" "8"))
                    {child_pins}))"#
            );
            let library = KicadSymbolLibrary::from_string(&content).unwrap();
            let child = library.get_symbol_lazy_as_eda("Child").unwrap().unwrap();
            let mut expected = vec![["3", "4"].into_iter().map(String::from).collect()];
            if !stack.is_empty() {
                expected.push(stack.into_iter().map(String::from).collect());
            }
            expected.sort();
            let mut actual = child.internal_connectivity.groups;
            actual.sort();
            assert_eq!(actual, expected, "{child_pins}");
        }
    }

    #[test]
    fn test_extends_inherits_parent_jumper_metadata() {
        // Matches KiCad's LIB_SYMBOL::Flatten(): jumper metadata on a derived
        // symbol is ignored; the parent's always wins.
        let content = r#"(kicad_symbol_lib
            (symbol "Base"
                (duplicate_pin_numbers_are_jumpers yes)
                (jumper_pin_groups ("1" "2"))
            )
            (symbol "Extended"
                (extends "Base")
                (duplicate_pin_numbers_are_jumpers no)
                (jumper_pin_groups ("5" "6"))
            )
        )"#;

        let lib = KicadSymbolLibrary::from_string(content).unwrap();
        let extended = lib.get_symbol_lazy("Extended").unwrap().unwrap();
        let expected: std::collections::BTreeSet<String> =
            ["1", "2"].into_iter().map(String::from).collect();

        assert!(extended.internal_connectivity.duplicate_numbers_are_jumpers);
        assert_eq!(extended.internal_connectivity.groups, vec![expected]);
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
