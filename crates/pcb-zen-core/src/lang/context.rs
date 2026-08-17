#![allow(clippy::needless_lifetimes)]

use std::{cell::RefCell, fmt::Display};

use allocative::Allocative;
use serde::Serialize;
use starlark::{
    any::ProvidesStaticType,
    codemap::ResolvedSpan,
    eval::CallStack,
    values::{
        Freeze, FreezeResult, Freezer, FrozenValue, StarlarkValue, Trace, Value, starlark_value,
    },
};

use starlark::collections::SmallMap;

use crate::lang::eval::EvalContext;

use super::module::{FrozenModuleValue, ModuleLoader, ModuleValue, parse_positions};
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

/// What one solved `pin_request` took, plus the pad namespace it took it from.
#[derive(Debug, Clone)]
pub(crate) struct PinClaim {
    pub(crate) instance: String,
    pub(crate) pins: Vec<String>,
    pub(crate) scope: String,
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
    /// Names of required io()/config() inputs that were not provided, recorded
    /// during this module's evaluation; not preserved across freeze.
    missing_inputs: RefCell<Vec<String>>,
    #[allocative(skip)]
    diagnostics: RefCell<Vec<crate::Diagnostic>>,
    #[allocative(skip)]
    #[serde(skip)]
    pending_children: RefCell<Vec<PendingChild<'v>>>,
    /// Instance and pins each solved request claimed, keyed by request name:
    /// pin/instance exclusivity spans every pin_solve of the module, and a
    /// re-solved request releases its own claims.
    #[allocative(skip)]
    #[serde(skip)]
    pin_claims: RefCell<std::collections::HashMap<(String, String), PinClaim>>,
    /// Requests served by the `if_connected` gate, for the post-eval check.
    #[allocative(skip)]
    #[serde(skip)]
    if_connected_served: RefCell<std::collections::HashSet<String>>,
    /// Every pad a part exposes, per part scope. Kept whole rather than net
    /// of the claims: a re-solve releases pads, and what is free is only
    /// known when `pin_map` asks.
    #[allocative(skip)]
    #[serde(skip)]
    exposed_pads: RefCell<std::collections::HashMap<String, Vec<String>>>,
    /// Open net each unclaimed pad was tied to, per `(part scope, pin)`, so
    /// two `pin_map` calls for one part hand back the same net.
    #[allocative(skip)]
    #[serde(skip)]
    tied_off: RefCell<SmallMap<(String, String), Value<'v>>>,
    /// Truthiness each `unless` axis had for a part, so a design says once
    /// whether a gated peripheral is there.
    #[allocative(skip)]
    #[serde(skip)]
    config_axes: RefCell<std::collections::HashMap<(String, String), bool>>,
    /// Net (id, name) each physical pin has been mapped to by `pin_map`,
    /// keyed by `(part scope, pin)`: two components may share pin names.
    #[allocative(skip)]
    #[serde(skip)]
    mapped_pins: RefCell<std::collections::HashMap<(String, String), (u64, String)>>,
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
    /// Create a new `ContextValue` from the current evaluation context.
    pub fn from_context(context: &EvalContext) -> Self {
        let source_path = context
            .source_path()
            .expect("source_path not set on Context");

        // Parse position data if file provider is available
        let positions = if let Some(contents) = context.config().contents.as_deref() {
            parse_positions(contents)
        } else {
            context
                .file_provider()
                .read_file(source_path)
                .ok()
                .map(|content| parse_positions(&content))
                .unwrap_or_default()
        };

        let module = ModuleValue::new(context.module_path().clone(), source_path, positions);

        Self {
            module: RefCell::new(module),
            strict_io_config: context.strict_io_config(),
            missing_inputs: RefCell::new(Vec::new()),
            diagnostics: RefCell::new(Vec::new()),
            pending_children: RefCell::new(Vec::new()),
            pin_claims: RefCell::new(std::collections::HashMap::new()),
            if_connected_served: RefCell::new(std::collections::HashSet::new()),
            exposed_pads: RefCell::new(std::collections::HashMap::new()),
            tied_off: RefCell::new(SmallMap::new()),
            config_axes: RefCell::new(std::collections::HashMap::new()),
            mapped_pins: RefCell::new(std::collections::HashMap::new()),
        }
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

    /// Record what `req` took. A name identifies one request per part, so a
    /// solve that places it elsewhere among `table`'s parts supersedes the
    /// earlier placement while another component's like-named request stands.
    pub(crate) fn record_pin_claim(&self, req: &str, claim: PinClaim, table: &[String]) {
        let mut claims = self.pin_claims.borrow_mut();
        claims.retain(|(scope, name), _| name != req || !table.contains(scope));
        claims.insert((claim.scope.clone(), req.to_owned()), claim);
    }

    /// Which part's pad namespace a solved request belongs to, when one name
    /// answers for one part only.
    pub(crate) fn pin_claim_scope(&self, req: &str) -> Option<String> {
        let claims = self.pin_claims.borrow();
        let mut found = claims.iter().filter(|((_, name), _)| name == req);
        let (_, claim) = found.next()?;
        found.next().is_none().then(|| claim.scope.clone())
    }

    /// Claims other than the ones `except` re-solves on `table`'s own parts:
    /// pin names only mean something inside the part that owns them.
    pub(crate) fn pin_claims_excluding(
        &self,
        except: &std::collections::HashSet<String>,
        table: &[String],
    ) -> Vec<PinClaim> {
        self.pin_claims
            .borrow()
            .iter()
            .filter(|((scope, name), _)| !(except.contains(name) && table.contains(scope)))
            .map(|(_, c)| c.clone())
            .collect()
    }

    /// Net a pad was already mapped to, within one part: a superseded solve's
    /// assignment must not re-map a pad to a second net.
    pub(crate) fn pin_map_net(&self, scope: &str, pin: &str) -> Option<(u64, String)> {
        self.mapped_pins
            .borrow()
            .get(&(scope.to_owned(), pin.to_owned()))
            .cloned()
    }

    /// Requests `if_connected` served because the caller supplied an input of
    /// that name, checked against the finished signature once it is complete.
    pub(crate) fn record_if_connected(&self, name: &str) {
        self.if_connected_served
            .borrow_mut()
            .insert(name.to_owned());
    }

    pub(crate) fn if_connected_served(&self) -> Vec<String> {
        let mut v: Vec<String> = self.if_connected_served.borrow().iter().cloned().collect();
        v.sort();
        v
    }

    pub(crate) fn record_exposed_pads(&self, scope: String, pads: Vec<String>) {
        self.exposed_pads.borrow_mut().insert(scope, pads);
    }

    /// Every part scope a `pin_solve` has run over in this module, sorted.
    pub(crate) fn exposed_pad_scopes(&self) -> Vec<String> {
        let mut v: Vec<String> = self.exposed_pads.borrow().keys().cloned().collect();
        v.sort();
        v
    }

    pub(crate) fn exposed_pads(&self, scope: &str) -> Vec<String> {
        self.exposed_pads
            .borrow()
            .get(scope)
            .cloned()
            .unwrap_or_default()
    }

    /// Pads a live claim holds in `scope`. A superseded solve's pads are not
    /// among them, so they are free to be tied off again.
    pub(crate) fn claimed_pins(&self, scope: &str) -> std::collections::HashSet<String> {
        self.pin_claims
            .borrow()
            .values()
            .filter(|c| c.scope == scope)
            .flat_map(|c| c.pins.iter().cloned())
            .collect()
    }

    /// Axes this part has already settled, whatever table named them.
    pub(crate) fn config_axes_for(&self, scope: &str) -> Vec<String> {
        self.config_axes
            .borrow()
            .keys()
            .filter(|(s, _)| s == scope)
            .map(|(_, axis)| axis.clone())
            .collect()
    }

    /// Record what `axis` meant for `scope`, returning what it meant before.
    pub(crate) fn record_config_axis(&self, scope: &str, axis: &str, on: bool) -> Option<bool> {
        self.config_axes
            .borrow_mut()
            .insert((scope.to_owned(), axis.to_owned()), on)
    }

    /// The open net a pad was already tied to, if this module tied it.
    pub(crate) fn tied_off_net(&self, scope: &str, pin: &str) -> Option<Value<'v>> {
        self.tied_off
            .borrow()
            .get(&(scope.to_owned(), pin.to_owned()))
            .copied()
    }

    pub(crate) fn record_tie_off(&self, scope: &str, pin: &str, net: Value<'v>) {
        self.tied_off
            .borrow_mut()
            .insert((scope.to_owned(), pin.to_owned()), net);
    }

    pub(crate) fn record_pin_map(&self, scope: String, pin: String, net: (u64, String)) {
        self.mapped_pins.borrow_mut().insert((scope, pin), net);
    }

    pub(crate) fn add_diagnostic<D: Into<crate::Diagnostic>>(&self, diag: D) {
        self.diagnostics.borrow_mut().push(diag.into());
    }

    /// Check if a child name already exists in this module (checks both pending modules and existing components).
    /// Returns the type of existing child if found ("module" or "component").
    pub(crate) fn find_existing_child_name(&self, name: &str) -> Option<&'static str> {
        // Check pending module children
        let pending = self.pending_children.borrow();
        for existing in pending.iter() {
            if existing.final_name == name {
                return Some("module");
            }
        }
        drop(pending);

        // Check existing components in module
        if self.module.borrow().has_component(name) {
            return Some("component");
        }

        None
    }

    /// Emit a warning diagnostic for duplicate child name
    pub(crate) fn warn_duplicate_child_name(
        &self,
        name: &str,
        existing_type: &str,
        path: &str,
        span: starlark::codemap::ResolvedSpan,
        call_stack: Option<starlark::eval::CallStack>,
    ) {
        let body = format!(
            "Duplicate child name '{}': a {} with this name already exists.",
            name, existing_type
        );
        let diag = crate::Diagnostic::categorized(
            path,
            &body,
            "module.duplicate_child_name",
            starlark::errors::EvalSeverity::Warning,
        )
        .with_span(Some(span))
        .with_call_stack(call_stack);
        self.add_diagnostic(diag);
    }

    /// Add a child module to this context. Checks for duplicate names against
    /// existing components and modules.
    pub(crate) fn enqueue_child(&self, child: PendingChild<'v>) {
        if let Some(existing_type) = self.find_existing_child_name(&child.final_name) {
            self.warn_duplicate_child_name(
                &child.final_name,
                existing_type,
                &child.call_site_path,
                child.call_site_span,
                Some(child.call_stack.clone()),
            );
        }
        self.pending_children.borrow_mut().push(child);
    }

    /// Add a child value (component, electrical check, testbench) to this module.
    /// For components, checks for duplicate names against existing components and modules.
    pub(crate) fn add_child(
        &self,
        name: Option<&str>,
        child: starlark::values::Value<'v>,
        call_site: Option<&starlark::codemap::FileSpan>,
    ) {
        // Only check duplicates for components (they have names we care about)
        if let Some(child_name) = name
            && let Some(existing_type) = self.find_existing_child_name(child_name)
            && let Some(site) = call_site
        {
            self.warn_duplicate_child_name(
                child_name,
                existing_type,
                site.filename(),
                site.resolve_span(),
                None,
            );
        }
        self.module.borrow_mut().add_child(child);
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
        assignment_inferable: bool,
        kind: &str,
    ) -> anyhow::Result<String> {
        self.module.borrow_mut().register_net(
            id,
            local_name.to_string(),
            assignment_inferable,
            kind.to_string(),
        )
    }

    /// Promote a provisional net name to an inferred variable name once the
    /// assignment target is known.
    pub(crate) fn infer_net_name(&self, id: NetId, inferred_name: &str) -> anyhow::Result<String> {
        self.module
            .borrow_mut()
            .infer_net_name(id, inferred_name.to_string())
    }

    /// Unregister a previously registered net from the current module.
    pub(crate) fn unregister_net(&self, id: NetId) {
        self.module.borrow_mut().unregister_net(id)
    }
}
