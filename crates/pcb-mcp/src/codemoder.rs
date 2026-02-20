use anyhow::{Context, Result};
use rquickjs::{Context as JsContext, Function, Object, Runtime, Type, Value};
use serde_json::Value as JsonValue;
use std::sync::Arc;

use crate::{CallToolResult, ToolInfo};

/// Trait for calling MCP tools from JavaScript.
/// Implementations provide access to the tool registry.
pub trait ToolCaller: Send + Sync {
    fn call_tool(&self, name: &str, args: Option<JsonValue>) -> Result<CallToolResult>;
    fn tools(&self) -> Vec<ToolInfo>;
}

#[derive(Debug, Clone)]
pub struct ImageData {
    pub data: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, Default)]
pub struct ExecutionResult {
    pub value: JsonValue,
    pub logs: Vec<String>,
    pub images: Vec<ImageData>,
    pub is_error: bool,
    pub error_message: Option<String>,
}

pub struct JsRuntime {
    runtime: Runtime,
}

impl JsRuntime {
    pub fn new() -> Result<Self> {
        let runtime = Runtime::new()?;
        Ok(Self { runtime })
    }

    pub fn execute(&self, code: &str) -> Result<JsonValue> {
        let context = JsContext::full(&self.runtime)?;

        context.with(|ctx| {
            let result: Value = ctx.eval(code.as_bytes().to_vec())?;
            value_to_json(&result)
        })
    }

    pub fn execute_with_tools(
        &self,
        code: &str,
        caller: Arc<dyn ToolCaller>,
    ) -> Result<ExecutionResult> {
        let tools = caller.tools();
        let tool_names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();

        // Build metadata object for tools._meta
        let tool_meta: serde_json::Map<String, JsonValue> = tools
            .iter()
            .map(|t| {
                let mut meta = serde_json::Map::new();
                meta.insert(
                    "description".to_string(),
                    JsonValue::String(t.description.to_string()),
                );
                meta.insert("inputSchema".to_string(), t.input_schema.clone());
                if let Some(ref output) = t.output_schema {
                    meta.insert("outputSchema".to_string(), output.clone());
                }
                (t.name.to_string(), JsonValue::Object(meta))
            })
            .collect();
        let tool_meta_json = serde_json::to_string(&tool_meta).unwrap_or_else(|_| "{}".to_string());

        let logs: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let logs_clone = logs.clone();
        let images: Arc<std::sync::Mutex<Vec<ImageData>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let images_clone = images.clone();

        let context = JsContext::full(&self.runtime)?;

        context.with(move |ctx| {
            let globals = ctx.globals();

            // Set up console.log
            let console = Object::new(ctx.clone())?;
            let logs_for_closure = logs_clone.clone();
            let log_fn = Function::new(ctx.clone(), move |args: String| {
                if let Ok(mut logs) = logs_for_closure.lock() {
                    logs.push(args);
                }
            })?;
            console.set("log", log_fn)?;
            globals.set("console", console)?;

            // Set up __stringify helper for console.log to handle objects
            let stringify_setup = r#"
                var __original_console_log = console.log;
                console.log = function() {
                    var parts = [];
                    for (var i = 0; i < arguments.length; i++) {
                        var arg = arguments[i];
                        if (typeof arg === 'object') {
                            parts.push(JSON.stringify(arg));
                        } else {
                            parts.push(String(arg));
                        }
                    }
                    __original_console_log(parts.join(' '));
                };
            "#;
            let _: Value = ctx.eval(stringify_setup.as_bytes().to_vec())?;

            // Set up raw tool functions that take JSON string args and return JSON string
            let raw_tools = Object::new(ctx.clone())?;

            for tool_name in &tool_names {
                let name = tool_name.clone();
                let caller_clone = caller.clone();
                let images_for_closure = images_clone.clone();

                let func = Function::new(ctx.clone(), move |args: String| {
                    let tool_name = name.clone();
                    let caller = caller_clone.clone();

                    let args_value: Option<JsonValue> = serde_json::from_str(&args).ok();
                    let result = caller.call_tool(&tool_name, args_value);

                    match result {
                        Ok(call_result) => format_call_result(&call_result, &images_for_closure),
                        Err(e) => serde_json::json!({"error": e.to_string()}).to_string(),
                    }
                })?;

                raw_tools.set(tool_name.as_str(), func)?;
            }

            globals.set("__raw_tools", raw_tools)?;

            // Set up the `tools` object with JSON serialization/deserialization wrapper
            let tool_names_json =
                serde_json::to_string(&tool_names).unwrap_or_else(|_| "[]".to_string());
            let tool_wrapper_code = format!(
                r#"
                var tools = {{}};
                var __tool_names = {tool_names_json};
                var __tool_meta = {tool_meta_json};
                tools._meta = __tool_meta;
                for (var i = 0; i < __tool_names.length; i++) {{
                    (function(toolName) {{
                        // Create function accessible via bracket notation (exact name)
                        tools[toolName] = function(args) {{
                            var jsonArgs = JSON.stringify(args || {{}});
                            var resultStr = __raw_tools[toolName](jsonArgs);
                            var result;
                            try {{
                                result = JSON.parse(resultStr);
                            }} catch (e) {{
                                result = resultStr;
                            }}
                            if (result && typeof result === 'object' && result.error) {{
                                throw new Error('Tool ' + toolName + ' failed: ' + result.error);
                            }}
                            return result;
                        }};
                        // Also create underscore version for identifier access (e.g., tools.search_registry)
                        var safeName = toolName.replace(/-/g, '_');
                        if (safeName !== toolName) {{
                            tools[safeName] = tools[toolName];
                        }}
                    }})(__tool_names[i]);
                }}
            "#
            );
            let wrapper_result: Result<Value, _> = ctx.eval(tool_wrapper_code.as_bytes().to_vec());
            if let Err(e) = wrapper_result {
                return Err(anyhow::anyhow!("Tool wrapper setup failed: {e:?}"));
            }

            // Execute the user's code
            let code_result: Result<Value, _> = ctx.eval(code.as_bytes().to_vec());
            match code_result {
                Ok(result) => Ok((value_to_json(&result)?, None)),
                Err(_e) => {
                    let error_msg = if let Some(exc) = ctx.catch().as_exception() {
                        exc.message().unwrap_or_default().to_string()
                    } else {
                        "Unknown JavaScript error".to_string()
                    };
                    Ok((JsonValue::Null, Some(error_msg)))
                }
            }
        })
        .map(|(value, error)| {
            let captured_logs = logs.lock().map(|l| l.clone()).unwrap_or_default();
            let captured_images = images.lock().map(|i| i.clone()).unwrap_or_default();
            ExecutionResult {
                value,
                logs: captured_logs,
                images: captured_images,
                is_error: error.is_some(),
                error_message: error,
            }
        })
    }
}

fn format_call_result(
    result: &CallToolResult,
    images: &Arc<std::sync::Mutex<Vec<ImageData>>>,
) -> String {
    let mut image_indices = collect_image_indices(result, images).into_iter();

    // Prefer structured_content if available, while redacting image payloads.
    if let Some(mut structured) = result.structured_content.clone() {
        redact_and_tag_image_data(&mut structured, &mut image_indices);
        return serde_json::to_string(&structured).unwrap_or_else(|_| "null".to_string());
    }

    // Otherwise convert content blocks to JSON-friendly values.
    let contents: Vec<JsonValue> = result
        .content
        .iter()
        .map(|content| content_block_to_json(content, &mut image_indices))
        .collect();

    // If there's a single content block, unwrap it for convenience.
    if contents.len() == 1 {
        if let Some(s) = contents[0].as_str() {
            // Try to parse as JSON first
            if let Ok(parsed) = serde_json::from_str::<JsonValue>(s) {
                return serde_json::to_string(&parsed).unwrap_or_else(|_| s.to_string());
            }
            return s.to_string();
        }
        return serde_json::to_string(&contents[0]).unwrap_or_else(|_| "null".to_string());
    }

    serde_json::to_string(&contents).unwrap_or_else(|_| "[]".to_string())
}

fn collect_image_indices(
    result: &CallToolResult,
    images: &Arc<std::sync::Mutex<Vec<ImageData>>>,
) -> Vec<usize> {
    let Ok(mut collected) = images.lock() else {
        return Vec::new();
    };

    let mut indices = Vec::new();
    for content in &result.content {
        if let crate::CallToolResultContent::Image { data, mime_type } = content {
            collected.push(ImageData {
                data: data.clone(),
                mime_type: mime_type.clone(),
            });
            indices.push(collected.len().saturating_sub(1));
        }
    }
    indices
}

fn redact_and_tag_image_data(
    value: &mut JsonValue,
    image_indices: &mut dyn Iterator<Item = usize>,
) {
    match value {
        JsonValue::Array(items) => {
            for item in items {
                redact_and_tag_image_data(item, image_indices);
            }
        }
        JsonValue::Object(obj) => {
            tag_redacted_image_object(obj, image_indices);
            for child in obj.values_mut() {
                redact_and_tag_image_data(child, image_indices);
            }
        }
        _ => {}
    }
}

fn content_block_to_json(
    content: &crate::CallToolResultContent,
    image_indices: &mut dyn Iterator<Item = usize>,
) -> JsonValue {
    match content {
        crate::CallToolResultContent::Text { text } => JsonValue::String(text.clone()),
        crate::CallToolResultContent::Image { mime_type, .. } => {
            let image_index = image_indices.next().unwrap_or(0);
            serde_json::json!({
                "type": "image",
                "mimeType": mime_type,
                "imageIndex": image_index,
            })
        }
        crate::CallToolResultContent::ResourceLink {
            uri,
            name,
            description,
            mime_type,
            annotations,
        } => serde_json::json!({
            "type": "resource_link",
            "uri": uri,
            "name": name,
            "description": description,
            "mimeType": mime_type,
            "annotations": annotations,
        }),
    }
}

fn tag_redacted_image_object(
    obj: &mut serde_json::Map<String, JsonValue>,
    image_indices: &mut dyn Iterator<Item = usize>,
) {
    let is_image = obj
        .get("type")
        .and_then(|v| v.as_str())
        .map(|t| t == "image")
        .unwrap_or(false);
    if !is_image {
        return;
    }

    obj.remove("data");
    if !obj.contains_key("imageIndex") {
        if let Some(idx) = image_indices.next() {
            obj.insert("imageIndex".to_string(), JsonValue::from(idx as u64));
        }
    }
}

fn value_to_json(value: &Value) -> Result<JsonValue> {
    let type_of = value.type_of();

    match type_of {
        Type::Undefined | Type::Null => Ok(JsonValue::Null),
        Type::Bool => {
            let b = value.as_bool().unwrap_or(false);
            Ok(JsonValue::Bool(b))
        }
        Type::Int => {
            let i = value.as_int().unwrap_or(0);
            Ok(JsonValue::Number(i.into()))
        }
        Type::Float => {
            let f = value.as_float().unwrap_or(0.0);
            Ok(serde_json::json!(f))
        }
        Type::String => {
            let s = value
                .as_string()
                .context("Expected string")?
                .to_string()
                .context("Failed to convert JS string")?;
            Ok(JsonValue::String(s))
        }
        Type::Array => {
            let arr = value.as_array().context("Expected array")?;
            let items: Result<Vec<JsonValue>> = arr
                .iter()
                .map(|item| {
                    let item = item?;
                    value_to_json(&item)
                })
                .collect();
            Ok(JsonValue::Array(items?))
        }
        Type::Object => {
            let obj = value.as_object().context("Expected object")?;
            let mut map = serde_json::Map::new();
            for key in obj.keys::<String>() {
                let key = key?;
                let val: Value = obj.get(&key)?;
                map.insert(key, value_to_json(&val)?);
            }
            Ok(JsonValue::Object(map))
        }
        _ => Ok(JsonValue::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_js_execution() {
        let runtime = JsRuntime::new().unwrap();
        let result = runtime.execute("1 + 2").unwrap();
        assert_eq!(result, serde_json::json!(3));
    }

    #[test]
    fn test_js_object_return() {
        let runtime = JsRuntime::new().unwrap();
        let result = runtime.execute(r#"({ name: "test", value: 42 })"#).unwrap();
        assert_eq!(result["name"], "test");
        assert_eq!(result["value"], 42);
    }

    #[test]
    fn test_js_array_return() {
        let runtime = JsRuntime::new().unwrap();
        let result = runtime.execute("[1, 2, 3]").unwrap();
        assert_eq!(result, serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn test_js_string_return() {
        let runtime = JsRuntime::new().unwrap();
        let result = runtime.execute(r#""hello world""#).unwrap();
        assert_eq!(result, serde_json::json!("hello world"));
    }

    struct MockToolCaller {
        tools: Vec<ToolInfo>,
    }

    impl ToolCaller for MockToolCaller {
        fn call_tool(&self, name: &str, args: Option<JsonValue>) -> Result<CallToolResult> {
            match name {
                "add" => {
                    let a = args
                        .as_ref()
                        .and_then(|v| v.get("a"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let b = args
                        .as_ref()
                        .and_then(|v| v.get("b"))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    Ok(CallToolResult::json(&serde_json::json!({"result": a + b})))
                }
                "greet" => {
                    let name = args
                        .as_ref()
                        .and_then(|v| v.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("world");
                    Ok(CallToolResult::json(
                        &serde_json::json!({"message": format!("Hello, {}!", name)}),
                    ))
                }
                "render" => Ok(CallToolResult {
                    content: vec![
                        crate::CallToolResultContent::Image {
                            data: "AA==".to_string(),
                            mime_type: "image/png".to_string(),
                        },
                        crate::CallToolResultContent::Text {
                            text: "Rendered".to_string(),
                        },
                    ],
                    structured_content: None,
                    is_error: false,
                }),
                "structured" => Ok(CallToolResult {
                    content: vec![crate::CallToolResultContent::Text {
                        text: "ignored".to_string(),
                    }],
                    structured_content: Some(serde_json::json!({
                        "answer": 42
                    })),
                    is_error: false,
                }),
                "structured_with_image" => Ok(CallToolResult {
                    content: vec![crate::CallToolResultContent::Image {
                        data: "AA==".to_string(),
                        mime_type: "image/png".to_string(),
                    }],
                    structured_content: Some(serde_json::json!({
                        "preview": {
                            "type": "image",
                            "data": "AA==",
                            "mimeType": "image/png"
                        }
                    })),
                    is_error: false,
                }),
                "resource_link_only" => Ok(CallToolResult {
                    content: vec![crate::CallToolResultContent::ResourceLink {
                        uri: "file:///tmp/example.txt".to_string(),
                        name: Some("example".to_string()),
                        description: Some("example file".to_string()),
                        mime_type: Some("text/plain".to_string()),
                        annotations: None,
                    }],
                    structured_content: None,
                    is_error: false,
                }),
                _ => Err(anyhow::anyhow!("Unknown tool: {}", name)),
            }
        }

        fn tools(&self) -> Vec<ToolInfo> {
            self.tools.clone()
        }
    }

    #[test]
    fn test_execute_with_tools() {
        let caller = Arc::new(MockToolCaller {
            tools: vec![
                ToolInfo {
                    name: "add",
                    description: "Add two numbers",
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "a": {"type": "integer"},
                            "b": {"type": "integer"}
                        }
                    }),
                    output_schema: None,
                },
                ToolInfo {
                    name: "greet",
                    description: "Greet someone",
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "name": {"type": "string"}
                        }
                    }),
                    output_schema: None,
                },
            ],
        });

        let runtime = JsRuntime::new().unwrap();

        // Test calling a tool
        let result = runtime
            .execute_with_tools("tools.add({a: 2, b: 3})", caller.clone())
            .unwrap();
        assert!(!result.is_error, "Error: {:?}", result.error_message);
        assert_eq!(
            result.value["result"].as_f64().map(|f| f as i64),
            Some(5),
            "Got: {:?}",
            result.value
        );

        // Test calling multiple tools
        let result = runtime
            .execute_with_tools(
                r#"
                var sum = tools.add({a: 10, b: 20});
                var greeting = tools.greet({name: "Alice"});
                ({sum: sum.result, greeting: greeting.message})
            "#,
                caller.clone(),
            )
            .unwrap();
        assert!(!result.is_error, "Error: {:?}", result.error_message);
        assert_eq!(
            result.value["sum"].as_f64().map(|f| f as i64),
            Some(30),
            "Got: {:?}",
            result.value
        );
        assert_eq!(result.value["greeting"], "Hello, Alice!");
    }

    #[test]
    fn test_image_content_preserved() {
        let caller = Arc::new(MockToolCaller {
            tools: vec![ToolInfo {
                name: "render",
                description: "Render a PNG image",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
                output_schema: None,
            }],
        });

        let runtime = JsRuntime::new().unwrap();
        let result = runtime
            .execute_with_tools("tools.render({})", caller)
            .unwrap();

        assert!(!result.is_error, "Error: {:?}", result.error_message);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].mime_type, "image/png");
        assert_eq!(result.value[0]["type"], "image");
        assert_eq!(result.value[0]["mimeType"], "image/png");
    }

    #[test]
    fn test_console_log_capture() {
        let caller = Arc::new(MockToolCaller { tools: vec![] });

        let runtime = JsRuntime::new().unwrap();
        let result = runtime
            .execute_with_tools(
                r#"
                console.log("hello");
                console.log("world", 42);
                console.log({foo: "bar"});
                "done"
            "#,
                caller,
            )
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(result.value, "done");
        assert_eq!(result.logs.len(), 3);
        assert_eq!(result.logs[0], "hello");
        assert_eq!(result.logs[1], "world 42");
        assert!(result.logs[2].contains("foo"));
    }

    #[test]
    fn test_structured_content_preserved() {
        let caller = Arc::new(MockToolCaller {
            tools: vec![ToolInfo {
                name: "structured",
                description: "Return structured content",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
                output_schema: None,
            }],
        });

        let runtime = JsRuntime::new().unwrap();
        let result = runtime
            .execute_with_tools("tools.structured({})", caller)
            .unwrap();

        assert!(!result.is_error, "Error: {:?}", result.error_message);
        assert_eq!(result.value["answer"].as_f64(), Some(42.0));
    }

    #[test]
    fn test_resource_link_preserved() {
        let caller = Arc::new(MockToolCaller {
            tools: vec![ToolInfo {
                name: "resource_link_only",
                description: "Return resource link content",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
                output_schema: None,
            }],
        });

        let runtime = JsRuntime::new().unwrap();
        let result = runtime
            .execute_with_tools("tools.resource_link_only({})", caller)
            .unwrap();

        assert!(!result.is_error, "Error: {:?}", result.error_message);
        assert_eq!(result.value["type"], "resource_link");
        assert_eq!(result.value["uri"], "file:///tmp/example.txt");
    }

    #[test]
    fn test_structured_content_image_tagged() {
        let caller = Arc::new(MockToolCaller {
            tools: vec![ToolInfo {
                name: "structured_with_image",
                description: "Return structured content containing an image object",
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {}
                }),
                output_schema: None,
            }],
        });

        let runtime = JsRuntime::new().unwrap();
        let result = runtime
            .execute_with_tools("tools.structured_with_image({})", caller)
            .unwrap();

        assert!(!result.is_error, "Error: {:?}", result.error_message);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.value["preview"]["type"], "image");
        assert_eq!(result.value["preview"]["mimeType"], "image/png");
        assert_eq!(result.value["preview"]["imageIndex"].as_f64(), Some(0.0));
        assert!(result.value["preview"].get("data").is_none());
    }

    #[test]
    fn test_js_error_handling() {
        let caller = Arc::new(MockToolCaller { tools: vec![] });

        let runtime = JsRuntime::new().unwrap();
        let result = runtime
            .execute_with_tools("throw new Error('test error')", caller)
            .unwrap();

        assert!(result.is_error);
        assert!(result.error_message.unwrap().contains("test error"));
    }
}
