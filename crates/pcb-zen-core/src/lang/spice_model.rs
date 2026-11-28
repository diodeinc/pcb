#![allow(clippy::needless_lifetimes)]

use regex::Regex;
use std::collections::HashSet;
use std::path::Path;

use allocative::Allocative;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::GlobalsBuilder,
    eval::Evaluator,
    starlark_complex_value,
    values::{
        Freeze, NoSerialize, StarlarkValue, Trace, Value, dict::StarlarkDict, list::StarlarkList,
        starlark_value,
    },
};

use crate::lang::evaluator_ext::EvaluatorExt;

use anyhow::anyhow;
use pcb_sch::physical::PhysicalValue;

/// SpiceModel reprents a sub circuit
#[derive(Clone, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct SpiceModelValue<'v> {
    pub definition: String,   // file content with the definition
    pub name: String,         // spice subckt name
    pub nets: Vec<Value<'v>>, // input nets
    pub args: SmallMap<String, String>,
}

impl<'v> std::fmt::Debug for SpiceModelValue<'v> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = f.debug_struct("SpiceModel");
        debug.field("definition", &self.definition);
        debug.field("name", &self.name);
        debug.field("nets", &self.nets);
        debug.field("args", &self.args);
        debug.finish()
    }
}

starlark_complex_value!(pub SpiceModelValue);

#[starlark_value(type = "SpiceModel")]
impl<'v> StarlarkValue<'v> for SpiceModelValue<'v>
where
    Self: ProvidesStaticType<'v>,
{
    #[allow(non_snake_case)]
    #[starlark(constructor)]
    fn SpiceModel(
        #[starlark(require = pos)] definition: Value<'v>,
        #[starlark(require = pos)] name: Value<'v>,
        nets: Value<'v>,
        args: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<SpiceModelValue<'v>> {
        let path = definition
            .unpack_str()
            .ok_or_else(|| starlark::Error::new_other(anyhow!("`path` must be a string")))?
            .to_owned();

        let name = name
            .unpack_str()
            .ok_or_else(|| starlark::Error::new_other(anyhow!("`name` must be a string")))?
            .to_owned();

        let inputs_list = StarlarkList::from_value(nets)
            .ok_or_else(|| starlark::Error::new_other(anyhow!("`nets` must be a list of Net")))?;

        let params_dict = StarlarkDict::from_value(args)
            .ok_or_else(|| starlark::Error::new_other(anyhow!("`args` must be a map")))?;

        let mut parsed_args: SmallMap<String, String> = SmallMap::new();
        for (k_val, v_val) in params_dict.entries() {
            let param_name = k_val
                .unpack_str()
                .ok_or_else(|| {
                    starlark::Error::new_other(anyhow!("parameter names must be strings"))
                })?
                .to_owned();
            let v_str = if let Some(s) = v_val.unpack_str() {
                s.to_owned()
            } else if let Some(pv) = v_val.downcast_ref::<PhysicalValue>() {
                pv.to_spice_string()
            } else {
                return Err(starlark::Error::new_other(anyhow!(
                    "parameter values must be a string or PhysicalValue"
                )));
            };
            parsed_args.insert(param_name, v_str);
        }

        let mut parsed_nets: Vec<Value<'v>> = Vec::new();
        for net_val in inputs_list.values() {
            if net_val.get_type() != "Net" {
                return Err(starlark::Error::new_other(anyhow!(format!(
                    "Expected Net, got {}",
                    net_val.get_type()
                ))));
            }
            parsed_nets.push(net_val);
        }

        let eval_ctx = eval.eval_context().unwrap();
        let current_file = eval_ctx
            .source_path()
            .ok_or_else(|| starlark::Error::new_other(anyhow!("No source path available")))?;
        let (contents, circuit) =
            resolve_spice_subcircuit(eval_ctx, Path::new(&current_file), &path, &name)?;
        validate_spice_model(&circuit, parsed_nets.len(), &parsed_args)?;

        Ok(SpiceModelValue {
            definition: contents,
            name,
            nets: parsed_nets,
            args: parsed_args,
        })
    }
}

impl<'v> std::fmt::Display for SpiceModelValue<'v> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "SpiceModel({}, nets={:?}, args={:?})",
            self.name, self.nets, self.args
        )?;
        Ok(())
    }
}

impl<'v> SpiceModelValue<'v> {
    pub fn nets(&self) -> &Vec<Value<'v>> {
        &self.nets
    }

    pub fn args(&self) -> &SmallMap<String, String> {
        &self.args
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn definition(&self) -> &str {
        &self.definition
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SubCircuit {
    pub(crate) nets: Vec<String>,
    pub(crate) params: HashSet<String>,
}

/// A spice subcircuit cached on the [`EvalCaches`] by (resolved model path,
/// subcircuit name): the model file's contents plus the parsed subcircuit.
///
/// [`EvalCaches`]: crate::lang::eval::EvalCaches
#[derive(Clone)]
pub(crate) struct CachedSpiceModel {
    pub(crate) definition: String,
    pub(crate) circuit: SubCircuit,
}

pub(crate) fn resolve_spice_subcircuit(
    eval_ctx: &crate::EvalContext,
    resolve_from: &Path,
    path: &str,
    name: &str,
) -> starlark::Result<(String, SubCircuit)> {
    let resolved_path = eval_ctx
        .get_config()
        .resolve_path(path, resolve_from)
        .map_err(|e| {
            starlark::Error::new_other(anyhow!("Failed to resolve spice model path: {}", e))
        })?;

    // Reading and parsing the subcircuit is pure in (resolved_path, name), so
    // the result is reusable across evaluations.
    let cache_key = (resolved_path, name.to_string());
    let cached = match eval_ctx.caches().spice_cache.get(&cache_key) {
        Some(cached) => cached,
        None => {
            let definition = eval_ctx
                .file_provider()
                .read_file(&cache_key.0)
                .map_err(|e| {
                    starlark::Error::new_other(anyhow!(
                        "Failed to read spice model file '{}': {}",
                        cache_key.0.display(),
                        e
                    ))
                })?;
            let circuit = get_sub_circuit(&definition, name)?;
            let loaded = CachedSpiceModel {
                definition,
                circuit,
            };
            eval_ctx
                .caches()
                .spice_cache
                .insert(cache_key, loaded.clone());
            loaded
        }
    };
    Ok((cached.definition, cached.circuit))
}

pub(crate) fn validate_spice_model(
    circuit: &SubCircuit,
    nets_len: usize,
    args: &SmallMap<String, String>,
) -> starlark::Result<()> {
    if nets_len != circuit.nets.len() {
        return Err(starlark::Error::new_other(anyhow!(
            "Expected {} nets, {} provided",
            circuit.nets.len(),
            nets_len
        )));
    }

    let missing: Vec<String> = circuit
        .params
        .iter()
        .filter_map(|param| {
            if !args.contains_key(param) {
                Some(param.clone())
            } else {
                None
            }
        })
        .collect();
    if !missing.is_empty() {
        return Err(starlark::Error::new_other(anyhow!(
            "Required argument(s) {:?} not provided",
            missing
        )));
    }

    let unexpected: Vec<String> = args
        .iter()
        .filter_map(|(param, _)| {
            if !circuit.params.contains(param) {
                Some(param.clone())
            } else {
                None
            }
        })
        .collect();
    if !unexpected.is_empty() {
        return Err(starlark::Error::new_other(anyhow!(
            "Unexpected argument(s) {:?} ",
            unexpected
        )));
    }

    Ok(())
}

fn parse_params(s: &str, circuit: &mut SubCircuit) -> anyhow::Result<()> {
    let params = s.split_whitespace();
    for p in params {
        let mut split = p.splitn(2, '=');
        let param_name = split.next().unwrap_or("");
        if param_name.is_empty() {
            return Err(anyhow!("Invalid PARAMS syntax: '{}'", p));
        }
        circuit.params.insert(param_name.to_string());
    }
    Ok(())
}

fn get_sub_circuit(s: &str, name: &str) -> anyhow::Result<SubCircuit> {
    let mut circuit = SubCircuit {
        nets: Vec::new(),
        params: HashSet::new(),
    };

    let decl_pattern = format!(
        r"(?i)^\.subckt\s+{}\b\s*((?:\S+\s*)*?)\s*(?:params:\s*(.*))?$",
        regex::escape(name)
    );
    let decl_re = Regex::new(&decl_pattern).unwrap();
    let params_re = Regex::new(r"(?i)^\s*\+\s*params:\s*(.*)$").unwrap();

    // Scan for the declaration
    let mut lines = s.lines().peekable();
    let mut found = false;
    for line in lines.by_ref() {
        if let Some(caps) = decl_re.captures(line) {
            circuit.nets = caps
                .get(1)
                .map(|m| m.as_str())
                .unwrap_or("")
                .split_whitespace()
                .map(|x| x.to_string())
                .collect();
            parse_params(caps.get(2).map(|m| m.as_str()).unwrap_or(""), &mut circuit)?;
            found = true;
            break;
        }
    }

    if !found {
        return Err(anyhow!(format!("cannot find subckt named {}", name)));
    }

    // Scan for out-of-line definition
    for line in lines {
        if let Some(caps) = params_re.captures(line) {
            parse_params(caps.get(1).map(|m| m.as_str()).unwrap_or(""), &mut circuit)?;
        } else {
            break;
        }
    }

    Ok(circuit)
}

pub(crate) fn model_globals(builder: &mut GlobalsBuilder) {
    builder.type_symbol::<SpiceModelValue>();
}
