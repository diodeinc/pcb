//! Peripheral capability model: pin functions, instance allocation, swap classes.
//!
//! Declares what a multi-function component's pins *can do* and solves the
//! assignment at elaboration:
//!
//! - `pin(name, af=, cost=, input_only=, strap=)` — one candidate physical pin.
//! - `peripheral(name, provides=[Iface...], signals={...}, rebind=, attrs=, unless=)`
//!   — a resource cluster (an MCU USART, one comparator of a dual package...).
//! - `pool(name, provides=[Iface], pins=[...])` — sugar for N single-signal
//!   interchangeable units (GPIO pools).
//! - `pin_request(name, Iface, uses=, instance=, prefer=, lock=, where=, direction=)`
//!   — a demand for a capability; the interface type *is* the request.
//! - `pin_solve(peripherals, requests, config=, previous=)` — deterministic
//!   joint instance x pin matching. Exclusivity at both tiers (an instance
//!   serves at most one request; a pin carries at most one function) is
//!   structural: violations are infeasible, not lint.
//!
//! Capability matching is nominal over `interface()` identities, closed over
//! `interface(implies=[...])`: a peripheral providing `Usart` satisfies a
//! `Uart` request when `Usart` implies `Uart` — never the reverse.
//!
//! The solved assignment and the residual routing freedom are stored as the
//! module properties `pin_assignment` and `swap_classes` (JSON), which flow
//! through the netlist into schematic/board fields.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::str::FromStr;

use pcb_sch::physical::PhysicalValue;
use starlark::collections::SmallMap;
use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::dict::{AllocDict, DictRef};
use starlark::values::list::{ListRef, UnpackList};
use starlark::values::none::NoneOr;
use starlark::values::typing::TypeInstanceId;
use starlark::values::{Heap, Value, ValueLike};

use crate::lang::evaluator_ext::EvaluatorExt;
use crate::lang::interface::{FrozenInterfaceFactory, InterfaceFactory};

const REBIND_VALUES: [&str; 3] = ["none", "firmware", "fixed"];
const PINMAP_CAP: usize = 512;
const SOLVER_BUDGET: usize = 200_000;

// ---------------------------------------------------------------------------
// Interface introspection (closure over `implies`)
// ---------------------------------------------------------------------------

struct IfaceInfo<'v> {
    id: TypeInstanceId,
    name: String,
    fields: Vec<String>,
    implies: Vec<Value<'v>>,
}

fn iface_info<'v>(v: Value<'v>) -> Option<IfaceInfo<'v>> {
    if let Some(f) = v.downcast_ref::<InterfaceFactory<'v>>() {
        return Some(IfaceInfo {
            id: f.type_instance_id(),
            name: f.type_name().unwrap_or_else(|| v.to_string()),
            fields: f.fields().iter().map(|(k, _)| k.clone()).collect(),
            implies: f.implies().iter().map(|x| x.to_value()).collect(),
        });
    }
    if let Some(f) = v.downcast_ref::<FrozenInterfaceFactory>() {
        return Some(IfaceInfo {
            id: f.type_instance_id(),
            name: f.type_name().unwrap_or_else(|| v.to_string()),
            fields: f.fields().iter().map(|(k, _)| k.clone()).collect(),
            implies: f.implies().iter().map(|x| x.to_value()).collect(),
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

// ---------------------------------------------------------------------------
// Dict helpers (the builtins exchange plain Starlark dicts)
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Internal solver model
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct RPin {
    name: String,
    af: Option<i32>,
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

// ---------------------------------------------------------------------------
// Parsing of peripheral/request dicts into the solver model
// ---------------------------------------------------------------------------

fn parse_rpin<'v>(v: Value<'v>, heap: Heap<'v>, ctx: &str) -> anyhow::Result<RPin> {
    if let Some(s) = v.unpack_str() {
        return Ok(RPin {
            name: s.to_owned(),
            af: None,
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
    Ok(RPin {
        name: dict_get_str(&d, heap, "name")
            .ok_or_else(|| anyhow::anyhow!("{ctx}: pin() missing name"))?,
        af: dict_get(&d, heap, "af").and_then(|v| v.unpack_i32()),
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

// ---------------------------------------------------------------------------
// Candidate generation
// ---------------------------------------------------------------------------

fn config_truthy<'v>(config: &SmallMap<String, Value<'v>>, key: &str) -> bool {
    config.get(key).map(|v| v.to_bool()).unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
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
        for (s, cands) in &cand_lists {
            let mut next = Vec::new();
            for base in &pinmaps {
                for c in cands {
                    if base.iter().any(|(_, b)| b.name == c.name) {
                        continue;
                    }
                    let mut d = base.clone();
                    d.push(((*s).to_owned(), (*c).clone()));
                    next.push(d);
                    if next.len() >= PINMAP_CAP {
                        break;
                    }
                }
            }
            pinmaps = next;
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

// ---------------------------------------------------------------------------
// Backtracking assignment (deterministic, bounded)
// ---------------------------------------------------------------------------

fn assign<'v>(reqs: &[RReq<'v>], all_combos: &[Vec<Combo>]) -> Option<Vec<usize>> {
    // Evaluation order: locked first, then fewest candidates, then declaration order.
    let mut order: Vec<usize> = (0..reqs.len()).collect();
    order.sort_by_key(|&i| (!reqs[i].lock, all_combos[i].len(), i));

    let mut choice = vec![0usize; reqs.len()];
    // chosen combo index per request (by original request index)
    let mut assigned: Vec<Option<usize>> = vec![None; reqs.len()];
    let mut used_inst: HashSet<usize> = HashSet::new();
    let mut used_pin: HashSet<String> = HashSet::new();
    let mut pos: usize = 0;

    for _ in 0..SOLVER_BUDGET {
        if pos == order.len() {
            return Some(assigned.iter().map(|c| c.unwrap()).collect());
        }
        let ri = order[pos];
        let combos = &all_combos[ri];
        let mut placed = false;
        let mut ci = choice[pos];
        while ci < combos.len() {
            let c = &combos[ci];
            let clash = used_inst.contains(&c.periph_idx)
                || c.pins.iter().any(|(_, p)| used_pin.contains(&p.name));
            if !clash {
                choice[pos] = ci;
                assigned[ri] = Some(ci);
                used_inst.insert(c.periph_idx);
                for (_, p) in &c.pins {
                    used_pin.insert(p.name.clone());
                }
                placed = true;
                break;
            }
            ci += 1;
        }
        if placed {
            pos += 1;
            if pos < order.len() {
                choice[pos] = 0;
            }
        } else {
            choice[pos] = 0;
            if pos == 0 {
                return None;
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
    None
}

// ---------------------------------------------------------------------------
// JSON -> Starlark conversion for the result value
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Builtins
// ---------------------------------------------------------------------------

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

    // Parse string attrs into physical values at declaration (same parser as
    // everywhere else); non-physical strings stay as strings.
    let mut attr_pairs: Vec<(Value, Value)> = Vec::new();
    for (k, v) in attrs.iter() {
        let parsed = match v.unpack_str() {
            Some(s) => match PhysicalValue::from_str(s) {
                Ok(pv) => heap.alloc(pv),
                Err(_) => *v,
            },
            None => *v,
        };
        attr_pairs.push((heap.alloc(k.as_str()), parsed));
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
    af: Option<i32>,
    cost: i32,
    input_only: bool,
    strap: bool,
) -> Value<'v> {
    let pairs: Vec<(Value, Value)> = vec![
        (heap.alloc("kind"), heap.alloc("pin")),
        (heap.alloc("name"), heap.alloc(name)),
        (
            heap.alloc("af"),
            af.map(|a| heap.alloc(a)).unwrap_or_else(Value::new_none),
        ),
        (heap.alloc("cost"), heap.alloc(cost)),
        (heap.alloc("input_only"), Value::new_bool(input_only)),
        (heap.alloc("strap"), Value::new_bool(strap)),
    ];
    heap.alloc(AllocDict(pairs))
}

#[starlark_module]
pub(crate) fn pinmux_globals(builder: &mut GlobalsBuilder) {
    /// One candidate physical pin for a peripheral signal.
    fn pin<'v>(
        #[starlark(require = pos)] name: String,
        #[starlark(require = named, default = NoneOr::None)] af: NoneOr<i32>,
        #[starlark(require = named, default = 0)] cost: i32,
        #[starlark(require = named, default = false)] input_only: bool,
        #[starlark(require = named, default = false)] strap: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        Ok(alloc_pin_dict(
            eval.heap(),
            &name,
            af.into_option(),
            cost,
            input_only,
            strap,
        ))
    }

    /// A resource cluster: signals -> candidate pins, provided interfaces,
    /// rebind cost, attributes, optional config-conditional availability.
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
    /// signals (`uses=`) consume pins.
    fn pin_request<'v>(
        #[starlark(require = pos)] name: String,
        #[starlark(require = pos)] iface: Value<'v>,
        #[starlark(require = named, default = NoneOr::None)] uses: NoneOr<UnpackList<String>>,
        #[starlark(require = named, default = NoneOr::None)] instance: NoneOr<String>,
        #[starlark(require = named, default = UnpackList::default())] prefer: UnpackList<String>,
        #[starlark(require = named, default = false)] lock: bool,
        #[starlark(require = named, default = NoneOr::None)] r#where: NoneOr<Value<'v>>,
        #[starlark(require = named, default = NoneOr::None)] direction: NoneOr<String>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let heap = eval.heap();
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
        let prefer_alloc: Vec<Value> = prefer
            .items
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
                if let Some(af) = p.af {
                    entry.insert("af".into(), serde_json::Value::Number(af.into()));
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

        let pairs: Vec<(Value, Value)> = vec![
            (
                heap.alloc("assignment"),
                json_to_value(heap, &assignment_json),
            ),
            (heap.alloc("swap_classes"), json_to_value(heap, &swap_json)),
        ];
        Ok(heap.alloc(AllocDict(pairs)))
    }
}
