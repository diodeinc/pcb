use crate::codegen::starlark;
use pcb_zen_core::lang::stackup as zen_stackup;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
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

pub struct RenderImportedBoardArgs<'a> {
    pub board_name: &'a str,
    pub copper_layers: usize,
    pub design_rules: Option<&'a zen_stackup::DesignRules>,
    pub stackup: Option<&'a zen_stackup::Stackup>,
    pub net_decls: &'a [ImportedNetDecl],
    pub module_decls: &'a [(String, String)],
    pub instance_calls: &'a [ImportedInstanceCall],
}

pub fn render_imported_board(args: RenderImportedBoardArgs<'_>) -> String {
    let mut out = String::new();

    out.push_str("\"\"\"\n");
    out.push_str(args.board_name);
    out.push_str("\n\"\"\"\n\n");

    out.push_str(&render_board_config_load_stmt(
        args.stackup.is_some(),
        args.design_rules.is_some(),
    ));

    out.push_str(&render_imported_module_body(
        &[],
        args.net_decls,
        args.module_decls,
        args.instance_calls,
    ));

    out.push_str("Board(\n");
    out.push_str(&format!(
        "    name = {},\n",
        starlark::string(args.board_name)
    ));
    out.push_str(&format!(
        "    layout_path = {},\n",
        starlark::string("layout")
    ));
    out.push_str(&format!("    layers = {},\n", args.copper_layers));

    if args.design_rules.is_some() || args.stackup.is_some() {
        out.push_str("    config = BoardConfig(\n");
        if let Some(design_rules) = args.design_rules {
            out.push_str("        design_rules = ");
            out.push_str(&render_design_rules_expr(design_rules, 2));
            out.push_str(",\n");
        }
        if let Some(stackup) = args.stackup {
            out.push_str("        stackup = ");
            out.push_str(&render_stackup_expr(stackup, 2));
            out.push_str(",\n");
        }
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
    ));

    out
}

fn render_imported_module_body(
    io_nets: &[ImportedIoNetDecl],
    internal_net_decls: &[ImportedNetDecl],
    module_decls: &[(String, String)],
    instance_calls: &[ImportedInstanceCall],
) -> String {
    let mut out = String::new();
    let uses_not_connected = instance_calls_use_not_connected(instance_calls);

    let uses_kind = |kind: ImportedNetKind| {
        internal_net_decls.iter().any(|n| n.kind == kind) || io_nets.iter().any(|n| n.kind == kind)
    };
    let uses_power = uses_kind(ImportedNetKind::Power);
    let uses_ground = uses_kind(ImportedNetKind::Ground);

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
            let ty = imported_net_ctor(net.kind);
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
            let ctor = imported_net_ctor(net.kind);
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

fn instance_calls_use_not_connected(instance_calls: &[ImportedInstanceCall]) -> bool {
    instance_calls.iter().any(|call| {
        call.io_nets
            .values()
            .any(|expr| expr.trim_start().starts_with("NotConnected("))
    })
}

fn imported_net_ctor(kind: ImportedNetKind) -> &'static str {
    match kind {
        ImportedNetKind::Net => "Net",
        ImportedNetKind::Power => "Power",
        ImportedNetKind::Ground => "Ground",
    }
}

fn render_board_config_load_stmt(has_stackup: bool, has_design_rules: bool) -> String {
    let mut symbols = vec!["Board", "BoardConfig"];
    if has_design_rules {
        symbols.extend([
            "DesignRules",
            "Constraints",
            "Copper",
            "Holes",
            "Uvias",
            "Silkscreen",
            "SolderMask",
            "Zones",
            "PredefinedSizes",
            "ViaDimension",
            "NetClass",
        ]);
    }
    if has_stackup {
        symbols.extend(["Stackup", "Material", "CopperLayer", "DielectricLayer"]);
    }

    let mut out = String::new();
    out.push_str("load(\"@stdlib/board_config.zen\", ");
    for (idx, symbol) in symbols.iter().enumerate() {
        if idx > 0 {
            out.push_str(", ");
        }
        out.push_str(&starlark::string(symbol));
    }
    out.push_str(")\n\n");
    out
}

fn render_design_rules_expr(design_rules: &zen_stackup::DesignRules, base_indent: usize) -> String {
    let indent0 = " ".repeat(base_indent * 4);
    let indent1 = " ".repeat((base_indent + 1) * 4);
    let indent2 = " ".repeat((base_indent + 2) * 4);

    let mut out = String::new();
    out.push_str("DesignRules(\n");

    if let Some(constraints) = design_rules
        .constraints
        .as_ref()
        .and_then(render_constraints_expr)
    {
        out.push_str(&format!("{indent1}constraints = {constraints},\n"));
    }

    if let Some(predefined_sizes) = design_rules
        .predefined_sizes
        .as_ref()
        .and_then(render_predefined_sizes_expr)
    {
        out.push_str(&format!(
            "{indent1}predefined_sizes = {predefined_sizes},\n"
        ));
    }

    if !design_rules.netclasses.is_empty() {
        out.push_str(&format!("{indent1}netclasses = [\n"));
        for netclass in &design_rules.netclasses {
            out.push_str(&format!("{indent2}{},\n", render_netclass_expr(netclass)));
        }
        out.push_str(&format!("{indent1}],\n"));
    }

    out.push_str(&format!("{indent0})"));
    out
}

fn render_constraints_expr(constraints: &JsonValue) -> Option<String> {
    deserialize_json::<ConstraintsView>(constraints)?.render_expr()
}

fn render_predefined_sizes_expr(predefined_sizes: &JsonValue) -> Option<String> {
    deserialize_json::<PredefinedSizesView>(predefined_sizes)?.render_expr()
}

fn render_netclass_expr(netclass: &zen_stackup::NetClass) -> String {
    let mut parts: Vec<String> = vec![format!("name = {}", starlark::string(&netclass.name))];
    for (name, value) in [
        ("clearance", netclass.clearance),
        ("track_width", netclass.track_width),
        ("via_diameter", netclass.via_diameter),
        ("via_drill", netclass.via_drill),
        ("microvia_diameter", netclass.microvia_diameter),
        ("microvia_drill", netclass.microvia_drill),
        ("diff_pair_width", netclass.diff_pair_width),
        ("diff_pair_gap", netclass.diff_pair_gap),
        ("diff_pair_via_gap", netclass.diff_pair_via_gap),
    ] {
        push_opt_float_field(&mut parts, name, value);
    }
    if let Some(priority) = netclass.priority {
        parts.push(format!("priority = {priority}"));
    }
    if let Some(color) = netclass.color.as_deref() {
        parts.push(format!("color = {}", starlark::string(color)));
    }

    format!("NetClass({})", parts.join(", "))
}

fn push_opt_float_field(parts: &mut Vec<String>, field_name: &str, value: Option<f64>) {
    if let Some(value) = value {
        parts.push(format!("{field_name} = {}", starlark::float(value)));
    }
}

fn render_float_ctor<const N: usize>(
    ctor: &str,
    fields: [(&str, Option<f64>); N],
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for (field, value) in fields {
        push_opt_float_field(&mut parts, field, value);
    }
    (!parts.is_empty()).then(|| format!("{ctor}({})", parts.join(", ")))
}

fn append_named_expr(parts: &mut Vec<String>, field_name: &str, expr: Option<String>) {
    if let Some(expr) = expr {
        parts.push(format!("{field_name} = {expr}"));
    }
}

fn deserialize_json<T: DeserializeOwned>(value: &JsonValue) -> Option<T> {
    serde_json::from_value(value.clone()).ok()
}

#[derive(Debug, Deserialize)]
struct ConstraintsView {
    #[serde(default)]
    copper: Option<CopperConstraintsView>,
    #[serde(default)]
    holes: Option<HolesConstraintsView>,
    #[serde(default)]
    uvias: Option<UviasConstraintsView>,
    #[serde(default)]
    silkscreen: Option<SilkscreenConstraintsView>,
    #[serde(default)]
    solder_mask: Option<SolderMaskConstraintsView>,
    #[serde(default)]
    zones: Option<ZonesConstraintsView>,
}

impl ConstraintsView {
    fn render_expr(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        append_named_expr(
            &mut parts,
            "copper",
            self.copper
                .as_ref()
                .and_then(CopperConstraintsView::render_expr),
        );
        append_named_expr(
            &mut parts,
            "holes",
            self.holes
                .as_ref()
                .and_then(HolesConstraintsView::render_expr),
        );
        append_named_expr(
            &mut parts,
            "uvias",
            self.uvias
                .as_ref()
                .and_then(UviasConstraintsView::render_expr),
        );
        append_named_expr(
            &mut parts,
            "silkscreen",
            self.silkscreen
                .as_ref()
                .and_then(SilkscreenConstraintsView::render_expr),
        );
        append_named_expr(
            &mut parts,
            "solder_mask",
            self.solder_mask
                .as_ref()
                .and_then(SolderMaskConstraintsView::render_expr),
        );
        append_named_expr(
            &mut parts,
            "zones",
            self.zones
                .as_ref()
                .and_then(ZonesConstraintsView::render_expr),
        );
        (!parts.is_empty()).then(|| format!("Constraints({})", parts.join(", ")))
    }
}

#[derive(Debug, Deserialize)]
struct CopperConstraintsView {
    #[serde(default)]
    minimum_clearance: Option<f64>,
    #[serde(default)]
    minimum_track_width: Option<f64>,
    #[serde(default)]
    minimum_connection_width: Option<f64>,
    #[serde(default)]
    minimum_annular_width: Option<f64>,
    #[serde(default)]
    minimum_via_diameter: Option<f64>,
    #[serde(default)]
    copper_to_hole_clearance: Option<f64>,
    #[serde(default)]
    copper_to_edge_clearance: Option<f64>,
}

impl CopperConstraintsView {
    fn render_expr(&self) -> Option<String> {
        render_float_ctor(
            "Copper",
            [
                ("minimum_clearance", self.minimum_clearance),
                ("minimum_track_width", self.minimum_track_width),
                ("minimum_connection_width", self.minimum_connection_width),
                ("minimum_annular_width", self.minimum_annular_width),
                ("minimum_via_diameter", self.minimum_via_diameter),
                ("copper_to_hole_clearance", self.copper_to_hole_clearance),
                ("copper_to_edge_clearance", self.copper_to_edge_clearance),
            ],
        )
    }
}

#[derive(Debug, Deserialize)]
struct HolesConstraintsView {
    #[serde(default)]
    minimum_through_hole: Option<f64>,
    #[serde(default)]
    hole_to_hole_clearance: Option<f64>,
}

impl HolesConstraintsView {
    fn render_expr(&self) -> Option<String> {
        render_float_ctor(
            "Holes",
            [
                ("minimum_through_hole", self.minimum_through_hole),
                ("hole_to_hole_clearance", self.hole_to_hole_clearance),
            ],
        )
    }
}

#[derive(Debug, Deserialize)]
struct UviasConstraintsView {
    #[serde(default)]
    minimum_uvia_diameter: Option<f64>,
    #[serde(default)]
    minimum_uvia_hole: Option<f64>,
}

impl UviasConstraintsView {
    fn render_expr(&self) -> Option<String> {
        render_float_ctor(
            "Uvias",
            [
                ("minimum_uvia_diameter", self.minimum_uvia_diameter),
                ("minimum_uvia_hole", self.minimum_uvia_hole),
            ],
        )
    }
}

#[derive(Debug, Deserialize)]
struct SilkscreenConstraintsView {
    #[serde(default)]
    minimum_item_clearance: Option<f64>,
    #[serde(default)]
    minimum_text_height: Option<f64>,
}

impl SilkscreenConstraintsView {
    fn render_expr(&self) -> Option<String> {
        render_float_ctor(
            "Silkscreen",
            [
                ("minimum_item_clearance", self.minimum_item_clearance),
                ("minimum_text_height", self.minimum_text_height),
            ],
        )
    }
}

#[derive(Debug, Deserialize)]
struct SolderMaskConstraintsView {
    #[serde(default)]
    clearance: Option<f64>,
    #[serde(default)]
    minimum_width: Option<f64>,
    #[serde(default)]
    to_copper_clearance: Option<f64>,
}

impl SolderMaskConstraintsView {
    fn render_expr(&self) -> Option<String> {
        render_float_ctor(
            "SolderMask",
            [
                ("clearance", self.clearance),
                ("minimum_width", self.minimum_width),
                ("to_copper_clearance", self.to_copper_clearance),
            ],
        )
    }
}

#[derive(Debug, Deserialize)]
struct ZonesConstraintsView {
    #[serde(default)]
    minimum_clearance: Option<f64>,
}

impl ZonesConstraintsView {
    fn render_expr(&self) -> Option<String> {
        render_float_ctor("Zones", [("minimum_clearance", self.minimum_clearance)])
    }
}

#[derive(Debug, Deserialize)]
struct PredefinedSizesView {
    #[serde(default)]
    track_widths: Vec<f64>,
    #[serde(default)]
    via_dimensions: Vec<ViaDimensionView>,
}

impl PredefinedSizesView {
    fn render_expr(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if !self.track_widths.is_empty() {
            let widths = self
                .track_widths
                .iter()
                .map(|value| starlark::float(*value))
                .collect::<Vec<_>>();
            parts.push(format!("track_widths = [{}]", widths.join(", ")));
        }

        let vias = self
            .via_dimensions
            .iter()
            .filter_map(ViaDimensionView::render_expr)
            .collect::<Vec<_>>();
        if !vias.is_empty() {
            parts.push(format!("via_dimensions = [{}]", vias.join(", ")));
        }

        (!parts.is_empty()).then(|| format!("PredefinedSizes({})", parts.join(", ")))
    }
}

#[derive(Debug, Deserialize)]
struct ViaDimensionView {
    #[serde(default)]
    diameter: Option<f64>,
    #[serde(default)]
    drill: Option<f64>,
}

impl ViaDimensionView {
    fn render_expr(&self) -> Option<String> {
        render_float_ctor(
            "ViaDimension",
            [("diameter", self.diameter), ("drill", self.drill)],
        )
    }
}

fn render_stackup_expr(stackup: &zen_stackup::Stackup, base_indent: usize) -> String {
    let indent0 = " ".repeat(base_indent * 4);
    let indent1 = " ".repeat((base_indent + 1) * 4);
    let indent2 = " ".repeat((base_indent + 2) * 4);

    let mut out = String::new();
    out.push_str("Stackup(\n");

    if let Some(materials) = stackup.materials.as_deref()
        && !materials.is_empty()
    {
        out.push_str(&format!("{indent1}materials = [\n"));
        for material in materials {
            out.push_str(&format!("{indent2}{},\n", render_material_expr(material)));
        }
        out.push_str(&format!("{indent1}],\n"));
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

    if let Some(layers) = stackup.layers.as_deref()
        && !layers.is_empty()
    {
        out.push_str(&format!("{indent1}layers = [\n"));
        for layer in layers {
            out.push_str(&format!("{indent2}{},\n", render_stackup_layer_expr(layer)));
        }
        out.push_str(&format!("{indent1}],\n"));
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
    use serde_json::json;

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
        );

        assert!(out.contains("Foo(\n"));
        assert!(out.contains("    name = \"H1\",\n"));
        assert!(out.contains("    P1 = GND,\n"));
    }

    #[test]
    fn imported_board_renders_design_rules_when_present() {
        let design_rules = zen_stackup::DesignRules {
            constraints: Some(json!({
                "copper": {
                    "minimum_clearance": 0.12,
                    "minimum_track_width": 0.11
                },
                "holes": {
                    "minimum_through_hole": 0.3
                },
                "solder_mask": {
                    "clearance": 0.02,
                    "minimum_width": 0.05,
                    "to_copper_clearance": 0.01
                },
                "zones": {
                    "minimum_clearance": 0.25
                }
            })),
            predefined_sizes: Some(json!({
                "track_widths": [0.15, 0.2],
                "via_dimensions": [{"diameter": 0.5, "drill": 0.3}]
            })),
            netclasses: vec![zen_stackup::NetClass {
                name: "Default".to_string(),
                clearance: Some(0.16),
                track_width: Some(0.16),
                via_diameter: Some(0.5),
                via_drill: Some(0.3),
                microvia_diameter: None,
                microvia_drill: None,
                diff_pair_width: Some(0.2),
                diff_pair_gap: Some(0.2),
                diff_pair_via_gap: None,
                priority: Some(i32::MAX),
                color: None,
                single_ended_impedance: None,
                differential_pair_impedance: None,
            }],
        };

        let out = render_imported_board(RenderImportedBoardArgs {
            board_name: "Demo",
            copper_layers: 4,
            design_rules: Some(&design_rules),
            stackup: None,
            net_decls: &[],
            module_decls: &[],
            instance_calls: &[],
        });

        assert!(out.contains("\"DesignRules\""));
        assert!(out.contains("\"Constraints\""));
        assert!(out.contains("\"SolderMask\""));
        assert!(out.contains("\"Zones\""));
        assert!(out.contains("\"PredefinedSizes\""));
        assert!(out.contains("\"ViaDimension\""));
        assert!(out.contains("\"NetClass\""));
        assert!(out.contains("design_rules = DesignRules("));
        assert!(out.contains("constraints = Constraints("));
        assert!(out.contains("solder_mask = SolderMask("));
        assert!(out.contains("to_copper_clearance = 0.01"));
        assert!(out.contains("zones = Zones("));
        assert!(out.contains("predefined_sizes = PredefinedSizes("));
        assert!(out.contains("netclasses = ["));
    }
}
