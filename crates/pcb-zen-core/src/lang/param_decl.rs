use std::fmt::Display;
use std::path::Path;

use allocative::Allocative;
use starlark::{
    any::ProvidesStaticType,
    eval::{Arguments, Evaluator},
    values::{Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, starlark_value},
};

use crate::lang::{evaluator_ext::EvaluatorExt, io_direction::IoDirection};

use super::module::{
    DeclarationSite, MissingInputError, ParameterMetadataInput, current_declaration_site,
    default_for_type, io_declaration_site, io_generated_default, record_parameter_metadata,
    run_checks, validate_or_convert,
};

#[derive(Debug, Clone, Trace, Allocative)]
struct DeclArgs<'v> {
    typ: Value<'v>,
    checks: Option<Value<'v>>,
    default: Option<Value<'v>>,
    convert: Option<Value<'v>>,
    optional: Option<bool>,
    help: Option<String>,
    direction: Option<IoDirection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Trace, Allocative)]
enum ParamKind {
    Config,
    Io,
}

impl ParamKind {
    fn kind_name(self) -> &'static str {
        match self {
            ParamKind::Config => "config",
            ParamKind::Io => "io",
        }
    }

    fn repr(self) -> &'static str {
        match self {
            ParamKind::Config => "config(...)",
            ParamKind::Io => "io(...)",
        }
    }

    fn allows_convert(self) -> bool {
        matches!(self, ParamKind::Config)
    }

    fn allows_direction(self) -> bool {
        matches!(self, ParamKind::Io)
    }

    fn unresolved_error(self) -> &'static str {
        match self {
            ParamKind::Config => {
                "config() without an explicit name must be assigned to a top-level variable"
            }
            ParamKind::Io => {
                "io() without an explicit name must be assigned to a top-level variable"
            }
        }
    }

    fn declaration_site(self, eval: &Evaluator<'_, '_, '_>) -> DeclarationSite {
        match self {
            ParamKind::Config => current_declaration_site(eval),
            ParamKind::Io => io_declaration_site(eval),
        }
    }

    fn resolve<'v>(
        self,
        variable_name: &str,
        args: &DeclArgs<'v>,
        declaration_site: &DeclarationSite,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        match self {
            ParamKind::Config => resolve_config(variable_name, args, declaration_site, eval),
            ParamKind::Io => resolve_io(variable_name, args, declaration_site, eval),
        }
    }
}

#[derive(Debug, Clone, Trace, ProvidesStaticType, NoSerialize, Allocative)]
#[repr(C)]
struct DeferredParam<'v> {
    kind: ParamKind,
    args: DeclArgs<'v>,
    declaration_site: DeclarationSite,
}

impl<'v> starlark::values::AllocValue<'v> for DeferredParam<'v> {
    fn alloc_value(self, heap: &'v Heap) -> Value<'v> {
        heap.alloc_complex(self)
    }
}

impl<'v> Freeze for DeferredParam<'v> {
    type Frozen = starlark::values::none::NoneType;

    fn freeze(
        self,
        _freezer: &starlark::values::Freezer,
    ) -> starlark::values::FreezeResult<Self::Frozen> {
        Err(starlark::values::FreezeError::new(
            self.kind.unresolved_error().to_owned(),
        ))
    }
}

impl<'v> Display for DeferredParam<'v> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind.repr())
    }
}

#[starlark_value(type = "DeferredParameter")]
impl<'v> StarlarkValue<'v> for DeferredParam<'v>
where
    Self: ProvidesStaticType<'v>,
{
    fn export_as(
        &self,
        variable_name: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<()> {
        let value = self
            .kind
            .resolve(variable_name, &self.args, &self.declaration_site, eval)?;
        eval.set_export_as_replacement(value)?;
        Ok(())
    }

    fn collect_repr(&self, collector: &mut String) {
        collector.push_str(self.kind.repr());
    }
}

fn unpack_bool_arg(value: Value<'_>, function: &str, parameter: &str) -> starlark::Result<bool> {
    value.unpack_bool().ok_or_else(|| {
        starlark::Error::new_other(anyhow::anyhow!("{function}() `{parameter}` must be a bool"))
    })
}

fn unpack_string_arg(
    value: Value<'_>,
    function: &str,
    parameter: &str,
) -> starlark::Result<String> {
    value.unpack_str().map(str::to_owned).ok_or_else(|| {
        starlark::Error::new_other(anyhow::anyhow!(
            "{function}() `{parameter}` must be a string"
        ))
    })
}

fn unpack_optional_string_arg(
    value: Value<'_>,
    function: &str,
    parameter: &str,
) -> starlark::Result<Option<String>> {
    if value.is_none() {
        Ok(None)
    } else {
        unpack_string_arg(value, function, parameter).map(Some)
    }
}

fn none_if_none(value: Value<'_>) -> Option<Value<'_>> {
    (!value.is_none()).then_some(value)
}

fn parse_decl_args<'v>(
    kind: ParamKind,
    args: &Arguments<'v, '_>,
    heap: &'v Heap,
) -> starlark::Result<(Option<String>, DeclArgs<'v>)> {
    let function = kind.kind_name();
    let positional_values: Vec<Value<'v>> = args.positions(heap)?.collect();
    if positional_values.is_empty() || positional_values.len() > 3 {
        return Err(starlark::Error::new_other(anyhow::anyhow!(
            "{function}() accepts `{function}(name, typ, checks?)` or `{function}(typ, checks?)`"
        )));
    }

    let mut default = None;
    let mut checks = None;
    let mut convert = None;
    let mut optional = None;
    let mut help = None;
    let mut direction = None;

    for (arg_name, value) in args.names_map()? {
        match arg_name.as_str() {
            "checks" => checks = none_if_none(value),
            "default" => default = Some(value),
            "convert" if kind.allows_convert() => convert = none_if_none(value),
            "optional" => optional = Some(unpack_bool_arg(value, function, "optional")?),
            "help" => help = unpack_optional_string_arg(value, function, "help")?,
            "direction" if kind.allows_direction() => {
                direction = IoDirection::parse_optional(
                    unpack_optional_string_arg(value, function, "direction")?.as_deref(),
                )?
            }
            other => {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "{function}() got unexpected named argument `{other}`"
                )));
            }
        }
    }

    let (name, typ, positional_checks) = match positional_values.as_slice() {
        [name_or_type] => {
            if name_or_type.unpack_str().is_some() {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "{function}(name, ...) requires a type as the second positional argument"
                )));
            }
            (None, *name_or_type, None)
        }
        [name_or_type, second] => {
            if let Some(name) = name_or_type.unpack_str() {
                (Some(name.to_owned()), *second, None)
            } else {
                (None, *name_or_type, Some(*second))
            }
        }
        [name_or_type, typ, checks] => {
            let Some(name) = name_or_type.unpack_str() else {
                return Err(starlark::Error::new_other(anyhow::anyhow!(
                    "{function}(typ, checks) accepts at most two positional arguments"
                )));
            };
            (Some(name.to_owned()), *typ, Some(*checks))
        }
        _ => unreachable!(),
    };
    let positional_checks = positional_checks.and_then(none_if_none);

    if checks.is_some() && positional_checks.is_some() {
        return Err(starlark::Error::new_other(anyhow::anyhow!(
            "{function}() got `checks` both positionally and by name"
        )));
    }

    Ok((
        name,
        DeclArgs {
            typ,
            checks: checks.or(positional_checks),
            default,
            convert,
            optional,
            help,
            direction,
        },
    ))
}

fn warn_deprecated_config_convert(
    declaration_site: &DeclarationSite,
    eval: &mut Evaluator<'_, '_, '_>,
) {
    let msg = "config() parameter `convert` is deprecated and will be removed in a future release"
        .to_string();
    let mut diag =
        starlark::errors::EvalMessage::from_any_error(Path::new(&declaration_site.path), &msg);
    diag.span = declaration_site.span;
    diag.severity = starlark::errors::EvalSeverity::Warning;
    eval.add_diagnostic(diag);
}

fn note_missing_input(name: &str, eval: &mut Evaluator<'_, '_, '_>) {
    if let Some(ctx) = eval.context_value() {
        ctx.add_missing_input(name.to_owned());
    }
}

fn missing_input_diag(
    name: &str,
    declaration_site: &DeclarationSite,
) -> starlark::errors::EvalMessage {
    let mut diag = starlark::errors::EvalMessage::from_any_error(
        Path::new(&declaration_site.path),
        &MissingInputError {
            name: name.to_owned(),
        }
        .to_string(),
    );
    diag.span = declaration_site.span;
    diag
}

fn strict_io_config(eval: &mut Evaluator<'_, '_, '_>) -> bool {
    eval.context_value()
        .map(|ctx| ctx.strict_io_config())
        .unwrap_or(false)
}

fn finish_resolution<'v>(
    name: &str,
    args: &DeclArgs<'v>,
    metadata: ParameterMetadataInput<'v>,
    declaration_site: &DeclarationSite,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    let actual_value = metadata.actual_value;
    run_checks(eval, args.checks, actual_value)?;
    record_parameter_metadata(name, metadata, declaration_site, eval);
    Ok(actual_value)
}

fn resolve_config<'v>(
    name: &str,
    args: &DeclArgs<'v>,
    declaration_site: &DeclarationSite,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    let is_optional = args.optional.unwrap_or(args.default.is_some());

    let value = if let Some(provided) = eval.request_input(name)? {
        validate_or_convert(name, provided, args.typ, args.convert, eval)?
    } else if is_optional {
        if let Some(default) = args.default {
            validate_or_convert(name, default, args.typ, args.convert, eval)?
        } else {
            Value::new_none()
        }
    } else {
        if strict_io_config(eval) {
            note_missing_input(name, eval);
            eval.add_diagnostic(missing_input_diag(name, declaration_site));
        }

        if let Some(default) = args.default {
            validate_or_convert(name, default, args.typ, args.convert, eval)?
        } else {
            let generated = default_for_type(eval, args.typ)?;
            validate_or_convert(name, generated, args.typ, args.convert, eval)?
        }
    };

    finish_resolution(
        name,
        args,
        ParameterMetadataInput {
            typ: args.typ,
            optional: is_optional,
            default: args.default,
            is_config: true,
            help: args.help.clone(),
            direction: None,
            actual_value: value,
        },
        declaration_site,
        eval,
    )
}

fn resolve_io<'v>(
    name: &str,
    args: &DeclArgs<'v>,
    declaration_site: &DeclarationSite,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    let type_name = args.typ.get_type();
    if !matches!(type_name, "NetType" | "InterfaceFactory") {
        return Err(anyhow::anyhow!(
            "builtin.io() requires a Net or interface type, got {type_name}."
        )
        .into());
    }

    let is_optional = args.optional.unwrap_or(false);
    let compute_default = |eval: &mut Evaluator<'v, '_, '_>, for_metadata_only: bool| {
        if let Some(default) = args.default {
            validate_or_convert(name, default, args.typ, None, eval).map_err(starlark::Error::from)
        } else {
            io_generated_default(eval, args.typ, name, for_metadata_only)
        }
    };

    let (value, metadata_default) = if let Some(provided) = eval.request_input(name)? {
        (
            validate_or_convert(name, provided, args.typ, None, eval)?,
            Some(compute_default(eval, true)?),
        )
    } else if is_optional {
        let default = compute_default(eval, false)?;
        if matches!(type_name, "NetType" | "InterfaceFactory") {
            (default, Some(default))
        } else {
            (Value::new_none(), Some(default))
        }
    } else if strict_io_config(eval) {
        note_missing_input(name, eval);
        return Err(MissingInputError {
            name: name.to_owned(),
        }
        .into());
    } else {
        let default = compute_default(eval, false)?;
        (default, Some(default))
    };

    finish_resolution(
        name,
        args,
        ParameterMetadataInput {
            typ: args.typ,
            optional: is_optional,
            default: metadata_default,
            is_config: false,
            help: args.help.clone(),
            direction: args.direction,
            actual_value: value,
        },
        declaration_site,
        eval,
    )
}

fn invoke_decl<'v>(
    kind: ParamKind,
    args: DeclArgs<'v>,
    declaration_site: DeclarationSite,
    eval: &mut Evaluator<'v, '_, '_>,
    explicit_name: Option<String>,
) -> starlark::Result<Value<'v>> {
    if let Some(name) = explicit_name {
        return kind.resolve(&name, &args, &declaration_site, eval);
    }

    Ok(eval.heap().alloc(DeferredParam {
        kind,
        args,
        declaration_site,
    }))
}

pub(crate) fn invoke_config<'v>(
    args: &Arguments<'v, '_>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    let kind = ParamKind::Config;
    let declaration_site = kind.declaration_site(eval);
    let (name, args) = parse_decl_args(kind, args, eval.heap())?;
    if args.convert.is_some() {
        warn_deprecated_config_convert(&declaration_site, eval);
    }
    invoke_decl(kind, args, declaration_site, eval, name)
}

pub(crate) fn invoke_builtin_io<'v>(
    args: &Arguments<'v, '_>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    let kind = ParamKind::Io;
    let declaration_site = kind.declaration_site(eval);
    let (name, args) = parse_decl_args(kind, args, eval.heap())?;
    invoke_decl(kind, args, declaration_site, eval, name)
}
