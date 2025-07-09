#![allow(clippy::needless_lifetimes)]

use std::{cell::RefCell, fmt::Display};

use allocative::Allocative;
use serde::Serialize;
use starlark::{
    any::ProvidesStaticType,
    values::{starlark_value, Freeze, FreezeResult, Freezer, StarlarkValue, Trace, Value},
};

use super::{
    input::InputMap,
    module::{FrozenModuleValue, ModuleValue},
};

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
        Ok(FrozenContextValue {
            module: self.module.freeze(freezer)?,
            strict_io_config: self.strict_io_config,
            diagnostics: self.diagnostics.into_inner(),
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
            )),
            strict_io_config: context.strict_io_config,
            missing_inputs: RefCell::new(Vec::new()),
            diagnostics: RefCell::new(Vec::new()),
            context: context as *const _,
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

    pub(crate) fn add_missing_input(&self, name: String) {
        self.missing_inputs.borrow_mut().push(name);
    }

    pub(crate) fn add_diagnostic(&self, diag: crate::Diagnostic) {
        self.diagnostics.borrow_mut().push(diag);
    }

    #[allow(dead_code)]
    pub(crate) fn diagnostics(&self) -> std::cell::Ref<'_, Vec<crate::Diagnostic>> {
        self.diagnostics.borrow()
    }

    pub fn inputs(&self) -> Option<&InputMap> {
        self.parent_context().inputs.as_ref()
    }

    /// Return the absolute source path of the Starlark file currently being evaluated.
    pub fn source_path(&self) -> String {
        self.module.borrow().source_path().to_owned()
    }

    /// Borrow the underlying `ModuleValue` immutably.
    pub(crate) fn module(&self) -> std::cell::Ref<'_, ModuleValue<'v>> {
        self.module.borrow()
    }

    /// Borrow the underlying `ModuleValue` mutably.
    pub(crate) fn module_mut(&self) -> std::cell::RefMut<'_, ModuleValue<'v>> {
        self.module.borrow_mut()
    }
}
