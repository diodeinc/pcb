use std::{
    cell::{Ref, RefMut},
    sync::Arc,
};

use starlark::{
    eval::Evaluator,
    values::{Value, ValueLike},
};

use crate::{
    lang::{context::ContextValue, eval::EvalContext, module::ModuleValue},
    Diagnostic,
};

/// Convenience trait that adds helper methods to Starlark `Evaluator`s so they can
/// interact with the current [`ContextValue`].
pub(crate) trait EvaluatorExt<'v> {
    /// Return a reference to the [`ContextValue`] associated with the evaluator if one
    /// is available.
    fn context_value(&self) -> Option<&ContextValue<'v>>;

    /// Fetch the input value and materialise it on the current heap, using
    /// `expected_typ` (the second argument passed to `io()` / `config()`) to guide
    /// reconstruction for complex types such as enums and records.
    fn request_input(
        &mut self,
        name: &str,
        expected_typ: Value<'v>,
    ) -> anyhow::Result<Option<Value<'v>>>;

    /// Add a property to the module value.
    fn add_property(&self, name: &str, value: Value<'v>);

    /// Return the path to the source file that is currently being evaluated.
    fn source_path(&self) -> Option<String>;

    /// Borrow the underlying [`ModuleValue`] immutably.
    #[allow(dead_code)]
    fn module_value(&self) -> Option<Ref<'_, ModuleValue<'v>>>;

    /// Borrow the underlying [`ModuleValue`] mutably.
    fn module_value_mut(&self) -> Option<RefMut<'_, ModuleValue<'v>>>;

    /// Add a diagnostic to the module value.
    fn add_diagnostic(&self, diagnostic: Diagnostic);

    /// Return the [`Context`] that is currently being used.
    fn eval_context(&self) -> Option<&EvalContext>;

    /// Return the FileProvider from the EvalContext if available.
    fn file_provider(&self) -> Option<Arc<dyn crate::FileProvider>>;

    /// Return the LoadResolver from the EvalContext if available.
    fn load_resolver(&self) -> Option<Arc<dyn crate::LoadResolver>>;
}

impl<'v> EvaluatorExt<'v> for Evaluator<'v, '_, '_> {
    fn context_value(&self) -> Option<&ContextValue<'v>> {
        self.module()
            .extra_value()
            .and_then(|extra| extra.downcast_ref::<ContextValue>())
    }

    fn request_input(
        &mut self,
        name: &str,
        expected_typ: Value<'v>,
    ) -> anyhow::Result<Option<Value<'v>>> {
        // Take a *copy* of the `InputValue` so we can drop the immutable borrow
        // of `self` before we try to materialise the value (which needs a
        // mutable borrow).
        let iv = if let Some(ctx) = self.context_value() {
            ctx.inputs().as_ref().and_then(|m| m.get(name).cloned())
        } else {
            None
        };

        match iv {
            Some(value) => {
                // Record that this input was consumed so we can detect unused parameters later.
                if let Some(ctx) = self.context_value() {
                    let mut module = ctx.module_mut();
                    module.add_signature_param(name.to_string(), expected_typ);
                }

                Ok(Some(value.to_value(self, Some(expected_typ))?))
            }
            None => Ok(None),
        }
    }

    fn add_property(&self, name: &str, value: Value<'v>) {
        if let Some(ctx) = self.context_value() {
            ctx.add_property(name.to_string(), value)
        }
    }

    fn add_diagnostic(&self, diagnostic: Diagnostic) {
        if let Some(ctx) = self.context_value() {
            ctx.add_diagnostic(diagnostic);
        }
    }

    fn source_path(&self) -> Option<String> {
        self.context_value().map(|ctx| ctx.source_path())
    }

    fn module_value(&self) -> Option<Ref<'_, ModuleValue<'v>>> {
        self.context_value().map(|ctx| ctx.module())
    }

    fn module_value_mut(&self) -> Option<RefMut<'_, ModuleValue<'v>>> {
        self.context_value().map(|ctx| ctx.module_mut())
    }

    fn eval_context(&self) -> Option<&EvalContext> {
        self.context_value().map(|ctx| ctx.parent_context())
    }

    fn file_provider(&self) -> Option<Arc<dyn crate::FileProvider>> {
        self.eval_context()
            .and_then(|ctx| ctx.file_provider.clone())
    }

    fn load_resolver(&self) -> Option<Arc<dyn crate::LoadResolver>> {
        self.eval_context()
            .and_then(|ctx| ctx.load_resolver.clone())
    }
}
