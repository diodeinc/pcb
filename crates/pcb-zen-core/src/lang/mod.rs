pub mod component;
pub(crate) mod context;
pub mod eval;
pub(crate) mod evaluator_ext;
pub mod input;
pub(crate) mod interface;
pub mod module;
pub mod net;
pub mod type_info;

// Misc helpers (error/check)
pub(crate) mod assert;

// File system access
pub(crate) mod file;

// Add public error module and Result alias
pub mod error;
