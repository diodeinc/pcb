use minijinja::{context, Environment};
use pcb_zen_core::lang::stackup::{BoardConfig, CopperFinish, DielectricForm, Layer};
use serde::Serialize;

const MM_PER_OZ: f64 = 0.035;
const TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>PCB Stackup - Fabrication Drawing</title>
    <style>
* { margin: 0; padding: 0; box-sizing: border-box; }
body {
    font-family: 'SF Mono', Monaco, 'Cascadia Code', 'Roboto Mono', Consolas, 'Courier New', monospace;
    font-size: 12px; line-height: 1.5; padding: 24px; max-width: 800px; margin: 0 auto;
    background: #fafafa; color: #1a1a1a;
}
h1 { font-size: 18px; font-weight: 600; margin-bottom: 16px; letter-spacing: -0.02em; }
h2 { font-size: 14px; font-weight: 600; margin: 24px 0 8px 0; color: #4a4a4a; text-transform: uppercase; letter-spacing: 0.05em; }
.summary { display: grid; grid-template-columns: 1fr; gap: 0; margin-bottom: 24px; border: 1px solid #d0d0d0; background: #fff; width: fit-content; min-width: 320px; max-width: 450px; }
.summary-item { display: grid; grid-template-columns: 140px 1fr; padding: 6px 12px; border-bottom: 1px solid #e8e8e8; align-items: center; }
.summary-item:last-child { border-bottom: none; }
.summary-label { font-weight: 500; color: #666; }
.summary-value { font-weight: 400; color: #1a1a1a; text-align: right; display: flex; align-items: center; justify-content: flex-end; gap: 6px; }
.color-swatch { display: inline-block; width: 16px; height: 16px; border: 1px solid #666; border-radius: 2px; }
table { width: 100%; border-collapse: collapse; margin-bottom: 24px; font-size: 11px; background: #fff; border: 1px solid #d0d0d0; }
th { background: #e8e8e8; color: #000; font-weight: 900; text-align: left; padding: 8px 10px; border: 1px solid #d0d0d0; white-space: nowrap; }
th.number { text-align: center; width: 32px; }
th.thickness { width: 110px; }
th.dk { width: 60px; }
th.loss { width: 90px; }
td { padding: 6px 10px; border: 1px solid #e0e0e0; }
.layer-copper { background: #fef3cd; }
.layer-core { background: #e3f2fd; }
.layer-prepreg { background: #e8f4f8; }
.text-center { text-align: center; }
.text-right { text-align: right; font-variant-numeric: tabular-nums; }
@media print { body { background: #fff; padding: 12px; } }
    </style>
</head>
<body>
    <h1>PCB Stackup - Fabrication Drawing</h1>
    <h2>Board Summary</h2>
    <div class="summary">
        {% for item in summary %}
        <div class="summary-item">
            <span class="summary-label">{{ item.label }}</span>
            <span class="summary-value">
                <span>{{ item.value }}</span>
                {% if item.color %}<span class="color-swatch" style="background: {{ item.color }};"></span>{% endif %}
            </span>
        </div>
        {% endfor %}
    </div>
    <h2>Detailed Stackup</h2>
    <table>
        <thead>
            <tr>
                <th class="number">#</th>
                <th>Layer Name</th>
                <th>Material</th>
                <th class="text-right thickness">Thickness (mm)</th>
                <th class="text-right dk">Dk</th>
                <th class="text-right loss">Loss Tangent</th>
            </tr>
        </thead>
        <tbody>
        {% for layer in layers %}
        <tr class="{{ layer.class }}">
            <td class="text-center">{{ layer.index }}</td>
            <td>{% if layer.is_copper %}<strong>{{ layer.name }}</strong>{% else %}{{ layer.name }}{% endif %}</td>
            <td>{{ layer.material }}</td>
            <td class="text-right">{{ layer.thickness }}</td>
            <td class="text-right">{{ layer.dk }}</td>
            <td class="text-right">{{ layer.loss }}</td>
        </tr>
        {% endfor %}
        </tbody>
    </table>
</body>
</html>"#;

#[derive(Serialize)]
struct SummaryItem {
    label: String,
    value: String,
    color: Option<String>,
}

#[derive(Serialize)]
struct LayerRow {
    index: usize,
    name: String,
    material: String,
    thickness: String,
    dk: String,
    loss: String,
    class: String,
    is_copper: bool,
}

fn color_to_hex(color: &str) -> &str {
    match color.to_lowercase().as_str() {
        "black" => "#000000",
        "white" => "#FFFFFF",
        "red" => "#CC0000",
        "blue" => "#0066CC",
        "green" => "#006600",
        "yellow" => "#FFDD00",
        "purple" => "#6600CC",
        "matte black" => "#1a1a1a",
        _ => "#808080",
    }
}

fn finish_to_hex(finish: &CopperFinish) -> &str {
    match finish {
        CopperFinish::Enig => "#D4AF37",
        CopperFinish::HalSnpb => "#C0C0C0",
        CopperFinish::HalLeadFree => "#E8E8E8",
    }
}

fn finish_name(finish: &CopperFinish) -> &str {
    match finish {
        CopperFinish::Enig => "ENIG",
        CopperFinish::HalSnpb => "HAL SnPb",
        CopperFinish::HalLeadFree => "HAL Lead-Free",
    }
}

pub fn generate_html(board_config: &BoardConfig) -> String {
    let Some(stackup) = &board_config.stackup else {
        return r#"<!DOCTYPE html><html><head><meta charset="UTF-8"><title>No Stackup Data</title></head><body><h1>No stackup data available</h1></body></html>"#.to_string();
    };

    let Some(layers) = &stackup.layers else {
        return r#"<!DOCTYPE html><html><head><meta charset="UTF-8"><title>No Stackup Data</title></head><body><h1>No stackup data available</h1></body></html>"#.to_string();
    };

    // Calculate copper metrics
    let copper_thicknesses: Vec<f64> = layers
        .iter()
        .filter_map(|l| {
            if let Layer::Copper { thickness, .. } = l {
                Some(*thickness)
            } else {
                None
            }
        })
        .collect();

    let copper_count = copper_thicknesses.len();
    assert!(
        copper_count >= 2,
        "Board must have at least 2 copper layers (has {})",
        copper_count
    );

    let total_thickness = stackup
        .thickness
        .unwrap_or_else(|| layers.iter().map(|l| l.thickness()).sum());

    let outer_oz = copper_thicknesses[0] / MM_PER_OZ;

    // Check if we have inner layers and if they're consistent
    let inner_oz = if copper_count > 2 {
        let inner_layers = &copper_thicknesses[1..copper_count - 1];
        if inner_layers.len() >= 2 {
            let first = inner_layers[0];
            let consistent = inner_layers.iter().all(|&t| (t - first).abs() < 0.001);
            if consistent {
                Some(first / MM_PER_OZ)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Build summary items
    let mut summary = vec![
        SummaryItem {
            label: "Copper Layers:".to_string(),
            value: copper_count.to_string(),
            color: None,
        },
        SummaryItem {
            label: "Board Thickness:".to_string(),
            value: format!(
                "{:.2} mm ({:.0} mil)",
                total_thickness,
                total_thickness * 39.3701
            ),
            color: None,
        },
        SummaryItem {
            label: "Outer Copper:".to_string(),
            value: format!("{:.1} oz", outer_oz),
            color: None,
        },
    ];

    if let Some(oz) = inner_oz {
        summary.push(SummaryItem {
            label: "Inner Copper:".to_string(),
            value: format!("{:.1} oz", oz),
            color: None,
        });
    }

    if let Some(finish) = &stackup.copper_finish {
        summary.push(SummaryItem {
            label: "Surface Finish:".to_string(),
            value: finish_name(finish).to_string(),
            color: Some(finish_to_hex(finish).to_string()),
        });
    }
    if let Some(mask) = &stackup.solder_mask_color {
        summary.push(SummaryItem {
            label: "Soldermask:".to_string(),
            value: mask.clone(),
            color: Some(color_to_hex(mask).to_string()),
        });
    }
    if let Some(silk) = &stackup.silk_screen_color {
        summary.push(SummaryItem {
            label: "Silkscreen:".to_string(),
            value: silk.clone(),
            color: Some(color_to_hex(silk).to_string()),
        });
    }

    // Build layer rows
    let mut layer_rows = Vec::new();
    let mut copper_idx = 1;

    for (i, layer) in layers.iter().enumerate() {
        let row = match layer {
            Layer::Copper { thickness, .. } => {
                let name = if copper_idx == 1 {
                    "Top Layer".to_string()
                } else if copper_idx == copper_count {
                    "Bottom Layer".to_string()
                } else {
                    format!("Inner {}", copper_idx - 1)
                };
                copper_idx += 1;

                LayerRow {
                    index: i + 1,
                    name,
                    material: "Copper".to_string(),
                    thickness: format!("{:.4}", thickness),
                    dk: "—".to_string(),
                    loss: "—".to_string(),
                    class: "layer-copper".to_string(),
                    is_copper: true,
                }
            }
            Layer::Dielectric {
                thickness,
                material,
                form,
            } => {
                let class = if *form == DielectricForm::Core {
                    "layer-core"
                } else {
                    "layer-prepreg"
                };
                let form_name = if *form == DielectricForm::Core {
                    "Core"
                } else {
                    "Prepreg"
                };

                let dk = stackup
                    .materials
                    .as_ref()
                    .and_then(|mats| mats.iter().find(|m| m.name.as_ref() == Some(material)))
                    .and_then(|m| m.relative_permittivity)
                    .map(|v| format!("{:.2}", v))
                    .unwrap_or_else(|| "—".to_string());

                let loss = stackup
                    .materials
                    .as_ref()
                    .and_then(|mats| mats.iter().find(|m| m.name.as_ref() == Some(material)))
                    .and_then(|m| m.loss_tangent)
                    .map(|v| format!("{:.4}", v))
                    .unwrap_or_else(|| "—".to_string());

                LayerRow {
                    index: i + 1,
                    name: form_name.to_string(),
                    material: material.clone(),
                    thickness: format!("{:.4}", thickness),
                    dk,
                    loss,
                    class: class.to_string(),
                    is_copper: false,
                }
            }
        };
        layer_rows.push(row);
    }

    // Render template
    let mut env = Environment::new();
    env.add_template("fab_drawing", TEMPLATE).unwrap();
    let tmpl = env.get_template("fab_drawing").unwrap();

    tmpl.render(context! {
        summary => summary,
        layers => layer_rows,
    })
    .unwrap_or_else(|e| format!("Template error: {}", e))
}
