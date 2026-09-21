pub mod avl;
pub mod bom;
pub mod content;
pub mod dictionary;
pub mod ecad;
pub mod metadata;
pub mod primitives;
pub mod transform;

#[allow(ambiguous_glob_reexports)]
pub use avl::*;
#[allow(ambiguous_glob_reexports)]
pub use bom::*;
#[allow(ambiguous_glob_reexports)]
pub use content::*;
#[allow(ambiguous_glob_reexports)]
pub use dictionary::*;
#[allow(ambiguous_glob_reexports)]
pub use ecad::*;
#[allow(ambiguous_glob_reexports)]
pub use metadata::*;
#[allow(ambiguous_glob_reexports)]
pub use primitives::*;
#[allow(ambiguous_glob_reexports)]
pub use transform::*;

/// A run of items in one of its owner's flat tables.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub count: u32,
}

impl Span {
    pub fn len(self) -> usize {
        self.count as usize
    }

    pub fn is_empty(self) -> bool {
        self.count == 0
    }

    pub fn slice<T>(self, items: &[T]) -> &[T] {
        &items[self.start as usize..][..self.count as usize]
    }
}

/// The IPC-2581 tokens of an enum: the one table its `as_str` and `from_ipc`
/// share. A variant with several spellings lists the one to write first.
pub(crate) type Tokens<T> = &'static [(&'static str, T)];

pub(crate) fn token<T: Copy + PartialEq>(tokens: Tokens<T>, value: T) -> &'static str {
    tokens
        .iter()
        .find_map(|(token, candidate)| (*candidate == value).then_some(*token))
        .expect("every variant has a token")
}

pub(crate) fn from_token<T: Copy>(tokens: Tokens<T>, attr: &str, token: &str) -> crate::Result<T> {
    tokens
        .iter()
        .find_map(|(candidate, value)| (*candidate == token).then_some(*value))
        .ok_or_else(|| crate::Ipc2581Error::InvalidAttribute(format!("Unknown {attr}: {token}")))
}
