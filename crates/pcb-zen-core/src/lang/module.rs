#![allow(clippy::needless_lifetimes)]

use std::collections::HashSet;
use std::path::Path;

use allocative::Allocative;
use log::error;
use starlark::environment::FrozenModule;
use starlark::values::enumeration::{EnumType, EnumValue, FrozenEnumType};
use starlark::values::record::{FrozenRecordType, RecordType};
use starlark::values::typing::{TypeCompiled, TypeType};
use starlark::values::{Heap, UnpackValue, ValueLifetimeless};
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::GlobalsBuilder,
    eval::{Arguments, Evaluator},
    starlark_complex_value, starlark_module, starlark_simple_value,
    values::{
        float::StarlarkFloat, list::ListRef, starlark_value, Coerce, Freeze, FreezeResult,
        NoSerialize, StarlarkValue, Trace, Value, ValueLike,
    },
};

use crate::lang::context::ContextValue;
use crate::lang::eval::EvalContext;
use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::input::{convert_from_starlark, InputMap};
use crate::Diagnostic;
use starlark::values::dict::DictRef;

use super::net::{generate_net_id, NetValue};
use crate::lang::context::FrozenContextValue;
use starlark::errors::{EvalMessage, EvalSeverity};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("Input '{name}' is required but was not provided and no default value was given")]
pub struct MissingInputError {
    name: String,
}

/// Metadata for a module parameter (from io() or config() calls)
#[derive(Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ParameterMetadataGen<V: ValueLifetimeless> {
    /// Parameter name
    pub name: String,
    /// Type value (e.g., Net, str, int, etc.)
    pub type_value: V,
    /// Whether the parameter is optional
    pub optional: bool,
    /// Default value if provided
    pub default_value: Option<V>,
    /// Whether this is a config parameter (vs io parameter)
    pub is_config: bool,
    /// Help text describing the parameter
    pub help: Option<String>,
}

// Manual because no instance for Option<V>
unsafe impl<From: Coerce<To> + ValueLifetimeless, To: ValueLifetimeless>
    Coerce<ParameterMetadataGen<To>> for ParameterMetadataGen<From>
{
}

starlark_complex_value!(pub ParameterMetadata);

#[starlark_value(type = "ParameterMetadata")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ParameterMetadataGen<V> where
    Self: ProvidesStaticType<'v>
{
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for ParameterMetadataGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ParameterMetadata({})", self.name)
    }
}

impl<'v, V: ValueLike<'v>> ParameterMetadataGen<V> {
    pub fn new(
        name: String,
        type_value: V,
        optional: bool,
        default_value: Option<V>,
        is_config: bool,
        help: Option<String>,
    ) -> Self {
        Self {
            name,
            type_value,
            optional,
            default_value,
            is_config,
            help,
        }
    }
}

#[derive(Clone, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct ModuleValueGen<V: ValueLifetimeless> {
    name: String,
    source_path: String,
    inputs: SmallMap<String, V>,
    children: Vec<V>,
    properties: SmallMap<String, V>,
    /// Ordered list of parameter metadata representing the module's signature
    signature: Vec<ParameterMetadataGen<V>>,
}

starlark_complex_value!(pub ModuleValue);

#[starlark_value(type = "Module")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for ModuleValueGen<V> where Self: ProvidesStaticType<'v>
{}

impl<'v, V: ValueLike<'v>> std::fmt::Display for ModuleValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Module({})", self.name)
    }
}

impl<'v, V: ValueLike<'v>> std::fmt::Debug for ModuleValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("Module");
        debug.field("name", &self.name);
        debug.field("source", &self.source_path);

        // Sort inputs for deterministic output
        if !self.inputs.is_empty() {
            let mut inputs: Vec<_> = self.inputs.iter().collect();
            inputs.sort_by_key(|(k, _)| k.as_str());
            let inputs_map: std::collections::BTreeMap<_, _> = inputs
                .into_iter()
                .map(|(k, v)| (k.as_str(), format!("{v:?}")))
                .collect();
            debug.field("inputs", &inputs_map);
        }

        // Sort properties for deterministic output
        if !self.properties.is_empty() {
            let mut props: Vec<_> = self.properties.iter().collect();
            props.sort_by_key(|(k, _)| k.as_str());
            let props_map: std::collections::BTreeMap<_, _> = props
                .into_iter()
                .map(|(k, v)| (k.as_str(), format!("{v:?}")))
                .collect();
            debug.field("properties", &props_map);
        }

        // Children - Vec already implements Debug properly
        if !self.children.is_empty() {
            debug.field("children", &self.children);
        }

        debug.finish()
    }
}

impl<'v, V: ValueLike<'v>> ModuleValueGen<V> {
    pub(crate) fn add_child(&mut self, child: V) {
        self.children.push(child);
    }

    pub(crate) fn add_property(&mut self, name: String, value: V) {
        self.properties.insert(name, value);
    }

    pub fn new(name: String, source_path: &Path) -> Self {
        let source_path = source_path.to_string_lossy().into_owned();
        ModuleValueGen {
            name,
            source_path,
            inputs: SmallMap::new(),
            children: Vec::new(),
            properties: SmallMap::new(),
            signature: Vec::new(),
        }
    }

    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn inputs(&self) -> &SmallMap<String, V> {
        &self.inputs
    }
    pub fn children(&self) -> &Vec<V> {
        &self.children
    }

    /// Return a reference to the custom property map attached to this Module.
    pub fn properties(&self) -> &SmallMap<String, V> {
        &self.properties
    }

    /// Set the user-visible name for this Module.
    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }

    /// Add a parameter to the module's signature with full metadata.
    pub fn add_parameter_metadata(
        &mut self,
        name: String,
        type_value: V,
        optional: bool,
        default_value: Option<V>,
        is_config: bool,
        help: Option<String>,
    ) {
        // Check if this parameter already exists
        if !self.signature.iter().any(|p| p.name == name) {
            self.signature.push(ParameterMetadataGen::new(
                name,
                type_value,
                optional,
                default_value,
                is_config,
                help,
            ));
        }
    }

    /// Get the module's signature.
    pub fn signature(&self) -> &Vec<ParameterMetadataGen<V>> {
        &self.signature
    }
}

#[derive(Clone, Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
pub struct ModuleLoader {
    pub name: String,
    pub source_path: String,
    /// List of placeholder names (io()/config()) declared by the module.  Populated lazily
    /// when the loader is constructed by evaluating the target file once with an empty
    /// input map so that signature help can surface them later without re-parsing.
    pub params: Vec<String>,

    /// Map of parameter names to their type information (e.g., "param_name" -> "Net")
    /// Extracted from diagnostics during the introspection pass.
    pub param_types: SmallMap<String, String>,

    #[freeze(identity)]
    pub frozen_module: Option<FrozenModule>,
}
starlark_simple_value!(ModuleLoader);

impl std::fmt::Display for ModuleLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<ModuleLoader {}>", self.name)
    }
}

#[starlark_value(type = "ModuleLoader")]
impl<'v> StarlarkValue<'v> for ModuleLoader
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let heap = eval.heap();
        // Only allow named arguments
        let positions_iter = args.positions(heap)?;
        if positions_iter.count() > 0 {
            return Err(starlark::Error::new_other(anyhow::anyhow!(
                "ModuleLoader only supports named arguments"
            )));
        }

        // Collect named arguments into InputMap while handling the special `name` parameter.
        let mut input_map: InputMap = InputMap::new();
        let mut provided_names: HashSet<String> = HashSet::new();
        let mut override_name: Option<String> = None;
        // Optional map of properties passed via `properties = {...}`.
        let mut properties_override: Option<
            starlark::collections::SmallMap<String, crate::lang::input::InputValue>,
        > = None;

        for (arg_name, value) in args.names_map()? {
            if arg_name.as_str() == "name" {
                // Ensure `name` is a string.
                let name_str = value
                    .unpack_str()
                    .ok_or_else(|| {
                        starlark::Error::new_other(anyhow::anyhow!(
                            "name parameter must be a string"
                        ))
                    })?
                    .to_string();
                override_name = Some(name_str);
                // Do *not* add `name` to the input map.
                continue;
            }

            if arg_name.as_str() == "properties" {
                // Expect a dict {str: any}
                let dict = DictRef::from_value(value).ok_or_else(|| {
                    starlark::Error::new_other(anyhow::anyhow!(
                        "properties parameter must be a dict"
                    ))
                })?;

                let mut map: starlark::collections::SmallMap<
                    String,
                    crate::lang::input::InputValue,
                > = starlark::collections::SmallMap::new();

                for (k, v) in dict.iter() {
                    let key_str = k.unpack_str().ok_or_else(|| {
                        starlark::Error::new_other(anyhow::anyhow!("property keys must be strings"))
                    })?;

                    let iv = convert_from_starlark(v, heap);
                    map.insert(key_str.to_string(), iv);
                }

                properties_override = Some(map);
                // Do *not* treat `properties` as an input placeholder.
                continue;
            }

            provided_names.insert(arg_name.as_str().to_string());
            let iv = convert_from_starlark(value, heap);
            input_map.insert(arg_name.as_str().to_string(), iv);
        }
        // `name` is required when instantiating a module via its loader.  If the
        // caller omitted it, emit a *soft* diagnostic (non-fatal) and fall back
        // to the loaderʼs default name so evaluation can continue.
        let final_name = if let Some(n) = override_name {
            n
        } else {
            if let Some(call_site) = eval.call_stack_top_location() {
                let msg = format!(
                    "Missing required argument `name` when instantiating module {}",
                    self.name
                );
                let mut diag = EvalMessage::from_any_error(Path::new(call_site.filename()), &msg);
                diag.span = Some(call_site.resolve_span());
                eval.add_diagnostic(crate::Diagnostic::from_eval_message(diag));
            } else {
                let msg = format!(
                    "Missing required argument `name` when instantiating module {}",
                    self.name
                );
                eval.add_diagnostic(crate::Diagnostic::from_eval_message(
                    EvalMessage::from_any_error(Path::new(&self.source_path), &msg),
                ));
            }

            // Use the file-stem derived name from the loader as a fallback.
            self.name.clone()
        };

        // Evaluate the module file with the given inputs
        let ctx = eval
            .eval_context()
            .expect("expected eval context")
            .child_context()
            .set_strict_io_config(true);

        let ctx = if let Some(props_map) = properties_override.clone() {
            ctx.set_properties(props_map)
        } else {
            ctx
        };

        let result = ctx
            .set_source_path(std::path::PathBuf::from(&self.source_path))
            .set_module_name(final_name.clone())
            .set_inputs(input_map)
            .eval();

        let context = eval
            .module()
            .extra_value()
            .and_then(|extra| extra.downcast_ref::<ContextValue>())
            .ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "unexpected context - ContextValue not found",
                ))
            })?;

        let call_site = eval.call_stack_top_location();

        // Propagate diagnostics from the instantiated module, but avoid
        // showing the same error twice: instead create a new diagnostic whose
        // primary span is the call-site inside the parent file and which
        // carries the child error(s) as `related` entries.

        let had_diags = !result.diagnostics.is_empty();

        for child in result.diagnostics.into_iter() {
            let diag_to_add = if let Some(cs) = &call_site {
                // Build a new primary message pointing at this ModuleLoader call-site.
                Diagnostic {
                    path: cs.filename().to_string(),
                    span: Some(cs.resolve_span()),
                    severity: EvalSeverity::Error,
                    body: format!("Error instantiating `{}`", self.name),
                    call_stack: Some(eval.call_stack().clone()),
                    child: Some(Box::new(child)),
                }
            } else {
                child
            };

            // Propagate the diagnostic upwards.
            context.add_diagnostic(diag_to_add);
        }

        match result.output {
            Some(output) => {
                // Add a reference to the dependent module's frozen heap so it stays alive.
                eval.frozen_heap()
                    .add_reference(output.star_module.frozen_heap());

                // Add the evaluated module as a child on the *current* evaluation context so that
                // it shows up in the final schematic.
                context.add_child(eval.frozen_heap().alloc(output.sch_module).to_value());

                let used_inputs: HashSet<String> = output
                    .star_module
                    .extra_value()
                    .and_then(|extra| extra.downcast_ref::<FrozenContextValue>())
                    .map(|fctx| {
                        fctx.module
                            .signature()
                            .iter()
                            .map(|param| param.name.clone())
                            .collect()
                    })
                    .unwrap_or_default();

                // Remove any potential `name` override from the unused-check set.
                let unused: Vec<String> =
                    provided_names.difference(&used_inputs).cloned().collect();

                if !unused.is_empty() {
                    let msg = format!(
                        "Unknown argument(s) provided to module {}: {}",
                        self.name,
                        unused.join(", ")
                    );

                    if let Some(cs) = &call_site {
                        let mut unused_diag =
                            EvalMessage::from_any_error(Path::new(cs.filename()), &msg);
                        unused_diag.span = Some(cs.resolve_span());
                        context.add_diagnostic(crate::Diagnostic::from_eval_message(unused_diag));
                    } else {
                        context.add_diagnostic(crate::Diagnostic::from_eval_message(
                            EvalMessage::from_any_error(Path::new(&self.source_path), &msg),
                        ));
                    }
                    // Continue execution without raising an error.
                }

                // Return `None` – in line with other factory functions like Component.
                Ok(Value::new_none())
            }
            None => {
                if !had_diags {
                    if let Some(call_site) = eval.call_stack_top_location() {
                        let msg = format!("Failed to instantiate module {}", self.name);
                        let mut call_diag =
                            EvalMessage::from_any_error(Path::new(call_site.filename()), &msg);
                        call_diag.span = Some(call_site.resolve_span());
                        context.add_diagnostic(crate::Diagnostic::from_eval_message(call_diag));
                    }
                }
                Ok(Value::new_none())
            }
        }
    }
    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        Some(<ModuleLoader as StarlarkValue>::get_type_starlark_repr())
    }

    // Expose exports from the target module as attributes on the loader so users can refer to
    // them via the familiar dot-notation (e.g. `Sub.Component`).  We lazily evaluate the target
    // file with an *empty* input map – mirroring the lightweight introspection pass in
    // `Module()` – and then deep-copy the requested symbol into the current heap so that it
    // lives alongside the callerʼs values.
    fn get_attr(&self, attr: &str, _heap: &'v Heap) -> Option<Value<'v>> {
        // Fast-path: ignore private/internal names.
        if attr.starts_with("__") {
            return None;
        }

        if let Some(frozen_module) = &self.frozen_module {
            return frozen_module.get_option(attr).ok().flatten().map(|owned| {
                // SAFETY: we know the frozen module is alive because we added a reference to it
                let fv = unsafe { owned.unchecked_frozen_value() };
                fv.to_value()
            });
        }

        None
    }
}

// Helper: given a Starlark `typ` value build a sensible default instance of that type.
fn default_for_type<'v>(
    eval: &mut Evaluator<'v, '_, '_>,
    typ: Value<'v>,
) -> anyhow::Result<Value<'v>> {
    let heap = eval.heap();

    if let Some(enum_type) = typ.downcast_ref::<EnumType>() {
        if let Ok(first_variant) = enum_type.at(heap.alloc(0), heap) {
            return Ok(first_variant.to_value());
        } else {
            return Err(anyhow::anyhow!(
                "EnumType provided to config/io() has no variants"
            ));
        }
    }

    if let Some(frozen_enum_type) = typ.downcast_ref::<FrozenEnumType>() {
        let variants = frozen_enum_type
            .get_attr("variants", heap)
            .expect("expected variants attribute");

        let list_ref =
            ListRef::from_value(variants).expect("expected variants attribute to be a list");

        if let Some(first_variant) = list_ref.first() {
            return Ok(first_variant.to_value());
        } else {
            return Err(anyhow::anyhow!(
                "EnumType provided to config/io() has no variants"
            ));
        }
    }

    if typ.downcast_ref::<RecordType>().is_some()
        || typ.downcast_ref::<FrozenRecordType>().is_some()
    {
        return Err(anyhow::anyhow!(
            "Record dependencies require a default value"
        ));
    }

    // Check if it's a TypeType (like str, int, float constructors)
    if TypeType::unpack_value_opt(typ).is_some() {
        // Use the string representation to determine the type
        let type_str = typ.to_string();
        match type_str.as_str() {
            "str" => return Ok(heap.alloc("").to_value()),
            "int" => return Ok(heap.alloc(0i32).to_value()),
            "float" => return Ok(heap.alloc(StarlarkFloat(0.0)).to_value()),
            _ => {
                // Fall through to try calling it as a constructor
            }
        }
    }

    // Try to call it as a constructor with no arguments
    if typ
        .check_callable_with([], [], None, None, &starlark::typing::Ty::any())
        .is_ok()
    {
        return typ
            .invoke(&starlark::eval::Arguments::default(), eval)
            .map_err(|e| anyhow::anyhow!(e.to_string()));
    }

    // Handle special types by their runtime type
    let default = match typ.get_type() {
        "NetType" => heap
            .alloc(NetValue::new(
                generate_net_id(),
                String::new(),
                SmallMap::new(),
                Value::new_none(),
            ))
            .to_value(),
        "InterfaceFactory" => typ
            .invoke(&starlark::eval::Arguments::default(), eval)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?,
        other => {
            return Err(anyhow::anyhow!(
                "config/io() only accepts Net, Interface, Enum, Record, str, int, or float types, got {other}"
            ));
        }
    };
    Ok(default)
}

// Helper: validate that `value` matches the requested `typ` value.
fn validate_type<'v>(
    name: &str,
    value: Value<'v>,
    typ: Value<'v>,
    heap: &'v Heap,
) -> anyhow::Result<()> {
    if TypeType::unpack_value_opt(typ).is_some() {
        let tc = TypeCompiled::new(typ, heap)?;
        if tc.matches(value) {
            return Ok(());
        }

        anyhow::bail!(
            "Input '{name}' (type) has wrong type for this placeholder: expected {typ}, got {value}"
        );
    }

    let is_ok = match typ.get_type() {
        "NetType" => value.downcast_ref::<crate::lang::net::NetValue>().is_some(),
        "InterfaceFactory" => value
            .downcast_ref::<crate::lang::interface::InterfaceValue>()
            .is_some(),
        "EnumType" => EnumValue::from_value(value).is_some(),
        "str" | "string" | "String" => value.unpack_str().is_some(),
        "int" | "Int" => value.unpack_i32().is_some(),
        "float" | "Float" => value.downcast_ref::<StarlarkFloat>().is_some(),
        _ => false,
    };

    if !is_ok {
        anyhow::bail!(
            "Input '{name}' has wrong type for this placeholder: expected {typ}, got {value}"
        );
    }

    Ok(())
}

// Add helper function to attempt converting a value to an enum variant when
// `typ` is an EnumType / FrozenEnumType and the provided `value` is not yet an
// `EnumValue`.  Returns `Ok(Some(converted))` if the conversion succeeds,
// `Ok(None)` if `typ` is not an enum type, and `Err(..)` if the conversion was
// attempted but failed.
fn try_enum_conversion<'v>(
    value: Value<'v>,
    typ: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Option<Value<'v>>> {
    // Only applicable for EnumType values.
    if typ.downcast_ref::<EnumType>().is_none() && typ.downcast_ref::<FrozenEnumType>().is_none() {
        return Ok(None);
    }

    // If the value is already an EnumValue, bail early – the caller should have
    // succeeded the type check in that case.
    if EnumValue::from_value(value).is_some() {
        return Ok(None);
    }

    // Attempt to call the enum factory with the provided `value` as a single
    // positional argument.  This supports common call patterns like passing the
    // variant label as a string (e.g. "NORTH") or the numeric variant index.
    match eval.eval_function(typ, &[value], &[]) {
        Ok(converted) => Ok(Some(converted)),
        Err(e) => Err(anyhow::anyhow!(e.to_string())),
    }
}

fn validate_or_convert<'v>(
    name: &str,
    value: Value<'v>,
    typ: Value<'v>,
    convert: Option<Value<'v>>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<Value<'v>> {
    // First, try a direct type match.
    if validate_type(name, value, typ, eval.heap()).is_ok() {
        return Ok(value);
    }

    // 1. If a custom converter was supplied, try that first.
    if let Some(conv_fn) = convert {
        log::debug!("Converting {name} from {value} to {typ}");
        let converted = eval
            .eval_function(conv_fn, &[value], &[])
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;

        log::debug!("Converted {name} to {converted}");

        // Ensure the converted value now matches the expected type.
        validate_type(name, converted, typ, eval.heap())?;
        log::debug!("Converted {name} to {converted} and validated");
        return Ok(converted);
    }

    // 2. Try automatic type conversions for common cases
    let type_str = typ.to_string();
    match type_str.as_str() {
        "float" | "Float" => {
            // Try to convert int to float
            if let Some(i) = value.unpack_i32() {
                let float_val = eval.heap().alloc(StarlarkFloat(i as f64));
                if validate_type(name, float_val, typ, eval.heap()).is_ok() {
                    return Ok(float_val);
                }
            }
        }
        _ => {}
    }

    // 3. Next, if the expected type is an enum, attempt to construct the variant
    //    by calling the enum factory with the provided value.
    if let Some(converted) = try_enum_conversion(value, typ, eval)? {
        // Ensure the converted value is of the correct type (it should be, but
        // keep the guard for completeness).
        validate_type(name, converted, typ, eval.heap())?;
        return Ok(converted);
    }

    // 4. None of the conversion paths worked – propagate the original validation
    //    error for a helpful message.
    validate_type(name, value, typ, eval.heap())?;
    unreachable!();
}

#[starlark_module]
pub fn module_globals(builder: &mut GlobalsBuilder) {
    fn Module<'v>(
        #[starlark(require = pos)] path: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Get the parent context from the evaluator's ContextValue if available
        let parent_context = eval.eval_context().expect("expected eval context");

        // Get the file provider
        let file_provider = parent_context
            .file_provider
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No file provider available"))?;

        // Get the load resolver
        let load_resolver = parent_context
            .get_load_resolver()
            .ok_or_else(|| anyhow::anyhow!("No load resolver available"))?;

        // Get the current file path
        let current_file = parent_context
            .get_source_path()
            .ok_or_else(|| anyhow::anyhow!("No source path available"))?;

        // Resolve the path using the load resolver
        let resolved_path = load_resolver
            .resolve_path(file_provider.as_ref(), &path, current_file)
            .map_err(|e| anyhow::anyhow!("Failed to resolve module path '{}': {}", path, e))?;

        // Verify the resolved path exists
        if !file_provider.exists(&resolved_path) {
            return Err(anyhow::anyhow!(
                "Module file not found: {}",
                resolved_path.display()
            ));
        }

        let loader = build_module_loader_from_path(&resolved_path, parent_context);

        // Retain the child heap so the cached values remain valid for the lifetime of the
        // parent module.
        if let Some(frozen_mod) = &loader.frozen_module {
            eval.frozen_heap().add_reference(frozen_mod.frozen_heap());
        }

        Ok(eval.heap().alloc(loader))
    }

    /// Declare a net/interface dependency on this module.
    fn io<'v>(
        #[starlark(require = pos)] name: String,
        #[starlark(require = pos)] typ: Value<'v>,
        #[starlark(require = named)] default: Option<Value<'v>>, // explicit default provided by caller
        #[starlark(require = named)] optional: Option<bool>, // if true, the placeholder is not required
        #[starlark(require = named)] help: Option<String>,   // help text describing the parameter
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Record that this placeholder was referenced in the module's signature
        if let Some(ctx) = eval.context_value() {
            let mut module = ctx.module_mut();
            module.add_parameter_metadata(
                name.clone(),
                typ,
                optional.unwrap_or(false),
                default,
                false, // is_config = false for io()
                help,
            );
        }

        // 1. Value supplied by the parent module.
        if let Some(provided) = eval.request_input(&name, typ)? {
            // First try a direct validation.
            if validate_type(name.as_str(), provided, typ, eval.heap()).is_ok() {
                return Ok(provided);
            }

            // If validation failed and `typ` is an enum type, attempt to convert
            // the provided value into an enum variant by calling the enum factory
            // function.
            if let Some(converted) = try_enum_conversion(provided, typ, eval)? {
                return Ok(converted);
            }

            // Fallback: propagate the original validation error.
            validate_type(name.as_str(), provided, typ, eval.heap())?;
            unreachable!();
        }

        // 2. Determine whether the placeholder is required.
        let is_optional = optional.unwrap_or(false);

        // 3. If the placeholder is optional and the parent did not supply a value we use:
        //    • the caller-provided default (after validation) if present
        //    • otherwise a synthetic default for Net/Interface types
        //    • otherwise `None`
        if is_optional {
            if let Some(default_val) = default {
                // Validate the provided default before using it.
                validate_type(name.as_str(), default_val, typ, eval.heap())?;
                return Ok(default_val);
            }

            match typ.get_type() {
                "NetType" | "InterfaceFactory" => {
                    // For io() we always materialise a default Net/Interface so Starlark code can
                    // rely on a valid object even when the dependency is optional.
                    return default_for_type(eval, typ);
                }
                _ => {}
            }

            return Ok(Value::new_none());
        }

        // 4. Placeholder is required (optional == false).
        if !is_optional {
            let strict = eval
                .context_value()
                .map(|ctx| ctx.strict_io_config())
                .unwrap_or(false);

            if strict {
                // Record the missing input so that the parent `ModuleLoader` can surface a helpful
                // diagnostic at the call-site.
                if let Some(ctx) = eval.context_value() {
                    ctx.add_missing_input(name.clone());
                }

                return Err(anyhow::Error::new(MissingInputError { name: name.clone() }));
            }
        }

        // 5. If the caller supplied an explicit default, always prefer it. Otherwise fall back to a
        // synthetic default value generated from the requested `typ`.
        let generated_default = if let Some(default_val) = default {
            // Validate the provided default before using it.
            validate_type(name.as_str(), default_val, typ, eval.heap())?;
            default_val
        } else {
            default_for_type(eval, typ)?
        };

        Ok(generated_default)
    }

    /// Declare a configuration value requirement. Works analogously to `io()` but typically
    /// used for primitive types coming from user configuration.
    fn config<'v>(
        #[starlark(require = pos)] name: String,
        #[starlark(require = pos)] typ: Value<'v>,
        #[starlark(require = named)] default: Option<Value<'v>>,
        #[starlark(require = named)] convert: Option<Value<'v>>,
        #[starlark(require = named)] optional: Option<bool>,
        #[starlark(require = named)] help: Option<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        // Record usage in the module's signature
        if let Some(ctx) = eval.context_value() {
            let mut module = ctx.module_mut();
            module.add_parameter_metadata(
                name.clone(),
                typ,
                optional.unwrap_or(false),
                default,
                true, // is_config = true for config()
                help,
            );
        }

        // 1. Value supplied by the parent module.
        if let Some(provided) = eval.request_input(&name, typ)? {
            return validate_or_convert(&name, provided, typ, convert, eval);
        }

        // 2. Determine whether the placeholder is required.
        let is_optional = optional.unwrap_or(false);

        // 3. If the placeholder is optional and no value was supplied by the parent we return:
        //    • the caller-provided default (if any, after conversion)
        //    • otherwise `None`
        if is_optional {
            if let Some(default_val) = default {
                let converted_default =
                    validate_or_convert(&name, default_val, typ, convert, eval)?;
                return Ok(converted_default);
            }

            return Ok(Value::new_none());
        }

        // 4. Placeholder is required (optional == false).
        // Check if we're in strict mode AND there's no default value
        if !is_optional {
            let strict = eval
                .context_value()
                .map(|ctx| ctx.strict_io_config())
                .unwrap_or(false);

            if strict && default.is_none() {
                // Only throw MissingInputError if there's no default value
                // Record the missing input so that the parent `ModuleLoader` can surface a helpful
                // diagnostic at the call-site.
                if let Some(ctx) = eval.context_value() {
                    ctx.add_missing_input(name.clone());
                }

                return Err(anyhow::Error::new(MissingInputError { name: name.clone() }));
            }
        }

        // 5. If the caller supplied an explicit default, always prefer it. Otherwise fall back to a
        // synthetic default value generated from the requested `typ`.
        let generated_default = if let Some(default_val) = default {
            validate_or_convert(&name, default_val, typ, convert, eval)?
        } else {
            let gen_value = default_for_type(eval, typ)?;
            validate_or_convert(&name, gen_value, typ, convert, eval)?
        };

        Ok(generated_default)
    }

    fn add_property<'v>(
        #[starlark(require = pos)] name: String,
        #[starlark(require = pos)] value: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        eval.add_property(&name, value);

        Ok(Value::new_none())
    }
}

/// Construct a `ModuleLoader` for the Starlark file at `path` by performing a
/// lightweight introspection pass (empty `InputMap`) so that we can populate
/// the placeholder parameter list ahead of time.  This helper is shared by
/// the public `Module()` built-in as well as the directory-style `load()`
/// implementation in `eval.rs` so that the logic lives in exactly one place.
pub fn build_module_loader_from_path(path: &Path, parent_ctx: &EvalContext) -> ModuleLoader {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    // Introspect the target module **once** with an empty InputMap so that we
    // can collect any `io()` / `config()` placeholder names for signature help
    // and cache the frozen module for later attribute look-ups.
    let result = parent_ctx
        .child_context()
        .set_source_path(path.to_path_buf())
        .set_module_name(name.clone())
        .set_inputs(InputMap::new())
        .eval();

    let mut params: Vec<String> = vec!["name".to_string(), "properties".to_string()];
    let mut param_types: SmallMap<String, String> = SmallMap::new();

    // Extract parameter names and types from the frozen module
    if let Some(output) = &result.output {
        if let Some(extra) = output
            .star_module
            .extra_value()
            .and_then(|e| e.downcast_ref::<FrozenContextValue>())
        {
            // Get the signature from the module
            for param in extra.module.signature().iter() {
                params.push(param.name.clone());
                param_types.insert(param.name.clone(), param.type_value.to_string());
            }
        }
    }

    params.sort();
    params.dedup();

    ModuleLoader {
        name,
        source_path: path.to_string_lossy().into_owned(),
        params,
        param_types,
        frozen_module: result.output.map(|o| o.star_module),
    }
}
