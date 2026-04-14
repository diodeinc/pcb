use std::collections::{BTreeSet, HashMap};

use pcb_sch::Schematic;
use starlark::codemap::ResolvedSpan;
use starlark::errors::EvalSeverity;
use starlark::values::ValueLike;

use crate::lang::pin_erc::{
    pin_no_connect_body, pin_types_are_only_no_connect, signal_pin_type_candidates,
};
use crate::lang::symbol::SymbolValue;
use crate::{Diagnostic, Diagnostics, EvalOutput, FrozenNetValue, ModulePath};

#[derive(Clone)]
struct NetMetadata {
    display_name: String,
    path: String,
    span: Option<ResolvedSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ComponentSignalKey {
    component_path: String,
    signal_name: String,
}

#[derive(Clone)]
struct NetPinAttachment {
    component_name: String,
    signal_name: String,
    pin_types: Vec<String>,
}

#[derive(Clone)]
struct ErcNet<'a> {
    net: &'a pcb_sch::Net,
    metadata: Option<NetMetadata>,
    pin_attachments: Vec<NetPinAttachment>,
}

struct SchematicErcContext<'a> {
    nets: Vec<ErcNet<'a>>,
}

trait SchematicErcPass {
    fn run(&self, ctx: &SchematicErcContext<'_>, diagnostics: &mut Diagnostics);
}

struct PinNoConnectPass;

fn component_path(module_path: &ModulePath, component_name: &str) -> String {
    if module_path.is_root() {
        component_name.to_string()
    } else {
        format!("{module_path}.{component_name}")
    }
}

fn signal_names(symbol: &SymbolValue) -> BTreeSet<&str> {
    symbol
        .pad_to_signal
        .values()
        .map(|value| value.as_str())
        .collect()
}

impl<'a> SchematicErcContext<'a> {
    fn build(eval_output: &EvalOutput, schematic: &'a Schematic) -> Self {
        let mut pin_types_by_component_signal: HashMap<ComponentSignalKey, Vec<String>> =
            HashMap::new();
        let mut net_metadata: HashMap<u64, NetMetadata> = HashMap::new();

        for (module_path, module) in eval_output.module_tree() {
            for component in module.components() {
                let component_path = component_path(&module_path, component.name());

                if let Some(symbol) = component.symbol().downcast_ref::<SymbolValue>() {
                    for signal_name in signal_names(symbol) {
                        let candidates = signal_pin_type_candidates(symbol, signal_name);
                        if !candidates.is_empty() {
                            pin_types_by_component_signal.insert(
                                ComponentSignalKey {
                                    component_path: component_path.clone(),
                                    signal_name: signal_name.to_string(),
                                },
                                candidates,
                            );
                        }
                    }
                }

                for net_value in component.connections().values() {
                    if let Some(net) = net_value.downcast_ref::<FrozenNetValue>() {
                        net_metadata.entry(net.id()).or_insert_with(|| NetMetadata {
                            display_name: net.name().to_string(),
                            path: net.declaration_path().unwrap_or_default().to_string(),
                            span: net.declaration_span(),
                        });
                    }
                }
            }
        }

        let mut nets = Vec::new();
        for net in schematic.nets.values() {
            let mut pin_attachments = Vec::new();

            for port_ref in &net.ports {
                let Some((component_ref, signal_name)) =
                    schematic.component_ref_and_pin_for_port(port_ref)
                else {
                    continue;
                };

                let component_path = component_ref.instance_path.join(".");
                let Some(pin_types) = pin_types_by_component_signal.get(&ComponentSignalKey {
                    component_path: component_path.clone(),
                    signal_name: signal_name.to_string(),
                }) else {
                    continue;
                };

                let component_name = component_ref
                    .instance_path
                    .last()
                    .map(String::as_str)
                    .unwrap_or("<component>")
                    .to_string();

                pin_attachments.push(NetPinAttachment {
                    component_name,
                    signal_name: signal_name.to_string(),
                    pin_types: pin_types.clone(),
                });
            }

            nets.push(ErcNet {
                net,
                metadata: net_metadata.get(&net.id).cloned(),
                pin_attachments,
            });
        }

        Self { nets }
    }
}

impl SchematicErcPass for PinNoConnectPass {
    fn run(&self, ctx: &SchematicErcContext<'_>, diagnostics: &mut Diagnostics) {
        for net in &ctx.nets {
            let net_kind = net.net.kind.as_str();

            if net_kind == "NotConnected" {
                continue;
            }

            for attachment in &net.pin_attachments {
                if !pin_types_are_only_no_connect(&attachment.pin_types) {
                    continue;
                }

                let body = pin_no_connect_body(
                    &attachment.component_name,
                    &attachment.signal_name,
                    net_kind,
                    net.metadata
                        .as_ref()
                        .map(|metadata| metadata.display_name.as_str())
                        .unwrap_or(net.net.name.as_str()),
                );
                let path = net
                    .metadata
                    .as_ref()
                    .map(|metadata| metadata.path.clone())
                    .unwrap_or_default();
                let span = net.metadata.as_ref().and_then(|metadata| metadata.span);

                diagnostics.diagnostics.push(
                    Diagnostic::categorized(&path, &body, "pin.no_connect", EvalSeverity::Warning)
                        .with_span(span),
                );
            }
        }
    }
}

pub fn run_schematic_erc(eval_output: &EvalOutput, schematic: &Schematic) -> Diagnostics {
    let ctx = SchematicErcContext::build(eval_output, schematic);
    let mut diagnostics = Diagnostics::default();
    let passes: [&dyn SchematicErcPass; 1] = [&PinNoConnectPass];

    for pass in passes {
        pass.run(&ctx, &mut diagnostics);
    }

    diagnostics
}
