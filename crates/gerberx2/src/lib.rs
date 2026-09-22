pub mod from_artwork;
pub mod geometry;
mod parse;
pub mod types;
pub mod write;

pub use pcb_intern::{Interner, Symbol};
use pcb_ir::geom::{AccuracyError, Span};
pub use types::*;
pub use write::{
    AttributeSets, AttributeValue, GerberLayer, WriterAperture, WriterApertureTemplate,
    WriterObject, escape_attribute_field, trim_decimal, unescape_attribute_field, write_layer,
};

use parse::Parser;
#[cfg(not(target_family = "wasm"))]
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GerberError {
    #[error(transparent)]
    Accuracy(#[from] AccuracyError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Syntax error at byte {offset}: {message}")]
    Syntax { offset: usize, message: String },

    #[error("Invalid Gerber structure: {0}")]
    InvalidStructure(String),

    #[error("Invalid numeric value: {0}")]
    InvalidNumber(String),
}

pub type Result<T> = std::result::Result<T, GerberError>;

#[derive(Debug)]
pub struct GerberX2 {
    interner: Interner,
    file_attributes: Vec<Attribute>,
    /// Every attribute set an aperture or object refers to.
    attributes: Vec<Attribute>,
    aperture_definitions: Vec<ApertureDefinition>,
    objects: Vec<GraphicalObject>,
    step_repeats: Vec<StepRepeatBlock>,
}

impl GerberX2 {
    pub fn parse(source: &str) -> Result<Self> {
        let mut parser = Parser::new(source);
        parser.parse()
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn parse_file(path: impl AsRef<Path>) -> Result<Self> {
        let source = std::fs::read_to_string(path)?;
        Self::parse(&source)
    }

    pub fn file_attributes(&self) -> &[Attribute] {
        &self.file_attributes
    }

    /// The attribute set an aperture definition or object refers to.
    pub fn attributes(&self, set: Span) -> &[Attribute] {
        set.slice(&self.attributes)
    }

    pub fn aperture_definitions(&self) -> &[ApertureDefinition] {
        &self.aperture_definitions
    }

    /// The object stream in file order. A step-repeated run appears once;
    /// [`Self::step_repeats`] says where it repeats.
    pub fn objects(&self) -> &[GraphicalObject] {
        &self.objects
    }

    /// The step-repeated runs of [`Self::objects`], in stream order.
    pub fn step_repeats(&self) -> &[StepRepeatBlock] {
        &self.step_repeats
    }

    pub fn resolve(&self, sym: Symbol) -> &str {
        self.interner.resolve(sym)
    }
}
