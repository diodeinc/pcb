// Simple string interner for IPC-2581 parsing
// Deduplicates repeated identifiers (layer names, material specs, etc.)
// Derived from: https://matklad.github.io/2020/03/22/fast-simple-rust-interner.html

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct Symbol(u32);

#[derive(Debug)]
pub struct Interner {
    map: HashMap<&'static str, Symbol>,
    vec: Vec<&'static str>,
    buf: String,
    full: Vec<String>,
}

impl Default for Interner {
    fn default() -> Self {
        Self::with_capacity(1024)
    }
}

impl Interner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            map: HashMap::with_capacity(cap),
            vec: Vec::with_capacity(cap),
            buf: String::with_capacity(cap),
            full: Vec::new(),
        }
    }

    pub fn intern(&mut self, s: &str) -> Symbol {
        if let Some(&sym) = self.map.get(s) {
            return sym;
        }

        // SAFETY: The returned reference points into `self.buf` or `self.full`,
        // which are never dropped or reallocated (only moved to `self.full`).
        // The 'static lifetime is sound because the Interner owns the storage
        // and the strings live as long as the Interner itself.
        let s = unsafe { self.alloc(s) };
        let sym = Symbol(u32::try_from(self.vec.len()).expect("too many symbols"));
        self.map.insert(s, sym);
        self.vec.push(s);
        sym
    }

    pub fn get(&self, s: &str) -> Option<Symbol> {
        self.map.get(s).copied()
    }

    pub fn resolve(&self, sym: Symbol) -> &str {
        self.vec[sym.0 as usize]
    }

    unsafe fn alloc(&mut self, s: &str) -> &'static str {
        let need = s.len();
        if self.buf.capacity() - self.buf.len() < need {
            let old_cap = self.buf.capacity();
            let new_cap = (old_cap + need).next_power_of_two();
            let old = std::mem::replace(&mut self.buf, String::with_capacity(new_cap));
            self.full.push(old);
        }

        let start = self.buf.len();
        self.buf.push_str(s);

        unsafe { &*(&self.buf[start..] as *const str) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_interning() {
        let mut interner = Interner::new();
        let x1 = interner.intern("Circle");
        let x2 = interner.intern("Circle");
        assert_eq!(x1, x2);
        assert_eq!(interner.resolve(x1), "Circle");
    }

    #[test]
    fn different_strings() {
        let mut interner = Interner::new();
        let circle = interner.intern("Circle");
        let rect = interner.intern("RectRound");
        assert_ne!(circle, rect);
        assert_eq!(interner.resolve(circle), "Circle");
        assert_eq!(interner.resolve(rect), "RectRound");
    }

    #[test]
    fn deduplication() {
        let mut interner = Interner::new();
        let s1 = interner.intern("repeated");
        let s2 = interner.intern("unique");
        let s3 = interner.intern("repeated");
        assert_eq!(s1, s3);
        assert_ne!(s1, s2);
    }
}
