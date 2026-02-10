use crate::codegen::starlark;
use pcb_zen_core::lang::stackup as zen_stackup;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportedNetKind {
    Net,
    Power,
    Ground,
}

#[derive(Debug, Clone)]
pub struct ImportedNetDecl {
    pub ident: String,
    /// The Zener net name to use in `Net("...")`.
    ///
    /// This is derived from the original KiCad net name with minimal sanitization so the
    /// net names stay recognizable.
    pub name: String,
    pub kind: ImportedNetKind,
}

#[derive(Debug, Clone)]
pub struct ImportedIoNetDecl {
    pub ident: String,
    pub kind: ImportedNetKind,
}

#[derive(Debug, Clone)]
pub struct ImportedInstanceCall {
    pub module_ident: String,
    pub refdes: String,
    pub dnp: bool,
    pub skip_bom: Option<bool>,
    pub skip_pos: Option<bool>,
    /// Config kwarg name -> raw string value.
    ///
    /// Rendered as `key = "value",` before IO net plumbing.
    pub config_args: BTreeMap<String, String>,
    /// IO name -> board net identifier
    pub io_nets: BTreeMap<String, String>,
}

pub fn render_imported_board(
    board_name: &str,
    copper_layers: usize,
    stackup: Option<&zen_stackup::Stackup>,
    uses_not_connected: bool,
    net_decls: &[ImportedNetDecl],
    module_decls: &[(String, String)],
    instance_calls: &[ImportedInstanceCall],
) -> String {
    let mut out = String::new();

    out.push_str("\"\"\"\n");
    out.push_str(board_name);
    out.push_str("\n\"\"\"\n\n");

    if stackup.is_some() {
        out.push_str(
            "load(\"@stdlib/board_config.zen\", \"Board\", \"BoardConfig\", \"Stackup\", \"Material\", \"CopperLayer\", \"DielectricLayer\")\n\n",
        );
    } else {
        out.push_str("load(\"@stdlib/board_config.zen\", \"Board\", \"BoardConfig\")\n\n");
    }

    out.push_str(&render_imported_module_body(
        &[],
        net_decls,
        module_decls,
        instance_calls,
        uses_not_connected,
    ));

    out.push_str("Board(\n");
    out.push_str(&format!("    name = {},\n", starlark::string(board_name)));
    out.push_str(&format!(
        "    layout_path = {},\n",
        starlark::string("layout")
    ));
    out.push_str(&format!("    layers = {copper_layers},\n"));

    if let Some(stackup) = stackup {
        out.push_str("    config = BoardConfig(\n");
        out.push_str("        stackup = ");
        out.push_str(&render_stackup_expr(stackup, 2));
        out.push_str(",\n");
        out.push_str("    ),\n");
    } else {
        out.push_str("    config = BoardConfig(),\n");
    }

    out.push_str(")\n");
    out
}

pub fn render_imported_sheet_module(
    module_doc: &str,
    io_nets: &[ImportedIoNetDecl],
    internal_net_decls: &[ImportedNetDecl],
    module_decls: &[(String, String)],
    instance_calls: &[ImportedInstanceCall],
    uses_not_connected: bool,
) -> String {
    let mut out = String::new();

    out.push_str("\"\"\"\n");
    out.push_str(module_doc);
    out.push_str("\n\"\"\"\n\n");

    out.push_str(&render_imported_module_body(
        io_nets,
        internal_net_decls,
        module_decls,
        instance_calls,
        uses_not_connected,
    ));

    out
}

fn render_imported_module_body(
    io_nets: &[ImportedIoNetDecl],
    internal_net_decls: &[ImportedNetDecl],
    module_decls: &[(String, String)],
    instance_calls: &[ImportedInstanceCall],
    uses_not_connected: bool,
) -> String {
    let mut out = String::new();

    let uses_power = internal_net_decls
        .iter()
        .any(|n| n.kind == ImportedNetKind::Power)
        || io_nets.iter().any(|n| n.kind == ImportedNetKind::Power);
    let uses_ground = internal_net_decls
        .iter()
        .any(|n| n.kind == ImportedNetKind::Ground)
        || io_nets.iter().any(|n| n.kind == ImportedNetKind::Ground);

    if uses_not_connected || uses_power || uses_ground {
        let mut items: Vec<&str> = Vec::new();
        if uses_not_connected {
            items.push("NotConnected");
        }
        if uses_power {
            items.push("Power");
        }
        if uses_ground {
            items.push("Ground");
        }
        out.push_str("load(\"@stdlib/interfaces.zen\", ");
        for (i, item) in items.iter().enumerate() {
            if i != 0 {
                out.push_str(", ");
            }
            out.push_str(&starlark::string(item));
        }
        out.push_str(")\n\n");
    }

    if !io_nets.is_empty() {
        for net in io_nets {
            let ty = match net.kind {
                ImportedNetKind::Net => "Net",
                ImportedNetKind::Power => "Power",
                ImportedNetKind::Ground => "Ground",
            };
            out.push_str(&net.ident);
            out.push_str(" = io(");
            out.push_str(&starlark::string(&net.ident));
            out.push_str(", ");
            out.push_str(ty);
            out.push_str(")\n");
        }
        out.push('\n');
    }

    if !internal_net_decls.is_empty() {
        for net in internal_net_decls {
            let ctor = match net.kind {
                ImportedNetKind::Net => "Net",
                ImportedNetKind::Power => "Power",
                ImportedNetKind::Ground => "Ground",
            };
            out.push_str(&net.ident);
            out.push_str(" = ");
            out.push_str(ctor);
            out.push('(');
            out.push_str(&starlark::string(&net.name));
            out.push_str(")\n");
        }
        out.push('\n');
    }

    if !module_decls.is_empty() {
        let mut stdlib_decls: Vec<(&str, &str)> = Vec::new();
        let mut local_decls: Vec<(&str, &str)> = Vec::new();
        for (ident, module_path) in module_decls {
            if module_path.starts_with("@stdlib/") {
                stdlib_decls.push((ident, module_path));
            } else {
                local_decls.push((ident, module_path));
            }
        }

        let has_stdlib = !stdlib_decls.is_empty();
        let has_local = !local_decls.is_empty();

        for (ident, module_path) in stdlib_decls {
            out.push_str(ident);
            out.push_str(" = Module(");
            out.push_str(&starlark::string(module_path));
            out.push_str(")\n");
        }
        if has_stdlib && has_local {
            out.push('\n');
        }
        for (ident, module_path) in local_decls {
            out.push_str(ident);
            out.push_str(" = Module(");
            out.push_str(&starlark::string(module_path));
            out.push_str(")\n");
        }
        out.push('\n');
    }

    if !instance_calls.is_empty() {
        for call in instance_calls {
            let mut args: Vec<String> = Vec::new();
            args.push(format!("name = {}", starlark::string(&call.refdes)));
            if call.dnp {
                args.push("dnp = True".to_string());
            }
            if let Some(skip_bom) = call.skip_bom {
                args.push(format!("skip_bom = {}", starlark::bool(skip_bom)));
            }
            if let Some(skip_pos) = call.skip_pos {
                args.push(format!("skip_pos = {}", starlark::bool(skip_pos)));
            }
            for (k, v) in &call.config_args {
                args.push(format!("{k} = {}", starlark::string(v)));
            }
            for (io, net_ident) in &call.io_nets {
                args.push(format!("{io} = {net_ident}"));
            }

            out.push_str(&call.module_ident);
            if args.len() <= 3 {
                out.push('(');
                out.push_str(&args.join(", "));
                out.push_str(")\n\n");
            } else {
                out.push_str("(\n");
                for arg in args {
                    out.push_str("    ");
                    out.push_str(&arg);
                    out.push_str(",\n");
                }
                out.push_str(")\n\n");
            }
        }
    }

    out
}

fn render_stackup_expr(stackup: &zen_stackup::Stackup, base_indent: usize) -> String {
    let indent0 = " ".repeat(base_indent * 4);
    let indent1 = " ".repeat((base_indent + 1) * 4);
    let indent2 = " ".repeat((base_indent + 2) * 4);

    let mut out = String::new();
    out.push_str("Stackup(\n");

    if let Some(materials) = stackup.materials.as_deref() {
        if !materials.is_empty() {
            out.push_str(&format!("{indent1}materials = [\n"));
            for material in materials {
                out.push_str(&format!("{indent2}{},\n", render_material_expr(material)));
            }
            out.push_str(&format!("{indent1}],\n"));
        }
    }

    if let Some(color) = stackup.silk_screen_color.as_deref() {
        out.push_str(&format!(
            "{indent1}silk_screen_color = {},\n",
            starlark::string(color)
        ));
    }
    if let Some(color) = stackup.solder_mask_color.as_deref() {
        out.push_str(&format!(
            "{indent1}solder_mask_color = {},\n",
            starlark::string(color)
        ));
    }

    if let Some(thickness) = stackup.thickness() {
        out.push_str(&format!(
            "{indent1}thickness = {},\n",
            starlark::float(thickness)
        ));
    }

    if let Some(layers) = stackup.layers.as_deref() {
        if !layers.is_empty() {
            out.push_str(&format!("{indent1}layers = [\n"));
            for layer in layers {
                out.push_str(&format!("{indent2}{},\n", render_stackup_layer_expr(layer)));
            }
            out.push_str(&format!("{indent1}],\n"));
        }
    }

    if let Some(finish) = stackup.copper_finish.as_ref() {
        out.push_str(&format!(
            "{indent1}copper_finish = {},\n",
            starlark::string(&finish.to_string())
        ));
    }

    out.push_str(&format!("{indent0})"));
    out
}

fn render_material_expr(material: &zen_stackup::Material) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = material.name.as_deref() {
        parts.push(format!("name = {}", starlark::string(name)));
    }
    if let Some(vendor) = material.vendor.as_deref() {
        parts.push(format!("vendor = {}", starlark::string(vendor)));
    }
    if let Some(er) = material.relative_permittivity {
        parts.push(format!("relative_permittivity = {}", starlark::float(er)));
    }
    if let Some(tan) = material.loss_tangent {
        parts.push(format!("loss_tangent = {}", starlark::float(tan)));
    }
    if let Some(freq) = material.reference_frequency {
        parts.push(format!("reference_frequency = {}", starlark::float(freq)));
    }

    if parts.is_empty() {
        return "Material()".to_string();
    }
    format!("Material({})", parts.join(", "))
}

fn render_stackup_layer_expr(layer: &zen_stackup::Layer) -> String {
    match layer {
        zen_stackup::Layer::Copper { thickness, role } => {
            format!(
                "CopperLayer(thickness = {}, role = {})",
                starlark::float(*thickness),
                starlark::string(&role.to_string())
            )
        }
        zen_stackup::Layer::Dielectric {
            thickness,
            material,
            form,
        } => {
            format!(
                "DielectricLayer(thickness = {}, material = {}, form = {})",
                starlark::float(*thickness),
                starlark::string(material),
                starlark::string(&form.to_string())
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_invocation_is_single_line_for_small_arg_counts() {
        let mut io_nets = BTreeMap::new();
        io_nets.insert("P1".to_string(), "GND".to_string());

        let out = render_imported_sheet_module(
            "TestModule",
            &[],
            &[],
            &[],
            &[ImportedInstanceCall {
                module_ident: "Foo".to_string(),
                refdes: "H1".to_string(),
                dnp: false,
                skip_bom: None,
                skip_pos: None,
                config_args: BTreeMap::new(),
                io_nets,
            }],
            false,
        );

        assert!(out.contains("Foo(name = \"H1\", P1 = GND)\n"));
        assert!(!out.contains("Foo(\n"));
    }

    #[test]
    fn module_invocation_is_multiline_for_larger_arg_counts() {
        let mut config_args = BTreeMap::new();
        config_args.insert("foo".to_string(), "bar".to_string());
        config_args.insert("baz".to_string(), "qux".to_string());

        let mut io_nets = BTreeMap::new();
        io_nets.insert("P1".to_string(), "GND".to_string());

        let out = render_imported_sheet_module(
            "TestModule",
            &[],
            &[],
            &[],
            &[ImportedInstanceCall {
                module_ident: "Foo".to_string(),
                refdes: "H1".to_string(),
                dnp: false,
                skip_bom: None,
                skip_pos: None,
                config_args,
                io_nets,
            }],
            false,
        );

        assert!(out.contains("Foo(\n"));
        assert!(out.contains("    name = \"H1\",\n"));
        assert!(out.contains("    P1 = GND,\n"));
    }
}
