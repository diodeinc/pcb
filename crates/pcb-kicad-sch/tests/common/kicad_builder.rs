use std::collections::BTreeMap;

use pcb_kicad_sch::{
    Junction, Label, LabelKind, LabelShape, PinInstance, Point, SchDocument, SchItem, SchPage,
    Sheet, SheetPin, Symbol, SymbolDefinition, SymbolField, Wire,
};

/// Compact semantic builder for connectivity tests.
///
/// Tests describe only electrically relevant geometry. Parser round-trip tests
/// remain responsible for validating the KiCad S-expression representation.
pub struct KicadBuilder {
    document: SchDocument,
    page: usize,
    next_id: usize,
}

impl KicadBuilder {
    pub fn new() -> Self {
        Self {
            document: SchDocument {
                pages: vec![page("root", "root.kicad_sch")],
                root_page_ids: vec!["root".to_string()],
            },
            page: 0,
            next_id: 0,
        }
    }

    pub fn add_page(&mut self, id: &str, file_name: &str) -> &mut Self {
        self.add_page_with_root(id, file_name, false)
    }

    pub fn add_root_page(&mut self, id: &str, file_name: &str) -> &mut Self {
        self.add_page_with_root(id, file_name, true)
    }

    fn add_page_with_root(&mut self, id: &str, file_name: &str, is_root: bool) -> &mut Self {
        let mut page = page(id, file_name);
        page.library = self.document.pages[self.page].library.clone();
        self.document.pages.push(page);
        if is_root {
            self.document.root_page_ids.push(id.to_string());
        }
        self.page = self.document.pages.len() - 1;
        self
    }

    pub fn select_page(&mut self, file_name: &str) -> &mut Self {
        self.page = self
            .document
            .pages
            .iter()
            .position(|page| page.file_name.as_deref() == Some(file_name))
            .unwrap_or_else(|| panic!("unknown test page {file_name}"));
        self
    }

    pub fn wire(&mut self, a: (f64, f64), b: (f64, f64)) -> &mut Self {
        let id = self.id("wire");
        self.push(SchItem::Wire(Wire {
            id,
            a: point(a),
            b: point(b),
            unsupported: Vec::new(),
        }));
        self
    }

    pub fn junction(&mut self, at: (f64, f64)) -> &mut Self {
        let id = self.id("junction");
        self.push(SchItem::Junction(Junction {
            id,
            at: point(at),
            unsupported: Vec::new(),
        }));
        self
    }

    pub fn local_label(&mut self, name: &str, at: (f64, f64)) -> &mut Self {
        self.label(name, at, LabelKind::Local)
    }

    pub fn global_label(&mut self, name: &str, at: (f64, f64)) -> &mut Self {
        self.label(
            name,
            at,
            LabelKind::Global {
                shape: LabelShape::Bidirectional,
            },
        )
    }

    pub fn hierarchical_label(&mut self, name: &str, at: (f64, f64)) -> &mut Self {
        self.label(
            name,
            at,
            LabelKind::Hierarchical {
                shape: LabelShape::Bidirectional,
            },
        )
    }

    pub fn directive_label(&mut self, at: (f64, f64)) -> &mut Self {
        self.label(
            "",
            at,
            LabelKind::Directive {
                shape: LabelShape::Round,
            },
        )
    }

    pub fn sheet(&mut self, file_name: &str, pins: &[(&str, (f64, f64))]) -> &mut Self {
        let id = self.id("sheet");
        let pins = pins
            .iter()
            .map(|(name, at)| SheetPin {
                id: self.id("sheet-pin"),
                name: (*name).to_string(),
                at: point(*at),
                rotation: Default::default(),
                shape: LabelShape::Bidirectional,
                unsupported: Vec::new(),
            })
            .collect();
        self.push(SchItem::Sheet(Box::new(Sheet {
            id,
            placed: true,
            at: None,
            size: None,
            name: None,
            file: SymbolField::new("Sheetfile", file_name, Point::default()),
            pins,
            unsupported: Vec::new(),
        })));
        self
    }

    pub fn define_symbol(&mut self, lib_id: &str, pins: &[TestPin<'_>]) -> &mut Self {
        let pin_text = pins
            .iter()
            .map(|pin| {
                format!(
                    "(pin {} line (at {} {} 0) (length 0) {} (name \"{}\") (number \"{}\"))",
                    pin.electrical_type,
                    pin.at.0,
                    pin.at.1,
                    if pin.hidden { "hide" } else { "" },
                    pin.name,
                    pin.number,
                )
            })
            .collect::<Vec<_>>()
            .join(" ");
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(&format!(
            "(symbol \"{lib_id}\" (symbol \"Test_1_1\" {pin_text}))"
        ))
        .expect("valid test symbol definition");
        self.document.pages[self.page]
            .library
            .definitions
            .insert(lib_id.to_string(), definition);
        self
    }

    pub fn define_symbol_raw(&mut self, sexpr: &str) -> &mut Self {
        let definition = SymbolDefinition::from_kicad_symbol_sexpr(sexpr)
            .expect("valid raw test symbol definition");
        self.document.pages[self.page]
            .library
            .definitions
            .insert(definition.lib_id.clone(), definition);
        self
    }

    pub fn component(&mut self, lib_id: &str, path: Option<&str>, at: (f64, f64)) -> &mut Self {
        self.placed_symbol(lib_id, path, None, at)
    }

    pub fn placed_symbol(
        &mut self,
        lib_id: &str,
        path: Option<&str>,
        value: Option<&str>,
        at: (f64, f64),
    ) -> &mut Self {
        let at = point(at);
        let mut fields = BTreeMap::new();
        if let Some(path) = path {
            fields.insert(
                "Path".to_string(),
                SymbolField::new("Path", path, at).with_hidden(true),
            );
        }
        if let Some(value) = value {
            fields.insert("Value".to_string(), SymbolField::new("Value", value, at));
        }
        let id = self.id("symbol");
        self.push(SchItem::Symbol(Symbol {
            id,
            lib_id: lib_id.to_string(),
            unit: 1,
            body_style: 1,
            at,
            rotation: Default::default(),
            mirror: None,
            dnp: false,
            in_bom: true,
            on_board: true,
            in_pos_files: true,
            fields_autoplaced: false,
            fields,
            pins: Vec::new(),
            unsupported: Vec::new(),
        }));
        self
    }

    pub fn pin_alternate(&mut self, number: &str, alternate: &str) -> &mut Self {
        let id = self.id("pin");
        let Some(SchItem::Symbol(symbol)) = self.document.pages[self.page].items.last_mut() else {
            panic!("pin_alternate requires a preceding symbol");
        };
        symbol.pins.push(PinInstance {
            number: number.to_string(),
            id,
            alternate: Some(alternate.to_string()),
            unsupported: Vec::new(),
        });
        self
    }

    pub fn build(self) -> SchDocument {
        self.document
    }

    fn label(&mut self, name: &str, at: (f64, f64), kind: LabelKind) -> &mut Self {
        let id = self.id("label");
        let mut label = Label::new(id, name, point(at));
        label.kind = kind;
        self.push(SchItem::Label(label));
        self
    }

    fn push(&mut self, item: SchItem) {
        self.document.pages[self.page].items.push(item);
    }

    fn id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }
}

pub struct TestPin<'a> {
    pub number: &'a str,
    pub name: &'a str,
    pub at: (f64, f64),
    pub electrical_type: &'a str,
    pub hidden: bool,
}

impl<'a> TestPin<'a> {
    pub fn passive(number: &'a str, at: (f64, f64)) -> Self {
        Self {
            number,
            name: number,
            at,
            electrical_type: "passive",
            hidden: false,
        }
    }
}

fn page(id: &str, file_name: &str) -> SchPage {
    let mut page = SchPage::new(id);
    page.file_name = Some(file_name.to_string());
    page
}

fn point((x, y): (f64, f64)) -> Point {
    Point::new(x, y)
}
