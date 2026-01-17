//! Extract module signatures by evaluating .zen files directly.

use crate::types::{ModuleSignature, ParamDoc};
use pcb_zen_core::lang::type_info::TypeInfo;
use std::path::Path;

/// Result of trying to get a module signature.
/// If the file has no signature parameters, it's a library, not a module.
pub enum SignatureResult {
    /// File is a module with a signature (has config/io parameters)
    Module(ModuleSignature),
    /// File is a library (no signature or empty signature)
    Library,
    /// Failed to parse (error in file)
    Error(anyhow::Error),
}

/// Try to get module signature, returning whether file is a module or library.
/// A file is considered a module if:
/// - It has io() or config() parameters in its signature, OR
/// - It instantiates components/submodules (module_tree has more than just the root)
pub fn try_get_signature(file: &Path) -> SignatureResult {
    let cfg = pcb_zen::EvalConfig::default();

    let result = pcb_zen::eval(file, cfg);

    let Some(eval_output) = result.output else {
        let errors: Vec<String> = result
            .diagnostics
            .diagnostics
            .iter()
            .map(|d| d.to_string())
            .collect();
        return SignatureResult::Error(anyhow::anyhow!(
            "Evaluation failed for {}: {}",
            file.display(),
            errors.join("\n")
        ));
    };

    // A file is a library if it has no signature AND doesn't instantiate anything.
    // Check for:
    // - Submodule instances (module_tree has more than the root)
    // - Component instances in the root module
    let module_tree = eval_output.module_tree();
    let has_submodules = module_tree.len() > 1;
    let has_components = module_tree
        .values()
        .next()
        .map(|root| root.components().next().is_some())
        .unwrap_or(false);
    let has_instances = has_submodules || has_components;

    if eval_output.signature.is_empty() && !has_instances {
        return SignatureResult::Library;
    }

    let mut configs = Vec::new();
    let mut ios = Vec::new();

    for param in &eval_output.signature {
        let param_doc = ParamDoc {
            name: param.name.clone(),
            type_repr: format_type_info(&param.type_info),
            has_default: param.default_display.is_some(),
            default_repr: param
                .default_display
                .as_ref()
                .map(|s| format_default_display(s))
                .unwrap_or_default(),
            optional: !param.required,
        };

        if param.is_config() {
            configs.push(param_doc);
        } else {
            ios.push(param_doc);
        }
    }

    SignatureResult::Module(ModuleSignature { configs, ios })
}

/// Format a default value display string for documentation.
/// Strips wrapper like `enum("value")` to just `"value"`.
/// Also normalizes smart quotes to straight quotes.
fn format_default_display(s: &str) -> String {
    // Strip enum(...) wrapper: enum("value") -> "value"
    let s = if let Some(inner) = s.strip_prefix("enum(").and_then(|s| s.strip_suffix(')')) {
        inner.to_string()
    } else {
        s.to_string()
    };

    // Normalize smart quotes to straight quotes
    s.replace(['"', '"'], "\"")
}

/// Format a TypeInfo for display.
fn format_type_info(type_info: &TypeInfo) -> String {
    match type_info {
        TypeInfo::String => "str".to_string(),
        TypeInfo::Int => "int".to_string(),
        TypeInfo::Float => "float".to_string(),
        TypeInfo::Bool => "bool".to_string(),
        TypeInfo::Net => "Net".to_string(),
        TypeInfo::List { element } => format!("list[{}]", format_type_info(element)),
        TypeInfo::Dict { key, value } => {
            format!(
                "dict[{}, {}]",
                format_type_info(key),
                format_type_info(value)
            )
        }
        TypeInfo::Enum { variants, .. } => variants.join(" | "),
        TypeInfo::Record { name, .. } => name.clone(),
        TypeInfo::Interface { name, .. } => name.clone(),
        TypeInfo::Unknown { type_name } => type_name.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_type_info_enum() {
        let type_info = TypeInfo::Enum {
            name: "MyEnum".to_string(),
            variants: vec!["A".to_string(), "B".to_string()],
        };
        assert_eq!(format_type_info(&type_info), "A | B");
    }

    #[test]
    fn test_format_type_info_net() {
        let type_info = TypeInfo::Net;
        assert_eq!(format_type_info(&type_info), "Net");
    }
}
