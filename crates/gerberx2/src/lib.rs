pub mod from_artwork;
pub mod geometry;
mod parse;
pub mod types;
pub mod write;

pub use pcb_intern::{Interner, Symbol};
pub use types::*;
pub use write::{
    AttributeValue, GerberLayer, WriterAperture, WriterApertureMacro, WriterApertureTemplate,
    WriterMacroExpression, WriterMacroPrimitive, WriterObject, sanitize_attribute_field,
    write_layer,
};

use parse::Parser;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GerberError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Syntax error at byte {offset}: {message}")]
    Syntax { offset: usize, message: String },

    #[error("Invalid Gerber structure: {0}")]
    InvalidStructure(String),

    #[error("Invalid numeric value: {0}")]
    InvalidNumber(String),

    #[error("Render error: {0}")]
    Render(String),
}

pub type Result<T> = std::result::Result<T, GerberError>;

#[derive(Debug)]
pub struct GerberX2 {
    interner: Interner,
    commands: Vec<Command>,
    file_attributes: Vec<Attribute>,
    aperture_attributes: Vec<Attribute>,
    object_attributes: Vec<Attribute>,
    aperture_definitions: Vec<ApertureDefinition>,
    aperture_macros: Vec<ApertureMacro>,
    objects: Vec<GraphicalObject>,
    final_state: GraphicsState,
}

impl GerberX2 {
    pub fn parse(source: &str) -> Result<Self> {
        let mut parser = Parser::new(source);
        parser.parse()
    }

    pub fn parse_file(path: impl AsRef<Path>) -> Result<Self> {
        let source = std::fs::read_to_string(path)?;
        Self::parse(&source)
    }

    pub fn commands(&self) -> &[Command] {
        &self.commands
    }

    pub fn file_attributes(&self) -> &[Attribute] {
        &self.file_attributes
    }

    pub fn aperture_attributes(&self) -> &[Attribute] {
        &self.aperture_attributes
    }

    pub fn object_attributes(&self) -> &[Attribute] {
        &self.object_attributes
    }

    pub fn aperture_definitions(&self) -> &[ApertureDefinition] {
        &self.aperture_definitions
    }

    pub fn aperture_macros(&self) -> &[ApertureMacro] {
        &self.aperture_macros
    }

    pub fn objects(&self) -> &[GraphicalObject] {
        &self.objects
    }

    pub fn final_state(&self) -> &GraphicsState {
        &self.final_state
    }

    pub fn resolve(&self, sym: Symbol) -> &str {
        self.interner.resolve(sym)
    }

    pub fn interner(&self) -> &Interner {
        &self.interner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_flash_file() {
        let gerber = GerberX2::parse(
            "%FSLAX26Y26*%\n%MOMM*%\n%TF.FileFunction,Paste,Top*%\n%TA.AperFunction,Material*%\n%ADD10C,1.5*%\nD10*\nX0Y0D03*\nM02*\n",
        )
        .unwrap();

        assert_eq!(gerber.aperture_definitions().len(), 1);
        assert_eq!(gerber.file_attributes().len(), 1);
        assert_eq!(gerber.objects().len(), 1);
        assert!(matches!(gerber.commands().last(), Some(Command::EndOfFile)));
    }
}
