use crate::codegen::starlark;
use pcb_zen_core::lang::stackup as zen_stackup;

#[derive(Debug, Clone)]
pub struct ImportedNetDecl {
    pub ident: String,
    /// Original KiCad net name (may contain characters not allowed by Zener).
    pub kicad_name: String,
}

pub fn render_imported_board(
    board_name: &str,
    copper_layers: usize,
    stackup: Option<&zen_stackup::Stackup>,
    net_decls: &[ImportedNetDecl],
    module_decls: &[(String, String)],
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

    if !net_decls.is_empty() {
        for net in net_decls {
            out.push_str(&net.ident);
            out.push_str(" = Net(");
            out.push_str(&starlark::string(&net.ident));
            out.push_str(")\n");
        }
        out.push('\n');

        out.push_str("KICAD_NET_NAME_MAP = {\n");
        for net in net_decls {
            out.push_str("    ");
            out.push_str(&starlark::string(&net.ident));
            out.push_str(": ");
            out.push_str(&starlark::string(&net.kicad_name));
            out.push_str(",\n");
        }
        out.push_str("}\n\n");
    }

    if !module_decls.is_empty() {
        for (ident, module_path) in module_decls {
            out.push_str(ident);
            out.push_str(" = Module(");
            out.push_str(&starlark::string(module_path));
            out.push_str(")\n");
        }
        out.push('\n');
    }

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
