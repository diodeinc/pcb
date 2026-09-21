use super::dictionary::*;
use super::{Tokens, from_token, token};
use crate::Symbol;

/// Content section of IPC-2581 file
#[derive(Debug, Clone)]
pub struct Content {
    pub role_ref: Symbol,
    pub function_mode: FunctionMode,
    pub step_refs: Vec<Symbol>,
    pub layer_refs: Vec<Symbol>,
    pub bom_refs: Vec<Symbol>,
    pub avl_refs: Vec<Symbol>,
    pub dictionary_color: DictionaryColor,
    pub dictionary_line_desc: DictionaryLineDesc,
    pub dictionary_fill_desc: DictionaryFillDesc,
    pub dictionary_font: DictionaryFont,
    pub dictionary_firmware: DictionaryFirmware,
    pub dictionary_standard: DictionaryStandard,
    pub dictionary_user: DictionaryUser,
}

/// Function mode describes the purpose of the IPC-2581 file
#[derive(Debug, Clone)]
pub struct FunctionMode {
    pub mode: Mode,
    pub level: Option<Level>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    UserDef,
    Bom,
    Stackup,
    Fabrication,
    Assembly,
    Test,
    Stencil,
    Dfx,
}

impl Mode {
    const TOKENS: Tokens<Self> = &[
        ("USERDEF", Self::UserDef),
        ("BOM", Self::Bom),
        ("STACKUP", Self::Stackup),
        ("FABRICATION", Self::Fabrication),
        ("ASSEMBLY", Self::Assembly),
        ("TEST", Self::Test),
        ("STENCIL", Self::Stencil),
        ("DFX", Self::Dfx),
    ];

    pub fn as_str(self) -> &'static str {
        token(Self::TOKENS, self)
    }

    pub fn from_ipc(token: &str) -> crate::Result<Self> {
        from_token(Self::TOKENS, "mode", token)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level(pub u8);
