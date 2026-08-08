//! Peripheral capability model: components declare what their pins *can do*,
//! `pin_solve` performs the joint instance x pin assignment at elaboration,
//! with exclusivity at both tiers structural (infeasible, not lint). Matching
//! is nominal over `interface()` identities, closed over `implies=[...]`;
//! results land in the `pin_assignment` / `swap_classes` module properties.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

use allocative::Allocative;
use pcb_sch::physical::PhysicalValue;
use starlark::collections::SmallMap;
use starlark::environment::MethodsBuilder;
use starlark::eval::Evaluator;
use starlark::values::dict::{AllocDict, DictRef};
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::none::NoneOr;
use starlark::values::typing::TypeInstanceId;
use starlark::values::{
    Coerce, Freeze, Heap, NoSerialize, ProvidesStaticType, StarlarkValue, Trace, Value,
    ValueLifetimeless, ValueLike, starlark_value,
};
use starlark::{starlark_complex_value, starlark_module};

use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::interface::{
    FrozenInterfaceFactory, FrozenInterfaceValue, InterfaceFactory, InterfaceValue,
};
use crate::lang::net::{FrozenNetType, FrozenNetValue, NetType, NetValue};

const REBIND_VALUES: [&str; 3] = ["none", "firmware", "fixed"];
/// Module properties `pin_solve` writes and merges across solves. User code
/// records its own data elsewhere, so the merge always reads back what it
/// wrote.
/// The assignment first, its swap classes second: both `pin_solve` writing
/// them and the builtin refusing them to user code read this one list.
pub(crate) const RESULT_PROPERTIES: [&str; 2] = ["pin_assignment", "swap_classes"];
const PINMAP_CAP: usize = 512;
/// In conflict checks, not search nodes: a node scans up to `PINMAP_CAP`
/// combos per providing instance.
const SOLVER_BUDGET: usize = 2_000_000;
const SOLVER_HARD_BUDGET: usize = 50_000_000;

struct IfaceInfo<'v> {
    id: TypeInstanceId,
    name: String,
    /// Net-typed leaves only, nested interfaces flattened (`Usb2.D` -> `D_P`,
    /// `D_N`). `field()` metadata like `DiffPair.impedance` consumes no pin;
    /// it reaches the router via `convert.rs` instead.
    signals: Vec<String>,
    implies: Vec<Value<'v>>,
    /// Declared capability-attribute vocabulary: name -> physical type value.
    attr_specs: Vec<(String, Value<'v>)>,
}

/// Fields of an interface *type* or *instance*, in declaration order.
fn iface_fields<'v>(v: Value<'v>) -> Option<Vec<(String, Value<'v>)>> {
    fn pairs<'v, V: ValueLike<'v>>(m: &SmallMap<String, V>) -> Vec<(String, Value<'v>)> {
        m.iter().map(|(k, v)| (k.clone(), v.to_value())).collect()
    }
    if let Some(f) = v.downcast_ref::<InterfaceFactory<'v>>() {
        Some(pairs(f.fields()))
    } else if let Some(f) = v.downcast_ref::<FrozenInterfaceFactory>() {
        Some(pairs(f.fields()))
    } else if let Some(i) = v.downcast_ref::<InterfaceValue<'v>>() {
        Some(pairs(i.fields()))
    } else {
        v.downcast_ref::<FrozenInterfaceValue>()
            .map(|i| pairs(i.fields()))
    }
}

/// `at()` constraints for the build, keyed by net: a net keeps its id across
/// module boundaries, so a solve at any depth can claim one.
#[derive(Default)]
pub(crate) struct PinConstraints {
    /// Set when a claim could not be settled; reported by the solve rather
    /// than resolved arbitrarily.
    ambiguous: Option<Ambiguity>,
    entries: Vec<PinConstraintEntry>,
    by_net: HashMap<u64, Vec<usize>>,
    /// Solves that read an `at()` wrapper before its io() bound, keyed by
    /// `(module, input, net)`: each pairs with exactly one later `record` of
    /// that module, so parallel siblings cannot spend each other's.
    preconsumed: HashMap<(Vec<String>, String, u64), usize>,
}

/// Why a claim could not be settled.
enum Ambiguity {
    /// Several inputs' constraints ride the net reaching this input.
    Entries { input: String, names: String },
}

/// What claiming a caller constraint for one io() input yielded.
enum Claimed {
    /// The pins, and whether the `at()` was declared soft.
    Yes(PinLock, bool),
    No,
    /// Several inputs' constraints ride the net; the solve reports it.
    Ambiguous,
    /// Another solve holds it. Recorded on the entry and reported at the
    /// root, so this solve neither errors nor earns a credit.
    Contended,
}

struct PinConstraintEntry {
    input: String,
    module: String,
    /// Where the `at()` was written, so a build error can point at it.
    span: Option<starlark::codemap::ResolvedSpan>,
    /// Instance whose io() carried the wrapper. A solve may claim this entry
    /// only from that instance or below it, so two parts sharing a net each
    /// get their own constraint.
    owner: Vec<String>,
    lock: PinLock,
    soft: bool,
    /// Solve that took this constraint, if any. Only that same solve may take
    /// it again, so re-solving a request keeps its pin while a sibling still
    /// cannot steal it.
    claimant: Option<Vec<String>>,
    /// Solves that wanted it once taken. Children elaborate in parallel, so
    /// the contention is reported at the root, from the `at()` site, rather
    /// than by whichever solve happened to lose the race.
    contested: Vec<Vec<String>>,
}

impl PinConstraints {
    #[allow(clippy::too_many_arguments)]
    fn record(
        &mut self,
        input: String,
        module: String,
        span: Option<starlark::codemap::ResolvedSpan>,
        owner: Vec<String>,
        nets: &[u64],
        lock: PinLock,
        soft: bool,
    ) {
        let key = |n: &u64| (owner.clone(), input.clone(), *n);
        let credited = nets
            .iter()
            .any(|n| self.preconsumed.get(&key(n)).is_some_and(|c| *c > 0));
        if credited {
            for n in nets {
                if let Some(c) = self.preconsumed.get_mut(&key(n)).filter(|c| **c > 0) {
                    *c -= 1;
                }
            }
        }
        // The solve that left the credit is this very module.
        let claimant = credited.then(|| owner.clone());
        let idx = self.entries.len();
        self.entries.push(PinConstraintEntry {
            input,
            module,
            span,
            owner,
            lock,
            soft,
            claimant,
            contested: Vec::new(),
        });
        for n in nets {
            self.by_net.entry(*n).or_default().push(idx);
        }
    }

    /// Settle `input`'s constraint for a solve that read it off the wrapper:
    /// claim the entry when its io() already bound, else leave a credit for the
    /// record still to come. Exactly one of the two, never both.
    ///
    /// Only this module's own entry for this input answers here. A constraint
    /// owned higher up rides the same net but belongs to whoever claims it,
    /// and what it is doing says nothing about whether ours was recorded.
    fn settle(&mut self, nets: &[u64], solver: &[String], input: &str) {
        let mine: Vec<usize> = nets
            .iter()
            .filter_map(|n| self.by_net.get(n))
            .flatten()
            .copied()
            .filter(|i| {
                let e = &self.entries[*i];
                e.owner == solver
                    && e.input == input
                    && e.claimant.as_deref().is_none_or(|c| c == solver)
            })
            .collect();
        if mine.is_empty() {
            // The credit is keyed on this module, this input and this net, so
            // only the record of this very declaration can spend it.
            self.preconsume(solver, input, nets);
            return;
        }
        for i in mine {
            self.entries[i].claimant = Some(solver.to_vec());
        }
    }

    fn preconsume(&mut self, solver: &[String], input: &str, nets: &[u64]) {
        for n in nets {
            *self
                .preconsumed
                .entry((solver.to_vec(), input.to_owned(), *n))
                .or_default() += 1;
        }
    }

    /// Claim the constraint for `input` of the module at `solver`. An entry
    /// owned by that very module must name the same io() input — sibling
    /// inputs often share a net and must not trade constraints. Entries owned
    /// higher up have no such name to match, so the closest one wins and
    /// forwarded constraints keep reaching nested solves. A second solve
    /// wanting one already taken is reported, never served first-come.
    /// Entries owned higher up that this solve could take, with their index.
    /// The caller weighs them with the lock released: doing so runs a `where=`
    /// predicate, and user code must never wait on the design-wide store.
    fn candidate_locks(&self, nets: &[u64], solver: &[String]) -> Vec<(usize, PinLock)> {
        nets.iter()
            .filter_map(|n| self.by_net.get(n))
            .flatten()
            .copied()
            .filter(|i| {
                let e = &self.entries[*i];
                e.owner.len() < solver.len()
                    && solver.starts_with(&e.owner)
                    && e.claimant.as_deref().is_none_or(|c| c == solver)
            })
            .map(|i| (i, self.entries[i].lock.clone()))
            .collect()
    }

    /// `usable` names the entries owned higher up this solve can take; `None`
    /// weighs none of them out.
    fn claim(
        &mut self,
        nets: &[u64],
        solver: &[String],
        input: &str,
        usable: Option<&HashSet<usize>>,
    ) -> Claimed {
        let mut eligible: Vec<usize> = Vec::new();
        let mut held: Vec<usize> = Vec::new();
        for i in nets
            .iter()
            .filter_map(|n| self.by_net.get(n))
            .flatten()
            .copied()
        {
            let e = &self.entries[i];
            if !solver.starts_with(&e.owner) {
                continue;
            }
            // Same module: only this very io() input's entry.
            if e.owner.len() == solver.len() && e.input != input {
                continue;
            }
            // Another solve got here first. Children elaborate in parallel, so
            // leaving it at that would hand the pin to whichever ran first —
            // and this must be read before the pin filter below, or a contest
            // would go unseen whenever the winner claimed it first.
            if e.claimant.as_deref().is_some_and(|c| c != solver) {
                held.push(i);
                continue;
            }
            // Owned higher up there is no name to pair with, so the pins do.
            if e.owner.len() < solver.len() && usable.is_some_and(|set| !set.contains(&i)) {
                continue;
            }
            eligible.push(i);
        }

        // Closest enclosing owner wins; among equals the matching input name
        // decides. Entries are appended by parallel child evaluations, so a
        // remaining tie is genuinely ambiguous rather than order-dependent.
        let Some(depth) = eligible.iter().map(|i| self.entries[*i].owner.len()).max() else {
            return self.contend(held, solver);
        };
        eligible.retain(|i| self.entries[*i].owner.len() == depth);
        if eligible.iter().any(|i| self.entries[*i].input == input) {
            eligible.retain(|i| self.entries[*i].input == input);
        }
        // Re-evaluation records the same declaration more than once, so only
        // distinct input names are a genuine ambiguity.
        let mut names: Vec<&str> = eligible
            .iter()
            .map(|i| self.entries[*i].input.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();
        if names.len() > 1 {
            self.ambiguous = Some(Ambiguity::Entries {
                input: input.to_owned(),
                names: names.join("`, `"),
            });
            return Claimed::Ambiguous;
        }
        let Some(&idx) = eligible.first() else {
            return self.contend(held, solver);
        };
        // The group is one declaration recorded several times; claiming all of
        // it keeps the duplicates from being reported as never consumed.
        for i in &eligible {
            self.entries[*i].claimant = Some(solver.to_vec());
        }
        let e = &self.entries[idx];
        Claimed::Yes(e.lock.clone(), e.soft)
    }

    /// Note that `solver` wanted constraints another solve holds.
    fn contend(&mut self, held: Vec<usize>, solver: &[String]) -> Claimed {
        if held.is_empty() {
            return Claimed::No;
        }
        for i in held {
            let e = &mut self.entries[i];
            if !e.contested.iter().any(|c| c == solver) {
                e.contested.push(solver.to_vec());
            }
        }
        Claimed::Contended
    }

    fn take_ambiguous(&mut self) -> Option<Ambiguity> {
        self.ambiguous.take()
    }

    /// Constraints more than one solve wanted, with every contender sorted:
    /// the winner depends on child-evaluation order, the report must not.
    #[allow(clippy::type_complexity)]
    pub(crate) fn contended(
        &self,
    ) -> Vec<(
        String,
        String,
        Option<starlark::codemap::ResolvedSpan>,
        Vec<String>,
    )> {
        let mut out: Vec<_> = self
            .entries
            .iter()
            .filter(|e| !e.contested.is_empty())
            .map(|e| {
                let mut who: Vec<String> = e
                    .claimant
                    .iter()
                    .chain(e.contested.iter())
                    .map(|p| p.join("."))
                    .collect();
                who.sort();
                who.dedup();
                (e.module.clone(), e.input.clone(), e.span, who)
            })
            .collect();
        out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        out
    }

    /// Hard constraints no solve claimed, as `(module, input, span)`, sorted.
    pub(crate) fn unconsumed_hard(
        &self,
    ) -> Vec<(String, String, Option<starlark::codemap::ResolvedSpan>)> {
        let mut out: Vec<_> = self
            .entries
            .iter()
            .filter(|e| !e.soft && e.claimant.is_none())
            .map(|e| (e.module.clone(), e.input.clone(), e.span))
            .collect();
        out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        out
    }
}

/// Net ids reachable in `v` (a net, or an interface instance's net leaves).
fn collect_net_ids<'v>(v: Value<'v>, out: &mut Vec<u64>) {
    if let Some((id, _)) = net_identity(v) {
        out.push(id);
        return;
    }
    if let Some(fields) = iface_fields(v) {
        for (_, val) in fields {
            collect_net_ids(val, out);
        }
    }
}

/// Net identity (id, display name), if `v` is a net instance.
fn net_identity<'v>(v: Value<'v>) -> Option<(u64, String)> {
    if let Some(n) = v.downcast_ref::<NetValue<'v>>() {
        return Some((n.id(), n.name().to_owned()));
    }
    v.downcast_ref::<FrozenNetValue>()
        .map(|n| (n.id(), n.name().to_owned()))
}

/// A connection field (net type or net instance) as opposed to metadata.
fn is_net_field<'v>(v: Value<'v>) -> bool {
    v.downcast_ref::<NetType<'v>>().is_some()
        || v.downcast_ref::<FrozenNetType>().is_some()
        || v.downcast_ref::<NetValue<'v>>().is_some()
        || v.downcast_ref::<FrozenNetValue>().is_some()
}

/// Net-typed leaves, nested fields flattened into `_`-joined paths. Runs on a
/// type and on an instance alike, so declarations and `pin_map()` agree on the
/// signal names by construction.
fn net_leaves<'v>(v: Value<'v>) -> Vec<(String, Value<'v>)> {
    fn walk<'v>(v: Value<'v>, prefix: &str, out: &mut Vec<(String, Value<'v>)>) {
        let Some(fields) = iface_fields(v) else {
            return;
        };
        for (name, val) in fields {
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}_{name}")
            };
            if is_net_field(val) {
                out.push((path, val));
            } else {
                // Nested interface; anything else is metadata and yields nothing.
                walk(val, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out
}

/// Flattening can collide (`D: DiffPair` next to `D_P: Net`); one signal must
/// not silently shadow the other.
fn check_signal_names(info: &IfaceInfo<'_>) -> anyhow::Result<()> {
    let mut seen = HashSet::new();
    for s in &info.signals {
        if !seen.insert(s.as_str()) {
            return Err(anyhow::anyhow!(
                "interface {}: nested fields flatten to two signals named `{s}` — rename one of them",
                info.name
            ));
        }
    }
    Ok(())
}

/// The build's interface-identity map, or a private one when there is no
/// evaluation context (identities then matter only within the call).
fn interface_ids<'v>(eval: &Evaluator<'v, '_, '_>) -> anyhow::Result<std::sync::Arc<InterfaceIds>> {
    // A private map per call would hand out ids nothing else shares, so two
    // capabilities from one declaration would quietly stop matching.
    eval.eval_context()
        .map(|c| c.interface_ids())
        .ok_or_else(|| anyhow::anyhow!("pinmux: capability identity needs an evaluation context"))
}

/// Nominal interface identities for one build, keyed by declaration site.
/// Session-scoped rather than process-global: sessions run concurrently, and a
/// shared map would let one build hand another a stale id.
pub(crate) type InterfaceIds = Mutex<HashMap<(String, String), TypeInstanceId>>;

/// Identity of an interface *declaration* (file + exported name), so a file
/// evaluated twice — the load cache is per package scope — stays one type.
/// An interface never bound to a name keeps its per-evaluation identity.
fn nominal_id(
    ids: &InterfaceIds,
    declaration_path: &str,
    name: Option<String>,
    evaluation_id: TypeInstanceId,
) -> TypeInstanceId {
    let Some(name) = name else {
        return evaluation_id;
    };
    *ids.lock()
        .unwrap()
        .entry((declaration_path.to_owned(), name))
        .or_insert_with(TypeInstanceId::r#gen)
}

fn iface_info<'v>(v: Value<'v>, ids: &InterfaceIds) -> Option<IfaceInfo<'v>> {
    let signals = || net_leaves(v).into_iter().map(|(k, _)| k).collect();
    if let Some(f) = v.downcast_ref::<InterfaceFactory<'v>>() {
        return Some(IfaceInfo {
            id: nominal_id(ids, f.declaration_path(), f.type_name(), f.evaluation_id()),
            name: f.type_name().unwrap_or_else(|| v.to_string()),
            signals: signals(),
            implies: f.implies().iter().map(|x| x.to_value()).collect(),
            attr_specs: f
                .attr_spec()
                .iter()
                .map(|(k, t)| (k.clone(), t.to_value()))
                .collect(),
        });
    }
    if let Some(f) = v.downcast_ref::<FrozenInterfaceFactory>() {
        return Some(IfaceInfo {
            id: nominal_id(ids, f.declaration_path(), f.type_name(), f.evaluation_id()),
            name: f.type_name().unwrap_or_else(|| v.to_string()),
            signals: signals(),
            implies: f.implies().iter().map(|x| x.to_value()).collect(),
            attr_specs: f
                .attr_spec()
                .iter()
                .map(|(k, t)| (k.clone(), t.to_value()))
                .collect(),
        });
    }
    None
}

/// BFS closure of an interface type over `implies`, deduplicated by nominal id.
fn iface_closure<'v>(root: Value<'v>, ids: &InterfaceIds) -> anyhow::Result<Vec<IfaceInfo<'v>>> {
    let root_info = iface_info(root, ids)
        .ok_or_else(|| anyhow::anyhow!("expected an interface type, got `{}`", root.get_type()))?;
    check_signal_names(&root_info)?;
    let mut seen: HashSet<TypeInstanceId> = HashSet::new();
    seen.insert(root_info.id);
    let mut out = vec![root_info];
    let mut cursor = 0;
    while cursor < out.len() {
        let implied: Vec<Value<'v>> = out[cursor].implies.clone();
        cursor += 1;
        for iv in implied {
            let info = iface_info(iv, ids).ok_or_else(|| {
                anyhow::anyhow!("interface(implies=...) entry is not an interface type")
            })?;
            if seen.insert(info.id) {
                check_signal_names(&info)?;
                out.push(info);
            }
        }
    }
    Ok(out)
}

fn dict_get<'v>(d: &DictRef<'v>, heap: Heap<'v>, key: &str) -> Option<Value<'v>> {
    d.get(heap.alloc(key)).ok().flatten()
}

fn dict_get_str<'v>(d: &DictRef<'v>, heap: Heap<'v>, key: &str) -> Option<String> {
    dict_get(d, heap, key).and_then(|v| v.unpack_str().map(|s| s.to_owned()))
}

fn is_kind<'v>(v: Value<'v>, heap: Heap<'v>, kind: &str) -> bool {
    DictRef::from_value(v)
        .map(|d| dict_get_str(&d, heap, "kind").as_deref() == Some(kind))
        .unwrap_or(false)
}

#[derive(Clone, Debug)]
struct RPin {
    name: String,
    /// Opaque realization data (e.g. an STM32 AF index, an ESP32 IOMUX FUNC
    /// number): whatever downstream tooling needs to realize this candidate.
    /// Never consulted by the solver; carried verbatim into the assignment.
    data: Vec<(String, serde_json::Value)>,
    cost: i64,
    input_only: bool,
    strap: bool,
}

struct RPeriph<'v> {
    name: String,
    /// Component this peripheral's pads belong to (`peripheral(part=)`).
    part: String,
    /// Index of `part` in the solve's part list.
    part_idx: usize,
    provides_ids: HashSet<TypeInstanceId>,
    /// Display names of everything provided, for diagnostics only.
    provides_names: HashSet<String>,
    signals: Vec<(String, Vec<RPin>)>,
    rebind: String,
    attrs: Value<'v>,
    unless: Option<String>,
    pool: Option<String>,
}

impl RPeriph<'_> {
    fn signal(&self, name: &str) -> Option<&Vec<RPin>> {
        self.signals.iter().find(|(s, _)| s == name).map(|(_, p)| p)
    }

    /// Provides-set key for cluster swap grouping.
    fn provides_key(&self) -> ProvidesKey {
        let mut ids: Vec<String> = self.provides_ids.iter().map(|i| format!("{i:?}")).collect();
        ids.sort();
        ProvidesKey(ids.join(","))
    }
}

/// Which capabilities a peripheral provides, as one comparable value. Built
/// from `TypeInstanceId`s, minted per build: it tells clusters apart within a
/// solve and means nothing outside it, so it is opaque — neither serializable
/// nor orderable, which keeps it out of emitted data and of sort keys.
#[derive(Clone, PartialEq, Eq, Hash)]
struct ProvidesKey(String);

#[derive(Clone)]
struct RReq<'v> {
    name: String,
    iface_id: TypeInstanceId,
    iface_name: String,
    iface_closure_len: usize,
    uses: Vec<String>,
    instance: Option<String>,
    prefer: PinLock,
    lock: bool,
    /// A caller's soft `at()` when the request is locked: it picks among the
    /// pins the module made mandatory instead of voiding them.
    soft_prefer: PinLock,
    where_fn: Option<Value<'v>>,
    direction: Option<String>,
    /// Serve this request only when the module input of the same name was
    /// actually connected by the caller (io() slot pattern).
    if_connected: bool,
    /// Value bound on the request itself (dict-of-roles pattern).
    bind: Option<Value<'v>>,
}

impl RReq<'_> {
    /// Pins `signal` may take when the request is locked: its own list, else
    /// the bare list once it names as many pins as there are signals — with
    /// pads exclusive, covering them all is the same requirement.
    fn allowed(&self, signal: &str) -> Option<&[String]> {
        if !self.lock {
            return None;
        }
        let covering = !self.prefer.any.is_empty() && self.prefer.any.len() == self.uses.len();
        self.prefer
            .allowed(signal)
            .or_else(|| covering.then_some(self.prefer.any.as_slice()))
    }

    /// Whether the lock holds `pin` where it is: a bare list must be claimed
    /// in full, so the signal sitting on one of its pins cannot move without
    /// dropping it.
    fn pinned_by_lock(&self, pin: &str) -> bool {
        self.lock && self.prefer.any.iter().any(|p| p == pin)
    }

    /// How much this request would rather have `pin`: what the request asked
    /// for, and what a caller wished on top. A wish outranks the request's own
    /// preference, and the two add up when they name the same pin.
    fn bias(&self, pin: &str) -> i64 {
        let mut b = 0;
        if self.prefer.mentions(pin) {
            b -= 10;
        }
        if self.soft_prefer.mentions(pin) {
            b -= 25;
        }
        b
    }
}

#[derive(Clone)]
struct Combo {
    periph_idx: usize,
    pins: Vec<(String, RPin)>,
    cost: i64,
    key: String,
}

struct PrevAssign {
    instance: String,
    pins: HashMap<String, String>,
}

fn parse_rpin<'v>(v: Value<'v>, heap: Heap<'v>, ctx: &str) -> anyhow::Result<RPin> {
    if let Some(s) = v.unpack_str() {
        return Ok(RPin {
            name: s.to_owned(),
            data: Vec::new(),
            cost: 0,
            input_only: false,
            strap: false,
        });
    }
    let d = DictRef::from_value(v)
        .ok_or_else(|| anyhow::anyhow!("{ctx}: pin candidate must be pin() or a string"))?;
    if dict_get_str(&d, heap, "kind").as_deref() != Some("pin") {
        return Err(anyhow::anyhow!(
            "{ctx}: pin candidate must be pin() or a string"
        ));
    }
    let mut data = Vec::new();
    if let Some(dv) = dict_get(&d, heap, "data")
        && let Some(dd) = DictRef::from_value(dv)
    {
        for (k, val) in dd.iter() {
            let key = k
                .unpack_str()
                .ok_or_else(|| anyhow::anyhow!("{ctx}: pin() data keys must be strings"))?;
            if key == "pin" {
                return Err(anyhow::anyhow!(
                    "{ctx}: pin() data key `pin` is reserved (it names the pin itself in the assignment)"
                ));
            }
            let json = val
                .to_json()
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .ok_or_else(|| {
                    anyhow::anyhow!("{ctx}: pin() data value for `{key}` is not serializable")
                })?;
            data.push((key.to_owned(), json));
        }
    }
    Ok(RPin {
        name: dict_get_str(&d, heap, "name")
            .ok_or_else(|| anyhow::anyhow!("{ctx}: pin() missing name"))?,
        data,
        cost: dict_get(&d, heap, "cost")
            .and_then(|v| v.unpack_i32())
            .unwrap_or(0) as i64,
        input_only: dict_get(&d, heap, "input_only")
            .map(|v| v.to_bool())
            .unwrap_or(false),
        strap: dict_get(&d, heap, "strap")
            .map(|v| v.to_bool())
            .unwrap_or(false),
    })
}

fn parse_rperiph<'v>(
    v: Value<'v>,
    heap: Heap<'v>,
    ids: &InterfaceIds,
) -> anyhow::Result<RPeriph<'v>> {
    let d = DictRef::from_value(v).ok_or_else(|| {
        anyhow::anyhow!("pin_solve: peripherals must be peripheral()/pool() values")
    })?;
    if dict_get_str(&d, heap, "kind").as_deref() != Some("peripheral") {
        return Err(anyhow::anyhow!(
            "pin_solve: peripherals must be peripheral()/pool() values"
        ));
    }
    let name = dict_get_str(&d, heap, "name")
        .ok_or_else(|| anyhow::anyhow!("pin_solve: peripheral missing name"))?;
    let provides = dict_get(&d, heap, "provides")
        .ok_or_else(|| anyhow::anyhow!("peripheral `{name}`: missing provides"))?;
    let provides_list = ListRef::from_value(provides)
        .ok_or_else(|| anyhow::anyhow!("peripheral `{name}`: provides must be a list"))?;
    let mut provides_ids = HashSet::new();
    let mut provides_names = HashSet::new();
    for p in provides_list.iter() {
        for info in iface_closure(p, ids)? {
            provides_ids.insert(info.id);
            provides_names.insert(info.name.clone());
        }
    }
    let signals_v = dict_get(&d, heap, "signals")
        .ok_or_else(|| anyhow::anyhow!("peripheral `{name}`: missing signals"))?;
    let signals_d = DictRef::from_value(signals_v)
        .ok_or_else(|| anyhow::anyhow!("peripheral `{name}`: signals must be a dict"))?;
    let mut signals = Vec::new();
    for (k, cand_list) in signals_d.iter() {
        let sig = k
            .unpack_str()
            .ok_or_else(|| anyhow::anyhow!("peripheral `{name}`: signal names must be strings"))?
            .to_owned();
        let cl = ListRef::from_value(cand_list).ok_or_else(|| {
            anyhow::anyhow!("peripheral `{name}`: signal `{sig}` candidates must be a list")
        })?;
        let mut pins = Vec::new();
        for c in cl.iter() {
            pins.push(parse_rpin(
                c,
                heap,
                &format!("peripheral `{name}` signal `{sig}`"),
            )?);
        }
        signals.push((sig, pins));
    }
    Ok(RPeriph {
        name,
        part: dict_get_str(&d, heap, "part").unwrap_or_default(),
        part_idx: 0,
        provides_ids,
        provides_names,
        signals,
        rebind: dict_get_str(&d, heap, "rebind").unwrap_or_else(|| "firmware".to_owned()),
        attrs: dict_get(&d, heap, "attrs").unwrap_or_else(Value::new_none),
        unless: dict_get_str(&d, heap, "unless"),
        pool: dict_get_str(&d, heap, "pool"),
    })
}

fn parse_rreq<'v>(v: Value<'v>, heap: Heap<'v>, ids: &InterfaceIds) -> anyhow::Result<RReq<'v>> {
    let d = DictRef::from_value(v)
        .ok_or_else(|| anyhow::anyhow!("pin_solve: requests must be pin_request() values"))?;
    if dict_get_str(&d, heap, "kind").as_deref() != Some("request") {
        return Err(anyhow::anyhow!(
            "pin_solve: requests must be pin_request() values"
        ));
    }
    let name = dict_get_str(&d, heap, "name")
        .ok_or_else(|| anyhow::anyhow!("pin_solve: request missing name"))?;
    let iface = dict_get(&d, heap, "iface")
        .ok_or_else(|| anyhow::anyhow!("request `{name}`: missing interface"))?;
    let closure = iface_closure(iface, ids)?;
    let uses = match dict_get(&d, heap, "uses") {
        Some(u) if !u.is_none() => ListRef::from_value(u)
            .ok_or_else(|| anyhow::anyhow!("request `{name}`: uses must be a list"))?
            .iter()
            .map(|s| {
                s.unpack_str().map(|s| s.to_owned()).ok_or_else(|| {
                    anyhow::anyhow!("request `{name}`: uses entries must be strings")
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        _ => closure[0].signals.clone(),
    };
    if uses.is_empty() {
        return Err(anyhow::anyhow!(
            "request `{name}`: uses must name at least one signal"
        ));
    }
    let mut seen_uses = HashSet::new();
    for s in &uses {
        if !seen_uses.insert(s.as_str()) {
            return Err(anyhow::anyhow!(
                "request `{name}`: duplicate signal `{s}` in uses"
            ));
        }
    }
    let prefer_by_signal = match dict_get(&d, heap, "prefer_by_signal") {
        Some(v) if !v.is_none() => {
            let dd = DictRef::from_value(v).ok_or_else(|| {
                anyhow::anyhow!("request `{name}`: prefer_by_signal must be a dict")
            })?;
            let mut out = Vec::new();
            for (k, val) in dd.iter() {
                let sig = k
                    .unpack_str()
                    .ok_or_else(|| anyhow::anyhow!("request `{name}`: signal must be a string"))?;
                let list = ListRef::from_value(val).ok_or_else(|| {
                    anyhow::anyhow!("request `{name}`: prefer_by_signal values must be lists")
                })?;
                let pins: Vec<String> = list
                    .iter()
                    .map(|e| {
                        e.unpack_str().map(|s| s.to_owned()).ok_or_else(|| {
                            anyhow::anyhow!("request `{name}`: pin names must be strings")
                        })
                    })
                    .collect::<anyhow::Result<Vec<_>>>()?;
                out.push((sig.to_owned(), pins));
            }
            out
        }
        _ => Vec::new(),
    };
    let prefer_any = match dict_get(&d, heap, "prefer") {
        Some(p) if !p.is_none() => ListRef::from_value(p)
            .ok_or_else(|| anyhow::anyhow!("request `{name}`: prefer must be a list"))?
            .iter()
            .map(|s| {
                s.unpack_str().map(|s| s.to_owned()).ok_or_else(|| {
                    anyhow::anyhow!("request `{name}`: prefer entries must be strings")
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        _ => Vec::new(),
    };
    let prefer = PinLock {
        any: prefer_any,
        by_signal: prefer_by_signal,
    };
    Ok(RReq {
        name,
        iface_id: closure[0].id,
        iface_name: closure[0].name.clone(),
        iface_closure_len: closure.len(),
        uses,
        instance: dict_get_str(&d, heap, "instance"),
        prefer,
        lock: dict_get(&d, heap, "lock")
            .map(|v| v.to_bool())
            .unwrap_or(false),
        soft_prefer: PinLock::default(),
        where_fn: dict_get(&d, heap, "where").filter(|v| !v.is_none()),
        direction: dict_get_str(&d, heap, "direction"),
        if_connected: dict_get(&d, heap, "if_connected")
            .map(|v| v.to_bool())
            .unwrap_or(false),
        bind: dict_get(&d, heap, "bind").filter(|v| !v.is_none()),
    })
}

fn parse_previous<'v>(v: Value<'v>, heap: Heap<'v>) -> HashMap<String, PrevAssign> {
    let mut out = HashMap::new();
    let Some(d) = DictRef::from_value(v) else {
        return out;
    };
    for (k, entry) in d.iter() {
        let (Some(req), Some(ed)) = (k.unpack_str(), DictRef::from_value(entry)) else {
            continue;
        };
        let Some(instance) = dict_get_str(&ed, heap, "instance") else {
            continue;
        };
        let mut pins = HashMap::new();
        if let Some(sigs) = dict_get(&ed, heap, "signals").and_then(DictRef::from_value) {
            for (s, pv) in sigs.iter() {
                if let (Some(sig), Some(pd)) = (s.unpack_str(), DictRef::from_value(pv))
                    && let Some(pin) = dict_get_str(&pd, heap, "pin")
                {
                    pins.insert(sig.to_owned(), pin);
                }
            }
        }
        out.insert(req.to_owned(), PrevAssign { instance, pins });
    }
    out
}

fn config_truthy<'v>(config: &SmallMap<String, Value<'v>>, key: &str) -> bool {
    config.get(key).map(|v| v.to_bool()).unwrap_or(false)
}

#[allow(clippy::type_complexity)]
fn combos_for_request<'v>(
    req: &RReq<'v>,
    periphs: &[RPeriph<'v>],
    config: &SmallMap<String, Value<'v>>,
    previous: &HashMap<String, PrevAssign>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> anyhow::Result<(Vec<Combo>, Vec<(String, String)>, Vec<String>)> {
    let mut out: Vec<Combo> = Vec::new();
    let mut capped_periphs: Vec<String> = Vec::new();
    let mut rejects: Vec<(String, String)> = Vec::new();
    let prev = previous.get(&req.name);

    for (pi, p) in periphs.iter().enumerate() {
        if let Some(axis) = &p.unless
            && config_truthy(config, axis)
        {
            rejects.push((p.name.clone(), format!("disabled by config axis `{axis}`")));
            continue;
        }
        if !p.provides_ids.contains(&req.iface_id) {
            // Same display name, different identity: two `interface()` calls,
            // which are two capabilities however alike they look.
            let reason = if p.provides_names.contains(&req.iface_name) {
                format!(
                    "provides a different {} — capability types are per interface() declaration, so two declarations never match",
                    req.iface_name
                )
            } else {
                format!("does not provide {}", req.iface_name)
            };
            rejects.push((p.name.clone(), reason));
            continue;
        }
        if let Some(inst) = &req.instance
            && p.name != *inst
            && p.pool.as_deref() != Some(inst.as_str())
        {
            rejects.push((p.name.clone(), format!("instance pinned to `{inst}`")));
            continue;
        }
        if let Some(where_fn) = req.where_fn {
            // A predicate that trips over an attr this provider does not set
            // rejects the candidate; it must not sink the whole solve.
            match eval.eval_function(where_fn, &[p.attrs], &[]) {
                Ok(v) if v.to_bool() => {}
                Ok(_) => {
                    rejects.push((p.name.clone(), "where= predicate rejected".to_owned()));
                    continue;
                }
                Err(e) => {
                    let msg = e.to_string();
                    let first = msg.lines().next().unwrap_or("error").trim().to_owned();
                    rejects.push((p.name.clone(), format!("where= predicate failed: {first}")));
                    continue;
                }
            }
        }

        // Candidate pins per used signal (direction-filtered).
        let mut cand_lists: Vec<(&str, Vec<&RPin>)> = Vec::new();
        let mut dead: Option<&str> = None;
        for s in &req.uses {
            let Some(cands) = p.signal(s) else {
                dead = Some(s);
                break;
            };
            let usable: Vec<&RPin> = cands
                .iter()
                .filter(|c| !(req.direction.as_deref() == Some("output") && c.input_only))
                .collect();
            if usable.is_empty() {
                dead = Some(s);
                break;
            }
            cand_lists.push((s, usable));
        }
        if let Some(s) = dead {
            rejects.push((
                p.name.clone(),
                format!("no usable candidate for signal `{s}`"),
            ));
            continue;
        }

        // Narrow the enumeration where the lock allows it: a per-signal entry
        // restricts that signal exactly, `any` pins only when they must fill
        // every signal. Otherwise they are merely enumerated first, so a wide
        // matrix can still truncate away a feasible combination — the `capped`
        // warning covers that case.
        let mut starved: Option<(String, Vec<String>)> = None;
        {
            for (sig, cands) in cand_lists.iter_mut() {
                let allowed: Option<&[String]> = req.allowed(sig);
                if let Some(allowed) = allowed {
                    *cands = cands
                        .iter()
                        .copied()
                        .filter(|c| allowed.contains(&c.name))
                        .collect();
                    if cands.is_empty() && starved.is_none() {
                        starved = Some(((*sig).to_owned(), allowed.to_vec()));
                    }
                }
            }
        }
        if !(req.prefer.is_empty() && req.soft_prefer.is_empty()) {
            for (_, cands) in cand_lists.iter_mut() {
                cands.sort_by_key(|c| req.bias(&c.name));
            }
        }

        // Enumerate pin maps (product), excluding duplicate pins within the instance.
        let mut pinmaps: Vec<Vec<(String, RPin)>> = vec![Vec::new()];
        let mut truncated = false;
        for (s, cands) in &cand_lists {
            let mut next = Vec::new();
            'bases: for base in &pinmaps {
                let usable = cands
                    .iter()
                    .filter(|c| !base.iter().any(|(_, b)| b.name == c.name));
                for c in usable {
                    if next.len() >= PINMAP_CAP {
                        truncated = true;
                        break 'bases;
                    }
                    let mut d = base.clone();
                    d.push(((*s).to_owned(), (*c).clone()));
                    next.push(d);
                }
            }
            pinmaps = next;
        }
        if truncated {
            capped_periphs.push(p.name.clone());
        }

        let surplus = p.provides_ids.len() as i64 - req.iface_closure_len as i64;
        let had_pinmaps = !pinmaps.is_empty();
        let combos_before = out.len();
        for pm in pinmaps {
            // A bare pin list must be claimed in full, whatever the signal
            // count; a per-signal entry bounds that signal's choice.
            if req.lock && !req.prefer.is_empty() {
                let all_claimed = req
                    .prefer
                    .any
                    .iter()
                    .all(|p| pm.iter().any(|(_, c)| c.name == *p));
                let per_signal_ok = req.prefer.by_signal.iter().all(|(sig, allowed)| {
                    pm.iter()
                        .find(|(s, _)| s == sig)
                        .is_none_or(|(_, c)| allowed.contains(&c.name))
                });
                if !(all_claimed && per_signal_ok) {
                    continue;
                }
            }
            let mut cost = 100 * surplus;
            for (sig, c) in &pm {
                cost += c.cost;
                if c.strap {
                    cost += 50;
                }
                cost += req.bias(&c.name);
                if let Some(prev) = prev
                    && prev.pins.get(sig) == Some(&c.name)
                {
                    cost -= 5;
                }
            }
            if let Some(prev) = prev
                && prev.instance == p.name
            {
                cost -= 20;
            }
            let key = pm
                .iter()
                .map(|(s, c)| format!("{s}={}", c.name))
                .collect::<Vec<_>>()
                .join(",");
            out.push(Combo {
                periph_idx: pi,
                pins: pm,
                cost,
                key,
            });
        }
        if out.len() == combos_before {
            let reason = if let Some((sig, allowed)) = &starved {
                format!(
                    "signal `{sig}` has no candidate among the locked pins `{}`",
                    allowed.join("`, `")
                )
            } else if had_pinmaps {
                format!(
                    "no pin combination satisfies the locked pins `{}`",
                    req.prefer.names().join("`, `")
                )
            } else {
                "candidate pins collide within the instance".to_owned()
            };
            rejects.push((p.name.clone(), reason));
        }
    }

    out.sort_by(|a, b| {
        (a.cost, &periphs[a.periph_idx].name, &a.key).cmp(&(
            b.cost,
            &periphs[b.periph_idx].name,
            &b.key,
        ))
    });
    Ok((out, rejects, capped_periphs))
}

enum AssignOutcome {
    Solved {
        chosen: Vec<usize>,
        capped: bool,
    },
    Infeasible,
    /// Hard ceiling hit with no incumbent: feasibility neither proven nor
    /// refuted — reported as such, never as infeasibility.
    Unknown,
}

/// Deterministic branch-and-bound over the joint assignment space (pin
/// conflicts couple otherwise independent instance choices, so this is
/// constraint optimization, not plain matching). Candidates are explored in
/// per-request cost order and pruned with an admissible lower bound, so the
/// result minimizes total cost. `SOLVER_BUDGET` only bounds the search for a
/// cheaper solution once one exists (`capped` reports the cut); with no
/// incumbent the search runs to the first feasible assignment, proven
/// infeasibility, or the unconditional `SOLVER_HARD_BUDGET` ceiling
/// (`Unknown`) so pathological instances cannot hang the build.
fn assign<'v>(reqs: &[RReq<'v>], all_combos: &[Vec<Combo>], part_of: &[usize]) -> AssignOutcome {
    let n = reqs.len();
    if n == 0 {
        return AssignOutcome::Solved {
            chosen: Vec::new(),
            capped: false,
        };
    }
    // Evaluation order: locked first, then fewest candidates, then declaration order.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (!reqs[i].lock, all_combos[i].len(), i));

    // Admissible bound: combos are cost-sorted, so index 0 is each request's floor.
    // suffix_min[k] = total floor of everything not yet placed at position k.
    let mut suffix_min = vec![0i64; n + 1];
    for k in (0..n).rev() {
        suffix_min[k] = suffix_min[k + 1] + all_combos[order[k]][0].cost;
    }

    // Pads interned once: the conflict scan below runs up to the hard budget,
    // so it must not allocate. `pads[ri][ci]` holds a combo's pad ids, which
    // already carry the part a pin name belongs to.
    let mut ids: HashMap<(usize, &str), usize> = HashMap::new();
    let pads: Vec<Vec<Vec<usize>>> = all_combos
        .iter()
        .map(|combos| {
            combos
                .iter()
                .map(|c| {
                    let part = part_of[c.periph_idx];
                    c.pins
                        .iter()
                        .map(|(_, p)| {
                            let next = ids.len();
                            *ids.entry((part, p.name.as_str())).or_insert(next)
                        })
                        .collect()
                })
                .collect()
        })
        .collect();
    let mut used_pad = vec![false; ids.len()];

    let mut choice = vec![0usize; n];
    // chosen combo index per request (by original request index)
    let mut assigned: Vec<Option<usize>> = vec![None; n];
    let mut partial_cost = vec![0i64; n + 1];
    let mut used_inst: HashSet<usize> = HashSet::new();
    let mut best: Option<(i64, Vec<usize>)> = None;
    let mut pos: usize = 0;
    let mut spent = 0usize;
    let mut capped = false;

    loop {
        if spent >= SOLVER_BUDGET && best.is_some() {
            capped = true;
            break;
        }
        if spent >= SOLVER_HARD_BUDGET {
            return AssignOutcome::Unknown;
        }
        spent += 1;
        if pos == n {
            let total = partial_cost[n];
            // Strict improvement keeps the first (deterministic) optimum.
            if best.as_ref().map(|(b, _)| total < *b).unwrap_or(true) {
                best = Some((total, assigned.iter().map(|c| c.unwrap()).collect()));
            }
            // Keep searching for a cheaper solution: force a backtrack.
            pos -= 1;
            let back_ri = order[pos];
            let back_ci = assigned[back_ri].unwrap();
            used_inst.remove(&all_combos[back_ri][back_ci].periph_idx);
            for &pad in &pads[back_ri][back_ci] {
                used_pad[pad] = false;
            }
            assigned[back_ri] = None;
            choice[pos] += 1;
            continue;
        }
        let ri = order[pos];
        let combos = &all_combos[ri];
        let mut placed = false;
        let mut ci = choice[pos];
        while ci < combos.len() {
            let c = &combos[ci];
            // Cost-sorted candidates: once the bound reaches the incumbent,
            // every later candidate in this subtree is at least as expensive.
            if let Some((b, _)) = &best
                && partial_cost[pos] + c.cost + suffix_min[pos + 1] >= *b
            {
                break;
            }
            // Charged per check, not per node: this scan is where the time goes.
            spent += 1;
            let clash =
                used_inst.contains(&c.periph_idx) || pads[ri][ci].iter().any(|&pad| used_pad[pad]);
            if !clash {
                choice[pos] = ci;
                assigned[ri] = Some(ci);
                used_inst.insert(c.periph_idx);
                for &pad in &pads[ri][ci] {
                    used_pad[pad] = true;
                }
                partial_cost[pos + 1] = partial_cost[pos] + c.cost;
                placed = true;
                break;
            }
            ci += 1;
        }
        if placed {
            pos += 1;
            if pos < n {
                choice[pos] = 0;
            }
        } else {
            choice[pos] = 0;
            if pos == 0 {
                break;
            }
            pos -= 1;
            let back_ri = order[pos];
            let back_ci = assigned[back_ri].unwrap();
            used_inst.remove(&all_combos[back_ri][back_ci].periph_idx);
            for &pad in &pads[back_ri][back_ci] {
                used_pad[pad] = false;
            }
            assigned[back_ri] = None;
            choice[pos] += 1;
        }
    }
    match best {
        Some((_, chosen)) => AssignOutcome::Solved { chosen, capped },
        None => AssignOutcome::Infeasible,
    }
}

fn json_to_value<'v>(heap: Heap<'v>, v: &serde_json::Value) -> Value<'v> {
    match v {
        serde_json::Value::Null => Value::new_none(),
        serde_json::Value::Bool(b) => Value::new_bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                heap.alloc(i)
            } else if let Some(u) = n.as_u64() {
                // Vendor data above i64::MAX still round-trips as an integer.
                heap.alloc(u)
            } else {
                heap.alloc(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => heap.alloc(s.as_str()),
        serde_json::Value::Array(items) => {
            let vals: Vec<Value> = items.iter().map(|i| json_to_value(heap, i)).collect();
            heap.alloc(vals)
        }
        serde_json::Value::Object(map) => {
            let pairs: Vec<(Value, Value)> = map
                .iter()
                .map(|(k, val)| (heap.alloc(k.as_str()), json_to_value(heap, val)))
                .collect();
            heap.alloc(AllocDict(pairs))
        }
    }
}

fn warn_at_call_site(eval: &mut Evaluator<'_, '_, '_>, msg: String) {
    if let Some(loc) = eval.call_stack_top_location() {
        let mut diag =
            starlark::errors::EvalMessage::from_any_error(Path::new(loc.filename()), &msg);
        diag.span = Some(loc.resolve_span());
        diag.severity = starlark::errors::EvalSeverity::Warning;
        eval.add_diagnostic(diag);
    }
}

/// Wrapper produced by `at(value, pins, soft=)`: the caller constrains which
/// physical pin(s) a capability may land on, on the connection itself
/// (`Mcu(IO0 = at(led_net, "PA8"))`). `io()` unwraps it at binding time: the
/// inner value flows as if passed directly, and the constraint is recorded on
/// the module context for `pin_solve` to consume.
#[derive(Clone, Debug, Coerce, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct PinAtGen<V: ValueLifetimeless> {
    pub inner: V,
    /// Where `at()` was written, so an unmet constraint points at the caller.
    pub site: String,
    #[freeze(identity)]
    #[trace(unsafe_ignore)]
    #[allocative(skip)]
    pub span: Option<starlark::codemap::ResolvedSpan>,
    pub pins: Vec<String>,
    pub pins_by_signal: Vec<(String, Vec<String>)>,
    pub soft: bool,
}

starlark_complex_value!(pub PinAt);

#[starlark_value(type = "PinAt")]
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for PinAtGen<V> where Self: ProvidesStaticType<'v> {}

impl<'v, V: ValueLike<'v>> std::fmt::Display for PinAtGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "at({}, {:?})", self.inner, self.pins)
    }
}

/// Pins a request is pinned to. `any` names pins that must each be claimed by
/// some signal; `by_signal` bounds what one named signal may take.
#[derive(Clone, Debug, Default)]
struct PinLock {
    any: Vec<String>,
    by_signal: Vec<(String, Vec<String>)>,
}

impl PinLock {
    fn is_empty(&self) -> bool {
        self.any.is_empty() && self.by_signal.is_empty()
    }

    fn allowed(&self, signal: &str) -> Option<&[String]> {
        self.by_signal
            .iter()
            .find(|(s, _)| s == signal)
            .map(|(_, p)| p.as_slice())
    }

    /// Does `pin` appear anywhere in the lock? Drives the cost bias.
    fn mentions(&self, pin: &str) -> bool {
        self.any.iter().any(|p| p == pin)
            || self
                .by_signal
                .iter()
                .any(|(_, ps)| ps.iter().any(|p| p == pin))
    }

    /// Every per-signal entry must name a signal the request actually uses,
    /// or the requirement would quietly evaporate at combo-check time.
    fn check_signals(&self, ctx: &str, uses: &[String]) -> anyhow::Result<()> {
        for (sig, _) in &self.by_signal {
            if !uses.iter().any(|u| u == sig) {
                return Err(anyhow::anyhow!(
                    "{ctx}: pin constraint names signal `{sig}`, which this request does not use (it uses {})",
                    uses.iter()
                        .map(|s| format!("`{s}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        Ok(())
    }

    fn names(&self) -> Vec<String> {
        let mut out = self.any.clone();
        for (s, ps) in &self.by_signal {
            for p in ps {
                out.push(format!("{s}={p}"));
            }
        }
        out
    }
}

/// `(pins to claim, per-signal allowed pins)`.
type PinSpec = (Vec<String>, Vec<(String, Vec<String>)>);

/// A pin spec: a pin name, a list of pin names (each must be claimed), or a
/// dict of signal -> pin(s) bounding one signal's choice.
fn parse_pin_spec<'v>(ctx: &str, v: Value<'v>, heap: Heap<'v>) -> anyhow::Result<PinSpec> {
    fn names(ctx: &str, v: Value<'_>) -> anyhow::Result<Vec<String>> {
        if let Some(s) = v.unpack_str() {
            return Ok(vec![s.to_owned()]);
        }
        let list = ListRef::from_value(v).ok_or_else(|| {
            anyhow::anyhow!(
                "{ctx}: expected a pin name or a list of pin names, got `{}`",
                v.get_type()
            )
        })?;
        list.iter()
            .map(|e| {
                e.unpack_str()
                    .map(|s| s.to_owned())
                    .ok_or_else(|| anyhow::anyhow!("{ctx}: pin names must be strings"))
            })
            .collect()
    }

    if let Some(d) = DictRef::from_value(v) {
        let mut by_signal = Vec::new();
        for (k, val) in d.iter() {
            let sig = k
                .unpack_str()
                .ok_or_else(|| anyhow::anyhow!("{ctx}: signal names must be strings"))?;
            let pins = names(ctx, val)?;
            if pins.is_empty() {
                return Err(anyhow::anyhow!("{ctx}: signal `{sig}` lists no pin"));
            }
            by_signal.push((sig.to_owned(), pins));
        }
        if by_signal.is_empty() {
            return Err(anyhow::anyhow!("{ctx}: pins must not be empty"));
        }
        let _ = heap;
        return Ok((Vec::new(), by_signal));
    }
    let any = names(ctx, v)?;
    if any.is_empty() {
        return Err(anyhow::anyhow!("{ctx}: pins must not be empty"));
    }
    Ok((any, Vec::new()))
}

/// `(inner, lock, soft, site, span)` of an `at()` wrapper, if `v` is one.
#[allow(clippy::type_complexity)]
fn unpack_pin_at_full<'v>(
    v: Value<'v>,
) -> Option<(
    Value<'v>,
    PinLock,
    bool,
    String,
    Option<starlark::codemap::ResolvedSpan>,
)> {
    if let Some(w) = v.downcast_ref::<PinAt<'v>>() {
        return Some((
            w.inner.to_value(),
            PinLock {
                any: w.pins.clone(),
                by_signal: w.pins_by_signal.clone(),
            },
            w.soft,
            w.site.clone(),
            w.span,
        ));
    }
    v.downcast_ref::<FrozenPinAt>().map(|w| {
        (
            w.inner.to_value(),
            PinLock {
                any: w.pins.clone(),
                by_signal: w.pins_by_signal.clone(),
            },
            w.soft,
            w.site.clone(),
            w.span,
        )
    })
}

/// `(inner, lock, soft)` of an `at()` wrapper, dropping its call site.
fn unpack_pin_at<'v>(v: Value<'v>) -> Option<(Value<'v>, PinLock, bool)> {
    if let Some(w) = v.downcast_ref::<PinAt<'v>>() {
        Some((
            w.inner.to_value(),
            PinLock {
                any: w.pins.clone(),
                by_signal: w.pins_by_signal.clone(),
            },
            w.soft,
        ))
    } else {
        v.downcast_ref::<FrozenPinAt>().map(|w| {
            (
                w.inner.to_value(),
                PinLock {
                    any: w.pins.clone(),
                    by_signal: w.pins_by_signal.clone(),
                },
                w.soft,
            )
        })
    }
}

/// Record, without unwrapping, every `at()` a caller tucked inside a config
/// value — the dict-of-roles pattern. `pin_request(bind=)` unwraps it later;
/// registering it here is what lets the root check notice one nobody used.
pub(crate) fn record_pin_at_in_config<'v>(
    name: &str,
    value: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> Value<'v> {
    // A role is named by the dict key holding it; a wrapper found anywhere
    // else keeps the config's own name, so a solve that cannot use it still
    // reports it rather than dropping it.
    fn walk<'v>(role: &str, v: Value<'v>, out: &mut Vec<(String, Value<'v>)>) {
        if unpack_pin_at(v).is_some() {
            out.push((role.to_owned(), v));
        } else if let Some(d) = DictRef::from_value(v) {
            for (k, val) in d.iter() {
                walk(k.unpack_str().unwrap_or(role), val, out);
            }
        } else if let Some(l) = ListRef::from_value(v) {
            for val in l.iter() {
                walk(role, val, out);
            }
        }
    }
    /// The same walk, rebuilding what held a wrapper. `None` when the value
    /// carries none, so an untouched dict keeps its identity.
    fn strip<'v>(v: Value<'v>, heap: Heap<'v>) -> Option<Value<'v>> {
        if let Some((inner, _, _)) = unpack_pin_at(v) {
            Some(inner)
        } else if let Some(d) = DictRef::from_value(v) {
            let entries: Vec<(Value<'v>, Value<'v>, Option<Value<'v>>)> = d
                .iter()
                .map(|(k, val)| (k, val, strip(val, heap)))
                .collect();
            entries.iter().any(|(_, _, s)| s.is_some()).then(|| {
                heap.alloc(AllocDict(
                    entries
                        .iter()
                        .map(|(k, val, s)| (*k, s.unwrap_or(*val)))
                        .collect::<Vec<_>>(),
                ))
            })
        } else if let Some(l) = ListRef::from_value(v) {
            let items: Vec<(Value<'v>, Option<Value<'v>>)> =
                l.iter().map(|val| (val, strip(val, heap))).collect();
            items.iter().any(|(_, s)| s.is_some()).then(|| {
                heap.alloc(
                    items
                        .iter()
                        .map(|(val, s)| s.unwrap_or(*val))
                        .collect::<Vec<_>>(),
                )
            })
        } else {
            None
        }
    }

    let mut found: Vec<(String, Value<'v>)> = Vec::new();
    walk(name, value, &mut found);
    if found.is_empty() {
        return value;
    }
    for (role, wrapper) in found {
        record_pin_at(&role, wrapper, eval);
    }
    // The constraints are in the store now, so what the module records is the
    // connection the caller meant — a wrapper here would reach the netlist.
    let stripped = strip(value, eval.heap()).unwrap_or(value);
    if let Some(ctx) = eval.context_value() {
        ctx.module_mut().add_input(name.to_owned(), stripped);
    }
    stripped
}

/// Register an `at()` wrapper's constraint against `name`.
fn record_pin_at<'v>(name: &str, wrapper: Value<'v>, eval: &mut Evaluator<'v, '_, '_>) {
    let Some((inner, lock, soft, site, span)) = unpack_pin_at_full(wrapper) else {
        return;
    };
    let mut nets = Vec::new();
    collect_net_ids(inner, &mut nets);
    if nets.is_empty() {
        let path = eval.source_path().unwrap_or_default();
        eval.add_diagnostic(crate::Diagnostic::categorized(
            &path,
            &format!(
                "at() on input `{name}` carries no net, so its pin constraint can never \
                 apply — wrap the connection itself"
            ),
            "pinmux.at_without_net",
            starlark::errors::EvalSeverity::Error,
        ));
        return;
    }
    let Some(store) = eval.eval_context().map(|c| c.pin_constraints()) else {
        return;
    };
    let owner = eval
        .context_value()
        .map(|ctx| ctx.module().path().segments.clone())
        .unwrap_or_default();
    store
        .lock()
        .unwrap()
        .record(name.to_owned(), site, span, owner, &nets, lock, soft);
}

/// If `value` is an `at()` wrapper, record its constraint against the io()
/// input `name` and return the inner value; otherwise return `value` as-is.
pub(crate) fn unwrap_pin_at<'v>(
    name: &str,
    value: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> Value<'v> {
    match unpack_pin_at_full(value) {
        Some((inner, lock, soft, site, span)) => {
            let mut nets = Vec::new();
            collect_net_ids(inner, &mut nets);
            if nets.is_empty() {
                // Nothing to pin: the constraint would vanish and the
                // "never consumed" net would never catch it.
                let path = eval.source_path().unwrap_or_default();
                eval.add_diagnostic(crate::Diagnostic::categorized(
                    &path,
                    &format!(
                        "at() on input `{name}` carries no net, so its pin constraint can never \
                         apply — wrap the connection itself"
                    ),
                    "pinmux.at_without_net",
                    starlark::errors::EvalSeverity::Error,
                ));
            }
            if !nets.is_empty()
                && let Some(store) = eval.eval_context().map(|c| c.pin_constraints())
            {
                let module = site.clone();
                let owner = eval
                    .context_value()
                    .map(|ctx| ctx.module().path().segments.clone())
                    .unwrap_or_default();
                store.lock().unwrap().record(
                    name.to_owned(),
                    module,
                    span,
                    owner,
                    &nets,
                    lock,
                    soft,
                );
            }
            // The constraint now lives in the store, so the module keeps the
            // value the caller meant to connect. A solve that runs before this
            // io() still reads the wrapper off the input, as it must.
            if let Some(ctx) = eval.context_value() {
                ctx.module_mut().add_input(name.to_owned(), inner);
            }
            inner
        }
        None => value,
    }
}

/// Shared construction of a peripheral dict (used by `peripheral()` and `pool()`).
#[allow(clippy::too_many_arguments)]
fn make_peripheral_dict<'v>(
    ids: &InterfaceIds,
    heap: Heap<'v>,
    name: &str,
    provides: &[Value<'v>],
    signals: Vec<(String, Vec<Value<'v>>)>,
    rebind: &str,
    attrs: &SmallMap<String, Value<'v>>,
    unless: Option<&str>,
    pool: Option<&str>,
    part: Option<&str>,
) -> anyhow::Result<Value<'v>> {
    if !REBIND_VALUES.contains(&rebind) {
        return Err(anyhow::anyhow!(
            "peripheral `{name}`: rebind must be one of {REBIND_VALUES:?}, got `{rebind}`"
        ));
    }
    if provides.is_empty() {
        return Err(anyhow::anyhow!(
            "peripheral `{name}`: provides must list at least one interface type"
        ));
    }

    // A declaration cannot overstate the datasheet: every field of every
    // provided interface (closure included) needs a non-empty candidate list.
    for p in provides {
        for info in iface_closure(*p, ids)? {
            for sig in &info.signals {
                let ok = signals
                    .iter()
                    .any(|(s, cands)| s == sig && !cands.is_empty());
                if !ok {
                    return Err(anyhow::anyhow!(
                        "peripheral `{name}` claims {} but has no candidate for signal `{sig}` \
                         (its signals are {})",
                        info.name,
                        info.signals
                            .iter()
                            .map(|s| format!("`{s}`"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
    }

    // Attrs are validated against the vocabulary DECLARED by the provided
    // interfaces (closure included): an unknown key or a value of the wrong
    // dimension is a declaration error — every provider of Uart spells
    // "baud_max" the same way, with a Frequency.
    let mut declared: Vec<(String, pcb_sch::physical::PhysicalUnitDims, String)> = Vec::new();
    for p in provides {
        for info in iface_closure(*p, ids)? {
            for (aname, ty) in &info.attr_specs {
                if let Some(t) = ty.downcast_ref::<pcb_sch::physical::PhysicalValueType>() {
                    match declared.iter().find(|(n, _, _)| n == aname) {
                        Some((_, dims, owner)) if *dims != t.dims() => {
                            return Err(anyhow::anyhow!(
                                "peripheral `{name}`: attr `{aname}` is declared with conflicting dimensions by {owner} and {}",
                                info.name
                            ));
                        }
                        Some(_) => {}
                        None => declared.push((aname.clone(), t.dims(), info.name.clone())),
                    }
                }
            }
        }
    }
    let mut attr_pairs: Vec<(Value, Value)> = Vec::new();
    for (k, v) in attrs.iter() {
        let Some((_, dims, owner)) = declared.iter().find(|(n, _, _)| n == k) else {
            let known = if declared.is_empty() {
                "the provided interfaces declare no attrs".to_owned()
            } else {
                format!(
                    "declared: {}",
                    declared
                        .iter()
                        .map(|(n, _, o)| format!("`{n}` ({o})"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            return Err(anyhow::anyhow!(
                "peripheral `{name}`: attr `{k}` is not declared by any provided interface — {known}"
            ));
        };
        let parsed: PhysicalValue = match v.unpack_str() {
            Some(txt) => PhysicalValue::from_str(txt).map_err(|e| {
                anyhow::anyhow!("peripheral `{name}`: attr `{k}`: cannot parse `{txt}`: {e}")
            })?,
            None => PhysicalValue::try_from(*v).map_err(|_| {
                anyhow::anyhow!(
                    "peripheral `{name}`: attr `{k}` must be a physical value, got `{}`",
                    v.get_type()
                )
            })?,
        };
        if parsed.unit != *dims {
            return Err(anyhow::anyhow!(
                "peripheral `{name}`: attr `{k}` (declared on {owner}) expects {}, got {}",
                dims.quantity(),
                parsed.unit.quantity()
            ));
        }
        attr_pairs.push((heap.alloc(k.as_str()), heap.alloc(parsed)));
    }

    let signal_pairs: Vec<(Value, Value)> = signals
        .into_iter()
        .map(|(s, cands)| (heap.alloc(s.as_str()), heap.alloc(cands)))
        .collect();

    let provides_vals: Vec<Value> = provides.to_vec();
    let pairs: Vec<(Value, Value)> = vec![
        (heap.alloc("kind"), heap.alloc("peripheral")),
        (heap.alloc("name"), heap.alloc(name)),
        (heap.alloc("provides"), heap.alloc(provides_vals)),
        (heap.alloc("signals"), heap.alloc(AllocDict(signal_pairs))),
        (heap.alloc("rebind"), heap.alloc(rebind)),
        (heap.alloc("attrs"), heap.alloc(AllocDict(attr_pairs))),
        (
            heap.alloc("unless"),
            unless
                .map(|u| heap.alloc(u))
                .unwrap_or_else(Value::new_none),
        ),
        (
            heap.alloc("pool"),
            pool.map(|p| heap.alloc(p)).unwrap_or_else(Value::new_none),
        ),
        (
            heap.alloc("part"),
            part.map(|p| heap.alloc(p)).unwrap_or_else(Value::new_none),
        ),
    ];
    Ok(heap.alloc(AllocDict(pairs)))
}

/// Normalize a `signals=` entry list into pin dict values (strings auto-wrap).
fn normalize_candidates<'v>(
    heap: Heap<'v>,
    name: &str,
    sig: &str,
    cands: Value<'v>,
) -> anyhow::Result<Vec<Value<'v>>> {
    let list = ListRef::from_value(cands).ok_or_else(|| {
        anyhow::anyhow!("peripheral `{name}`: signal `{sig}` candidates must be a list")
    })?;
    let mut out = Vec::new();
    for c in list.iter() {
        if let Some(s) = c.unpack_str() {
            out.push(alloc_pin_dict(heap, s, None, 0, false, false));
        } else if is_kind(c, heap, "pin") {
            out.push(c);
        } else {
            return Err(anyhow::anyhow!(
                "peripheral `{name}`: signal `{sig}` candidates must be pin() or strings, got `{}`",
                c.get_type()
            ));
        }
    }
    Ok(out)
}

fn alloc_pin_dict<'v>(
    heap: Heap<'v>,
    name: &str,
    data: Option<Value<'v>>,
    cost: i32,
    input_only: bool,
    strap: bool,
) -> Value<'v> {
    let pairs: Vec<(Value, Value)> = vec![
        (heap.alloc("kind"), heap.alloc("pin")),
        (heap.alloc("name"), heap.alloc(name)),
        (heap.alloc("data"), data.unwrap_or_else(Value::new_none)),
        (heap.alloc("cost"), heap.alloc(cost)),
        (heap.alloc("input_only"), Value::new_bool(input_only)),
        (heap.alloc("strap"), Value::new_bool(strap)),
    ];
    heap.alloc(AllocDict(pairs))
}

/// The `builtin.pinmux` namespace.
#[derive(Clone, Copy, Debug, ProvidesStaticType, Freeze, Allocative, NoSerialize)]
pub struct Pinmux;

impl std::fmt::Display for Pinmux {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "builtin.pinmux")
    }
}

starlark::starlark_simple_value!(Pinmux);
starlark::methods_static!(PINMUX_METHODS = pinmux_methods);

#[starlark_value(type = "pinmux")]
impl<'v> StarlarkValue<'v> for Pinmux {
    fn get_methods() -> Option<&'static starlark::environment::Methods> {
        Some(PINMUX_METHODS.methods())
    }
}

#[starlark_module]
fn pinmux_methods(methods: &mut MethodsBuilder) {
    /// Constrain which physical pin(s) a connection may use, at the
    /// connection site: `Mcu(IO0 = at(led_net, "PA8"))`. Hard by default
    /// (build error if unsatisfiable); `soft = True` makes it a preference
    /// the solver may fall back from.
    fn at<'v>(
        #[allow(unused_variables)] this: &Pinmux,
        #[starlark(require = pos)] value: Value<'v>,
        #[starlark(require = pos)] pins: Value<'v>,
        #[starlark(require = named, default = false)] soft: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let (pin_list, by_signal) = parse_pin_spec("at()", pins, eval.heap())?;
        let (site, span) = eval
            .call_stack_top_location()
            .map(|loc| (loc.file.filename().to_string(), Some(loc.resolve_span())))
            .unwrap_or_else(|| (eval.source_path().unwrap_or_default(), None));
        Ok(eval.heap().alloc(PinAt {
            inner: value,
            site,
            span,
            pins: pin_list,
            pins_by_signal: by_signal,
            soft,
        }))
    }
    /// One candidate physical pin for a peripheral signal. `data=` carries
    /// opaque realization info (e.g. `{"af": 1}` on STM32, `{"iomux_func": 0}`
    /// on ESP32) verbatim into the solved assignment; the solver ignores it.
    fn pin<'v>(
        #[allow(unused_variables)] this: &Pinmux,
        #[starlark(require = pos)] name: String,
        #[starlark(require = named, default = SmallMap::default())] data: SmallMap<
            String,
            Value<'v>,
        >,
        #[starlark(require = named, default = 0)] cost: i32,
        #[starlark(require = named, default = false)] input_only: bool,
        #[starlark(require = named, default = false)] strap: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        let data_v = if data.is_empty() {
            None
        } else {
            if data.contains_key("pin") {
                return Err(anyhow::anyhow!(
                    "pin `{name}`: data key `pin` is reserved (it names the pin itself in the assignment)"
                ));
            }
            let pairs: Vec<(Value, Value)> = data
                .iter()
                .map(|(k, v)| (heap.alloc(k.as_str()), *v))
                .collect();
            Some(heap.alloc(AllocDict(pairs)))
        };
        Ok(alloc_pin_dict(heap, &name, data_v, cost, input_only, strap))
    }

    /// A resource cluster: signals -> candidate pins, provided interfaces,
    /// rebind cost, attributes, optional config-conditional availability.
    #[allow(clippy::too_many_arguments)]
    fn peripheral<'v>(
        #[allow(unused_variables)] this: &Pinmux,
        #[starlark(require = pos)] name: String,
        #[starlark(require = named)] provides: UnpackList<Value<'v>>,
        #[starlark(require = named)] signals: SmallMap<String, Value<'v>>,
        #[starlark(require = named)] rebind: String,
        #[starlark(require = named, default = SmallMap::default())] attrs: SmallMap<
            String,
            Value<'v>,
        >,
        #[starlark(require = named, default = NoneOr::None)] unless: NoneOr<String>,
        #[starlark(require = named, default = NoneOr::None)] part: NoneOr<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        let ids = interface_ids(eval)?;
        for p in &provides.items {
            if iface_info(*p, &ids).is_none() {
                return Err(anyhow::anyhow!(
                    "peripheral `{name}`: provides entries must be interface types, got `{}`",
                    p.get_type()
                ));
            }
        }
        let mut normalized = Vec::new();
        for (sig, cands) in signals.iter() {
            normalized.push((sig.clone(), normalize_candidates(heap, &name, sig, *cands)?));
        }
        make_peripheral_dict(
            &ids,
            heap,
            &name,
            &provides.items,
            normalized,
            &rebind,
            &attrs,
            unless.into_option().as_deref(),
            None,
            part.into_option().as_deref(),
        )
    }

    /// Sugar for N interchangeable single-signal units over a pin set (GPIO pools).
    fn pool<'v>(
        #[allow(unused_variables)] this: &Pinmux,
        #[starlark(require = pos)] name: String,
        #[starlark(require = named)] provides: UnpackList<Value<'v>>,
        #[starlark(require = named)] pins: UnpackList<Value<'v>>,
        #[starlark(require = named, default = "firmware".to_string())] rebind: String,
        #[starlark(require = named, default = NoneOr::None)] part: NoneOr<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        let ids = interface_ids(eval)?;
        let part = part.into_option();
        if provides.items.len() != 1 {
            return Err(anyhow::anyhow!(
                "pool `{name}`: provides must list exactly one interface type"
            ));
        }
        let info = iface_info(provides.items[0], &ids).ok_or_else(|| {
            anyhow::anyhow!("pool `{name}`: provides entry must be an interface type")
        })?;
        check_signal_names(&info)?;
        if info.signals.len() != 1 {
            return Err(anyhow::anyhow!(
                "pool `{name}`: the provided interface must have exactly one signal, `{}` has {}",
                info.name,
                info.signals.len()
            ));
        }
        let field = info.signals[0].clone();
        let mut units: Vec<Value> = Vec::new();
        for entry in &pins.items {
            let pin_dict = if let Some(s) = entry.unpack_str() {
                alloc_pin_dict(heap, s, None, 0, false, false)
            } else if is_kind(*entry, heap, "pin") {
                *entry
            } else {
                return Err(anyhow::anyhow!(
                    "pool `{name}`: pins entries must be pin() or strings, got `{}`",
                    entry.get_type()
                ));
            };
            let pin_name = DictRef::from_value(pin_dict)
                .and_then(|d| dict_get_str(&d, heap, "name"))
                .ok_or_else(|| anyhow::anyhow!("pool `{name}`: invalid pin entry"))?;
            units.push(make_peripheral_dict(
                &ids,
                heap,
                &format!("{name}.{pin_name}"),
                &provides.items,
                vec![(field.clone(), vec![pin_dict])],
                &rebind,
                &SmallMap::default(),
                None,
                Some(&name),
                part.as_deref(),
            )?);
        }
        Ok(heap.alloc(units))
    }

    /// A capability demand: the interface type is the request. Only connected
    /// signals (`uses=`) consume pins. With `if_connected=True` the request is
    /// served only when the caller actually connected the module input of the
    /// same name — the `io(Iface, optional=True)` slot pattern.
    #[allow(clippy::too_many_arguments)]
    fn pin_request<'v>(
        #[allow(unused_variables)] this: &Pinmux,
        #[starlark(require = pos)] name: String,
        #[starlark(require = pos)] iface: Value<'v>,
        #[starlark(require = named, default = NoneOr::None)] uses: NoneOr<UnpackList<String>>,
        #[starlark(require = named, default = NoneOr::None)] instance: NoneOr<String>,
        #[starlark(require = named, default = NoneOr::None)] prefer: NoneOr<Value<'v>>,
        #[starlark(require = named, default = false)] lock: bool,
        #[starlark(require = named, default = NoneOr::None)] r#where: NoneOr<Value<'v>>,
        #[starlark(require = named, default = NoneOr::None)] direction: NoneOr<String>,
        #[starlark(require = named, default = false)] if_connected: bool,
        #[starlark(require = named, default = NoneOr::None)] bind: NoneOr<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        // bind= carries the connected value on the request itself (the
        // dict-of-roles pattern). A wrapper rides along untouched — pin_solve
        // weighs it against what the request asked for — while validation
        // below looks at the value it wraps.
        let bound = match bind {
            NoneOr::Other(v) => Some(v),
            NoneOr::None => None,
        };
        let bind_val = bound.map(|v| unpack_pin_at(v).map(|(inner, _, _)| inner).unwrap_or(v));
        // Validate the bound value early, with the role name in the message —
        // otherwise a bad dict entry only surfaces at Component() as
        // "Pin 'PAx' must be connected to a Net", naming the solved pin
        // instead of the role.
        if let Some(v) = bind_val
            && v.downcast_ref::<NetValue<'v>>().is_none()
            && v.downcast_ref::<FrozenNetValue>().is_none()
            && v.downcast_ref::<InterfaceValue<'v>>().is_none()
            && v.downcast_ref::<FrozenInterfaceValue>().is_none()
        {
            return Err(anyhow::anyhow!(
                "pin_request `{name}`: bind must be a Net or interface instance (optionally wrapped in at()), got `{}`",
                v.get_type()
            ));
        }
        let ids = interface_ids(eval)?;
        let info = iface_info(iface, &ids).ok_or_else(|| {
            anyhow::anyhow!(
                "pin_request `{name}`: expected an interface type, got `{}`",
                iface.get_type()
            )
        })?;
        if let NoneOr::Other(dir) = &direction
            && !["input", "output"].contains(&dir.as_str())
        {
            return Err(anyhow::anyhow!(
                "pin_request `{name}`: direction must be \"input\" or \"output\""
            ));
        }
        check_signal_names(&info)?;
        let uses_vals: Vec<String> = match uses {
            NoneOr::Other(u) => {
                let mut seen = HashSet::new();
                for s in &u.items {
                    if !info.signals.contains(s) {
                        return Err(anyhow::anyhow!(
                            "pin_request `{name}`: `{s}` is not a signal of {} (its signals are {})",
                            info.name,
                            info.signals
                                .iter()
                                .map(|s| format!("`{s}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                    if !seen.insert(s.as_str()) {
                        return Err(anyhow::anyhow!(
                            "pin_request `{name}`: duplicate signal `{s}` in uses"
                        ));
                    }
                }
                u.items
            }
            NoneOr::None => info.signals.clone(),
        };
        if uses_vals.is_empty() {
            return Err(anyhow::anyhow!(
                "pin_request `{name}`: uses must name at least one signal"
            ));
        }
        let uses_alloc: Vec<Value> = uses_vals.iter().map(|s| heap.alloc(s.as_str())).collect();
        // A bound at() rides on the value and is weighed by pin_solve like any
        // other, so the request keeps what it asked for: one rule decides how
        // a wish and a lock compose, whichever path delivers the connection.
        let (pref, lock) = {
            let (any, by_signal) = match prefer {
                NoneOr::Other(v) => {
                    parse_pin_spec(&format!("pin_request `{name}`: prefer"), v, heap)?
                }
                NoneOr::None => (Vec::new(), Vec::new()),
            };
            (PinLock { any, by_signal }, lock)
        };
        pref.check_signals(&format!("pin_request `{name}`"), &uses_vals)?;
        let prefer_alloc: Vec<Value> = pref.any.iter().map(|s| heap.alloc(s.as_str())).collect();
        let by_signal_alloc: Vec<(Value, Value)> = pref
            .by_signal
            .iter()
            .map(|(sig, pins)| {
                let list: Vec<Value> = pins.iter().map(|p| heap.alloc(p.as_str())).collect();
                (heap.alloc(sig.as_str()), heap.alloc(list))
            })
            .collect();
        let pairs: Vec<(Value, Value)> = vec![
            (heap.alloc("kind"), heap.alloc("request")),
            (heap.alloc("name"), heap.alloc(name.as_str())),
            (heap.alloc("iface"), iface),
            (heap.alloc("uses"), heap.alloc(uses_alloc)),
            (
                heap.alloc("instance"),
                match instance {
                    NoneOr::Other(i) => heap.alloc(i),
                    NoneOr::None => Value::new_none(),
                },
            ),
            (heap.alloc("prefer"), heap.alloc(prefer_alloc)),
            (
                heap.alloc("prefer_by_signal"),
                heap.alloc(AllocDict(by_signal_alloc)),
            ),
            (heap.alloc("lock"), Value::new_bool(lock)),
            (
                heap.alloc("where"),
                match r#where {
                    NoneOr::Other(w) => w,
                    NoneOr::None => Value::new_none(),
                },
            ),
            (
                heap.alloc("direction"),
                match direction {
                    NoneOr::Other(d) => heap.alloc(d),
                    NoneOr::None => Value::new_none(),
                },
            ),
            (heap.alloc("if_connected"), Value::new_bool(if_connected)),
            (heap.alloc("bind"), bound.unwrap_or_else(Value::new_none)),
        ];
        Ok(heap.alloc(AllocDict(pairs)))
    }

    /// Deterministic joint instance x pin assignment. Fails elaboration when
    /// infeasible; records `pin_assignment` and `swap_classes` module
    /// properties (JSON) for downstream consumers.
    fn pin_solve<'v>(
        #[allow(unused_variables)] this: &Pinmux,
        #[starlark(require = pos)] peripherals: Value<'v>,
        #[starlark(require = pos)] requests: Value<'v>,
        #[starlark(require = named, default = SmallMap::default())] config: SmallMap<
            String,
            Value<'v>,
        >,
        #[starlark(require = named, default = NoneOr::None)] previous: NoneOr<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();

        let ids = interface_ids(eval)?;
        // One level of nesting is flattened: `pool()` yields a list of units,
        // so `[usart1, gpio_pool]` is one table.
        let plist = ListRef::from_value(peripherals).ok_or_else(|| {
            anyhow::anyhow!("pin_solve: peripherals must be a list of peripheral()/pool() values")
        })?;
        let mut periphs: Vec<RPeriph> = Vec::new();
        for entry in plist.iter() {
            match ListRef::from_value(entry) {
                Some(nested) => {
                    for e in nested.iter() {
                        periphs.push(parse_rperiph(e, heap, &ids)?);
                    }
                }
                None => periphs.push(parse_rperiph(entry, heap, &ids)?),
            }
        }
        // Pads belong to a part: peripherals declaring the same `part=` share a
        // pin namespace, and those declaring none form a single anonymous part.
        let mut part_names: Vec<String> = Vec::new();
        for p in &periphs {
            if !part_names.contains(&p.part) {
                part_names.push(p.part.clone());
            }
        }
        for p in periphs.iter_mut() {
            p.part_idx = part_names.iter().position(|n| *n == p.part).unwrap_or(0);
        }
        let mut seen_names = HashSet::new();
        for p in &periphs {
            if !seen_names.insert((p.part_idx, p.name.clone())) {
                return Err(anyhow::anyhow!(
                    "pin_solve: duplicate peripheral name `{}`",
                    p.name
                ));
            }
        }

        let rlist = ListRef::from_value(requests)
            .ok_or_else(|| anyhow::anyhow!("pin_solve: requests must be a list"))?;
        let mut reqs: Vec<RReq> = Vec::new();
        for entry in rlist.iter() {
            reqs.push(parse_rreq(entry, heap, &ids)?);
        }
        let mut seen_reqs = HashSet::new();
        for r in &reqs {
            if !seen_reqs.insert(r.name.clone()) {
                return Err(anyhow::anyhow!(
                    "pin_solve: duplicate request name `{}`",
                    r.name
                ));
            }
        }

        // `inputs()` holds every caller-supplied argument, config() included;
        // a request pairs with io() slots only. A config declared after this
        // solve is not yet known, so pairing with one stays possible.
        let configs: HashSet<String> = eval
            .context_value()
            .map(|ctx| {
                ctx.module()
                    .signature()
                    .iter()
                    .filter(|p| p.is_config)
                    .map(|p| p.name.clone())
                    .collect()
            })
            .unwrap_or_default();

        // io() slot pattern: an `if_connected` request is served only when the
        // caller actually connected the module input of the same name.
        let connected: HashSet<String> = eval
            .context_value()
            .map(|ctx| {
                ctx.module()
                    .inputs()
                    .iter()
                    .map(|(k, _)| k.clone())
                    .filter(|k| !configs.contains(k))
                    .collect()
            })
            .unwrap_or_default();
        reqs.retain(|r| !r.if_connected || connected.contains(&r.name));
        // A config() declared after this solve is not in `configs` yet, so the
        // gate may have read one as a connection; the module records what it
        // served and the check runs once its signature is complete.
        if let Some(ctx) = eval.context_value() {
            for r in reqs.iter().filter(|r| r.if_connected) {
                ctx.record_if_connected(&r.name);
            }
        }

        let prev_map = match previous {
            NoneOr::Other(v) => {
                let parsed = parse_previous(v, heap);
                let provided_entries = DictRef::from_value(v)
                    .map(|d| d.iter().count())
                    .unwrap_or(1);
                if parsed.is_empty() && provided_entries > 0 {
                    warn_at_call_site(
                        eval,
                        "pin_solve: previous= contains no usable assignment entry; pass the `assignment` dict of a prior solve".to_owned(),
                    );
                }
                parsed
            }
            NoneOr::None => HashMap::new(),
        };

        // `at()` constraints override request-side prefer/lock. A request
        // pairs with the io() input of its name; the constraint rides on the
        // input's nets. Read the inputs first so the claims below, which need
        // the evaluator to weigh candidates, do not hold the module borrow.
        let scopes: Vec<String> = part_names.clone();
        let mut claimed_instances: HashSet<(String, String)> = HashSet::new();
        let mut claimed_pads: HashSet<(usize, String)> = HashSet::new();
        if let Some(ctx) = eval.context_value() {
            // A peripheral is identified by its part and its name: two chips
            // may legitimately name a resource alike.
            let ours: HashSet<(&str, &str)> = periphs
                .iter()
                .map(|p| (scopes[p.part_idx].as_str(), p.name.as_str()))
                .collect();
            let current: HashSet<String> = reqs.iter().map(|r| r.name.clone()).collect();
            for claim in ctx.pin_claims_excluding(&current, &scopes) {
                // A pad belongs to the part, not to the peripheral that took
                // it: another table of the same part still owns it.
                if let Some(part) = scopes.iter().position(|s| *s == claim.scope) {
                    claimed_pads.extend(claim.pins.iter().map(|p| (part, p.clone())));
                }
                // An instance is only ours to reserve when this table has it.
                if ours.contains(&(claim.scope.as_str(), claim.instance.as_str())) {
                    claimed_instances.insert((claim.scope, claim.instance));
                }
            }
        }
        let store = eval.eval_context().map(|c| c.pin_constraints());
        let mut solver: Vec<String> = Vec::new();
        // (request, the nets it rides, its own at() when it carries one)
        type Ride = (usize, Vec<u64>, Option<(PinLock, bool)>);
        let mut rides: Vec<Ride> = Vec::new();
        if let Some(ctx) = eval.context_value() {
            solver = ctx.module().path().segments.clone();
            for (i, r) in reqs.iter().enumerate() {
                // A config() of the request's name is not a connection. A role
                // carries its own value, so that name cannot mislead it.
                if r.bind.is_none() && configs.contains(&r.name) {
                    continue;
                }
                // The value bound on the request itself when the caller passed
                // a dict of roles, else the io() input of this name.
                let Some(input) = r.bind.or_else(|| {
                    ctx.module()
                        .inputs()
                        .get(r.name.as_str())
                        .map(|v| v.to_value())
                }) else {
                    continue;
                };
                // Solving before the io() binds: the wrapper is still here.
                let (value, raw) = match unpack_pin_at(input) {
                    Some((inner, pins, soft)) => (inner, Some((pins, soft))),
                    None => (input, None),
                };
                let mut nets = Vec::new();
                collect_net_ids(value, &mut nets);
                rides.push((i, nets, raw));
            }
        }
        if let Some(pin_store) = store {
            for (i, nets, raw) in rides {
                let mut store = pin_store.lock().unwrap();
                let constraint = match raw {
                    Some(c) => {
                        store.settle(&nets, &solver, &reqs[i].name);
                        Some(c)
                    }
                    None => {
                        // An ancestor's constraint names an input of its own
                        // namespace, so only the pins pair it with a request:
                        // one that has no candidate on them must leave it to
                        // the request that has. Weighing that runs user code,
                        // so it happens with the store released.
                        let candidates = store.candidate_locks(&nets, &solver);
                        drop(store);
                        let mut usable: HashSet<usize> = HashSet::new();
                        for (idx, lock) in candidates {
                            let mut probe = reqs[i].clone();
                            probe.prefer = lock;
                            probe.lock = true;
                            // Weighed against the same candidates the solve will
                            // see, earlier solves' claims included, or a pad
                            // already taken would look like a fit.
                            let fits =
                                combos_for_request(&probe, &periphs, &config, &prev_map, eval)
                                    .map(|(combos, _, _)| {
                                        combos.iter().any(|c| {
                                            let p = &periphs[c.periph_idx];
                                            !claimed_instances.contains(&(
                                                scopes[p.part_idx].clone(),
                                                p.name.clone(),
                                            )) && !c.pins.iter().any(|(_, pin)| {
                                                claimed_pads
                                                    .contains(&(p.part_idx, pin.name.clone()))
                                            })
                                        })
                                    })
                                    .unwrap_or(false);
                            if fits {
                                usable.insert(idx);
                            }
                        }
                        store = pin_store.lock().unwrap();
                        match store.claim(&nets, &solver, &reqs[i].name, Some(&usable)) {
                            Claimed::Yes(pins, soft) => Some((pins, soft)),
                            Claimed::No | Claimed::Ambiguous | Claimed::Contended => None,
                        }
                    }
                };
                if let Some(Ambiguity::Entries { input, names }) = store.take_ambiguous() {
                    return Err(anyhow::anyhow!(
                        "pin_solve: at() constraints on `{names}` all ride the net reaching input \
                         `{input}`; name the io() inputs apart so each constraint has one owner"
                    ));
                }
                if let Some((pins, soft)) = constraint {
                    let r = &mut reqs[i];
                    pins.check_signals(&format!("pin_solve: at() on input `{}`", r.name), &r.uses)?;
                    if soft {
                        // A wish never replaces what the request asked for: it
                        // outranks it where both can be had, and leaves it to
                        // decide where the wish cannot be met.
                        r.soft_prefer = pins;
                    } else {
                        r.prefer = pins;
                        r.lock = true;
                    }
                }
            }
        }

        // Candidate combos per request, with reasoned rejections.
        let mut all_combos: Vec<Vec<Combo>> = Vec::new();
        let mut truncated_reqs: Vec<String> = Vec::new();
        for r in &reqs {
            let (combos, rejects, capped) =
                combos_for_request(r, &periphs, &config, &prev_map, eval)?;
            if !capped.is_empty() {
                truncated_reqs.push(r.name.clone());
                warn_at_call_site(
                    eval,
                    format!(
                        "pin_solve: request `{}`: pin combinations capped at {PINMAP_CAP} for `{}`; the assignment may be suboptimal",
                        r.name,
                        capped.join("`, `")
                    ),
                );
            }
            if combos.is_empty() {
                let detail = rejects
                    .iter()
                    .map(|(n, why)| format!("  {n}: rejected — {why}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Err(anyhow::anyhow!(
                    "pin_solve: request `{}` ({}) has no candidate:\n{detail}",
                    r.name,
                    r.iface_name
                ));
            }
            all_combos.push(combos);
        }

        // Pin/instance exclusivity spans every pin_solve of the module:
        // candidates claimed by another solve's requests are unavailable
        // (a re-solved request releases its own claims, so `previous=` can
        // widen an earlier solve). Only claims on a peripheral of *this* table
        // count: pin names belong to a part, so another part's `7` is not ours.
        // The claimed sets also seed the reported residual freedom below, so
        // free/alternate/spare listings match the exclusivity enforced here.
        let part_of: Vec<usize> = periphs.iter().map(|p| p.part_idx).collect();
        // A part names its own pad namespace; peripherals declaring no part
        // share the module's single anonymous one, so pads keep colliding
        // across solves exactly as they did before parts existed.
        // Whether a gated peripheral is there belongs to the design, not to
        // one solve: an axis that said one thing already cannot say another,
        // or which pads the component exposes would depend on solve order.
        if let Some(ctx) = eval.context_value() {
            for (part_idx, part) in scopes.iter().enumerate() {
                // Axes this table's peripherals name, plus any this part has
                // already settled and this call speaks about — a later table
                // may omit the gated peripheral yet still configure it.
                let mut axes: Vec<String> = periphs
                    .iter()
                    .filter(|p| p.part_idx == part_idx)
                    .filter_map(|p| p.unless.clone())
                    .collect();
                axes.extend(
                    ctx.config_axes_for(part)
                        .into_iter()
                        .filter(|axis| config.get(axis.as_str()).is_some()),
                );
                axes.sort();
                axes.dedup();
                for axis in axes {
                    let on = config_truthy(&config, &axis);
                    if let Some(before) = ctx.record_config_axis(part, &axis, on)
                        && before != on
                    {
                        let whose = if part.is_empty() {
                            "this part".to_owned()
                        } else {
                            format!("`{part}`")
                        };
                        return Err(anyhow::anyhow!(
                            "pin_solve: config axis `{axis}` was {before} for {whose} in an \
                             earlier solve and is {on} here; a part is configured once"
                        ));
                    }
                }
            }
        }
        if !(claimed_instances.is_empty() && claimed_pads.is_empty()) {
            for (i, combos) in all_combos.iter_mut().enumerate() {
                combos.retain(|c| {
                    !claimed_instances.contains(&(
                        scopes[part_of[c.periph_idx]].clone(),
                        periphs[c.periph_idx].name.clone(),
                    )) && !c.pins.iter().any(|(_, p)| {
                        claimed_pads.contains(&(part_of[c.periph_idx], p.name.clone()))
                    })
                });
                if combos.is_empty() {
                    return Err(anyhow::anyhow!(
                        "pin_solve: request `{}`: every candidate uses a pin or instance already claimed by an earlier pin_solve in this module",
                        reqs[i].name
                    ));
                }
            }
        }

        // Joint assignment. A truncated enumeration may have dropped the very
        // combinations that would have fit, so failures must say so.
        let failure_detail = |reqs: &[RReq], all_combos: &[Vec<Combo>]| {
            let mut detail = reqs
                .iter()
                .enumerate()
                .map(|(i, r)| format!("  {}: {} candidate combo(s)", r.name, all_combos[i].len()))
                .collect::<Vec<_>>()
                .join("\n");
            if !truncated_reqs.is_empty() {
                detail.push_str(&format!(
                    "\nnote: pin combinations for `{}` were capped at {PINMAP_CAP}; a feasible \
                     assignment may have been dropped — constrain those requests \
                     (instance=/prefer=) to narrow the enumeration",
                    truncated_reqs.join("`, `")
                ));
            }
            detail
        };
        let chosen = match assign(&reqs, &all_combos, &part_of) {
            AssignOutcome::Solved { chosen, capped } => {
                if capped {
                    warn_at_call_site(
                        eval,
                        format!(
                            "pin_solve: solver budget ({SOLVER_BUDGET} conflict checks) exhausted; the assignment is feasible but may be suboptimal"
                        ),
                    );
                }
                if let Some(ctx) = eval.context_value() {
                    for (i, r) in reqs.iter().enumerate() {
                        let combo = &all_combos[i][chosen[i]];
                        ctx.record_pin_claim(
                            &r.name,
                            crate::lang::context::PinClaim {
                                instance: periphs[combo.periph_idx].name.clone(),
                                pins: combo.pins.iter().map(|(_, p)| p.name.clone()).collect(),
                                scope: scopes[periphs[combo.periph_idx].part_idx].clone(),
                            },
                            &scopes,
                        );
                    }
                }
                chosen
            }
            AssignOutcome::Infeasible => {
                // Undeclared peripherals form one part, so identically named
                // pads of two components collide.
                let hint = if part_names.len() == 1 && part_names[0].is_empty() {
                    "\nnote: these peripherals share one pin namespace; if they belong to \
                     different components, give each `peripheral(part = ...)` so its pads \
                     stay its own"
                } else {
                    ""
                };
                return Err(anyhow::anyhow!(
                    "pin_solve: no feasible assignment (instance/pin exclusivity):\n{}{hint}",
                    failure_detail(&reqs, &all_combos)
                ));
            }
            AssignOutcome::Unknown => {
                return Err(anyhow::anyhow!(
                    "pin_solve: search budget exhausted before feasibility could be proven; \
                     constrain requests (instance=/prefer=) to narrow the search:\n{}",
                    failure_detail(&reqs, &all_combos)
                ));
            }
        };

        // Build results (JSON first; the Starlark value mirrors it). Pins
        // claimed by earlier solves count as used: alternates, spares and the
        // tie-off list must never offer a pad another request already owns.
        let mut used_pads: HashSet<(usize, String)> = claimed_pads;
        for (i, &ci) in chosen.iter().enumerate() {
            let part = part_of[all_combos[i][ci].periph_idx];
            for (_, p) in &all_combos[i][ci].pins {
                used_pads.insert((part, p.name.clone()));
            }
        }

        let mut assignment = serde_json::Map::new();
        for (i, r) in reqs.iter().enumerate() {
            let combo = &all_combos[i][chosen[i]];
            let periph = &periphs[combo.periph_idx];
            let mut signals = serde_json::Map::new();
            let mut alternates = serde_json::Map::new();
            for (sig, p) in &combo.pins {
                let mut entry = serde_json::Map::new();
                entry.insert("pin".into(), serde_json::Value::String(p.name.clone()));
                // Realization data is splatted next to "pin" so consumers see
                // e.g. {"pin": "PA9", "af": 1} without a vendor-specific schema.
                for (k, val) in &p.data {
                    entry.insert(k.clone(), val.clone());
                }
                signals.insert(sig.clone(), serde_json::Value::Object(entry));
                if p.strap {
                    warn_at_call_site(
                        eval,
                        format!(
                            "pin_solve: request `{}` signal `{sig}` uses strapping pin `{}`",
                            r.name, p.name
                        ),
                    );
                }
                // Same filters the candidates went through, or the alternates
                // would offer a pad this request could not have taken.
                let allowed = r.allowed(sig);
                let free: Vec<serde_json::Value> = if r.pinned_by_lock(&p.name) {
                    Vec::new()
                } else {
                    periph
                        .signal(sig)
                        .map(|cands| {
                            cands
                                .iter()
                                .filter(|c| allowed.is_none_or(|a| a.contains(&c.name)))
                                .filter(|c| {
                                    !(r.direction.as_deref() == Some("output") && c.input_only)
                                })
                                .filter(|c| !used_pads.contains(&(periph.part_idx, c.name.clone())))
                                .map(|c| serde_json::Value::String(c.name.clone()))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                if !free.is_empty() {
                    alternates.insert(sig.clone(), serde_json::Value::Array(free));
                }
            }
            let mut entry = serde_json::Map::new();
            entry.insert(
                "instance".into(),
                serde_json::Value::String(periph.name.clone()),
            );
            // Two components may name a resource alike, so the part is what
            // makes the instance unambiguous.
            entry.insert(
                "part".into(),
                match part_names[periph.part_idx].as_str() {
                    "" => serde_json::Value::Null,
                    p => serde_json::Value::String(p.to_owned()),
                },
            );
            entry.insert(
                "iface".into(),
                serde_json::Value::String(r.iface_name.clone()),
            );
            entry.insert(
                "pool".into(),
                periph
                    .pool
                    .clone()
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null),
            );
            entry.insert(
                "rebind".into(),
                serde_json::Value::String(periph.rebind.clone()),
            );
            entry.insert("signals".into(), serde_json::Value::Object(signals));
            entry.insert("alternates".into(), serde_json::Value::Object(alternates));
            assignment.insert(r.name.clone(), serde_json::Value::Object(entry));
        }

        // Instances occupied module-wide: this solve's choices plus earlier
        // solves' claims. Spare-unit listings (and the property merge below)
        // must never offer an occupied unit.
        let assigned_instances: HashSet<(String, String)> = chosen
            .iter()
            .enumerate()
            .map(|(i, &ci)| {
                let p = &periphs[all_combos[i][ci].periph_idx];
                (scopes[p.part_idx].clone(), p.name.clone())
            })
            .chain(claimed_instances.iter().cloned())
            .collect();

        // Residual freedom: pool classes (pin granularity) + identical-provides
        // rebind="none" clusters (gate swap). Collected unfiltered: the
        // emission rule (two members, or one member plus spares) runs on the
        // returned value now, and again on the merged module property once
        // prior solves' members are folded in — a class below the threshold
        // alone can cross it combined with an earlier solve's members.
        // (part, pool, rebind, members as (request, pin), spare pins)
        type PoolClass = (String, String, String, Vec<(String, String)>, Vec<String>);
        // (part, unit names, members as (request, unit), spare units)
        type ClusterClass = (String, HashSet<String>, Vec<(String, String)>, Vec<String>);
        // Members grouped by (part, class key).
        type Grouped<K> = Vec<((String, K), Vec<(String, String)>)>;
        let mut pool_classes: Vec<PoolClass> = Vec::new();
        let mut cluster_classes: Vec<ClusterClass> = Vec::new();
        {
            // Keyed by part too: two components may hold a pool of one name.
            let mut by_pool: Grouped<String> = Vec::new();
            for (i, r) in reqs.iter().enumerate() {
                // A lock with pins removes the request from the swap freedom;
                // a bare lock=True constrains nothing and swaps normally.
                if r.lock && !r.prefer.is_empty() {
                    continue;
                }
                let combo = &all_combos[i][chosen[i]];
                let periph = &periphs[combo.periph_idx];
                let Some(pool) = &periph.pool else { continue };
                let pin = combo.pins[0].1.name.clone();
                let key = (scopes[periph.part_idx].clone(), pool.clone());
                match by_pool.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, members)) => members.push((r.name.clone(), pin)),
                    None => by_pool.push((key, vec![(r.name.clone(), pin)])),
                }
            }
            by_pool.sort_by(|a, b| a.0.cmp(&b.0));
            for ((part, pool), mut members) in by_pool {
                members.sort();
                let same_pool = |p: &&RPeriph| {
                    p.pool.as_deref() == Some(pool.as_str()) && scopes[p.part_idx] == part
                };
                let mut spares: Vec<String> = periphs
                    .iter()
                    .filter(same_pool)
                    .filter_map(|p| {
                        p.signals
                            .first()
                            .and_then(|(_, c)| c.first())
                            .map(|c| (p.part_idx, c.name.clone()))
                    })
                    .filter(|pad| !used_pads.contains(pad))
                    .map(|(_, n)| n)
                    .collect();
                spares.sort();
                let rebind = periphs
                    .iter()
                    .find(same_pool)
                    .map(|p| p.rebind.clone())
                    .unwrap_or_else(|| "firmware".to_owned());
                pool_classes.push((part, pool, rebind, members, spares));
            }

            let mut by_cluster: Grouped<ProvidesKey> = Vec::new();
            for (i, r) in reqs.iter().enumerate() {
                if r.lock && !r.prefer.is_empty() {
                    continue;
                }
                let combo = &all_combos[i][chosen[i]];
                let periph = &periphs[combo.periph_idx];
                if periph.rebind != "none" || periph.pool.is_some() {
                    continue;
                }
                let key = periph.provides_key();
                let member = (r.name.clone(), periph.name.clone());
                let key = (scopes[periph.part_idx].clone(), key);
                match by_cluster.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, members)) => members.push(member),
                    None => by_cluster.push((key, vec![member])),
                }
            }
            // Order clusters by their members, not by key: TypeInstanceIds
            // depend on concurrent module-evaluation order, so the key is
            // only stable within one build.
            for (_, members) in by_cluster.iter_mut() {
                members.sort();
            }
            by_cluster.sort_by(|a, b| a.1.cmp(&b.1));
            for ((part, key), members) in by_cluster {
                let scoped: Vec<(String, String)> = periphs
                    .iter()
                    .filter(|p| {
                        p.rebind == "none"
                            && p.pool.is_none()
                            && p.provides_key() == key
                            && scopes[p.part_idx] == part
                    })
                    .map(|p| (scopes[p.part_idx].clone(), p.name.clone()))
                    .collect();
                let units: HashSet<String> = scoped.iter().map(|(_, n)| n.clone()).collect();
                let mut spares: Vec<String> = scoped
                    .iter()
                    .filter(|u| !assigned_instances.contains(u))
                    .map(|(_, n)| n.clone())
                    .collect();
                spares.sort();
                cluster_classes.push((part, units, members, spares));
            }
        }
        let emits = |members: &[(String, String)], n_spares: usize| {
            members.len() >= 2 || (!members.is_empty() && n_spares > 0)
        };
        let pool_class_json = |(part, pool, rebind, members, spares): &PoolClass| {
            serde_json::json!({
                "granularity": "pin",
                "rebind": rebind,
                "part": if part.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(part.clone()) },
                "pool": pool,
                "members": members
                    .iter()
                    .map(|(r, p)| serde_json::json!({"request": r, "pin": p}))
                    .collect::<Vec<_>>(),
                "spare_pins": spares,
            })
        };
        let cluster_class_json = |(part, _, members, spares): &ClusterClass| {
            serde_json::json!({
                "granularity": "cluster",
                "rebind": "none",
                "part": if part.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(part.clone()) },
                "members": members
                    .iter()
                    .map(|(r, u)| serde_json::json!({"request": r, "instance": u}))
                    .collect::<Vec<_>>(),
                "spare_units": spares,
            })
        };
        let mut swap_classes: Vec<serde_json::Value> = Vec::new();
        for c in &pool_classes {
            if emits(&c.3, c.4.len()) {
                swap_classes.push(pool_class_json(c));
            }
        }
        for c in &cluster_classes {
            if emits(&c.2, c.3.len()) {
                swap_classes.push(cluster_class_json(c));
            }
        }

        let assignment_json = serde_json::Value::Object(assignment);
        let swap_json = serde_json::Value::Array(swap_classes);

        // Persist as module properties so the data reaches the netlist and,
        // through schematic export, board-side fields. A module may solve
        // several parts: the properties merge every solve, keyed by request
        // name — a re-solved request replaces its own entry and drops the
        // prior swap classes that mention it (their freedom is stale).
        let mut property_assignment = assignment_json.clone();
        let mut property_swaps = swap_json.clone();
        if let Some(ctx) = eval.context_value() {
            // Pin and unit names belong to a part. A prior entry built on
            // silicon this solve never touched keeps its freedom verbatim.
            // Belt and braces with the builtin's own refusal: a value this
            // solve cannot read would be dropped without a word.
            let prior = |key: &str, object: bool| -> anyhow::Result<Option<serde_json::Value>> {
                let module = ctx.module();
                let Some(v) = module.properties().get(key) else {
                    return Ok(None);
                };
                v.to_value()
                    .unpack_str()
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
                    .filter(|j| if object { j.is_object() } else { j.is_array() })
                    .map(Some)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "pin_solve: module property `{key}` is written by pin_solve; \
                             record your own data under another name"
                        )
                    })
            };
            if let Some(prev) = prior(RESULT_PROPERTIES[0], true)?
                && let Some(prev_obj) = prev.as_object()
            {
                let cur = property_assignment
                    .as_object_mut()
                    .expect("assignment is an object");
                for (k, v) in prev_obj {
                    cur.entry(k.clone()).or_insert_with(|| {
                        // Prior alternates net of pins this solve claimed, but
                        // only when the entry sits on a part of this table.
                        let mut v = v.clone();
                        // Pads belong to the part the entry names — instance
                        // names repeat across parts. Other silicon is left be.
                        let named = v.get("part").and_then(|p| p.as_str()).unwrap_or_default();
                        let Some(part) = part_names.iter().position(|p| p == named) else {
                            return v;
                        };
                        if let Some(alts) = v.get_mut("alternates").and_then(|a| a.as_object_mut())
                        {
                            for list in alts.values_mut() {
                                if let Some(arr) = list.as_array_mut() {
                                    arr.retain(|p| {
                                        p.as_str()
                                            .map(|n| !used_pads.contains(&(part, n.to_owned())))
                                            .unwrap_or(true)
                                    });
                                }
                            }
                            alts.retain(|_, l| l.as_array().is_none_or(|a| !a.is_empty()));
                        }
                        v
                    });
                }
            }
            if let Some(prev) = prior(RESULT_PROPERTIES[1], false)?
                && let Some(prev_arr) = prev.as_array()
            {
                // Prior classes stay live, minus what this solve consumed:
                // re-solved members leave their old class (their freedom is
                // stale), spares lose newly claimed pins/units, and members
                // on a pool or cluster this solve touched again fold into the
                // fresh class — whose spares already account for every claim
                // in the module — so one class never fragments across solves.
                let solved: HashSet<&str> = reqs.iter().map(|r| r.name.as_str()).collect();
                let parse_members =
                    |class: &serde_json::Value, value_key: &str| -> Vec<(String, String)> {
                        class
                            .get("members")
                            .and_then(|m| m.as_array())
                            .map(|ms| {
                                ms.iter()
                                    .filter_map(|m| {
                                        let r = m.get("request").and_then(|v| v.as_str())?;
                                        let v = m.get(value_key).and_then(|v| v.as_str())?;
                                        (!solved.contains(r)).then(|| (r.to_owned(), v.to_owned()))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                let class_part = |c: &serde_json::Value| {
                    c.get("part")
                        .and_then(|p| p.as_str())
                        .unwrap_or_default()
                        .to_owned()
                };
                let mut merged_pools = pool_classes.clone();
                let mut merged_clusters = cluster_classes.clone();
                let mut merged: Vec<serde_json::Value> = Vec::new();
                for prior_class in prev_arr {
                    if prior_class.get("granularity").and_then(|g| g.as_str()) == Some("pin") {
                        let live = parse_members(prior_class, "pin");
                        let pool = prior_class
                            .get("pool")
                            .and_then(|p| p.as_str())
                            .unwrap_or_default();
                        let part = class_part(prior_class);
                        if let Some(cur) =
                            merged_pools.iter_mut().find(|c| c.0 == part && c.1 == pool)
                        {
                            cur.3.extend(live);
                            cur.3.sort();
                            continue;
                        }
                        // Pool this solve did not place on: keep it, net of the
                        // pins this solve claimed only if it is the same part.
                        let mine = part_names.iter().position(|p| *p == part);
                        let spares: Vec<String> = prior_class
                            .get("spare_pins")
                            .and_then(|s| s.as_array())
                            .map(|s| {
                                s.iter()
                                    .filter_map(|p| p.as_str())
                                    .filter(|n| {
                                        mine.is_none_or(|part| {
                                            !used_pads.contains(&(part, (*n).to_owned()))
                                        })
                                    })
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default();
                        if emits(&live, spares.len()) {
                            let rebind = prior_class
                                .get("rebind")
                                .and_then(|r| r.as_str())
                                .unwrap_or("firmware")
                                .to_owned();
                            merged.push(pool_class_json(&(
                                part,
                                pool.to_owned(),
                                rebind,
                                live,
                                spares,
                            )));
                        }
                    } else {
                        let live = parse_members(prior_class, "instance");
                        // A cluster's identity is its unit set (member units
                        // plus spares); overlap means the same silicon.
                        let mut prior_units: HashSet<String> = prior_class
                            .get("members")
                            .and_then(|m| m.as_array())
                            .map(|ms| {
                                ms.iter()
                                    .filter_map(|m| m.get("instance").and_then(|v| v.as_str()))
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default();
                        if let Some(spares) =
                            prior_class.get("spare_units").and_then(|s| s.as_array())
                        {
                            prior_units.extend(
                                spares.iter().filter_map(|s| s.as_str()).map(str::to_owned),
                            );
                        }
                        let part = class_part(prior_class);
                        if let Some(cur) = merged_clusters
                            .iter_mut()
                            .find(|c| c.0 == part && !c.1.is_disjoint(&prior_units))
                        {
                            cur.2.extend(live);
                            cur.2.sort();
                            continue;
                        }
                        // Cluster this solve did not place on: same rule, the
                        // units must belong to silicon this table knows.
                        let mine = part_names.iter().position(|p| *p == part);
                        let spares: Vec<String> = prior_class
                            .get("spare_units")
                            .and_then(|s| s.as_array())
                            .map(|s| {
                                s.iter()
                                    .filter_map(|u| u.as_str())
                                    .filter(|n| {
                                        mine.is_none_or(|part| {
                                            !assigned_instances
                                                .contains(&(scopes[part].clone(), (*n).to_owned()))
                                        })
                                    })
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default();
                        if emits(&live, spares.len()) {
                            merged.push(cluster_class_json(&(part, HashSet::new(), live, spares)));
                        }
                    }
                }
                for c in &merged_pools {
                    if emits(&c.3, c.4.len()) {
                        merged.push(pool_class_json(c));
                    }
                }
                for c in &merged_clusters {
                    if emits(&c.2, c.3.len()) {
                        merged.push(cluster_class_json(c));
                    }
                }
                property_swaps = serde_json::Value::Array(merged);
            }
        }
        eval.add_property(
            RESULT_PROPERTIES[0],
            heap.alloc(serde_json::to_string_pretty(&property_assignment).unwrap_or_default()),
        );
        eval.add_property(
            RESULT_PROPERTIES[1],
            heap.alloc(serde_json::to_string_pretty(&property_swaps).unwrap_or_default()),
        );

        // Every pad this part exposes, per part, for `pin_map` to tie off what
        // no request holds. Kept cumulative and whole: a module may solve one
        // part over several tables, and a re-solve hands pads back.
        if let Some(ctx) = eval.context_value() {
            for (i, _) in part_names.iter().enumerate() {
                let exposed = periphs
                    .iter()
                    .filter(|p| p.part_idx == i)
                    // A pad only reachable through a disabled peripheral is not
                    // this design's to tie off.
                    .filter(|p| {
                        p.unless
                            .as_deref()
                            .is_none_or(|axis| !config_truthy(&config, axis))
                    })
                    .flat_map(|p| p.signals.iter())
                    .flat_map(|(_, cands)| cands.iter())
                    .map(|c| c.name.clone());
                let mut v: Vec<String> = ctx
                    .exposed_pads(&scopes[i])
                    .into_iter()
                    .chain(exposed)
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();
                v.sort();
                ctx.record_exposed_pads(scopes[i].clone(), v);
            }
        }

        let pairs: Vec<(Value, Value)> = vec![
            (
                heap.alloc("assignment"),
                json_to_value(heap, &assignment_json),
            ),
            (heap.alloc("swap_classes"), json_to_value(heap, &swap_json)),
        ];
        Ok(heap.alloc(AllocDict(pairs)))
    }

    /// Turn a solved assignment into a `Component(pins=...)`-ready dict:
    /// physical pin name -> the net carried by the matching interface field.
    /// `ifaces` maps request names to the io()/interface instances holding the
    /// nets (a bare Net is accepted for single-signal requests). Requests
    /// absent from the assignment (e.g. dropped by `if_connected`) are
    /// skipped, so the two dicts can be declared side by side.
    fn pin_map<'v>(
        #[allow(unused_variables)] this: &Pinmux,
        #[starlark(require = pos)] assignment: Value<'v>,
        #[starlark(require = pos)] ifaces: SmallMap<String, Value<'v>>,
        #[starlark(require = named, default = NoneOr::None)] part: NoneOr<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        let ad = DictRef::from_value(assignment).ok_or_else(|| {
            anyhow::anyhow!("pin_map: first argument must be the `assignment` dict from pin_solve")
        })?;
        let mut out: Vec<(Value, Value)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        // Which part this call maps: the one named, else the single part the
        // assignment's requests all belong to.
        // The entry says which part it was solved on; the claim store only
        // answers for dicts built by hand.
        let scope_of = |req: &str| {
            dict_get(&ad, heap, req)
                .and_then(|e| DictRef::from_value(e).and_then(|ed| dict_get(&ed, heap, "part")))
                .map(|p| p.unpack_str().unwrap_or_default().to_owned())
                .or_else(|| {
                    eval.context_value()
                        .and_then(|ctx| ctx.pin_claim_scope(req))
                })
        };
        let mut assigned: Vec<String> = ad
            .keys()
            .filter_map(|k| k.unpack_str())
            .filter_map(scope_of)
            .collect();
        assigned.sort();
        assigned.dedup();
        // A solve that served no request still has pads to tie off, and the
        // module may have solved parts this assignment says nothing about.
        let served_none = assigned.is_empty();
        let scopes: Vec<String> = if served_none {
            eval.context_value()
                .map(|ctx| ctx.exposed_pad_scopes())
                .unwrap_or_default()
        } else {
            assigned
        };
        let want_scope: Option<String> = match &part {
            NoneOr::Other(p) => Some(p.clone()),
            NoneOr::None => {
                if scopes.len() > 1 {
                    return Err(if served_none {
                        anyhow::anyhow!(
                            "pin_map: this assignment serves no request and the module solved \
                             `{}`; pass part= to say which component to tie off",
                            scopes.join("`, `")
                        )
                    } else {
                        anyhow::anyhow!(
                            "pin_map: this assignment spans several parts; pass part= to map one \
                             component at a time"
                        )
                    });
                }
                scopes.first().cloned()
            }
        };
        let want_part = want_scope.as_deref();
        for (req_name, raw_val) in ifaces.iter() {
            // One call feeds one Component, so a multi-part solve maps a part
            // at a time.
            if let Some(want) = want_part
                && scope_of(req_name).is_some_and(|s| s != want)
            {
                continue;
            }
            // Accept at()-wrapped values: the constraint was consumed at solve
            // time, only the inner net matters here.
            let iface_val = &unpack_pin_at(*raw_val)
                .map(|(inner, _, _)| inner)
                .unwrap_or(*raw_val);
            let Some(entry) = dict_get(&ad, heap, req_name) else {
                continue;
            };
            let sigs = DictRef::from_value(entry)
                .and_then(|ed| dict_get(&ed, heap, "signals"))
                .and_then(DictRef::from_value)
                .ok_or_else(|| {
                    anyhow::anyhow!("pin_map: malformed assignment entry for `{req_name}`")
                })?;
            let sig_count = sigs.iter().count();
            // Declaration-side flattening applied to the instance.
            let leaves = net_leaves(*iface_val);
            for (s, pv) in sigs.iter() {
                let sig = s.unpack_str().unwrap_or_default().to_owned();
                let pin_name = DictRef::from_value(pv)
                    .and_then(|pd| dict_get_str(&pd, heap, "pin"))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "pin_map: malformed assignment entry for `{req_name}`/`{sig}`"
                        )
                    })?;
                let net = match leaves.iter().find(|(n, _)| *n == sig).map(|(_, v)| *v) {
                    Some(n) => n,
                    None if sig_count == 1
                        && (iface_val.downcast_ref::<NetValue>().is_some()
                            || iface_val.downcast_ref::<FrozenNetValue>().is_some()) =>
                    {
                        *iface_val
                    }
                    None => {
                        let carried = if leaves.is_empty() {
                            "it carries no signal".to_owned()
                        } else {
                            format!(
                                "it carries {}",
                                leaves
                                    .iter()
                                    .map(|(n, _)| format!("`{n}`"))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        };
                        return Err(anyhow::anyhow!(
                            "pin_map: request `{req_name}`: no signal `{sig}` on the provided \
                             value — {carried} (pass the io()/interface instance, or a bare Net \
                             for single-signal requests)"
                        ));
                    }
                };
                if !seen.insert(pin_name.clone()) {
                    return Err(anyhow::anyhow!(
                        "pin_map: physical pin `{pin_name}` mapped by two requests"
                    ));
                }
                // Across calls too: a request re-solved by a later pin_solve
                // releases its claims, so a superseded assignment can still
                // hand this pin to a different net.
                // The entry names the part it was solved on, so the guard holds
                // even when two components share a request name.
                if let Some(id) = net_identity(net)
                    && let Some(scope) = scope_of(req_name).or_else(|| want_part.map(str::to_owned))
                    && let Some(ctx) = eval.context_value()
                {
                    if let Some(prev) = ctx.pin_map_net(&scope, &pin_name)
                        && prev.0 != id.0
                    {
                        return Err(anyhow::anyhow!(
                            "pin_map: physical pin `{pin_name}` already mapped to net `{}` in \
                             this module, now `{}` — map the result of the last pin_solve for \
                             each request",
                            prev.1,
                            id.1
                        ));
                    }
                    ctx.record_pin_map(scope, pin_name.clone(), id);
                }
                out.push((heap.alloc(pin_name.as_str()), net));
            }
        }
        // The reverse of the skip above. Not an error: mapping an assignment
        // in several calls is legitimate, and so is mapping a superseded one.
        let mut unmapped: Vec<String> = ad
            .keys()
            .filter_map(|k| k.unpack_str())
            .filter(|k| !ifaces.contains_key(*k))
            .filter(|k| want_part.is_none_or(|want| scope_of(k).is_none_or(|s| s == want)))
            .map(|k| k.to_owned())
            .collect();
        if !unmapped.is_empty() {
            unmapped.sort();
            warn_at_call_site(
                eval,
                format!(
                    "pin_map: solved request(s) `{}` are absent from the ifaces dict, so their pads are wired nowhere — a claimed pad is not tied off either",
                    unmapped.join("`, `")
                ),
            );
        }
        // Pads this part exposes that no request holds are intentionally open,
        // so one call yields the component's whole pin table.
        if let Some(scope) = want_part {
            let free: Vec<String> = eval
                .context_value()
                .map(|ctx| {
                    let held = ctx.claimed_pins(scope);
                    ctx.exposed_pads(scope)
                        .into_iter()
                        .filter(|pad| !held.contains(pad) && seen.insert(pad.clone()))
                        .collect()
                })
                .unwrap_or_default();
            for pad in free {
                // The same pad answers with the same net however many calls
                // map this part, so two dicts merge without contradicting.
                let open = match eval
                    .context_value()
                    .and_then(|ctx| ctx.tied_off_net(scope, &pad))
                {
                    Some(net) => net,
                    None => {
                        let net = crate::lang::net::alloc_open_net(eval);
                        if let Some(ctx) = eval.context_value() {
                            ctx.record_tie_off(scope, &pad, net);
                        }
                        net
                    }
                };
                out.push((heap.alloc(pad.as_str()), open));
            }
        }
        Ok(heap.alloc(AllocDict(out)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock(pin: &str) -> PinLock {
        PinLock {
            any: vec![pin.to_owned()],
            by_signal: Vec::new(),
        }
    }

    fn path(segs: &[&str]) -> Vec<String> {
        segs.iter().map(|s| (*s).to_owned()).collect()
    }

    fn record(
        c: &mut PinConstraints,
        input: &str,
        module: &str,
        owner: Vec<String>,
        net: u64,
        pin: &str,
    ) {
        c.record(
            input.to_owned(),
            module.to_owned(),
            None,
            owner,
            &[net],
            lock(pin),
            false,
        );
    }

    /// Inline because the scenario is not reachable from `.zen`: whether a
    /// second constraint on the same net is recorded before or after the early
    /// solve depends on child-evaluation order, which a design cannot control.
    /// A credit is per `(module, input, net)`, so it absolves one entry of its
    /// own module and nothing of a sibling's.
    #[test]
    fn a_preconsume_credit_absolves_one_later_entry_only() {
        let (a, b) = (path(&["A"]), path(&["B"]));
        let mut c = PinConstraints::default();
        c.preconsume(&a, "IO", &[5]);
        record(&mut c, "IO", "a.zen", a, 5, "PA1");
        record(&mut c, "IO", "b.zen", b, 5, "PA2");

        let left = c.unconsumed_hard();
        assert_eq!(left.len(), 1, "only the sibling's remains, got {left:?}");
        assert_eq!(left[0].0, "b.zen");
    }

    /// A credit spent on one net must not subtract from a sibling net already
    /// at zero: `credited` is decided by any net, the decrement is per net.
    #[test]
    fn spending_a_credit_never_underflows_a_zero_net() {
        let a = path(&["A"]);
        let mut c = PinConstraints::default();
        c.preconsume(&a, "X", &[1, 2]);
        c.record(
            "X".into(),
            "m".into(),
            None,
            a.clone(),
            &[1, 2],
            lock("PA1"),
            false,
        );
        c.preconsume(&a, "X", &[1]);
        c.record("X".into(), "m".into(), None, a, &[1, 2], lock("PA2"), false);
    }

    /// Settling an entry that already exists must not also leave a credit: the
    /// stray one would absolve a sibling instance's constraint on the same net.
    #[test]
    fn settling_an_existing_entry_leaves_no_stray_credit() {
        let (a, b) = (path(&["A"]), path(&["B"]));
        let mut c = PinConstraints::default();
        record(&mut c, "IO", "a.zen", a.clone(), 9, "P1");
        c.settle(&[9], &a, "IO");
        record(&mut c, "IO", "b.zen", b, 9, "P2");

        let left = c.unconsumed_hard();
        assert_eq!(
            left.len(),
            1,
            "B's constraint must still be reported, got {left:?}"
        );
    }

    /// A constraint another solve holds is not this input's, so its own entry
    /// is still to come and must keep its credit.
    #[test]
    fn a_contended_settle_still_credits() {
        let som = path(&["SOM"]);
        let leaf = path(&["SOM", "U1"]);
        let other = path(&["SOM", "U2"]);
        let mut c = PinConstraints::default();
        record(&mut c, "RAIL", "som.zen", som, 9, "PA1");
        assert!(matches!(
            c.claim(&[9], &other, "RAIL", None),
            Claimed::Yes(..)
        ));

        // U1 solves before its own io() records: the ancestor entry is taken,
        // so nothing here is U1's to claim.
        c.settle(&[9], &leaf, "IO");
        record(&mut c, "IO", "u1.zen", leaf, 9, "PA2");

        let left = c.unconsumed_hard();
        assert!(
            !left.iter().any(|(_, input, _)| input == "IO"),
            "U1's own constraint was honoured, got {left:?}"
        );
    }

    /// Constraints owned higher up, however tangled among themselves, do not
    /// cost a module the credit for the wrapper it read off its own input —
    /// the credit is keyed on that module, that input and that net.
    #[test]
    fn an_ancestor_tie_does_not_cost_the_module_its_credit() {
        let som = path(&["SOM"]);
        let leaf = path(&["SOM", "U1"]);
        let mut c = PinConstraints::default();
        record(&mut c, "A", "som.zen", som.clone(), 9, "PA1");
        record(&mut c, "B", "som.zen", som, 9, "PA2");

        c.settle(&[9], &leaf, "IO");
        record(&mut c, "IO", "u1.zen", leaf, 9, "PA3");

        let left = c.unconsumed_hard();
        assert!(
            !left.iter().any(|(_, input, _)| input == "IO"),
            "the solve read it off the wrapper, got {left:?}"
        );
        assert_eq!(left.len(), 2, "the ancestors' two remain, got {left:?}");
    }

    /// Re-evaluation records one declaration twice; the claim takes the whole
    /// group, so no duplicate is left to be reported as never consumed.
    #[test]
    fn a_duplicated_record_is_claimed_with_its_group() {
        let som = path(&["SOM"]);
        let leaf = path(&["SOM", "U1"]);
        let mut c = PinConstraints::default();
        record(&mut c, "LED", "som.zen", som.clone(), 9, "PA7");
        record(&mut c, "LED", "som.zen", som, 9, "PA7");

        assert!(matches!(
            c.claim(&[9], &leaf, "LED", None),
            Claimed::Yes(..)
        ));
        let left = c.unconsumed_hard();
        assert!(left.is_empty(), "got {left:?}");
    }

    /// A re-solve of the same request keeps its constraint, while a sibling
    /// request on the same net still cannot take it.
    #[test]
    fn a_claim_is_reserved_for_the_solve_that_took_it() {
        let som = path(&["SOM"]);
        let leaf = path(&["SOM", "U1"]);
        let other = path(&["SOM", "U2"]);

        let mut c = PinConstraints::default();
        record(&mut c, "LED", "som.zen", som, 9, "PA7");
        let took = |c: &mut PinConstraints, who: &[String]| {
            matches!(c.claim(&[9], who, "LED", None), Claimed::Yes(..))
        };
        assert!(took(&mut c, &leaf), "first solve takes it");
        assert!(!took(&mut c, &other), "a sibling may not");
        assert!(took(&mut c, &leaf), "the same solve may take it again");
        // Both are named, sorted, whichever of them ran first.
        let contended = c.contended();
        assert_eq!(contended.len(), 1, "got {contended:?}");
        assert_eq!(contended[0].3, ["SOM.U1", "SOM.U2"], "got {contended:?}");
        assert!(c.unconsumed_hard().is_empty());
    }

    /// Two ancestor-owned constraints on one net at the same depth cannot be
    /// told apart by owner, and entry order is not stable under parallel child
    /// evaluation, so the solve reports rather than picks.
    #[test]
    fn two_forwarded_constraints_on_one_net_are_reported_not_guessed() {
        let som = path(&["SOM"]);
        let leaf = path(&["SOM", "U1"]);

        let mut c = PinConstraints::default();
        record(&mut c, "A", "som.zen", som.clone(), 9, "PA1");
        record(&mut c, "B", "som.zen", som, 9, "PA2");

        assert!(
            matches!(c.claim(&[9], &leaf, "LED", None), Claimed::Ambiguous),
            "ambiguous, no guess"
        );
        let Some(Ambiguity::Entries { input, names }) = c.take_ambiguous() else {
            panic!("ambiguity reported");
        };
        assert_eq!(input, "LED");
        assert_eq!(names, "A`, `B");
    }
}
