/// An enum of IPC-2581 tokens. `as_str` writes a variant's first spelling,
/// `from_ipc` reads any of them and names the attribute in its error.
macro_rules! ipc_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident($attr:literal) {
            $($variant:ident = $token:literal $(| $alias:literal)*),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $token),+
                }
            }

            pub fn from_ipc(token: &str) -> crate::Result<Self> {
                match token {
                    $($token $(| $alias)* => Ok(Self::$variant),)+
                    _ => Err(crate::Ipc2581Error::InvalidAttribute(format!(
                        "Unknown {}: {token}",
                        $attr
                    ))),
                }
            }
        }
    };
}

pub mod avl;
pub mod bom;
pub mod content;
pub mod dictionary;
pub mod ecad;
pub mod metadata;
pub mod primitives;
pub mod transform;

pub use avl::*;
pub use bom::*;
pub use content::*;
pub use dictionary::*;
pub use ecad::*;
pub use metadata::*;
pub use primitives::*;
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
