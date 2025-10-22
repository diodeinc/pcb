use std::{
    cell::{Ref, RefMut},
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
};

use starlark::{
    eval::Evaluator,
    values::{Value, ValueLike},
};

use crate::{
    lang::{
        context::ContextValue,
        eval::EvalContext,
        module::{ModulePath, ModuleValue},
    },
    Diagnostic, FrozenComponentValue, FrozenModuleValue,
};

/// Convenience trait that adds helper methods to Starlark `Evaluator`s so they can
/// interact with the current [`ContextValue`].
pub(crate) trait EvaluatorExt<'v> {
    /// Return a reference to the [`ContextValue`] associated with the evaluator if one
    /// is available.
    fn context_value(&self) -> Option<&ContextValue<'v>>;

    /// Fetch the input value from module.inputs (already copied from parent)
    fn request_input(&mut self, name: &str) -> anyhow::Result<Option<Value<'v>>>;

    /// Add a property to the module value.
    fn add_property(&self, name: &str, value: Value<'v>);

    /// Return the path to the source file that is currently being evaluated.
    fn source_path(&self) -> Option<String>;

    /// Borrow the underlying [`ModuleValue`] immutably.
    #[allow(dead_code)]
    fn module_value(&self) -> Option<Ref<'_, ModuleValue<'v>>>;

    /// Borrow the underlying [`ModuleValue`] mutably.
    #[allow(dead_code)]
    fn module_value_mut(&self) -> Option<RefMut<'_, ModuleValue<'v>>>;

    /// Add a diagnostic to the module value.
    fn add_diagnostic<D: Into<Diagnostic>>(&self, diagnostic: D);

    /// Return the [`Context`] that is currently being used.
    fn eval_context(&self) -> Option<&EvalContext>;

    fn module_tree(&self) -> Option<Arc<Mutex<BTreeMap<ModulePath, FrozenModuleValue>>>>;

    /// Recursively collect components from a module and all its submodules
    /// Returns a map of component_path -> component_value
    fn collect_components(
        &self,
        module_path: &ModulePath,
    ) -> HashMap<ModulePath, FrozenComponentValue>;
}

impl<'v> EvaluatorExt<'v> for Evaluator<'v, '_, '_> {
    fn context_value(&self) -> Option<&ContextValue<'v>> {
        self.module()
            .extra_value()
            .and_then(|extra| extra.downcast_ref::<ContextValue>())
    }

    fn request_input(&mut self, name: &str) -> anyhow::Result<Option<Value<'v>>> {
        // Check module.inputs (already copied from parent using deep_copy_to!)
        if let Some(ctx) = self.context_value() {
            let module = ctx.module();
            if let Some(value) = module.inputs().get(name) {
                return Ok(Some(value.to_value()));
            }
        }

        Ok(None)
    }

    fn add_property(&self, name: &str, value: Value<'v>) {
        if let Some(ctx) = self.context_value() {
            ctx.add_property(name.to_string(), value)
        }
    }

    fn add_diagnostic<D: Into<Diagnostic>>(&self, diagnostic: D) {
        if let Some(ctx) = self.context_value() {
            ctx.add_diagnostic(diagnostic.into());
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

    fn module_tree(&self) -> Option<Arc<Mutex<BTreeMap<ModulePath, FrozenModuleValue>>>> {
        self.eval_context().map(|ctx| ctx.module_tree.clone())
    }

    fn collect_components(
        &self,
        module_path: &ModulePath,
    ) -> HashMap<ModulePath, FrozenComponentValue> {
        let Some(tree) = self.module_tree() else {
            return HashMap::new();
        };
        let tree = tree.lock().unwrap();

        let mut result = HashMap::new();

        // Iterate through all modules in the tree (no downcasting needed!)
        for (child_module_path, module) in tree.iter() {
            // Check if this module is a descendant of (or is) the target module_path
            if is_descendant_path(module_path, child_module_path) {
                // Add all components from this module with their full paths (cloned since they're frozen)
                for component in module.components() {
                    // Build component path: module_path.component_name
                    let mut component_path = child_module_path.clone();
                    component_path.push(component.name());
                    result.insert(component_path, component.clone());
                }
            }
        }

        result
    }
}

/// Helper: Check if a path is a descendant of (or equal to) a parent path
fn is_descendant_path(parent_path: &ModulePath, child_path: &ModulePath) -> bool {
    if parent_path.is_root() {
        // Root module - all entries are descendants
        true
    } else {
        // Check if child_path starts with parent_path (including exact match)
        child_path.starts_with(parent_path)
    }
}
