use crate::codegen::starlark;
use pcb_zen_core::lang::stackup as zen_stackup;

pub fn render_board_with_stackup(
    board_name: &str,
    copper_layers: usize,
    stackup: &zen_stackup::Stackup,
) -> String {
    let mut out = String::new();

    out.push_str("\"\"\"\n");
    out.push_str(board_name);
    out.push_str("\n\"\"\"\n\n");

    out.push_str(
        "load(\"@stdlib/board_config.zen\", \"Board\", \"BoardConfig\", \"Stackup\", \"Material\", \"CopperLayer\", \"DielectricLayer\")\n\n",
    );

    out.push_str("Board(\n");
    out.push_str(&format!("    name = {},\n", starlark::string(board_name)));
    out.push_str(&format!(
        "    layout_path = {},\n",
        starlark::string(&format!("layout/{board_name}"))
    ));
    out.push_str(&format!("    layers = {copper_layers},\n"));
    out.push_str("    config = BoardConfig(\n");
    out.push_str("        stackup = ");
    out.push_str(&render_stackup_expr(stackup, 2));
    out.push_str(",\n");
    out.push_str("    ),\n");
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
            let role_str = match role {
                zen_stackup::CopperRole::Signal => "signal",
                zen_stackup::CopperRole::Power => "power",
                zen_stackup::CopperRole::Ground => "power",
                zen_stackup::CopperRole::Mixed => "mixed",
            };
            format!(
                "CopperLayer(thickness = {}, role = {})",
                starlark::float(*thickness),
                starlark::string(role_str)
            )
        }
        zen_stackup::Layer::Dielectric {
            thickness,
            material,
            form,
        } => {
            let form_str = match form {
                zen_stackup::DielectricForm::Core => "core",
                zen_stackup::DielectricForm::Prepreg => "prepreg",
            };
            format!(
                "DielectricLayer(thickness = {}, material = {}, form = {})",
                starlark::float(*thickness),
                starlark::string(material),
                starlark::string(form_str)
            )
        }
    }
}
