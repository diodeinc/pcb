// String interner adapted from detain (https://github.com/akhilles/detain)
// Optimized for IPC-2581 XML parsing with common identifiers pre-cached

use phf::phf_map;
use std::collections::HashMap;
use std::hash::BuildHasherDefault;

type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<rustc_hash::FxHasher>>;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct Symbol(u32);

// Common IPC-2581 identifiers - edit here, sequential indices starting at 0
static COMMON: phf::Map<&'static str, u32> = phf_map! {
    // Common element names
    "Circle" => 0, "RectRound" => 1, "RectCenter" => 2, "RectCham" => 3,
    "RectCorner" => 4, "Oval" => 5, "Polygon" => 6, "Polyline" => 7,
    "PolyBegin" => 8, "PolyStepSegment" => 9, "PolyStepCurve" => 10,
    "Line" => 11, "Arc" => 12, "Contour" => 13, "Cutout" => 14,
    "Location" => 15, "Xform" => 16, "Color" => 17, "ColorRef" => 18,
    "LineDesc" => 19, "LineDescRef" => 20, "FillDesc" => 21, "FillDescRef" => 22,
    "StandardPrimitiveRef" => 23, "UserPrimitiveRef" => 24,
    "Component" => 25, "Package" => 26, "Pin" => 27, "Pad" => 28,
    "Layer" => 29, "Stackup" => 30, "Step" => 31, "Content" => 32,
    "LogisticHeader" => 33, "HistoryRecord" => 34, "Bom" => 35, "BomItem" => 36,
    "Ecad" => 37, "Avl" => 38, "AvlItem" => 39,
    "DictionaryColor" => 40, "DictionaryLineDesc" => 41, "DictionaryFillDesc" => 42,
    "DictionaryStandard" => 43, "DictionaryUser" => 44,
    "EntryColor" => 45, "EntryLineDesc" => 46, "EntryFillDesc" => 47,
    "EntryStandard" => 48, "EntryUser" => 49,
    "Butterfly" => 50, "Diamond" => 51, "Donut" => 52, "Ellipse" => 53,
    "Hexagon" => 54, "Moire" => 55, "Octagon" => 56, "Thermal" => 57, "Triangle" => 58,
    "UserSpecial" => 59, "Property" => 60, "PinRef" => 61,

    // Common attribute names
    "id" => 62, "name" => 63, "x" => 64, "y" => 65,
    "width" => 66, "height" => 67, "diameter" => 68, "radius" => 69,
    "rotation" => 70, "mirror" => 71, "scale" => 72,
    "startX" => 73, "startY" => 74, "endX" => 75, "endY" => 76,
    "centerX" => 77, "centerY" => 78, "clockwise" => 79,
    "lineWidth" => 80, "lineEnd" => 81, "lineProperty" => 82,
    "fillProperty" => 83, "r" => 84, "g" => 85, "b" => 86,
    "upperRight" => 87, "upperLeft" => 88, "lowerRight" => 89, "lowerLeft" => 90,
    "refDes" => 91, "packageRef" => 92, "part" => 93, "populate" => 94,
    "layerRef" => 95, "pin" => 96, "net" => 97,
    "revision" => 98, "units" => 99, "type" => 100, "mode" => 101,

    // Common enumerated values
    "ROUND" => 102, "SQUARE" => 103, "FLAT" => 104,
    "SOLID" => 105, "HOLLOW" => 106, "FILL" => 107, "VOID" => 108,
    "MILLIMETER" => 109, "INCH" => 110, "MICRON" => 111, "MILS" => 112,
    "ASSEMBLY" => 113, "FABRICATION" => 114, "STACKUP" => 115, "BOM" => 116,
    "true" => 117, "false" => 118,

    // Common layer names
    "F.Cu" => 119, "B.Cu" => 120, "F.Mask" => 121, "B.Mask" => 122,
    "F.Paste" => 123, "B.Paste" => 124, "F.Silkscreen" => 125, "B.Silkscreen" => 126,
    "In1.Cu" => 127, "In2.Cu" => 128,

    // Common values
    "C" => 129, "KICAD" => 130, "Owner" => 131,
    // To add more: "your_string" => 132,
};

#[derive(Debug)]
pub struct Interner {
    map: FxHashMap<&'static str, Symbol>,
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
        let mut interner = Self {
            map: FxHashMap::with_capacity_and_hasher(cap, BuildHasherDefault::default()),
            vec: Vec::with_capacity(COMMON.len() + cap),
            buf: String::with_capacity(cap),
            full: Vec::new(),
        };

        interner.vec.resize(COMMON.len(), "");
        for (k, &v) in COMMON.entries() {
            interner.vec[v as usize] = k;
        }

        interner
    }

    pub fn common_count() -> usize {
        COMMON.len()
    }

    pub fn intern(&mut self, s: &str) -> Symbol {
        if s.len() <= 16 {
            if let Some(&idx) = COMMON.get(s) {
                return Symbol(idx);
            }
        }

        if let Some(&sym) = self.map.get(s) {
            return sym;
        }

        let s = unsafe { self.alloc(s) };
        let sym = Symbol(u32::try_from(self.vec.len()).expect("too many symbols"));
        self.map.insert(s, sym);
        self.vec.push(s);
        sym
    }

    pub fn get(&self, s: &str) -> Option<Symbol> {
        if s.len() <= 16 {
            if let Some(&idx) = COMMON.get(s) {
                return Some(Symbol(idx));
            }
        }
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
    fn common_identifiers() {
        let mut interner = Interner::new();
        let circle = interner.intern("Circle");
        let rect = interner.intern("RectRound");
        assert_eq!(circle.0, 0);
        assert_eq!(rect.0, 1);
    }

    #[test]
    fn dynamic_strings() {
        let mut interner = Interner::new();
        let custom = interner.intern("my_custom_pad_id_12345");
        assert!(custom.0 >= Interner::common_count() as u32);
        assert_eq!(interner.resolve(custom), "my_custom_pad_id_12345");
    }

    #[test]
    fn common_values() {
        let interner = Interner::new();
        assert_eq!(interner.get("MILLIMETER"), Some(Symbol(109)));
        assert_eq!(interner.get("true"), Some(Symbol(117)));
        assert_eq!(interner.get("F.Cu"), Some(Symbol(119)));
    }
}
