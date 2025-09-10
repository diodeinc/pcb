pub mod component;
pub(crate) mod context;
pub mod eval;
pub(crate) mod evaluator_ext;
pub mod input;
pub(crate) mod interface;
pub(crate) mod interface_validation;
pub mod module;
pub mod net;
pub mod spice_model;
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
