//! Data types for stdlib documentation.

/// Documentation for a .zen file (either a library or module).
#[derive(Debug, Clone)]
pub enum FileDoc {
    /// Library file with exported functions, types, constants
    Library(LibraryDoc),
    /// Module file (instantiable component/subcircuit)
    Module(ModuleDoc),
}

impl FileDoc {
    pub fn path(&self) -> &str {
        match self {
            FileDoc::Library(l) => &l.path,
            FileDoc::Module(m) => &m.path,
        }
    }
}

/// Documentation for a library file (functions, types, constants).
#[derive(Debug, Clone)]
pub struct LibraryDoc {
    pub path: String,
    pub file_doc: Option<DocString>,
    pub functions: Vec<FunctionDoc>,
    pub types: Vec<TypeDoc>,
    pub constants: Vec<ConstDoc>,
}

/// Documentation for a module file (instantiable component).
#[derive(Debug, Clone)]
pub struct ModuleDoc {
    pub path: String,
    pub file_doc: Option<DocString>,
    pub signature: ModuleSignature,
}

/// A docstring with summary and description.
#[derive(Debug, Clone)]
pub struct DocString {
    pub summary: String,
    pub description: String,
}

/// Documentation for a function.
#[derive(Debug, Clone)]
pub struct FunctionDoc {
    pub name: String,
    pub signature: String,
    pub doc: Option<DocString>,
}

/// Documentation for a type (enum, interface, etc).
#[derive(Debug, Clone)]
pub struct TypeDoc {
    pub name: String,
    pub kind: String,
}

/// Documentation for a constant.
#[derive(Debug, Clone)]
pub struct ConstDoc {
    pub name: String,
}

/// Module signature extracted from evaluation.
#[derive(Debug, Clone, Default)]
pub struct ModuleSignature {
    pub configs: Vec<ParamDoc>,
    pub ios: Vec<ParamDoc>,
}

/// Documentation for a module parameter (config or io).
#[derive(Debug, Clone)]
pub struct ParamDoc {
    pub name: String,
    pub type_repr: String,
    pub has_default: bool,
    pub default_repr: String,
    pub optional: bool,
}
