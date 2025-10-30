pub mod builtin;
pub mod component;
pub mod context;
pub mod electrical_check;
pub mod r#enum;
pub mod eval;
pub(crate) mod evaluator_ext;
pub(crate) mod interface;
pub mod module;
pub mod net;
pub mod spice_model;
pub mod stackup;
pub mod symbol;
pub mod test_bench;
pub mod type_info;

// Misc helpers (error/check)
pub(crate) mod assert;

// File system access
pub(crate) mod file;

// Add public error module and Result alias
pub mod error;

// Validation utilities
pub(crate) mod validation;
