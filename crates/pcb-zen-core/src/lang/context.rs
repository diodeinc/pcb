#![allow(clippy::needless_lifetimes)]

use std::{cell::RefCell, collections::HashSet, fmt::Display, path::Path};

use allocative::Allocative;
use serde::Serialize;
use starlark::{
    any::ProvidesStaticType,
    codemap::ResolvedSpan,
    errors::{EvalMessage, EvalSeverity},
    eval::CallStack,
    values::{
        starlark_value, Freeze, FreezeError, FreezeResult, Freezer, FrozenValue, StarlarkValue,
        Trace, Value, ValueLike,
    },
};

use starlark::collections::SmallMap;

use super::module::{parse_positions, FrozenModuleValue, ModuleLoader, ModuleValue};
use super::net::NetId;

#[derive(Debug, Trace)]
pub(crate) struct PendingChild<'v> {
    pub(crate) loader: ModuleLoader,
    pub(crate) final_name: String,
    pub(crate) inputs: SmallMap<String, Value<'v>>,
    pub(crate) properties: Option<SmallMap<String, Value<'v>>>,
    pub(crate) provided_names: Vec<String>,
    pub(crate) call_site_path: String,
    pub(crate) call_site_span: ResolvedSpan,
    pub(crate) call_stack: CallStack,
}

#[derive(Debug, Trace, ProvidesStaticType, Allocative, Serialize)]
#[repr(C)]
pub(crate) struct ContextValue<'v> {
    module: RefCell<ModuleValue<'v>>,
    /// If `true`, missing required inputs declared via io()/config() should be treated as
    /// hard errors.  This flag is set when the module is instantiated via a `ModuleLoader`
    /// call.  When evaluating library files (e.g. via load()) or when running in other
    /// contexts we leave this `false` so that io()/config() placeholders behave
    /// permissively and synthesize defaults instead of failing.
    strict_io_config: bool,
    missing_inputs: RefCell<Vec<String>>,
    #[allocative(skip)]
    diagnostics: RefCell<Vec<crate::Diagnostic>>,
    /// The eval::Context that the current evaluator is running in.
    #[allocative(skip)]
    #[serde(skip)]
    context: *const crate::lang::eval::EvalContext,
    #[allocative(skip)]
    #[serde(skip)]
    pending_children: RefCell<Vec<PendingChild<'v>>>,
}

#[derive(Debug, Trace, ProvidesStaticType, Allocative, Serialize)]
#[repr(C)]
pub(crate) struct FrozenContextValue {
    pub(crate) module: FrozenModuleValue,
    pub(crate) strict_io_config: bool,
    #[allocative(skip)]
    pub(crate) diagnostics: Vec<crate::Diagnostic>,
}

impl Freeze for ContextValue<'_> {
    type Frozen = FrozenContextValue;

    fn freeze(self, freezer: &Freezer) -> FreezeResult<Self::Frozen> {
        let mut module = self.module.into_inner();
        let mut diagnostics = self.diagnostics.into_inner();
        let strict_io_config = self.strict_io_config;
        let pending_children = self.pending_children.into_inner();
        let parent_context = unsafe { &*self.context };

        for pending in pending_children {
            process_pending_child(
                pending,
                parent_context,
                freezer,
                &mut module,
                &mut diagnostics,
            )?;
        }

        Ok(FrozenContextValue {
            module: module.freeze(freezer)?,
            strict_io_config,
            diagnostics,
        })
    }
}

impl Display for ContextValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ContextValue")
    }
}

impl Display for FrozenContextValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FrozenContextValue")
    }
}

#[starlark_value(type = "ContextValue")]
impl<'v> StarlarkValue<'v> for ContextValue<'v> where Self: ProvidesStaticType<'v> {}

#[starlark_value(type = "FrozenContextValue")]
impl<'v> StarlarkValue<'v> for FrozenContextValue
where
    Self: ProvidesStaticType<'v>,
{
    type Canonical = ContextValue<'v>;
}

impl FrozenContextValue {
    #[allow(dead_code)]
    pub(crate) fn diagnostics(&self) -> &Vec<crate::Diagnostic> {
        &self.diagnostics
    }
}

impl<'v> ContextValue<'v> {
    /// Create a new `ContextValue` with a parent eval::Context for sharing caches
    pub(crate) fn from_context(context: &crate::lang::eval::EvalContext) -> Self {
        let source_path = context
            .source_path
            .as_ref()
            .expect("source_path not set on Context");

        // Parse position data if file provider is available
        let positions = context
            .file_provider()
            .read_file(source_path)
            .ok()
            .map(|content| parse_positions(&content))
            .unwrap_or_default();

        Self {
            module: RefCell::new(ModuleValue::new(
                context.name.clone().unwrap_or(
                    source_path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                ),
                source_path,
                positions,
            )),
            strict_io_config: context.strict_io_config,
            missing_inputs: RefCell::new(Vec::new()),
            diagnostics: RefCell::new(Vec::new()),
            context: context as *const _,
            pending_children: RefCell::new(Vec::new()),
        }
    }

    /// Get the parent eval::Context
    pub(crate) fn parent_context(&self) -> &crate::lang::eval::EvalContext {
        // SAFETY: We ensure the parent Context outlives this ContextValue
        unsafe { &*self.context }
    }

    /// Return whether missing required io()/config() placeholders should be treated as
    /// errors in this evaluation context.
    pub(crate) fn strict_io_config(&self) -> bool {
        self.strict_io_config
    }

    pub(crate) fn add_child(&self, child: Value<'v>) {
        self.module.borrow_mut().add_child(child);
    }

    pub(crate) fn add_property(&self, name: String, value: Value<'v>) {
        self.module.borrow_mut().add_property(name, value);
    }

    pub(crate) fn add_moved_directive(
        &self,
        old_path: String,
        new_path: String,
        auto_generated: bool,
    ) {
        self.module
            .borrow_mut()
            .add_moved_directive(old_path, new_path, auto_generated);
    }

    pub(crate) fn add_missing_input(&self, name: String) {
        self.missing_inputs.borrow_mut().push(name);
    }

    pub(crate) fn add_diagnostic<D: Into<crate::Diagnostic>>(&self, diag: D) {
        self.diagnostics.borrow_mut().push(diag.into());
    }

    pub(crate) fn enqueue_child(&self, child: PendingChild<'v>) {
        self.pending_children.borrow_mut().push(child);
    }

    #[allow(dead_code)]
    pub(crate) fn diagnostics(&self) -> std::cell::Ref<'_, Vec<crate::Diagnostic>> {
        self.diagnostics.borrow()
    }

    /// Return the absolute source path of the Starlark file currently being evaluated.
    pub fn source_path(&self) -> String {
        self.module.borrow().source_path().to_owned()
    }

    /// Borrow the underlying `ModuleValue` immutably.
    #[allow(dead_code)]
    pub(crate) fn module(&self) -> std::cell::Ref<'_, ModuleValue<'v>> {
        self.module.borrow()
    }

    /// Borrow the underlying `ModuleValue` mutably.
    pub(crate) fn module_mut(&self) -> std::cell::RefMut<'_, ModuleValue<'v>> {
        self.module.borrow_mut()
    }

    /// Register a newly created net with this module. Enforces per-module uniqueness of names.
    pub(crate) fn register_net(&self, id: NetId, local_name: &str) -> anyhow::Result<String> {
        self.module
            .borrow_mut()
            .register_net(id, local_name.to_string())
    }

    /// Unregister a previously registered net from the current module.
    pub(crate) fn unregister_net(&self, id: NetId) {
        self.module.borrow_mut().unregister_net(id)
    }
}

fn process_pending_child<'v>(
    pending: PendingChild<'v>,
    parent_context: &crate::lang::eval::EvalContext,
    freezer: &Freezer,
    module: &mut ModuleValue<'v>,
    diagnostics: &mut Vec<crate::Diagnostic>,
) -> FreezeResult<()> {
    let PendingChild {
        loader,
        final_name,
        inputs,
        properties,
        provided_names,
        call_site_path,
        call_site_span,
        call_stack,
    } = pending;

    let mut frozen_inputs: SmallMap<String, FrozenValue> = SmallMap::new();
    for (name, value) in inputs.into_iter() {
        frozen_inputs.insert(name, freezer.freeze(value)?);
    }

    let mut frozen_properties: Option<SmallMap<String, FrozenValue>> = None;
    if let Some(props) = properties {
        let mut map = SmallMap::new();
        for (name, value) in props.into_iter() {
            map.insert(name, freezer.freeze(value)?);
        }
        frozen_properties = Some(map);
    }

    let mut child_ctx = parent_context
        .child_context()
        .set_strict_io_config(true)
        .set_source_path(std::path::PathBuf::from(&loader.source_path))
        .set_module_name(final_name.clone());

    if let Some(props) = frozen_properties {
        child_ctx
            .set_properties_from_frozen_values(props)
            .map_err(|e| FreezeError::new(e.to_string()))?;
    }
    child_ctx
        .set_inputs_from_frozen_values(frozen_inputs)
        .map_err(|e| FreezeError::new(e.to_string()))?;

    let (output, child_diags) = child_ctx.eval().unpack();
    let had_errors = child_diags.has_errors();

    for child_diag in child_diags.into_iter() {
        let diag_to_add = {
            let (severity, message) = match child_diag.severity {
                EvalSeverity::Error => (
                    EvalSeverity::Error,
                    format!("Error instantiating `{}`", loader.name),
                ),
                EvalSeverity::Warning => (
                    EvalSeverity::Warning,
                    format!("Warning from `{}`", loader.name),
                ),
                other => (other, format!("Issue in `{}`", loader.name)),
            };

            crate::Diagnostic {
                path: call_site_path.clone(),
                span: Some(call_site_span),
                severity,
                body: message,
                call_stack: Some(call_stack.clone()),
                child: Some(Box::new(child_diag)),
                source_error: None,
            }
        };

        diagnostics.push(diag_to_add);
    }

    match output {
        Some(output) => {
            freezer
                .frozen_heap()
                .add_reference(output.star_module.frozen_heap());

            let child_value = freezer.frozen_heap().alloc(output.sch_module).to_value();
            module.add_child(child_value);

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

            let provided_set: HashSet<String> = provided_names.into_iter().collect();

            let unused: Vec<String> = provided_set.difference(&used_inputs).cloned().collect();

            if !unused.is_empty() {
                let msg = format!(
                    "Unknown argument(s) provided to module {}: {}",
                    loader.name,
                    unused.join(", ")
                );

                let mut diag = EvalMessage::from_any_error(Path::new(&call_site_path), &msg);
                diag.span = Some(call_site_span);
                diagnostics.push(diag.into());
            }
        }
        None => {
            if !had_errors {
                let msg = format!("Failed to instantiate module {}", loader.name);
                let mut diag = EvalMessage::from_any_error(Path::new(&call_site_path), &msg);
                diag.span = Some(call_site_span);
                diagnostics.push(diag.into());
            }
        }
    }

    Ok(())
}
