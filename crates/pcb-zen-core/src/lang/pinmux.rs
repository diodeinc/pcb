//! Peripheral capability model: components declare what their pins *can do*,
//! `pin_solve` performs the joint instance x pin assignment at elaboration,
//! with exclusivity at both tiers structural (infeasible, not lint). Matching
//! is nominal over `interface()` identities, closed over `implies=[...]`;
//! results land in the `pin_assignment` / `swap_classes` module properties.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::str::FromStr;

use allocative::Allocative;
use pcb_sch::physical::PhysicalValue;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
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
use crate::lang::net::{FrozenNetValue, NetValue};

const REBIND_VALUES: [&str; 3] = ["none", "firmware", "fixed"];
const PINMAP_CAP: usize = 512;
const SOLVER_BUDGET: usize = 200_000;

struct IfaceInfo<'v> {
    id: TypeInstanceId,
    name: String,
    fields: Vec<String>,
    implies: Vec<Value<'v>>,
    /// Declared capability-attribute vocabulary: name -> physical type value.
    attr_specs: Vec<(String, Value<'v>)>,
}

fn iface_info<'v>(v: Value<'v>) -> Option<IfaceInfo<'v>> {
    if let Some(f) = v.downcast_ref::<InterfaceFactory<'v>>() {
        return Some(IfaceInfo {
            id: f.type_instance_id(),
            name: f.type_name().unwrap_or_else(|| v.to_string()),
            fields: f.fields().iter().map(|(k, _)| k.clone()).collect(),
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
            id: f.type_instance_id(),
            name: f.type_name().unwrap_or_else(|| v.to_string()),
            fields: f.fields().iter().map(|(k, _)| k.clone()).collect(),
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
fn iface_closure<'v>(root: Value<'v>) -> anyhow::Result<Vec<IfaceInfo<'v>>> {
    let root_info = iface_info(root)
        .ok_or_else(|| anyhow::anyhow!("expected an interface type, got `{}`", root.get_type()))?;
    let mut seen: HashSet<TypeInstanceId> = HashSet::new();
    seen.insert(root_info.id);
    let mut out = vec![root_info];
    let mut cursor = 0;
    while cursor < out.len() {
        let implied: Vec<Value<'v>> = out[cursor].implies.clone();
        cursor += 1;
        for iv in implied {
            let info = iface_info(iv).ok_or_else(|| {
                anyhow::anyhow!("interface(implies=...) entry is not an interface type")
            })?;
            if seen.insert(info.id) {
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
    provides_ids: HashSet<TypeInstanceId>,
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

    /// Provides-set key for cluster swap grouping (sorted nominal ids).
    fn provides_key(&self) -> String {
        let mut ids: Vec<String> = self.provides_ids.iter().map(|i| format!("{i:?}")).collect();
        ids.sort();
        ids.join(",")
    }
}

struct RReq<'v> {
    name: String,
    iface_id: TypeInstanceId,
    iface_name: String,
    iface_closure_len: usize,
    uses: Vec<String>,
    instance: Option<String>,
    prefer: Vec<String>,
    lock: bool,
    where_fn: Option<Value<'v>>,
    direction: Option<String>,
    /// Serve this request only when the module input of the same name was
    /// actually connected by the caller (io() slot pattern).
    if_connected: bool,
}

#[derive(Clone)]
struct Combo {
    periph_idx: usize,
    /// Pin choice per used signal, in `uses` order.
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

fn parse_rperiph<'v>(v: Value<'v>, heap: Heap<'v>) -> anyhow::Result<RPeriph<'v>> {
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
    for p in provides_list.iter() {
        for info in iface_closure(p)? {
            provides_ids.insert(info.id);
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
        provides_ids,
        signals,
        rebind: dict_get_str(&d, heap, "rebind").unwrap_or_else(|| "firmware".to_owned()),
        attrs: dict_get(&d, heap, "attrs").unwrap_or_else(Value::new_none),
        unless: dict_get_str(&d, heap, "unless"),
        pool: dict_get_str(&d, heap, "pool"),
    })
}

fn parse_rreq<'v>(v: Value<'v>, heap: Heap<'v>) -> anyhow::Result<RReq<'v>> {
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
    let closure = iface_closure(iface)?;
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
        _ => closure[0].fields.clone(),
    };
    let prefer = match dict_get(&d, heap, "prefer") {
        Some(p) if !p.is_none() => ListRef::from_value(p)
            .ok_or_else(|| anyhow::anyhow!("request `{name}`: prefer must be a list"))?
            .iter()
            .filter_map(|s| s.unpack_str().map(|s| s.to_owned()))
            .collect(),
        _ => Vec::new(),
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
        where_fn: dict_get(&d, heap, "where").filter(|v| !v.is_none()),
        direction: dict_get_str(&d, heap, "direction"),
        if_connected: dict_get(&d, heap, "if_connected")
            .map(|v| v.to_bool())
            .unwrap_or(false),
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
) -> anyhow::Result<(Vec<Combo>, Vec<(String, String)>)> {
    let mut out: Vec<Combo> = Vec::new();
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
            rejects.push((
                p.name.clone(),
                format!("does not provide {}", req.iface_name),
            ));
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
            let verdict = eval.eval_function(where_fn, &[p.attrs], &[]).map_err(|e| {
                anyhow::anyhow!("request `{}`: where= predicate failed: {e}", req.name)
            })?;
            if !verdict.to_bool() {
                rejects.push((p.name.clone(), "where= predicate rejected".to_owned()));
                continue;
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

        // Enumerate pin maps (product), excluding duplicate pins within the instance.
        let mut pinmaps: Vec<Vec<(String, RPin)>> = vec![Vec::new()];
        let mut truncated = false;
        for (s, cands) in &cand_lists {
            let mut next = Vec::new();
            for base in &pinmaps {
                let mut usable = cands
                    .iter()
                    .filter(|c| !base.iter().any(|(_, b)| b.name == c.name));
                for c in &mut usable {
                    let mut d = base.clone();
                    d.push(((*s).to_owned(), (*c).clone()));
                    next.push(d);
                    if next.len() >= PINMAP_CAP {
                        break;
                    }
                }
                truncated |= usable.next().is_some();
            }
            pinmaps = next;
        }
        if truncated {
            warn_at_call_site(
                eval,
                format!(
                    "pin_solve: request `{}`: pin combinations for `{}` capped at {PINMAP_CAP}; the assignment may be suboptimal",
                    req.name, p.name
                ),
            );
        }

        let surplus = p.provides_ids.len() as i64 - req.iface_closure_len as i64;
        for pm in pinmaps {
            if req.lock
                && !req.prefer.is_empty()
                && !pm.iter().all(|(_, c)| req.prefer.contains(&c.name))
            {
                continue;
            }
            let mut cost = 100 * surplus;
            for (sig, c) in &pm {
                cost += c.cost;
                if c.strap {
                    cost += 50;
                }
                if req.prefer.contains(&c.name) {
                    cost -= 10;
                }
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
    }

    out.sort_by(|a, b| {
        (a.cost, &periphs[a.periph_idx].name, &a.key).cmp(&(
            b.cost,
            &periphs[b.periph_idx].name,
            &b.key,
        ))
    });
    Ok((out, rejects))
}

/// Deterministic branch-and-bound over the joint assignment space (pin
/// conflicts couple otherwise independent instance choices, so this is
/// constraint optimization, not plain matching). Candidates are explored in
/// per-request cost order and pruned with an admissible lower bound, so the
/// result minimizes total cost. On budget exhaustion the best solution found
/// so far is returned; only optimality can degrade, never feasibility.
fn assign<'v>(reqs: &[RReq<'v>], all_combos: &[Vec<Combo>]) -> Option<Vec<usize>> {
    let n = reqs.len();
    if n == 0 {
        return Some(Vec::new());
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

    let mut choice = vec![0usize; n];
    // chosen combo index per request (by original request index)
    let mut assigned: Vec<Option<usize>> = vec![None; n];
    let mut partial_cost = vec![0i64; n + 1];
    let mut used_inst: HashSet<usize> = HashSet::new();
    let mut used_pin: HashSet<String> = HashSet::new();
    let mut best: Option<(i64, Vec<usize>)> = None;
    let mut pos: usize = 0;

    for _ in 0..SOLVER_BUDGET {
        if pos == n {
            let total = partial_cost[n];
            // Strict improvement keeps the first (deterministic) optimum.
            if best.as_ref().map(|(b, _)| total < *b).unwrap_or(true) {
                best = Some((total, assigned.iter().map(|c| c.unwrap()).collect()));
            }
            // Keep searching for a cheaper solution: force a backtrack.
            pos -= 1;
            let back_ri = order[pos];
            let back = &all_combos[back_ri][assigned[back_ri].unwrap()];
            used_inst.remove(&back.periph_idx);
            for (_, p) in &back.pins {
                used_pin.remove(&p.name);
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
            let clash = used_inst.contains(&c.periph_idx)
                || c.pins.iter().any(|(_, p)| used_pin.contains(&p.name));
            if !clash {
                choice[pos] = ci;
                assigned[ri] = Some(ci);
                used_inst.insert(c.periph_idx);
                for (_, p) in &c.pins {
                    used_pin.insert(p.name.clone());
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
            let back = &all_combos[back_ri][assigned[back_ri].unwrap()];
            used_inst.remove(&back.periph_idx);
            for (_, p) in &back.pins {
                used_pin.remove(&p.name);
            }
            assigned[back_ri] = None;
            choice[pos] += 1;
        }
    }
    best.map(|(_, sol)| sol)
}

fn json_to_value<'v>(heap: Heap<'v>, v: &serde_json::Value) -> Value<'v> {
    match v {
        serde_json::Value::Null => Value::new_none(),
        serde_json::Value::Bool(b) => Value::new_bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                heap.alloc(i as i32)
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
    pub pins: Vec<String>,
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

/// `(inner, pins, soft)` of an `at()` wrapper (mutable or frozen), if `v` is one.
fn unpack_pin_at<'v>(v: Value<'v>) -> Option<(Value<'v>, Vec<String>, bool)> {
    if let Some(w) = v.downcast_ref::<PinAt<'v>>() {
        Some((w.inner.to_value(), w.pins.clone(), w.soft))
    } else {
        v.downcast_ref::<FrozenPinAt>()
            .map(|w| (w.inner.to_value(), w.pins.clone(), w.soft))
    }
}

/// If `value` is an `at()` wrapper, record its constraint against the io()
/// input `name` and return the inner value; otherwise return `value` as-is.
pub(crate) fn unwrap_pin_at<'v>(
    name: &str,
    value: Value<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> Value<'v> {
    match unpack_pin_at(value) {
        Some((inner, pins, soft)) => {
            if let Some(ctx) = eval.context_value() {
                ctx.add_pin_constraint(name, pins, soft);
            }
            inner
        }
        None => value,
    }
}

/// Shared construction of a peripheral dict (used by `peripheral()` and `pool()`).
#[allow(clippy::too_many_arguments)]
fn make_peripheral_dict<'v>(
    heap: Heap<'v>,
    name: &str,
    provides: &[Value<'v>],
    signals: Vec<(String, Vec<Value<'v>>)>,
    rebind: &str,
    attrs: &SmallMap<String, Value<'v>>,
    symmetric: Value<'v>,
    unless: Option<&str>,
    pool: Option<&str>,
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
        for info in iface_closure(*p)? {
            for field in &info.fields {
                let ok = signals
                    .iter()
                    .any(|(s, cands)| s == field && !cands.is_empty());
                if !ok {
                    return Err(anyhow::anyhow!(
                        "peripheral `{name}` claims {} but has no candidate for signal `{field}`",
                        info.name
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
        for info in iface_closure(*p)? {
            for (aname, ty) in &info.attr_specs {
                if let Some(t) = ty.downcast_ref::<pcb_sch::physical::PhysicalValueType>()
                    && !declared.iter().any(|(n, _, _)| n == aname)
                {
                    declared.push((aname.clone(), t.dims(), info.name.clone()));
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
        (heap.alloc("symmetric"), symmetric),
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

#[starlark_module]
pub(crate) fn pinmux_globals(builder: &mut GlobalsBuilder) {
    /// Constrain which physical pin(s) a connection may use, at the
    /// connection site: `Mcu(IO0 = at(led_net, "PA8"))`. Hard by default
    /// (build error if unsatisfiable); `soft = True` makes it a preference
    /// the solver may fall back from.
    fn at<'v>(
        #[starlark(require = pos)] value: Value<'v>,
        #[starlark(require = pos)] pins: Value<'v>,
        #[starlark(require = named, default = false)] soft: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let pin_list: Vec<String> = if let Some(s) = pins.unpack_str() {
            vec![s.to_owned()]
        } else if let Some(l) = ListRef::from_value(pins) {
            l.iter()
                .map(|v| {
                    v.unpack_str().map(|s| s.to_owned()).ok_or_else(|| {
                        anyhow::anyhow!("at(): pins must be a pin name or a list of pin names")
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?
        } else {
            return Err(anyhow::anyhow!(
                "at(): pins must be a pin name or a list of pin names, got `{}`",
                pins.get_type()
            ));
        };
        if pin_list.is_empty() {
            return Err(anyhow::anyhow!("at(): pins must not be empty"));
        }
        Ok(eval.heap().alloc(PinAt {
            inner: value,
            pins: pin_list,
            soft,
        }))
    }
    /// One candidate physical pin for a peripheral signal. `data=` carries
    /// opaque realization info (e.g. `{"af": 1}` on STM32, `{"iomux_func": 0}`
    /// on ESP32) verbatim into the solved assignment; the solver ignores it.
    fn pin<'v>(
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
        #[starlark(require = pos)] name: String,
        #[starlark(require = named)] provides: UnpackList<Value<'v>>,
        #[starlark(require = named)] signals: SmallMap<String, Value<'v>>,
        #[starlark(require = named)] rebind: String,
        #[starlark(require = named, default = SmallMap::default())] attrs: SmallMap<
            String,
            Value<'v>,
        >,
        #[starlark(require = named, default = NoneOr::None)] symmetric: NoneOr<Value<'v>>,
        #[starlark(require = named, default = NoneOr::None)] unless: NoneOr<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        for p in &provides.items {
            if iface_info(*p).is_none() {
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
        let symmetric_v = match symmetric {
            NoneOr::None => heap.alloc(Vec::<Value>::new()),
            NoneOr::Other(v) => v,
        };
        make_peripheral_dict(
            heap,
            &name,
            &provides.items,
            normalized,
            &rebind,
            &attrs,
            symmetric_v,
            unless.into_option().as_deref(),
            None,
        )
    }

    /// Sugar for N interchangeable single-signal units over a pin set (GPIO pools).
    fn pool<'v>(
        #[starlark(require = pos)] name: String,
        #[starlark(require = named)] provides: UnpackList<Value<'v>>,
        #[starlark(require = named)] pins: UnpackList<Value<'v>>,
        #[starlark(require = named, default = "firmware".to_string())] rebind: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        if provides.items.len() != 1 {
            return Err(anyhow::anyhow!(
                "pool `{name}`: provides must list exactly one interface type"
            ));
        }
        let info = iface_info(provides.items[0]).ok_or_else(|| {
            anyhow::anyhow!("pool `{name}`: provides entry must be an interface type")
        })?;
        if info.fields.len() != 1 {
            return Err(anyhow::anyhow!(
                "pool `{name}`: the provided interface must have exactly one signal, `{}` has {}",
                info.name,
                info.fields.len()
            ));
        }
        let field = info.fields[0].clone();
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
                heap,
                &format!("{name}.{pin_name}"),
                &provides.items,
                vec![(field.clone(), vec![pin_dict])],
                &rebind,
                &SmallMap::default(),
                heap.alloc(Vec::<Value>::new()),
                None,
                Some(&name),
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
        #[starlark(require = pos)] name: String,
        #[starlark(require = pos)] iface: Value<'v>,
        #[starlark(require = named, default = NoneOr::None)] uses: NoneOr<UnpackList<String>>,
        #[starlark(require = named, default = NoneOr::None)] instance: NoneOr<String>,
        #[starlark(require = named, default = UnpackList::default())] prefer: UnpackList<String>,
        #[starlark(require = named, default = false)] lock: bool,
        #[starlark(require = named, default = NoneOr::None)] r#where: NoneOr<Value<'v>>,
        #[starlark(require = named, default = NoneOr::None)] direction: NoneOr<String>,
        #[starlark(require = named, default = false)] if_connected: bool,
        #[starlark(require = named, default = NoneOr::None)] bind: NoneOr<Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        // bind= carries the connected value on the request itself (the
        // dict-of-roles pattern). An at() wrapper contributes its pin
        // constraint here and the inner value flows on.
        let (bind_val, bind_pins, bind_soft) = match bind {
            NoneOr::Other(v) => match unpack_pin_at(v) {
                Some((inner, pins, soft)) => (Some(inner), Some(pins), soft),
                None => (Some(v), None, false),
            },
            NoneOr::None => (None, None, false),
        };
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
        let info = iface_info(iface).ok_or_else(|| {
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
        let uses_vals: Vec<String> = match uses {
            NoneOr::Other(u) => {
                for s in &u.items {
                    if !info.fields.contains(s) {
                        return Err(anyhow::anyhow!(
                            "pin_request `{name}`: `{s}` is not a signal of {}",
                            info.name
                        ));
                    }
                }
                u.items
            }
            NoneOr::None => info.fields.clone(),
        };
        let uses_alloc: Vec<Value> = uses_vals.iter().map(|s| heap.alloc(s.as_str())).collect();
        // An at()-derived constraint applies unless the request already sets
        // prefer/lock explicitly.
        let (prefer_items, lock) = match (&bind_pins, prefer.items.is_empty()) {
            (Some(pins), true) => (pins.clone(), !bind_soft),
            _ => (prefer.items.clone(), lock),
        };
        let prefer_alloc: Vec<Value> = prefer_items
            .iter()
            .map(|s| heap.alloc(s.as_str()))
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
            (heap.alloc("bind"), bind_val.unwrap_or_else(Value::new_none)),
        ];
        Ok(heap.alloc(AllocDict(pairs)))
    }

    /// Deterministic joint instance x pin assignment. Fails elaboration when
    /// infeasible; records `pin_assignment` and `swap_classes` module
    /// properties (JSON) for downstream consumers.
    fn pin_solve<'v>(
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

        // Flatten one level of nesting so `[usart1] + gpio_pool` and
        // `[usart1, gpio_pool]` both work.
        let plist = ListRef::from_value(peripherals)
            .ok_or_else(|| anyhow::anyhow!("pin_solve: peripherals must be a list"))?;
        let mut periphs: Vec<RPeriph> = Vec::new();
        for entry in plist.iter() {
            if let Some(nested) = ListRef::from_value(entry) {
                for e in nested.iter() {
                    periphs.push(parse_rperiph(e, heap)?);
                }
            } else {
                periphs.push(parse_rperiph(entry, heap)?);
            }
        }
        let mut seen_names = HashSet::new();
        for p in &periphs {
            if !seen_names.insert(p.name.clone()) {
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
            reqs.push(parse_rreq(entry, heap)?);
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

        // io() slot pattern: an `if_connected` request is served only when the
        // caller actually connected the module input of the same name.
        let connected: HashSet<String> = eval
            .context_value()
            .map(|ctx| {
                ctx.module()
                    .inputs()
                    .iter()
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();
        reqs.retain(|r| !r.if_connected || connected.contains(&r.name));

        // `at()` constraints recorded at io-binding time override any
        // request-side prefer/lock defaults.
        if let Some(ctx) = eval.context_value() {
            for r in reqs.iter_mut() {
                if let Some((pins, soft)) = ctx.pin_constraint(&r.name) {
                    r.prefer = pins;
                    r.lock = !soft;
                }
            }
        }

        let prev_map = match previous {
            NoneOr::Other(v) => parse_previous(v, heap),
            NoneOr::None => HashMap::new(),
        };

        // Candidate combos per request, with reasoned rejections.
        let mut all_combos: Vec<Vec<Combo>> = Vec::new();
        for r in &reqs {
            let (combos, rejects) = combos_for_request(r, &periphs, &config, &prev_map, eval)?;
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

        // Joint assignment.
        let Some(chosen) = assign(&reqs, &all_combos) else {
            let detail = reqs
                .iter()
                .enumerate()
                .map(|(i, r)| format!("  {}: {} candidate combo(s)", r.name, all_combos[i].len()))
                .collect::<Vec<_>>()
                .join("\n");
            return Err(anyhow::anyhow!(
                "pin_solve: no feasible assignment (instance/pin exclusivity):\n{detail}"
            ));
        };

        // Build results (JSON first; the Starlark value mirrors it).
        let mut used_pins: HashSet<String> = HashSet::new();
        for (i, &ci) in chosen.iter().enumerate() {
            for (_, p) in &all_combos[i][ci].pins {
                used_pins.insert(p.name.clone());
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
                let free: Vec<serde_json::Value> = periph
                    .signal(sig)
                    .map(|cands| {
                        cands
                            .iter()
                            .filter(|c| !used_pins.contains(&c.name))
                            .map(|c| serde_json::Value::String(c.name.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                if !free.is_empty() {
                    alternates.insert(sig.clone(), serde_json::Value::Array(free));
                }
            }
            let mut entry = serde_json::Map::new();
            entry.insert(
                "instance".into(),
                serde_json::Value::String(periph.name.clone()),
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

        // Residual freedom: pool classes (pin granularity) + identical-provides
        // rebind="none" clusters (gate swap).
        let mut swap_classes: Vec<serde_json::Value> = Vec::new();
        {
            let mut by_pool: Vec<(String, Vec<(String, String)>)> = Vec::new();
            for (i, r) in reqs.iter().enumerate() {
                if r.lock {
                    continue;
                }
                let combo = &all_combos[i][chosen[i]];
                let periph = &periphs[combo.periph_idx];
                let Some(pool) = &periph.pool else { continue };
                let pin = combo.pins[0].1.name.clone();
                match by_pool.iter_mut().find(|(p, _)| p == pool) {
                    Some((_, members)) => members.push((r.name.clone(), pin)),
                    None => by_pool.push((pool.clone(), vec![(r.name.clone(), pin)])),
                }
            }
            by_pool.sort_by(|a, b| a.0.cmp(&b.0));
            for (pool, mut members) in by_pool {
                members.sort();
                let mut spares: Vec<String> = periphs
                    .iter()
                    .filter(|p| p.pool.as_deref() == Some(pool.as_str()))
                    .filter_map(|p| p.signals.first().and_then(|(_, c)| c.first()))
                    .map(|c| c.name.clone())
                    .filter(|n| !used_pins.contains(n))
                    .collect();
                spares.sort();
                let rebind = periphs
                    .iter()
                    .find(|p| p.pool.as_deref() == Some(pool.as_str()))
                    .map(|p| p.rebind.clone())
                    .unwrap_or_else(|| "firmware".to_owned());
                if members.len() >= 2 || (!members.is_empty() && !spares.is_empty()) {
                    swap_classes.push(serde_json::json!({
                        "granularity": "pin",
                        "rebind": rebind,
                        "pool": pool,
                        "members": members
                            .iter()
                            .map(|(r, p)| serde_json::json!({"request": r, "pin": p}))
                            .collect::<Vec<_>>(),
                        "spare_pins": spares,
                    }));
                }
            }

            let mut by_cluster: Vec<(String, Vec<(String, String)>)> = Vec::new();
            for (i, r) in reqs.iter().enumerate() {
                if r.lock {
                    continue;
                }
                let combo = &all_combos[i][chosen[i]];
                let periph = &periphs[combo.periph_idx];
                if periph.rebind != "none" || periph.pool.is_some() {
                    continue;
                }
                let key = periph.provides_key();
                let member = (r.name.clone(), periph.name.clone());
                match by_cluster.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, members)) => members.push(member),
                    None => by_cluster.push((key, vec![member])),
                }
            }
            by_cluster.sort_by(|a, b| a.0.cmp(&b.0));
            let assigned_instances: HashSet<String> = chosen
                .iter()
                .enumerate()
                .map(|(i, &ci)| periphs[all_combos[i][ci].periph_idx].name.clone())
                .collect();
            for (key, mut members) in by_cluster {
                members.sort();
                let mut spares: Vec<String> = periphs
                    .iter()
                    .filter(|p| {
                        p.rebind == "none"
                            && p.pool.is_none()
                            && p.provides_key() == key
                            && !assigned_instances.contains(&p.name)
                    })
                    .map(|p| p.name.clone())
                    .collect();
                spares.sort();
                if members.len() >= 2 || (!members.is_empty() && !spares.is_empty()) {
                    swap_classes.push(serde_json::json!({
                        "granularity": "cluster",
                        "rebind": "none",
                        "members": members
                            .iter()
                            .map(|(r, u)| serde_json::json!({"request": r, "instance": u}))
                            .collect::<Vec<_>>(),
                        "spare_units": spares,
                    }));
                }
            }
        }

        let assignment_json = serde_json::Value::Object(assignment);
        let swap_json = serde_json::Value::Array(swap_classes);

        // Persist as module properties so the data reaches the netlist and,
        // through schematic export, board-side fields.
        eval.add_property(
            "pin_assignment",
            heap.alloc(serde_json::to_string_pretty(&assignment_json).unwrap_or_default()),
        );
        eval.add_property(
            "swap_classes",
            heap.alloc(serde_json::to_string_pretty(&swap_json).unwrap_or_default()),
        );

        // Candidate pins no served request claimed: the component wires them
        // as intentionally open (or reuses them), without re-listing them.
        let mut free_pins: Vec<String> = periphs
            .iter()
            .flat_map(|p| p.signals.iter())
            .flat_map(|(_, cands)| cands.iter())
            .map(|c| c.name.clone())
            .filter(|n| !used_pins.contains(n))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        free_pins.sort();
        let free_vals: Vec<Value> = free_pins.iter().map(|n| heap.alloc(n.as_str())).collect();

        let pairs: Vec<(Value, Value)> = vec![
            (
                heap.alloc("assignment"),
                json_to_value(heap, &assignment_json),
            ),
            (heap.alloc("swap_classes"), json_to_value(heap, &swap_json)),
            (heap.alloc("free_pins"), heap.alloc(free_vals)),
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
        #[starlark(require = pos)] assignment: Value<'v>,
        #[starlark(require = pos)] ifaces: SmallMap<String, Value<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
        let ad = DictRef::from_value(assignment).ok_or_else(|| {
            anyhow::anyhow!("pin_map: first argument must be the `assignment` dict from pin_solve")
        })?;
        let mut out: Vec<(Value, Value)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (req_name, raw_val) in ifaces.iter() {
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
            for (s, pv) in sigs.iter() {
                let sig = s.unpack_str().unwrap_or_default().to_owned();
                let pin_name = DictRef::from_value(pv)
                    .and_then(|pd| dict_get_str(&pd, heap, "pin"))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "pin_map: malformed assignment entry for `{req_name}`/`{sig}`"
                        )
                    })?;
                let net = match interface_field(*iface_val, &sig) {
                    Some(n) => n,
                    None if sig_count == 1 => *iface_val,
                    None => {
                        return Err(anyhow::anyhow!(
                            "pin_map: request `{req_name}`: no field `{sig}` on the provided value \
                             (pass the io()/interface instance, or a bare Net for single-signal requests)"
                        ));
                    }
                };
                if !seen.insert(pin_name.clone()) {
                    return Err(anyhow::anyhow!(
                        "pin_map: physical pin `{pin_name}` mapped by two requests"
                    ));
                }
                out.push((heap.alloc(pin_name.as_str()), net));
            }
        }
        Ok(heap.alloc(AllocDict(out)))
    }
}

/// Field of an interface *instance* (mutable or frozen), if `v` is one.
fn interface_field<'v>(v: Value<'v>, field: &str) -> Option<Value<'v>> {
    if let Some(i) = v.downcast_ref::<InterfaceValue<'v>>() {
        return i.fields().get(field).map(|x| x.to_value());
    }
    if let Some(i) = v.downcast_ref::<FrozenInterfaceValue>() {
        return i.fields().get(field).map(|x| x.to_value());
    }
    None
}
