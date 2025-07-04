use crate::Symbol;
use anyhow::Result;
use sexp::{parse, Atom, Sexp};
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
}
