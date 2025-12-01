#![allow(clippy::needless_lifetimes)]

use std::{cell::RefCell, fmt::Display};

use allocative::Allocative;
use serde::Serialize;
use starlark::{
    any::ProvidesStaticType,
    codemap::ResolvedSpan,
    eval::CallStack,
    values::{
        starlark_value, Freeze, FreezeResult, Freezer, FrozenValue, StarlarkValue, Trace, Value,
    },
};

use starlark::collections::SmallMap;

use crate::lang::eval::EvalContext;

use super::module::{parse_positions, FrozenModuleValue, ModuleLoader, ModuleValue};
use super::net::NetId;

#[derive(Debug, Trace)]
pub(crate) struct PendingChild<'v> {
    pub(crate) loader: ModuleLoader,
    pub(crate) final_name: String,
    pub(crate) inputs: SmallMap<String, Value<'v>>,
    pub(crate) properties: Option<SmallMap<String, Value<'v>>>,
    pub(crate) component_modifiers: Vec<Value<'v>>,
    pub(crate) provided_names: Vec<String>,
    pub(crate) call_site_path: String,
    pub(crate) call_site_span: ResolvedSpan,
    pub(crate) call_stack: CallStack,
}

impl<'v> Freeze for PendingChild<'v> {
    type Frozen = FrozenPendingChild;

    fn freeze(self, freezer: &Freezer) -> FreezeResult<Self::Frozen> {
        Ok(FrozenPendingChild {
            loader: self.loader,
            final_name: self.final_name,
            inputs: self.inputs.freeze(freezer)?,
            properties: self.properties.map(|m| m.freeze(freezer)).transpose()?,
            component_modifiers: self.component_modifiers.freeze(freezer)?,
            provided_names: self.provided_names,
            call_site_path: self.call_site_path,
            call_site_span: self.call_site_span,
            call_stack: self.call_stack,
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FrozenPendingChild {
    pub(crate) loader: ModuleLoader,
    pub(crate) final_name: String,
    pub(crate) inputs: SmallMap<String, FrozenValue>,
    pub(crate) properties: Option<SmallMap<String, FrozenValue>>,
    pub(crate) component_modifiers: Vec<FrozenValue>,
    pub(crate) provided_names: Vec<String>,
    pub(crate) call_site_path: String,
    pub(crate) call_site_span: ResolvedSpan,
    pub(crate) call_stack: CallStack,
}

#[derive(Debug, Trace, ProvidesStaticType, Allocative, Serialize)]
#[repr(C)]
pub struct ContextValue<'v> {
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
    context: *const EvalContext,
    #[allocative(skip)]
    #[serde(skip)]
    pending_children: RefCell<Vec<PendingChild<'v>>>,
}

#[derive(Debug, Trace, ProvidesStaticType, Allocative, Serialize)]
#[repr(C)]
pub struct FrozenContextValue {
    pub(crate) module: FrozenModuleValue,
    pub(crate) strict_io_config: bool,
    #[allocative(skip)]
    pub(crate) diagnostics: Vec<crate::Diagnostic>,
    /// Pending children to process after this module is frozen
    #[serde(skip)]
    #[allocative(skip)]
    pub(crate) pending_children: Vec<FrozenPendingChild>,
}

impl Freeze for ContextValue<'_> {
    type Frozen = FrozenContextValue;

    fn freeze(self, freezer: &Freezer) -> FreezeResult<Self::Frozen> {
        Ok(FrozenContextValue {
            module: self.module.freeze(freezer)?,
            strict_io_config: self.strict_io_config,
            diagnostics: self.diagnostics.into_inner(),
            pending_children: self.pending_children.into_inner().freeze(freezer)?,
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
    pub fn from_context(context: &EvalContext) -> Self {
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

        let module = ModuleValue::new(context.module_path.clone(), source_path, positions);

        Self {
            module: RefCell::new(module),
            strict_io_config: context.strict_io_config,
            missing_inputs: RefCell::new(Vec::new()),
            diagnostics: RefCell::new(Vec::new()),
            context: context as *const _,
            pending_children: RefCell::new(Vec::new()),
        }
    }

    /// Get the parent eval::Context
    pub(crate) fn parent_context(&self) -> &EvalContext {
        // SAFETY: We ensure the parent Context outlives this ContextValue
        unsafe { &*self.context }
    }

    /// Return whether missing required io()/config() placeholders should be treated as
    /// errors in this evaluation context.
    pub(crate) fn strict_io_config(&self) -> bool {
        self.strict_io_config
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

    /// Borrow the pending children mutably to update them before freezing
    pub(crate) fn pending_children_mut(&self) -> std::cell::RefMut<'_, Vec<PendingChild<'v>>> {
        self.pending_children.borrow_mut()
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
    pub(crate) fn register_net(
        &self,
        id: NetId,
        local_name: &str,
        call_stack: starlark::eval::CallStack,
    ) -> anyhow::Result<String> {
        self.module
            .borrow_mut()
            .register_net(id, local_name.to_string(), call_stack)
    }

    /// Unregister a previously registered net from the current module.
    pub(crate) fn unregister_net(&self, id: NetId) {
        self.module.borrow_mut().unregister_net(id)
    }
}
