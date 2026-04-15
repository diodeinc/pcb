use std::fmt::Display;
use std::path::Path;

use allocative::Allocative;
use pcb_sch::physical::PhysicalValue;
use starlark::{
    any::ProvidesStaticType,
    collections::SmallMap,
    eval::{Arguments, Evaluator},
    values::{Freeze, Heap, NoSerialize, StarlarkValue, Trace, Value, ValueLike, starlark_value},
};

use crate::lang::{evaluator_ext::EvaluatorExt, io_direction::IoDirection};

use super::context::ContextValue;
use super::interface::{
    FrozenInterfaceValue, InstancePrefix, InterfaceValue, instantiate_interface,
};
use super::module::{
    DeclarationSite, MissingInputError, ParameterMetadataInput, current_declaration_site,
    default_for_type, io_declaration_site, io_generated_default, normalize_allowed_values,
    normalize_config_default, record_parameter_metadata, run_checks, validate_allowed_config_value,
    validate_or_convert,
};
use super::net::{FrozenNetValue, NetType, NetValue};

#[derive(Debug, Clone, Trace, Allocative)]
struct DeclArgs<'v> {
    typ: Value<'v>,
    checks: Option<Value<'v>>,
    default: Option<Value<'v>>,
    allowed: Option<Value<'v>>,
    convert: Option<Value<'v>>,
    optional: Option<bool>,
    help: Option<String>,
    direction: Option<IoDirection>,
}

#[derive(Debug, Clone)]
enum ImplicitCheck<'v> {
    VoltageWithin {
        template_voltage: Value<'v>,
        template_display: String,
    },
}

#[derive(Debug, Clone)]
struct NormalizedIoArgs<'v> {
    typ: Value<'v>,
    template: Option<IoTemplateValue<'v>>,
    implicit_checks: Vec<ImplicitCheck<'v>>,
}

#[derive(Debug, Clone, Copy)]
enum IoTemplateValue<'v> {
    Net(Value<'v>),
    Interface(Value<'v>),
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

    fn allows_allowed(self) -> bool {
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
    let mut allowed = None;
    let mut convert = None;
    let mut optional = None;
    let mut help = None;
    let mut direction = None;

    for (arg_name, value) in args.names_map()? {
        match arg_name.as_str() {
            "checks" => checks = none_if_none(value),
            "default" => default = none_if_none(value),
            "allowed" if kind.allows_allowed() => allowed = none_if_none(value),
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
            allowed,
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
    let convert_value = |eval: &mut Evaluator<'v, '_, '_>, value| {
        validate_or_convert(name, value, args.typ, args.convert, eval)
            .map_err(starlark::Error::from)
    };
    let allowed_values = normalize_allowed_values(name, args.typ, args.allowed, eval)
        .map_err(starlark::Error::from)?;
    let default_value = normalize_config_default(
        name,
        args.default,
        args.typ,
        allowed_values.as_deref(),
        args.convert,
        eval,
    )
    .map_err(starlark::Error::from)?;
    let is_optional = args.optional.unwrap_or(default_value.is_some());

    let value = if let Some(provided) = eval.request_input(name)? {
        convert_value(eval, provided)?
    } else if is_optional {
        default_value.unwrap_or_else(Value::new_none)
    } else {
        if strict_io_config(eval) {
            note_missing_input(name, eval);
            eval.add_diagnostic(missing_input_diag(name, declaration_site));
        }

        if let Some(default) = default_value {
            default
        } else if let Some(first_allowed) = allowed_values
            .as_ref()
            .and_then(|values| values.first())
            .copied()
        {
            first_allowed
        } else {
            let generated = default_for_type(eval, args.typ)?;
            convert_value(eval, generated)?
        }
    };

    if !value.is_none() {
        validate_allowed_config_value(name, value, allowed_values.as_deref())
            .map_err(starlark::Error::from)?;
    }

    finish_resolution(
        name,
        args,
        ParameterMetadataInput {
            typ: args.typ,
            optional: is_optional,
            default: default_value,
            allowed_values,
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
    let normalized = normalize_io_args(name, args, eval)?;
    let type_name = normalized.typ.get_type();
    if !matches!(type_name, "NetType" | "InterfaceFactory") {
        return Err(anyhow::anyhow!(
            "builtin.io() requires a Net or interface type, got {type_name}."
        )
        .into());
    }

    let is_optional = args.optional.unwrap_or(false);
    let stamp_io_declaration_site = |value: Value<'v>, eval: &mut Evaluator<'v, '_, '_>| {
        if let Some(net) = value.downcast_ref::<NetValue<'v>>() {
            net.with_declaration_site(&declaration_site.path, declaration_site.span, eval.heap())
        } else if let Some(net) = value.downcast_ref::<FrozenNetValue>() {
            net.with_declaration_site(&declaration_site.path, declaration_site.span, eval.heap())
        } else {
            value
        }
    };
    let compute_default = |eval: &mut Evaluator<'v, '_, '_>, for_metadata_only: bool| {
        if let Some(template) = normalized.template {
            template
                .instantiate(name, for_metadata_only, eval)
                .map(|value| stamp_io_declaration_site(value, eval))
        } else if let Some(default) = args.default {
            validate_or_convert(name, default, normalized.typ, None, eval)
                .map_err(starlark::Error::from)
        } else {
            io_generated_default(eval, normalized.typ, name, for_metadata_only)
                .map(|value| stamp_io_declaration_site(value, eval))
        }
    };

    let (value, metadata_default) = if let Some(provided) = eval.request_input(name)? {
        let converted = validate_or_convert(name, provided, normalized.typ, None, eval)?;
        run_implicit_checks(name, &normalized.implicit_checks, converted)
            .map_err(starlark::Error::from)?;
        (converted, Some(compute_default(eval, true)?))
    } else if is_optional {
        let default = compute_default(eval, false)?;
        (default, Some(compute_default(eval, true)?))
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
            typ: normalized.typ,
            optional: is_optional,
            default: metadata_default,
            allowed_values: None,
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

impl<'v> IoTemplateValue<'v> {
    fn from_value(value: Value<'v>) -> Option<Self> {
        if value.downcast_ref::<NetValue<'v>>().is_some()
            || value.downcast_ref::<FrozenNetValue>().is_some()
        {
            Some(Self::Net(value))
        } else if value.downcast_ref::<InterfaceValue<'v>>().is_some()
            || value.downcast_ref::<FrozenInterfaceValue>().is_some()
        {
            Some(Self::Interface(value))
        } else {
            None
        }
    }

    fn value(self) -> Value<'v> {
        match self {
            Self::Net(value) | Self::Interface(value) => value,
        }
    }

    fn unregister(self, ctx: &ContextValue) {
        match self {
            Self::Net(value) => unregister_net_template(value, ctx),
            Self::Interface(value) => unregister_interface_template(value, ctx),
        }
    }

    fn infer_type(self, eval: &mut Evaluator<'v, '_, '_>) -> starlark::Result<Value<'v>> {
        match self {
            Self::Net(value) => {
                let net_type = NetType::new(net_template_type_name(value)?, SmallMap::new(), eval)?;
                Ok(eval.heap().alloc(net_type))
            }
            Self::Interface(value) => interface_template_factory(value),
        }
    }

    fn derive_implicit_checks(self) -> Vec<ImplicitCheck<'v>> {
        match self {
            Self::Net(value) => derive_net_implicit_checks(value),
            Self::Interface(_) => Vec::new(),
        }
    }

    fn instantiate(
        self,
        name: &str,
        for_metadata_only: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        let should_register = !for_metadata_only;
        match self {
            Self::Net(value) => instantiate_net_template(name, value, should_register, eval),
            Self::Interface(value) => instantiate_interface(
                value,
                &InstancePrefix::from_root(name),
                should_register,
                eval.heap(),
                eval,
            ),
        }
    }
}

fn unregister_net_template<'v>(value: Value<'v>, ctx: &ContextValue) {
    if let Some(net) = value.downcast_ref::<NetValue<'v>>() {
        ctx.unregister_net(net.id());
    } else if let Some(net) = value.downcast_ref::<FrozenNetValue>() {
        ctx.unregister_net(net.id());
    }
}

fn unregister_interface_template<'v>(value: Value<'v>, ctx: &ContextValue) {
    if let Some(interface) = value.downcast_ref::<InterfaceValue<'v>>() {
        for (_field_name, field_value) in interface.fields().iter() {
            if let Some(template) = IoTemplateValue::from_value(field_value.to_value()) {
                template.unregister(ctx);
            }
        }
    }
    if let Some(interface) = value.downcast_ref::<FrozenInterfaceValue>() {
        for (_field_name, field_value) in interface.fields().iter() {
            if let Some(template) = IoTemplateValue::from_value(field_value.to_value()) {
                template.unregister(ctx);
            }
        }
    }
}

fn interface_template_factory<'v>(template: Value<'v>) -> starlark::Result<Value<'v>> {
    if let Some(interface) = template.downcast_ref::<InterfaceValue<'v>>() {
        return Ok(interface.factory().to_value());
    }
    if let Some(interface) = template.downcast_ref::<FrozenInterfaceValue>() {
        return Ok(interface.factory().to_value());
    }
    Err(anyhow::anyhow!(
        "builtin.io() requires an interface template, got {}.",
        template.get_type()
    )
    .into())
}

fn net_template_type_name<'v>(template: Value<'v>) -> starlark::Result<String> {
    if let Some(net) = template.downcast_ref::<NetValue<'v>>() {
        Ok(net.net_type_name().to_owned())
    } else if let Some(net) = template.downcast_ref::<FrozenNetValue>() {
        Ok(net.net_type_name().to_owned())
    } else {
        Err(anyhow::anyhow!(
            "builtin.io() requires a Net template, got {}.",
            template.get_type()
        )
        .into())
    }
}

fn net_template_name<'v>(template: Value<'v>) -> Option<&'v str> {
    if let Some(net) = template.downcast_ref::<NetValue<'v>>() {
        net.template_name_opt()
    } else if let Some(net) = template.downcast_ref::<FrozenNetValue>() {
        net.template_name_opt()
    } else {
        None
    }
}

fn net_property_value<'v>(value: Value<'v>, property: &str) -> Option<Value<'v>> {
    if let Some(net) = value.downcast_ref::<NetValue<'v>>() {
        net.properties().get(property).map(|v| v.to_value())
    } else if let Some(net) = value.downcast_ref::<FrozenNetValue>() {
        net.properties().get(property).map(|v| v.to_value())
    } else {
        None
    }
}

fn clone_net_template<'v>(template: Value<'v>, heap: &'v Heap) -> starlark::Result<Value<'v>> {
    if let Some(net) = template.downcast_ref::<NetValue<'v>>() {
        Ok(net.with_new_id(heap))
    } else if let Some(net) = template.downcast_ref::<FrozenNetValue>() {
        Ok(net.with_new_id(heap))
    } else {
        Err(anyhow::anyhow!(
            "builtin.io() requires a Net template, got {}.",
            template.get_type()
        )
        .into())
    }
}

fn derive_net_implicit_checks<'v>(template: Value<'v>) -> Vec<ImplicitCheck<'v>> {
    // Future work: all overlapping fields must be compatible.
    net_property_value(template, "voltage")
        .and_then(|value| value.downcast_ref::<PhysicalValue>().map(|_| value))
        .map(|template_voltage| {
            vec![ImplicitCheck::VoltageWithin {
                template_display: template_voltage.to_repr(),
                template_voltage,
            }]
        })
        .unwrap_or_default()
}

fn normalize_io_args<'v>(
    name: &str,
    args: &DeclArgs<'v>,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<NormalizedIoArgs<'v>> {
    let positional_template = IoTemplateValue::from_value(args.typ);
    if positional_template.is_some() && args.default.is_some() {
        return Err(anyhow::anyhow!(
            "io() cannot accept both a template positional argument and `default=`; remove `default=`"
        )
        .into());
    }

    let default_template = args.default.and_then(IoTemplateValue::from_value);
    let template = positional_template.or(default_template);
    if let Some(template) = template
        && let Some(ctx) = eval.context_value()
    {
        template.unregister(ctx);
    }

    let typ = if let Some(template) = positional_template {
        template.infer_type(eval)?
    } else {
        args.typ
    };

    if let Some(template) = default_template {
        validate_or_convert(name, template.value(), typ, None, eval)
            .map_err(starlark::Error::from)?;
    }

    Ok(NormalizedIoArgs {
        typ,
        template,
        implicit_checks: template
            .map(IoTemplateValue::derive_implicit_checks)
            .unwrap_or_default(),
    })
}

fn instantiate_net_template<'v>(
    name: &str,
    template: Value<'v>,
    should_register: bool,
    eval: &mut Evaluator<'v, '_, '_>,
) -> starlark::Result<Value<'v>> {
    let net_type = NetType::new(net_template_type_name(template)?, SmallMap::new(), eval)?;
    let net = clone_net_template(template, eval.heap())?;
    let net = net
        .downcast_ref::<NetValue<'v>>()
        .expect("net template clone should produce NetValue");
    net_type.instantiate(
        Some(net),
        net_template_name(template)
            .is_none()
            .then(|| name.to_owned()),
        SmallMap::new(),
        super::net::NetInstantiateOptions {
            should_register,
            assignment_inferable: false,
        },
        eval,
    )
}

fn run_implicit_checks<'v>(
    name: &str,
    checks: &[ImplicitCheck<'v>],
    value: Value<'v>,
) -> anyhow::Result<()> {
    for check in checks {
        match check {
            ImplicitCheck::VoltageWithin {
                template_voltage,
                template_display,
            } => {
                let Some(template_voltage) = template_voltage.downcast_ref::<PhysicalValue>()
                else {
                    continue;
                };
                let actual_voltage = net_property_value(value, "voltage")
                    .and_then(|value| value.downcast_ref::<PhysicalValue>())
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Input '{name}' is missing voltage required by template voltage {template_display}"
                        )
                    })?;

                if !actual_voltage.fits_within_default(template_voltage) {
                    anyhow::bail!(
                        "Input '{name}' voltage {} is not within template voltage {template_display}",
                        actual_voltage
                    );
                }
            }
        }
    }

    Ok(())
}
