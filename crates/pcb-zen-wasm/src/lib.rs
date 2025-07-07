use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use log::debug;
use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

use pcb_zen_core::{
    convert::ToSchematic, lang::type_info::TypeInfo, Diagnostic, InputMap, InputValue,
};
use starlark::errors::EvalSeverity;

mod bundle;

use bundle::LoadedBundle;
use pcb_zen_core::InMemoryFileProvider;

// Global module storage - stores the file providers and metadata for loaded modules
lazy_static::lazy_static! {
    static ref LOADED_MODULES: Arc<Mutex<HashMap<String, ModuleInfo>>> = Arc::new(Mutex::new(HashMap::new()));
    static ref MODULE_COUNTER: Arc<Mutex<u64>> = Arc::new(Mutex::new(0));
}

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    console_log::init_with_level(log::Level::Debug).expect("Failed to initialize console log");
    debug!("Initialized pcb-zen-wasm logger");
}

/// Information about a loaded module
struct ModuleInfo {
    file_provider: Arc<dyn pcb_zen_core::FileProvider>,
    load_resolver: Option<Arc<dyn pcb_zen_core::LoadResolver>>,
    main_file: PathBuf,
    module_name: String,
}

/// A module that can be introspected or evaluated
#[wasm_bindgen]
pub struct Module {
    id: String,
}

#[wasm_bindgen]
impl Module {
    /// Create a module from individual files
    #[wasm_bindgen(js_name = fromFiles)]
    pub fn from_files(
        files_json: &str,
        main_file: &str,
        module_name: &str,
    ) -> Result<Module, JsValue> {
        let files: HashMap<String, String> = serde_json::from_str(files_json)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse files JSON: {e}")))?;

        // Create InMemoryFileProvider with files
        let file_provider = Arc::new(InMemoryFileProvider::new(files));

        // Generate unique ID using counter
        let id = {
            let mut counter = MODULE_COUNTER.lock().unwrap();
            *counter += 1;
            format!("module_{}", *counter)
        };

        // Store the module info
        let module_info = ModuleInfo {
            file_provider: file_provider as Arc<dyn pcb_zen_core::FileProvider>,
            load_resolver: None,
            main_file: PathBuf::from(main_file),
            module_name: module_name.to_string(),
        };

        let mut modules = LOADED_MODULES.lock().unwrap();
        modules.insert(id.clone(), module_info);

        Ok(Module { id })
    }

    /// Create a module from a bundle zip file
    #[wasm_bindgen(js_name = fromBundle)]
    pub fn from_bundle(bundle_bytes: &[u8], module_name: &str) -> Result<Module, JsValue> {
        // Load the bundle
        let loaded_bundle = LoadedBundle::from_zip_bytes(bundle_bytes)
            .map_err(|e| JsValue::from_str(&format!("Failed to load bundle: {e}")))?;

        let main_file = loaded_bundle.entry_point().to_path_buf();
        let bundle_resolver = Arc::new(pcb_zen_core::bundle::BundleLoadResolver::new(
            loaded_bundle.bundle.clone(),
        ));

        // Generate unique ID using counter
        let id = {
            let mut counter = MODULE_COUNTER.lock().unwrap();
            *counter += 1;
            format!("module_{}", *counter)
        };

        // Store the module info
        let module_info = ModuleInfo {
            file_provider: loaded_bundle.file_provider.clone(),
            load_resolver: Some(bundle_resolver as Arc<dyn pcb_zen_core::LoadResolver>),
            main_file,
            module_name: module_name.to_string(),
        };

        let mut modules = LOADED_MODULES.lock().unwrap();
        modules.insert(id.clone(), module_info);

        Ok(Module { id })
    }

    /// Introspect the module to get its parameters
    #[wasm_bindgen]
    pub fn introspect(&self, partial_inputs_json: Option<String>) -> Result<String, JsValue> {
        let modules = LOADED_MODULES.lock().unwrap();
        let module_info = modules
            .get(&self.id)
            .ok_or_else(|| JsValue::from_str("Module not found"))?;

        // Create evaluation context
        let mut eval_ctx =
            pcb_zen_core::EvalContext::with_file_provider(module_info.file_provider.clone());
        if let Some(resolver) = &module_info.load_resolver {
            eval_ctx = eval_ctx.set_load_resolver(resolver.clone());
        }

        // Parse partial inputs if provided
        let mut input_map = InputMap::new();
        if let Some(json) = partial_inputs_json {
            let inputs: HashMap<String, serde_json::Value> = serde_json::from_str(&json)
                .map_err(|e| JsValue::from_str(&format!("Failed to parse inputs JSON: {e}")))?;

            // Convert JSON inputs to InputValue
            for (key, value) in inputs {
                if let Some(input_value) = json_to_input_value(&value) {
                    input_map.insert(key, input_value);
                }
            }
        }

        // Use the typed introspection API
        let introspection_result =
            eval_ctx.introspect_module_typed(&module_info.main_file, &module_info.module_name);

        let (parameters, diagnostics) = match introspection_result.output {
            Some(param_infos) => {
                let params: Vec<Parameter> = param_infos
                    .into_iter()
                    .map(|param_info| {
                        // Extract enum values if this is an enum type
                        let (is_enum, enum_values) = match &param_info.type_info {
                            TypeInfo::Enum { variants, .. } => (true, Some(variants.clone())),
                            _ => (false, None),
                        };

                        Parameter {
                            name: param_info.name.clone(),
                            param_type: format!("{:?}", param_info.type_info), // For backward compatibility
                            required: param_info.required,
                            is_config: param_info.is_config(),
                            is_enum,
                            enum_values,
                            type_info: param_info.type_info,
                        }
                    })
                    .collect();
                (Some(params), introspection_result.diagnostics)
            }
            None => (None, introspection_result.diagnostics),
        };

        let result = IntrospectionResult {
            success: parameters.is_some(),
            parameters,
            diagnostics: diagnostics
                .into_iter()
                .map(|d| diagnostic_to_json(&d))
                .collect(),
        };

        serde_json::to_string(&result)
            .map_err(|e| JsValue::from_str(&format!("Failed to serialize result: {e}")))
    }

    /// Evaluate the module with the given inputs
    #[wasm_bindgen]
    pub fn evaluate(&self, inputs_json: &str) -> Result<String, JsValue> {
        let modules = LOADED_MODULES.lock().unwrap();
        let module_info = modules
            .get(&self.id)
            .ok_or_else(|| JsValue::from_str("Module not found"))?;

        // Create evaluation context
        let mut eval_ctx =
            pcb_zen_core::EvalContext::with_file_provider(module_info.file_provider.clone());
        if let Some(resolver) = &module_info.load_resolver {
            eval_ctx = eval_ctx.set_load_resolver(resolver.clone());
        }

        // Parse inputs
        let inputs: HashMap<String, serde_json::Value> = serde_json::from_str(inputs_json)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse inputs JSON: {e}")))?;

        // Convert JSON inputs to InputMap
        let mut input_map = InputMap::new();
        for (key, value) in inputs {
            if let Some(input_value) = json_to_input_value(&value) {
                input_map.insert(key, input_value);
            }
        }

        // Evaluate the module
        let eval_result = eval_ctx
            .child_context()
            .set_source_path(module_info.main_file.clone())
            .set_module_name(module_info.module_name.clone())
            .set_inputs(input_map)
            .eval();

        // Convert diagnostics
        let diagnostics: Vec<DiagnosticInfo> = eval_result
            .diagnostics
            .into_iter()
            .map(|d| diagnostic_to_json(&d))
            .collect();

        if let Some(output) = eval_result.output {
            // Convert the frozen module to a schematic
            let schematic = output
                .sch_module
                .to_schematic()
                .map_err(|e| JsValue::from_str(&format!("Failed to convert to schematic: {e}")))?;

            // Serialize the schematic to JSON
            let schematic_json = serde_json::to_value(schematic)
                .map_err(|e| JsValue::from_str(&format!("Failed to serialize schematic: {e}")))?;

            let result = EvaluationResult {
                success: true,
                schematic: Some(schematic_json),
                diagnostics,
            };

            serde_json::to_string(&result)
                .map_err(|e| JsValue::from_str(&format!("Failed to serialize result: {e}")))
        } else {
            // Evaluation failed
            let result = EvaluationResult {
                success: false,
                schematic: None,
                diagnostics,
            };

            serde_json::to_string(&result)
                .map_err(|e| JsValue::from_str(&format!("Failed to serialize result: {e}")))
        }
    }

    #[wasm_bindgen(getter)]
    pub fn id(&self) -> String {
        self.id.clone()
    }

    /// Free the module from memory
    #[wasm_bindgen]
    pub fn free_module(&self) {
        let mut modules = LOADED_MODULES.lock().unwrap();
        modules.remove(&self.id);
    }
}

#[derive(Serialize, Deserialize)]
pub struct Parameter {
    name: String,
    param_type: String,
    required: bool,
    is_config: bool,                  // true for config params, false for io params
    is_enum: bool,                    // true if this is an enum type
    enum_values: Option<Vec<String>>, // possible enum values if available
    type_info: TypeInfo,              // Full structured type information
}

#[derive(Serialize, Deserialize)]
pub struct DiagnosticInfo {
    level: String,
    message: String,
    file: Option<String>,
    line: Option<u32>,
    child: Option<Box<DiagnosticInfo>>,
}

#[derive(Serialize, Deserialize)]
pub struct IntrospectionResult {
    success: bool,
    parameters: Option<Vec<Parameter>>,
    diagnostics: Vec<DiagnosticInfo>,
}

#[derive(Serialize, Deserialize)]
pub struct EvaluationResult {
    success: bool,
    schematic: Option<serde_json::Value>,
    diagnostics: Vec<DiagnosticInfo>,
}

// Helper functions

fn json_to_input_value(json: &serde_json::Value) -> Option<InputValue> {
    match json {
        serde_json::Value::Null => Some(InputValue::None),
        serde_json::Value::Bool(b) => Some(InputValue::Bool(*b)),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(InputValue::Int(i as i32))
            } else {
                n.as_f64().map(InputValue::Float)
            }
        }
        serde_json::Value::String(s) => Some(InputValue::String(s.clone())),
        serde_json::Value::Array(arr) => {
            let values: Option<Vec<_>> = arr.iter().map(json_to_input_value).collect();
            values.map(InputValue::List)
        }
        serde_json::Value::Object(obj) => {
            // Check if this is a special typed object
            if let Some(type_field) = obj.get("__type") {
                if let Some(type_str) = type_field.as_str() {
                    if type_str == "Net" {
                        if let Some(name) = obj.get("name").and_then(|v| v.as_str()) {
                            // For WASM, we'll use a simple counter for net IDs
                            // In production, this would need proper ID generation
                            return Some(InputValue::Net {
                                id: 1, // Placeholder ID
                                name: name.to_string(),
                                properties: starlark::collections::SmallMap::new(),
                            });
                        }
                    }
                }
            }

            // Regular dict
            let mut map = HashMap::new();
            for (k, v) in obj {
                if let Some(value) = json_to_input_value(v) {
                    map.insert(k.clone(), value);
                }
            }
            Some(InputValue::Dict(
                starlark::collections::SmallMap::from_iter(map),
            ))
        }
    }
}

fn diagnostic_to_json(diag: &Diagnostic) -> DiagnosticInfo {
    let level = match diag.severity {
        EvalSeverity::Error => "error",
        EvalSeverity::Warning => "warning",
        EvalSeverity::Advice => "info",
        EvalSeverity::Disabled => "info",
    }
    .to_string();

    DiagnosticInfo {
        level,
        message: diag.body.clone(),
        file: Some(diag.path.clone()),
        line: diag.span.as_ref().map(|s| s.begin.line as u32),
        child: diag.child.as_ref().map(|c| Box::new(diagnostic_to_json(c))),
    }
}
