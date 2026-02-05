use crate::codegen::starlark;
use pcb_zen_core::lang::stackup as zen_stackup;
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ImportedNetDecl {
    pub ident: String,
    /// The Zener net name to use in `Net("...")`.
    ///
    /// This is derived from the original KiCad net name with minimal sanitization so the
    /// net names stay recognizable.
    pub name: String,
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
        out.push_str("load(\"@stdlib/board_config.zen\", \"Board\")\n\n");
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
        starlark::string(&format!("layout/{board_name}"))
    ));
    out.push_str(&format!("    layers = {copper_layers},\n"));

    if let Some(stackup) = stackup {
        out.push_str("    config = BoardConfig(\n");
        out.push_str("        stackup = ");
        out.push_str(&render_stackup_expr(stackup, 2));
        out.push_str(",\n");
        out.push_str("    ),\n");
    }

    out.push_str(")\n");
    out
}

pub fn render_imported_sheet_module(
    module_doc: &str,
    io_net_idents: &[String],
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
        io_net_idents,
        internal_net_decls,
        module_decls,
        instance_calls,
        uses_not_connected,
    ));

    out
}

fn render_imported_module_body(
    io_net_idents: &[String],
    internal_net_decls: &[ImportedNetDecl],
    module_decls: &[(String, String)],
    instance_calls: &[ImportedInstanceCall],
    uses_not_connected: bool,
) -> String {
    let mut out = String::new();

    if uses_not_connected {
        out.push_str("load(\"@stdlib/interfaces.zen\", \"NotConnected\")\n\n");
    }

    if !io_net_idents.is_empty() {
        for ident in io_net_idents {
            out.push_str(ident);
            out.push_str(" = io(");
            out.push_str(&starlark::string(ident));
            out.push_str(", Net)\n");
        }
        out.push('\n');
    }

    if !internal_net_decls.is_empty() {
        for net in internal_net_decls {
            out.push_str(&net.ident);
            out.push_str(" = Net(");
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
            out.push_str(&call.module_ident);
            out.push_str("(\n");
            out.push_str(&format!("    name = {},\n", starlark::string(&call.refdes)));
            if call.dnp {
                out.push_str("    dnp = True,\n");
            }
            if let Some(skip_bom) = call.skip_bom {
                out.push_str("    skip_bom = ");
                out.push_str(starlark::bool(skip_bom));
                out.push_str(",\n");
            }
            if let Some(skip_pos) = call.skip_pos {
                out.push_str("    skip_pos = ");
                out.push_str(starlark::bool(skip_pos));
                out.push_str(",\n");
            }

            for (k, v) in &call.config_args {
                out.push_str("    ");
                out.push_str(k);
                out.push_str(" = ");
                out.push_str(&starlark::string(v));
                out.push_str(",\n");
            }

            for (io, net_ident) in &call.io_nets {
                out.push_str("    ");
                out.push_str(io);
                out.push_str(" = ");
                out.push_str(net_ident);
                out.push_str(",\n");
            }
            out.push_str(")\n\n");
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
