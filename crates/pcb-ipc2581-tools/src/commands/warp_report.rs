//! A self-contained field report for [`crate::warp`].
//!
//! Bow and twist are the last step of the chain and they discard everything
//! before them. Per-layer copper, the moment field and the deflection surface
//! are all fields over the panel, all computed on the way to those two numbers,
//! so the report renders the fields and leaves the scalars as a summary.
//!
//! Laid out as a technical report rather than a dashboard: numbered sections
//! and figures, units in every heading, tabular figures throughout, rules
//! instead of cards, and no colour anywhere it does not carry data. Signed
//! fields get a diverging scale with a neutral midpoint pinned at zero and a
//! symmetric range so the sign reads; unsigned magnitude gets one hue, light to
//! dark, on a scale shared by every layer so they compare directly.

use std::fmt::Write;

use pcb_ir::geom::warp::PanelField;

use crate::warp::WarpAnalysis;

/// Bow beyond which IPC-6012 rejects a board carrying surface-mount parts.
const SURFACE_MOUNT_LIMIT_PERCENT: f64 = 0.75;
/// Mirrored-layer copper mismatch fabricators advise staying under.
const MISMATCH_GUIDANCE: f64 = 0.10;

/// Cool and warm poles for signed fields, either side of a neutral midpoint.
const COOL: [f64; 3] = [33.0, 92.0, 176.0];
const NEUTRAL: [f64; 3] = [242.0, 240.0, 235.0];
const WARM: [f64; 3] = [178.0, 52.0, 30.0];
/// Light-to-dark single hue for unsigned magnitude.
const MAGNITUDE: ([f64; 3], [f64; 3]) = ([250.0, 249.0, 246.0], [38.0, 42.0, 48.0]);

pub fn render(analysis: &WarpAnalysis) -> String {
    let mut html = String::from(HEAD);
    masthead(&mut html, analysis);
    results(&mut html, analysis);
    stack_table(&mut html, analysis);
    mode_table(&mut html, analysis);
    field_figures(&mut html, analysis);
    layer_figures(&mut html, analysis);
    html.push_str(FOOT);
    html
}

fn masthead(html: &mut String, analysis: &WarpAnalysis) {
    let stack = &analysis.stack;
    let _ = write!(
        html,
        r#"<header>
<h1>Panel warp analysis</h1>
<p class="standfirst">Bow and twist estimated from the through-stack copper distribution,
reported after the method of IPC-TM-650 2.4.22.</p>
<dl class="params">
<div><dt>Panel</dt><dd>{:.1} &times; {:.1} mm</dd></div>
<div><dt>Stack</dt><dd>{:.3} mm &middot; {} Cu</dd></div>
<div><dt>Rigidity</dt><dd>{:.2} GPa&#183;mm&#179;</dd></div>
<div><dt>&Delta;T</dt><dd>{:.0} K</dd></div>
<div><dt>Samples</dt><dd>{}</dd></div>
</dl>
<p class="caveat"><b>Modelled, not measured.</b> The absolute figure rests on textbook material
constants and an assumed drop from where the laminate stops relaxing. Comparison between
panelisations of one stackup is firmer by a wide margin, since that constant multiplies both
and cancels.</p>
</header>"#,
        analysis.bounds.width(),
        analysis.bounds.height(),
        stack.total_thickness_mm(),
        analysis.layers.len(),
        stack.flexural_rigidity_gpa_mm3(),
        analysis.temperature_drop_k,
        analysis.samples.len(),
    );
}

fn results(html: &mut String, analysis: &WarpAnalysis) {
    let warp = &analysis.warp;
    let verdict = if warp.bow_percent <= SURFACE_MOUNT_LIMIT_PERCENT {
        format!(
            r#"<span class="pass">PASS</span> {:.0}&times; margin"#,
            SURFACE_MOUNT_LIMIT_PERCENT / warp.bow_percent.max(f64::MIN_POSITIVE)
        )
    } else {
        format!(
            r#"<span class="fail">OVER</span> {:.1}&times; limit"#,
            warp.bow_percent / SURFACE_MOUNT_LIMIT_PERCENT
        )
    };
    let _ = write!(
        html,
        r#"<section><h2>1&emsp;Result</h2>
<table class="numeric"><thead><tr>
<th>Quantity</th><th>mm</th><th>%</th><th>Limit %</th><th>Assessment</th>
</tr></thead><tbody>
<tr><td>Bow</td><td>{:.3}</td><td>{:.3}</td><td>{SURFACE_MOUNT_LIMIT_PERCENT:.2}</td>
<td>{verdict}</td></tr>
<tr><td>Twist</td><td>{:.3}</td><td>{:.3}</td><td>{SURFACE_MOUNT_LIMIT_PERCENT:.2}</td>
<td class="muted">weakly determined, see &sect;3</td></tr>
</tbody></table></section>"#,
        warp.bow_mm, warp.bow_percent, warp.twist_mm, warp.twist_percent,
    );
}

/// The stack, paired about the neutral axis. Mirrored layers carry opposite
/// arms, so their coverage difference is what survives into the moment — which
/// is the quantity fabricators put a rule of thumb on.
fn stack_table(html: &mut String, analysis: &WarpAnalysis) {
    let conductors = analysis.stack.conductor_weights();
    let mut rows = String::new();
    for (index, (layer, conductor)) in analysis.layers.iter().zip(&conductors).enumerate() {
        let mirror = conductors.len() - 1 - index;
        let pair = match (index < mirror).then(|| layer.mean - analysis.layers[mirror].mean) {
            None => String::new(),
            Some(difference) if difference.abs() <= MISMATCH_GUIDANCE => {
                format!(r#"<span class="pass">{difference:+.3}</span>"#)
            }
            Some(difference) => format!(r#"<span class="fail">{difference:+.3}</span>"#),
        };
        let _ = write!(
            rows,
            r#"<tr><td>{}</td><td>{:+.4}</td><td>{:.3}</td>
<td class="bar"><span style="width:{:.1}%"></span></td><td>{:+.5}</td><td>{pair}</td></tr>"#,
            layer.layer_name,
            conductor.lever_arm_mm,
            layer.mean,
            100.0 * layer.mean,
            conductor.moment_arm_mm2 * layer.mean,
        );
    }
    let _ = write!(
        html,
        r#"<section><h2>2&emsp;Through the stack</h2>
<p class="blurb">Lever arms run from the stiffness-weighted neutral axis, so mirrored layers
carry equal and opposite values and equal copper on them cancels. The last column is each pair's
coverage difference, which is what survives into the moment; fabricators advise keeping it under
{:.0}&nbsp;%.</p>
<table class="numeric"><thead><tr>
<th>Layer</th><th>Arm z mm</th><th>Cu</th><th>Coverage</th><th>Moment t&middot;z&middot;&rho; mm&#178;</th>
<th>Pair &Delta;</th>
</tr></thead><tbody>{rows}</tbody></table></section>"#,
        100.0 * MISMATCH_GUIDANCE,
    );
}

fn mode_table(html: &mut String, analysis: &WarpAnalysis) {
    let total = analysis
        .warp
        .modes
        .iter()
        .map(|mode| mode.deflection_mm)
        .sum::<f64>()
        .max(f64::MIN_POSITIVE);
    let mut rows = String::new();
    for mode in &analysis.warp.modes {
        let share = 100.0 * mode.deflection_mm / total;
        let _ = write!(
            rows,
            r#"<tr><td>{}</td><td>{:+.5}</td><td>{:.4}</td>
<td class="bar"><span style="width:{share:.1}%"></span></td><td>{share:.1}</td></tr>"#,
            mode.mode.name(),
            mode.amplitude,
            mode.deflection_mm,
        );
    }
    let _ = write!(
        html,
        r#"<section><h2>3&emsp;Deflection by shape</h2>
<p class="blurb">The moment field resolved into low-order shapes. Deflection grows with the
square of wavelength, so the broadest shapes dominate and copper detail finer than the panel
contributes almost nothing. Twist reads off the saddle term, which the second integral of
curvature fixes only up to a harmonic function; it is the least trustworthy number here, and a
cross-ply layup gives copper no way to drive twist in any case.</p>
<table class="numeric"><thead><tr>
<th>Shape</th><th>Amplitude mm&#178;</th><th>Deflection mm</th><th>Share</th><th>%</th>
</tr></thead><tbody>{rows}</tbody></table></section>"#
    );
}

fn field_figures(html: &mut String, analysis: &WarpAnalysis) {
    let _ = write!(
        html,
        r#"<section><h2>4&emsp;Fields</h2>
<div class="figures">
<figure><figcaption><b>Fig 1</b>&emsp;Predicted panel shape, levelled onto the corners.
Warm high, cool low.</figcaption>{}</figure>
<figure><figcaption><b>Fig 2</b>&emsp;Copper moment about the neutral axis. Warm is copper
weighted above it, cool below, neutral is balanced.</figcaption>{}</figure>
</div></section>"#,
        diverging(
            &analysis.warp.deflection,
            analysis,
            "height off the corner plane, mm"
        ),
        diverging(
            &analysis.moment,
            analysis,
            "moment about the neutral axis, mm\u{b2}"
        ),
    );
}

fn layer_figures(html: &mut String, analysis: &WarpAnalysis) {
    let ceiling = analysis
        .layers
        .iter()
        .flat_map(|layer| layer.coverage.iter())
        .fold(f64::MIN_POSITIVE, |peak, value| peak.max(*value));
    let mut plates = String::new();
    for layer in &analysis.layers {
        let map = sequential(&layer.coverage, analysis, ceiling);
        let _ = write!(
            plates,
            r#"<figure><figcaption><b>{}</b><span>{:.3}</span></figcaption>{map}</figure>"#,
            layer.layer_name, layer.mean,
        );
    }
    let _ = write!(
        html,
        r#"<section><h2>5&emsp;Copper by layer</h2>
<p class="blurb">One plate per copper layer, top of the stack first, drawn in panel
coordinates. Each cell is one {:.2}&nbsp;mm sample and its darkness is the fraction of that cell
covered by copper, on a scale shared by every plate so the plates compare directly; the number
beside each name is that layer's mean coverage. What drives warp is the difference between a
layer and its mirror at the same place, weighted by the lever arms in &sect;2: two plates that
look alike cancel, and a patch dark on one face where the opposite face is light is what bends
the panel there. Read against Fig&nbsp;2, which is exactly that difference summed through the
stack.</p>
<div class="plates">{plates}</div>
{}</section>"#,
        analysis.sample_pitch_mm,
        legend(
            "fraction of the cell covered by copper",
            &[
                (magnitude_colour(0.0), "0 bare".to_string()),
                (magnitude_colour(0.5), format!("{:.2}", ceiling / 2.0)),
                (magnitude_colour(1.0), format!("{ceiling:.2} solid")),
            ]
        ),
    );
}

/// Signed field: two hues either side of a neutral midpoint pinned at zero,
/// with a symmetric range so the midpoint really is zero.
fn diverging(field: &PanelField, analysis: &WarpAnalysis, caption: &str) -> String {
    let extent = field
        .values
        .iter()
        .fold(f64::MIN_POSITIVE, |peak, value| peak.max(value.abs()));
    let cells = field
        .values
        .iter()
        .map(|value| signed_colour(value / extent))
        .collect::<Vec<_>>();
    format!(
        "{}{}",
        map(analysis, &cells),
        legend(
            caption,
            &[
                (signed_colour(-1.0), format!("{:+.4}", -extent)),
                (signed_colour(0.0), "0".to_string()),
                (signed_colour(1.0), format!("{extent:+.4}")),
            ]
        )
    )
}

fn sequential(values: &[f64], analysis: &WarpAnalysis, ceiling: f64) -> String {
    let cells = values
        .iter()
        .map(|value| magnitude_colour(value / ceiling))
        .collect::<Vec<_>>();
    map(analysis, &cells)
}

fn signed_colour(fraction: f64) -> String {
    let fraction = fraction.clamp(-1.0, 1.0);
    let pole = if fraction >= 0.0 { WARM } else { COOL };
    blend(NEUTRAL, pole, fraction.abs())
}

fn magnitude_colour(fraction: f64) -> String {
    blend(MAGNITUDE.0, MAGNITUDE.1, fraction.clamp(0.0, 1.0))
}

fn blend(from: [f64; 3], to: [f64; 3], amount: f64) -> String {
    let channel = |index: usize| (from[index] + amount * (to[index] - from[index])) as u8;
    format!("#{:02x}{:02x}{:02x}", channel(0), channel(1), channel(2))
}

/// One cell per sample, in panel coordinates.
fn map(analysis: &WarpAnalysis, cells: &[String]) -> String {
    let bounds = analysis.bounds;
    let columns = analysis
        .samples
        .iter()
        .filter(|point| (point.y - analysis.samples[0].y).abs() < 1e-9)
        .count()
        .max(1);
    let rows = analysis.samples.len().div_ceil(columns);
    let width = bounds.width() / columns as f64;
    let height = bounds.height() / rows as f64;
    let mut svg = format!(
        r#"<svg viewBox="{} {} {} {}" preserveAspectRatio="xMidYMid meet">"#,
        bounds.min.x,
        bounds.min.y,
        bounds.width(),
        bounds.height()
    );
    for (point, colour) in analysis.samples.iter().zip(cells) {
        // Panel y runs up, SVG y runs down; mirror about the panel centre.
        let _ = write!(
            svg,
            r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" fill="{colour}"/>"#,
            point.x - width / 2.0,
            bounds.min.y + bounds.max.y - point.y - height / 2.0,
            width,
            height,
        );
    }
    svg.push_str("</svg>");
    svg
}

fn legend(caption: &str, stops: &[(String, String)]) -> String {
    let swatches = stops
        .iter()
        .map(|(colour, label)| {
            format!(r#"<span><i style="background:{colour}"></i>{label}</span>"#)
        })
        .collect::<String>();
    format!(r#"<div class="legend"><b>{caption}</b>{swatches}</div>"#)
}

const HEAD: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Panel warp analysis</title><style>
:root {
  --ink:#16181d; --muted:#5b6068; --rule:#c9c6c0; --paper:#fbfaf7; --plate:#ffffff;
  --mono:ui-monospace,"SFMono-Regular",Menlo,Consolas,monospace;
}
@media (prefers-color-scheme:dark) {
  :root { --ink:#e7e5e0; --muted:#989ca3; --rule:#3a3d43; --paper:#131518; --plate:#0d0f12; }
}
* { box-sizing:border-box; }
body { margin:0 auto; padding:2.5rem clamp(1rem,4vw,3rem) 5rem; max-width:58rem;
  background:var(--paper); color:var(--ink);
  font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,sans-serif; }
h1 { font-size:1.3rem; font-weight:600; margin:0 0 .3rem; letter-spacing:-.01em; }
h2 { font-size:.78rem; font-weight:700; margin:0 0 .55rem; text-transform:uppercase;
  letter-spacing:.1em; padding-bottom:.28rem; border-bottom:1.5px solid var(--ink); }
header { border-bottom:3px double var(--ink); padding-bottom:1rem; margin-bottom:1.9rem; }
.standfirst { margin:0 0 .9rem; color:var(--muted); max-width:62ch; }
.params { display:grid; grid-template-columns:repeat(auto-fit,minmax(10rem,1fr));
  gap:0 1.6rem; margin:0 0 .9rem; }
.params div { display:flex; justify-content:space-between; gap:.8rem; padding:.18rem 0;
  border-bottom:1px dotted var(--rule); }
.params dt { color:var(--muted); font-size:.72rem; text-transform:uppercase;
  letter-spacing:.06em; padding-top:.12rem; }
.params dd { margin:0; font-family:var(--mono); font-variant-numeric:tabular-nums;
  font-size:.82rem; }
.caveat { margin:0; padding:.5rem .7rem; font-size:.84rem; color:var(--muted);
  border-left:2px solid var(--rule); max-width:76ch; }
section { margin:0 0 2.1rem; }
.blurb { margin:0 0 .75rem; color:var(--muted); max-width:76ch; font-size:.86rem; }
table.numeric { border-collapse:collapse; width:100%; font-family:var(--mono);
  font-variant-numeric:tabular-nums; font-size:.82rem; }
table.numeric th, table.numeric td { padding:.28rem .7rem; text-align:right;
  border-bottom:1px solid var(--rule); }
table.numeric th { font-size:.68rem; text-transform:uppercase; letter-spacing:.07em;
  color:var(--muted); font-weight:600; border-bottom:1.5px solid var(--ink);
  font-family:inherit; white-space:nowrap; }
table.numeric td:first-child, table.numeric th:first-child { text-align:left; }
table.numeric tr:last-child td { border-bottom:1.5px solid var(--ink); }
td.bar { width:20%; padding-right:0; }
td.bar span { display:block; height:.5rem; background:var(--ink); opacity:.7; }
.pass { color:#1a6b34; font-weight:700; } .fail { color:#a32b1c; font-weight:700; }
.muted { color:var(--muted); }
figure { margin:0 0 1.4rem; }
figcaption { font-size:.8rem; color:var(--muted); margin-bottom:.35rem; max-width:76ch; }
svg { width:100%; height:auto; display:block; background:var(--plate);
  border:1px solid var(--rule); }
.legend { display:flex; flex-wrap:wrap; align-items:baseline; gap:.35rem 1.2rem;
  margin-top:.4rem; font-family:var(--mono); font-size:.74rem; color:var(--muted);
  font-variant-numeric:tabular-nums; }
.legend b { font-family:inherit; font-weight:400; margin-right:.2rem; }
.legend b::after { content:":"; }
.legend i { display:inline-block; width:.8rem; height:.8rem; margin-right:.32rem;
  vertical-align:-1px; border:1px solid var(--rule); }
.figures { display:grid; grid-template-columns:repeat(2,1fr); gap:1.4rem; }
.figures figure { margin:0; }
.plates { display:grid; grid-template-columns:repeat(3,1fr); gap:1rem 1.2rem; }
.plates figure { margin:0; }
.plates figcaption { display:flex; justify-content:space-between; gap:.4rem;
  font-family:var(--mono); font-size:.74rem; margin-bottom:.28rem;
  border-bottom:1px solid var(--rule); padding-bottom:.18rem; }
.plates figcaption b { font-weight:600; color:var(--ink); }
@media (max-width:48rem) {
  .figures, .plates { grid-template-columns:1fr; }
}
</style></head><body>
"##;

const FOOT: &str = "</body></html>";
