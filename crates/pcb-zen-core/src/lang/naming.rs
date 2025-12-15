//! Naming convention checks for code style diagnostics.
//!
//! This module provides utilities to check naming conventions for:
//! - `io()` parameters: should be UPPERCASE (e.g., `VCC`, `GND`, `IN1`)
//! - `config()` parameters: should be snake_case (e.g., `enable_debug`, `num_channels`)
//! - `Net()` explicit names: should be UPPERCASE (e.g., `Net("VCC")`)

use crate::lang::error::CategorizedDiagnostic;
use crate::Diagnostic;
use starlark::codemap::ResolvedSpan;
use starlark::errors::EvalSeverity;
use std::path::Path;

/// Diagnostic category for io() naming conventions
pub const STYLE_NAMING_IO: &str = "style.naming.io";

/// Diagnostic category for config() naming conventions
pub const STYLE_NAMING_CONFIG: &str = "style.naming.config";

/// Diagnostic category for Net() naming conventions
pub const STYLE_NAMING_NET: &str = "style.naming.net";

/// Check if a name follows UPPERCASE convention.
///
/// UPPERCASE names:
/// - Consist only of uppercase letters, digits, and underscores
/// - Must start with a letter
/// - Cannot be empty
///
/// Examples: `VCC`, `GND`, `IN1`, `DATA_OUT`, `CLK_100MHZ`
pub fn is_uppercase(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    let mut chars = name.chars();

    // First character must be an uppercase letter
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }

    // Rest must be uppercase letters, digits, or underscores
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Check if a name follows snake_case convention.
///
/// snake_case names:
/// - Consist only of lowercase letters, digits, and underscores
/// - Must start with a lowercase letter
/// - Cannot have consecutive underscores
/// - Cannot end with an underscore
///
/// Examples: `enable_debug`, `num_channels`, `output_voltage`
pub fn is_snake_case(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }

    let mut chars = name.chars().peekable();

    // First character must be a lowercase letter
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }

    let mut prev_was_underscore = false;

    while let Some(c) = chars.next() {
        if c == '_' {
            // No consecutive underscores
            if prev_was_underscore {
                return false;
            }
            // No trailing underscore
            if chars.peek().is_none() {
                return false;
            }
            prev_was_underscore = true;
        } else if c.is_ascii_lowercase() || c.is_ascii_digit() {
            prev_was_underscore = false;
        } else {
            return false;
        }
    }

    true
}

/// Convert a name to UPPERCASE convention.
pub fn to_uppercase(name: &str) -> String {
    name.to_ascii_uppercase()
}

/// Convert a name to snake_case convention.
pub fn to_snake_case(name: &str) -> String {
    let mut result = String::with_capacity(name.len() + 4);
    let mut prev_was_lowercase = false;

    for c in name.chars() {
        if c.is_ascii_uppercase() {
            if prev_was_lowercase && !result.is_empty() {
                result.push('_');
            }
            result.push(c.to_ascii_lowercase());
            prev_was_lowercase = false;
        } else if c == '-' || c == ' ' {
            if !result.ends_with('_') && !result.is_empty() {
                result.push('_');
            }
            prev_was_lowercase = false;
        } else {
            result.push(c.to_ascii_lowercase());
            prev_was_lowercase = c.is_ascii_lowercase();
        }
    }

    // Clean up any trailing or consecutive underscores
    let mut cleaned = String::with_capacity(result.len());
    let mut prev_was_underscore = false;
    for c in result.chars() {
        if c == '_' {
            if !prev_was_underscore && !cleaned.is_empty() {
                cleaned.push(c);
            }
            prev_was_underscore = true;
        } else {
            cleaned.push(c);
            prev_was_underscore = false;
        }
    }

    // Remove trailing underscore
    if cleaned.ends_with('_') {
        cleaned.pop();
    }

    cleaned
}

/// Check io() parameter naming and return a diagnostic if it doesn't follow UPPERCASE convention.
pub fn check_io_naming(name: &str, span: Option<ResolvedSpan>, path: &Path) -> Option<Diagnostic> {
    if is_uppercase(name) {
        return None;
    }

    let suggested = to_uppercase(name);
    let message = format!(
        "io() parameter '{}' should be UPPERCASE: '{}'",
        name, suggested
    );

    Some(create_style_diagnostic(
        message,
        STYLE_NAMING_IO,
        span,
        path,
    ))
}

/// Check config() parameter naming and return a diagnostic if it doesn't follow snake_case convention.
pub fn check_config_naming(
    name: &str,
    span: Option<ResolvedSpan>,
    path: &Path,
) -> Option<Diagnostic> {
    if is_snake_case(name) {
        return None;
    }

    let suggested = to_snake_case(name);
    let message = format!(
        "config() parameter '{}' should be snake_case: '{}'",
        name, suggested
    );

    Some(create_style_diagnostic(
        message,
        STYLE_NAMING_CONFIG,
        span,
        path,
    ))
}

/// Check Net() explicit name and return a diagnostic if it doesn't follow UPPERCASE convention.
pub fn check_net_naming(name: &str, span: Option<ResolvedSpan>, path: &Path) -> Option<Diagnostic> {
    // Skip auto-generated names (starting with underscore or N followed by digits)
    if name.starts_with('_')
        || name.starts_with('N') && name[1..].chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }

    if is_uppercase(name) {
        return None;
    }

    let suggested = to_uppercase(name);
    let message = format!("Net name '{}' should be UPPERCASE: '{}'", name, suggested);

    Some(create_style_diagnostic(
        message,
        STYLE_NAMING_NET,
        span,
        path,
    ))
}

/// Create a style diagnostic with the given message and category.
fn create_style_diagnostic(
    message: String,
    kind: &str,
    span: Option<ResolvedSpan>,
    path: &Path,
) -> Diagnostic {
    let categorized = CategorizedDiagnostic::new(message.clone(), kind.to_string())
        .expect("style diagnostic kind should be valid");

    Diagnostic::new(message, EvalSeverity::Advice, path)
        .with_span(span)
        .with_source_error(Some(categorized))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_uppercase() {
        // Valid UPPERCASE names
        assert!(is_uppercase("VCC"));
        assert!(is_uppercase("GND"));
        assert!(is_uppercase("IN1"));
        assert!(is_uppercase("DATA_OUT"));
        assert!(is_uppercase("CLK_100MHZ"));
        assert!(is_uppercase("A"));
        assert!(is_uppercase("A1"));
        assert!(is_uppercase("A_B_C"));

        // Invalid names
        assert!(!is_uppercase(""));
        assert!(!is_uppercase("vcc")); // lowercase
        assert!(!is_uppercase("Vcc")); // mixed case
        assert!(!is_uppercase("1VCC")); // starts with digit
        assert!(!is_uppercase("_VCC")); // starts with underscore
        assert!(!is_uppercase("VCC-GND")); // contains hyphen
        assert!(!is_uppercase("VCC GND")); // contains space
    }

    #[test]
    fn test_is_snake_case() {
        // Valid snake_case names
        assert!(is_snake_case("enable_debug"));
        assert!(is_snake_case("num_channels"));
        assert!(is_snake_case("output_voltage"));
        assert!(is_snake_case("a"));
        assert!(is_snake_case("a1"));
        assert!(is_snake_case("a_b_c"));
        assert!(is_snake_case("resistor1"));

        // Invalid names
        assert!(!is_snake_case(""));
        assert!(!is_snake_case("VCC")); // uppercase
        assert!(!is_snake_case("enableDebug")); // camelCase
        assert!(!is_snake_case("Enable_Debug")); // mixed case
        assert!(!is_snake_case("1enable")); // starts with digit
        assert!(!is_snake_case("_enable")); // starts with underscore
        assert!(!is_snake_case("enable_")); // ends with underscore
        assert!(!is_snake_case("enable__debug")); // consecutive underscores
    }

    #[test]
    fn test_to_uppercase() {
        assert_eq!(to_uppercase("vcc"), "VCC");
        assert_eq!(to_uppercase("Vcc"), "VCC");
        assert_eq!(to_uppercase("data_out"), "DATA_OUT");
        assert_eq!(to_uppercase("clk100mhz"), "CLK100MHZ");
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("enableDebug"), "enable_debug");
        assert_eq!(to_snake_case("EnableDebug"), "enable_debug");
        assert_eq!(to_snake_case("ENABLE_DEBUG"), "enable_debug");
        assert_eq!(to_snake_case("numChannels"), "num_channels");
        assert_eq!(to_snake_case("enable-debug"), "enable_debug");
        assert_eq!(to_snake_case("enable debug"), "enable_debug");
    }

    #[test]
    fn test_check_io_naming() {
        let path = Path::new("test.zen");

        // Valid names should return None
        assert!(check_io_naming("VCC", None, path).is_none());
        assert!(check_io_naming("GND", None, path).is_none());
        assert!(check_io_naming("DATA_OUT", None, path).is_none());

        // Invalid names should return a diagnostic
        let diag = check_io_naming("vcc", None, path);
        assert!(diag.is_some());
        let diag = diag.unwrap();
        assert!(diag.body.contains("should be UPPERCASE"));
        assert!(diag.body.contains("'VCC'"));
    }

    #[test]
    fn test_check_config_naming() {
        let path = Path::new("test.zen");

        // Valid names should return None
        assert!(check_config_naming("enable_debug", None, path).is_none());
        assert!(check_config_naming("num_channels", None, path).is_none());

        // Invalid names should return a diagnostic
        let diag = check_config_naming("enableDebug", None, path);
        assert!(diag.is_some());
        let diag = diag.unwrap();
        assert!(diag.body.contains("should be snake_case"));
        assert!(diag.body.contains("'enable_debug'"));
    }

    #[test]
    fn test_check_net_naming() {
        let path = Path::new("test.zen");

        // Valid names should return None
        assert!(check_net_naming("VCC", None, path).is_none());
        assert!(check_net_naming("GND", None, path).is_none());

        // Auto-generated names should be skipped
        assert!(check_net_naming("_vcc", None, path).is_none());
        assert!(check_net_naming("N123", None, path).is_none());

        // Invalid names should return a diagnostic
        let diag = check_net_naming("vcc", None, path);
        assert!(diag.is_some());
        let diag = diag.unwrap();
        assert!(diag.body.contains("should be UPPERCASE"));
        assert!(diag.body.contains("'VCC'"));
    }
}
