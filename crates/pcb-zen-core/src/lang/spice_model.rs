#![allow(clippy::needless_lifetimes)]

use regex::Regex;
use std::collections::HashSet;

use allocative::Allocative;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    environment::GlobalsBuilder,
    eval::{Arguments, Evaluator, ParametersSpec, ParametersSpecParam},
    starlark_complex_value, starlark_module, starlark_simple_value,
    values::{
        dict::DictRef, list::ListRef, starlark_value, Coerce, Freeze, FreezeResult, NoSerialize,
        StarlarkValue, Trace, Value, ValueLike,
    },
};

use crate::lang::evaluator_ext::EvaluatorExt;

use anyhow::anyhow;

/// SpiceModel reprents a sub circuit
#[derive(Clone, Trace, Coerce, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct SpiceModelValueGen<V> {
    pub definition: String, // file content with the definition
    pub name: String,       // spice subckt name
    pub nets: Vec<V>,       // input nets
    pub args: SmallMap<String, String>,
}

impl<V: std::fmt::Debug> std::fmt::Debug for SpiceModelValueGen<V> {
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
impl<'v, V: ValueLike<'v>> StarlarkValue<'v> for SpiceModelValueGen<V> where
    Self: ProvidesStaticType<'v>
{
}

impl<'v, V: ValueLike<'v>> std::fmt::Display for SpiceModelValueGen<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "SpiceModel({}, nets={:?}, args={:?})",
            self.name, self.nets, self.args
        )?;
        Ok(())
    }
}

impl<'v, V: ValueLike<'v>> SpiceModelValueGen<V> {
    pub fn nets(&self) -> &Vec<V> {
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

/// SpiceModelFactory is a value that represents a factory for a SpiceModel.
#[derive(Debug, Trace, ProvidesStaticType, NoSerialize, Allocative, Freeze)]
#[repr(C)]
pub struct SpiceModelType;

starlark_simple_value!(SpiceModelType);

#[starlark_value(type = "SpiceModel")]
impl<'v> StarlarkValue<'v> for SpiceModelType
where
    Self: ProvidesStaticType<'v>,
{
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let param_spec = ParametersSpec::new_parts(
            "SpiceModel",
            [
                ("definition", ParametersSpecParam::<Value<'_>>::Required),
                ("name", ParametersSpecParam::<Value<'_>>::Required),
            ],
            // Named parameters
            [
                ("nets", ParametersSpecParam::<Value<'_>>::Required),
                ("args", ParametersSpecParam::<Value<'_>>::Required),
            ],
            false,
            std::iter::empty::<(&str, ParametersSpecParam<_>)>(),
            false,
        );

        let (path, name, nets, args) =
            param_spec.parser(args, eval, |param_parser, _eval_ctx| {
                let path_val: Value = param_parser.next()?;
                let path = path_val
                    .unpack_str()
                    .ok_or_else(|| starlark::Error::new_other(anyhow!("`path` must be a string")))?
                    .to_owned();

                let name_val: Value = param_parser.next()?;
                let name = name_val
                    .unpack_str()
                    .ok_or_else(|| starlark::Error::new_other(anyhow!("`path` must be a string")))?
                    .to_owned();

                let inputs_val: Value = param_parser.next()?;
                let inputs_list = ListRef::from_value(inputs_val).ok_or_else(|| {
                    starlark::Error::new_other(anyhow!("`nets` must be a list of Net"))
                })?;

                let params_val: Value = param_parser.next()?;
                let params_dict = DictRef::from_value(params_val)
                    .ok_or_else(|| starlark::Error::new_other(anyhow!("`args` must be a map")))?;

                // Parse the parameters
                let mut args: SmallMap<String, String> = SmallMap::new();
                for (k_val, v_val) in params_dict.iter() {
                    let param_name = k_val
                        .unpack_str()
                        .ok_or_else(|| {
                            starlark::Error::new_other(anyhow!("parameter names must be strings"))
                        })?
                        .to_owned();
                    let v_str = v_val
                        .unpack_str()
                        .ok_or_else(|| {
                            starlark::Error::new_other(anyhow!("parameter values must be strings"))
                        })?
                        .to_owned();
                    args.insert(param_name, v_str);
                }

                // Figure out the input nets
                let mut nets: Vec<Value<'v>> = Vec::new();

                for net_val in inputs_list.iter() {
                    if net_val.get_type() != "Net" {
                        return Err(starlark::Error::new_other(anyhow!(format!(
                            "Expected Net, got {}",
                            net_val.get_type()
                        ))));
                    }
                    nets.push(net_val);
                }

                Ok((path, name, nets, args))
            })?;

        let eval_ctx = eval.eval_context().unwrap();

        let load_resolver = eval_ctx
            .get_load_resolver()
            .ok_or_else(|| starlark::Error::new_other(anyhow!("No load resolver available")))?;

        let current_file = eval_ctx
            .source_path
            .as_ref()
            .ok_or_else(|| starlark::Error::new_other(anyhow!("No source path available")))?;

        let resolved_path = load_resolver
            .resolve_path(&path, std::path::Path::new(&current_file))
            .map_err(|e| {
                starlark::Error::new_other(anyhow!("Failed to resolve spice model path: {}", e))
            })?;

        let file_provider = eval_ctx
            .file_provider
            .as_ref()
            .ok_or_else(|| starlark::Error::new_other(anyhow!("No file provider available")))?;

        let contents = file_provider.read_file(&resolved_path).map_err(|e| {
            starlark::Error::new_other(anyhow!(
                "Failed to read symbol library '{}': {}",
                resolved_path.display(),
                e
            ))
        })?;

        let circuit = get_sub_circuit(&contents, &name)?;

        // Check missing nets
        if nets.len() != circuit.nets.len() {
            return Err(starlark::Error::new_other(anyhow!(
                "Expected {} nets, {} provided",
                circuit.nets.len(),
                nets.len()
            )));
        }

        // Check missing arguments
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

        // Check unexpected arguments
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

        Ok(eval.heap().alloc_complex(SpiceModelValue {
            definition: contents,
            name,
            nets,
            args,
        }))
    }

    fn eval_type(&self) -> Option<starlark::typing::Ty> {
        Some(<SpiceModelType as StarlarkValue>::get_type_starlark_repr())
    }
}

#[derive(Debug)]
struct SubCircuit {
    nets: Vec<String>,
    params: HashSet<String>,
}

fn parse_params(s: &str, circuit: &mut SubCircuit) {
    let params = s.split_whitespace();
    for p in params {
        let mut split = p.splitn(2, '=');
        let param_name = split.next().unwrap_or("");
        assert!(!param_name.is_empty());
        circuit.params.insert(param_name.to_string());
    }
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
            parse_params(caps.get(2).map(|m| m.as_str()).unwrap_or(""), &mut circuit);
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
            parse_params(caps.get(1).map(|m| m.as_str()).unwrap_or(""), &mut circuit);
        } else {
            break;
        }
    }

    Ok(circuit)
}

impl std::fmt::Display for SpiceModelType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<SpiceModel>")
    }
}

#[starlark_module]
pub(crate) fn model_globals(builder: &mut GlobalsBuilder) {
    const SpiceModel: SpiceModelType = SpiceModelType;
}
