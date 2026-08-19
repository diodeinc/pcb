//! SVG + HTML report for the POC.

use crate::pattern::Pattern;
use crate::route::{RouteResult, TwoPinNet};
use crate::score::Score;
use crate::types::{Assign, Problem};

fn kind_color(k: crate::types::Kind) -> &'static str {
    match k {
        crate::types::Kind::UsbHs => "#c026d3",
        crate::types::Kind::Vtarget => "#dc2626",
        crate::types::Kind::Vusb => "#ea580c",
        crate::types::Kind::Gnd => "#525252",
        crate::types::Kind::Ls => "#2563eb",
    }
}

#[allow(clippy::too_many_arguments)]
pub fn svg_panel(
    sheet_w: f64,
    sheet_h: f64,
    problem: &Problem,
    pattern: &Pattern,
    nets: &[TwoPinNet],
    route: &RouteResult,
    boards: &[([f64; 2], f64, f64)],
    title: &str,
) -> String {
    let sx = 4.0;
    let mut parts = Vec::new();
    parts.push(format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="-10 -24 {} {}" width="{:.0}" height="{:.0}">"#,
        sheet_w * sx + 20.0,
        sheet_h * sx + 40.0,
        sheet_w * sx + 20.0,
        sheet_h * sx + 40.0
    ));
    parts.push(format!(
        "<rect x=\"0\" y=\"0\" width=\"{w}\" height=\"{h}\" fill=\"#fafafa\" stroke=\"#111\"/>",
        w = sheet_w * sx,
        h = sheet_h * sx
    ));
    // The A7 mate region in the sheet's folded orientation.
    let (mw, mh) = crate::pattern::mate_dims(sheet_w, sheet_h);
    parts.push(format!(
        "<rect x=\"0\" y=\"0\" width=\"{w}\" height=\"{h}\" fill=\"none\" stroke=\"#16a34a\" stroke-dasharray=\"4 3\"/>",
        w = mw * sx,
        h = mh * sx
    ));
    // Board outlines, so the assembly panel is legible.
    for (origin, bw, bh) in boards {
        parts.push(format!(
            "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" fill=\"none\" stroke=\"#a8a29e\" stroke-width=\"0.8\"/>",
            origin[0] * sx,
            origin[1] * sx,
            bw * sx,
            bh * sx
        ));
    }
    parts.push(format!(
        r#"<text x="4" y="-8" font-family="ui-sans-serif,system-ui" font-size="12">{}</text>"#,
        esc(title)
    ));
    let routed: std::collections::HashSet<_> = route.traces.iter().map(|t| t.contact).collect();
    let poured: std::collections::HashSet<_> = route.poured.iter().copied().collect();
    for n in nets {
        if routed.contains(&n.contact) || poured.contains(&n.contact) {
            continue;
        }
        parts.push(format!(
            "<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" stroke=\"#ef4444\" stroke-width=\"0.6\" stroke-dasharray=\"2 2\" opacity=\"0.45\"/>",
            n.src[0] * sx,
            n.src[1] * sx,
            n.dst[0] * sx,
            n.dst[1] * sx
        ));
    }
    for t in &route.traces {
        // Split on layer changes so top (layer 1) can be dashed.
        let mut seg: Vec<(f64, f64)> = Vec::new();
        let mut seg_z = t.layer_of.first().copied().unwrap_or(0);
        let flush = |seg: &[(f64, f64)], z: u8, parts: &mut Vec<String>| {
            if seg.len() < 2 {
                return;
            }
            let pts: String = seg
                .iter()
                .map(|(x, y)| format!("{x:.1},{y:.1}"))
                .collect::<Vec<_>>()
                .join(" ");
            let dash = if z == 1 {
                " stroke-dasharray=\"3 2\""
            } else {
                ""
            };
            let op = if z == 1 { "0.75" } else { "0.92" };
            let sw = (t.width * sx).max(0.8);
            parts.push(format!(
                "<polyline points=\"{pts}\" fill=\"none\" stroke=\"{}\" stroke-width=\"{sw:.2}\" stroke-linejoin=\"round\" stroke-linecap=\"round\" opacity=\"{op}\"{dash}/>",
                kind_color(t.kind)
            ));
        };
        for (i, p) in t.points.iter().enumerate() {
            let z = t.layer_of.get(i).copied().unwrap_or(0);
            if z != seg_z && !seg.is_empty() {
                flush(&seg, seg_z, &mut parts);
                seg.clear();
            }
            seg_z = z;
            seg.push((p[0] * sx, p[1] * sx));
        }
        flush(&seg, seg_z, &mut parts);
        for v in &t.vias {
            parts.push(format!(
                "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"3.4\" height=\"3.4\" fill=\"#111\" stroke=\"#fbbf24\" stroke-width=\"0.8\"/>",
                v[0] * sx - 1.7,
                v[1] * sx - 1.7
            ));
        }
        if let Some(v) = t.term_via {
            // Terminal via-in-pad: a small annulus at the pad.
            parts.push(format!(
                "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"1.9\" fill=\"#fff\" stroke=\"#111\" stroke-width=\"1.0\"/>",
                v[0] * sx,
                v[1] * sx
            ));
        }
    }
    for c in problem.contacts.values() {
        parts.push(format!(
            "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"1.8\" fill=\"#111\"/>",
            c.xy[0] * sx,
            c.xy[1] * sx
        ));
    }
    // Pogo-array connector bodies (2×3 kits, 2×4 LS arrays) behind the lands.
    let kind_of_pin: std::collections::HashMap<_, _> = pattern
        .slots
        .iter()
        .flat_map(|s| s.pins.iter().map(move |p| (*p, s.kind)))
        .collect();
    let pin_xy: std::collections::HashMap<_, _> =
        pattern.pins.iter().map(|p| (p.id, p.xy)).collect();
    for arr in &pattern.arrays {
        let (mut min, mut max) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for id in arr {
            let xy = pin_xy[id];
            for k in 0..2 {
                min[k] = min[k].min(xy[k]);
                max[k] = max[k].max(xy[k]);
            }
        }
        let body = 1.6; // connector body overhang beyond pin centers (mm)
        let is_kit = arr
            .iter()
            .any(|id| kind_of_pin.get(id) == Some(&crate::types::Kind::UsbHs));
        let stroke = if is_kit { "#b45309" } else { "#0f766e" };
        parts.push(format!(
            "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"3\" fill=\"{stroke}\" fill-opacity=\"0.10\" stroke=\"{stroke}\" stroke-width=\"1.0\"/>",
            (min[0] - body) * sx,
            (min[1] - body) * sx,
            (max[0] - min[0] + 2.0 * body) * sx,
            (max[1] - min[1] + 2.0 * body) * sx,
        ));
    }
    for p in &pattern.pins {
        parts.push(format!(
            "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"3.2\" height=\"3.2\" fill=\"#0f766e\"/>",
            p.xy[0] * sx - 1.6,
            p.xy[1] * sx - 1.6
        ));
    }
    parts.push("</svg>".into());
    parts.join("\n")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Debug, Clone)]
pub struct CaseRow {
    pub board: String,
    pub sheet: String,
    pub strategy: String,
    pub score: Score,
    pub svg: String,
    pub note: String,
}

pub fn html_report(rows: &[CaseRow], ranking: &[(String, f64)]) -> String {
    let mut html = String::from(
        r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>Interposer POC</title>
<style>
body{font:14px/1.45 ui-sans-serif,system-ui;margin:24px;color:#111}
table{border-collapse:collapse;margin:12px 0 28px}
td,th{border:1px solid #ddd;padding:6px 8px;text-align:right}
th:first-child,td:first-child{text-align:left}
svg{max-width:100%;height:auto;border:1px solid #e5e5e5;margin:8px 0 24px;background:#fff}
h1{font-size:22px} h2{font-size:18px;margin-top:28px}
.note{color:#555}
</style></head><body>
<h1>Pogo-pin interposer POC</h1>
<p>PTH pogos, router <b>R5</b>: octilinear A* + gridless smoothing, true USB
diff-pair ribbons (length-matched), 0.9&nbsp;mm 2&nbsp;A power, self-DRC
(solid = bottom, dashed = top, yellow squares = vias). <b>S10</b> is the
ring constellation on the funnel-facing A7 edges. Maze totals exclude poured
GND. Success = fully escaped boards.</p>
"#,
    );
    // Dashboard
    let live: Vec<&CaseRow> = rows.iter().filter(|r| r.score.boards_total > 0).collect();
    let mut b_ok = 0usize;
    let mut b_n = 0usize;
    let mut n_ok = 0usize;
    let mut n_n = 0usize;
    let mut usb_ok = 0usize;
    let mut usb_n = 0usize;
    let mut p_ok = 0usize;
    let mut p_n = 0usize;
    let mut ls_ok = 0usize;
    let mut ls_n = 0usize;
    for r in &live {
        b_ok += r.score.boards_complete;
        b_n += r.score.boards_total;
        n_ok += r.score.nets_routed;
        n_n += r.score.nets_total;
        usb_ok += r.score.usb.routed;
        usb_n += r.score.usb.total;
        p_ok += r.score.power.routed;
        p_n += r.score.power.total;
        ls_ok += r.score.ls.routed;
        ls_n += r.score.ls.total;
    }
    let pct = |a, b| {
        if b == 0 {
            0.0
        } else {
            100.0 * a as f64 / b as f64
        }
    };
    html.push_str(&format!(
        "<h2>Overall success (all evaluated cases)</h2>\
         <table><tr><th>boards</th><th>maze nets</th><th>USB</th><th>power</th><th>LS</th></tr>\
         <tr><td>{:.0}% ({b_ok}/{b_n})</td><td>{:.0}% ({n_ok}/{n_n})</td>\
         <td>{:.0}% ({usb_ok}/{usb_n})</td><td>{:.0}% ({p_ok}/{p_n})</td>\
         <td>{:.0}% ({ls_ok}/{ls_n})</td></tr></table>",
        pct(b_ok, b_n),
        pct(n_ok, n_n),
        pct(usb_ok, usb_n),
        pct(p_ok, p_n),
        pct(ls_ok, ls_n)
    ));
    // Per-strategy board success (the number that matters).
    html.push_str("<h2>Board success by strategy</h2><table><tr><th>strategy</th><th>boards</th><th>maze</th><th>USB</th><th>power</th><th>LS</th></tr>");
    let mut by = std::collections::BTreeMap::<String, [usize; 10]>::new();
    for r in &live {
        let e = by.entry(r.strategy.clone()).or_insert([0; 10]);
        e[0] += r.score.boards_complete;
        e[1] += r.score.boards_total;
        e[2] += r.score.nets_routed;
        e[3] += r.score.nets_total;
        e[4] += r.score.usb.routed;
        e[5] += r.score.usb.total;
        e[6] += r.score.power.routed;
        e[7] += r.score.power.total;
        e[8] += r.score.ls.routed;
        e[9] += r.score.ls.total;
    }
    let mut rows_s: Vec<_> = by.into_iter().collect();
    rows_s.sort_by(|a, b| {
        let pa = if a.1[1] == 0 {
            0.0
        } else {
            a.1[0] as f64 / a.1[1] as f64
        };
        let pb = if b.1[1] == 0 {
            0.0
        } else {
            b.1[0] as f64 / b.1[1] as f64
        };
        pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
    });
    for (k, e) in rows_s {
        html.push_str(&format!(
            "<tr><td>{}</td><td>{:.0}% ({}/{})</td><td>{:.0}% ({}/{})</td><td>{:.0}% ({}/{})</td><td>{:.0}% ({}/{})</td><td>{:.0}% ({}/{})</td></tr>",
            esc(&k),
            pct(e[0], e[1]), e[0], e[1],
            pct(e[2], e[3]), e[2], e[3],
            pct(e[4], e[5]), e[4], e[5],
            pct(e[6], e[7]), e[6], e[7],
            pct(e[8], e[9]), e[8], e[9]
        ));
    }
    html.push_str("</table>");
    html.push_str(
        "<h2>Strategy ranking (worst-panel quality Q, lower is better)</h2><table><tr><th>strategy</th><th>Q</th></tr>",
    );
    for (k, q) in ranking {
        html.push_str(&format!("<tr><td>{}</td><td>{:.1}</td></tr>", esc(k), q));
    }
    html.push_str("</table><h2>Best board coverage by panel</h2><table><tr><th>board</th><th>sheet</th><th>best</th><th>boards</th><th>maze</th><th>USB</th><th>power</th><th>LS</th><th>GND pour</th></tr>");
    let mut best_cov: std::collections::BTreeMap<(String, String), &CaseRow> =
        std::collections::BTreeMap::new();
    for r in rows {
        if r.score.nets_total == 0 {
            continue;
        }
        let key = (r.board.clone(), r.sheet.clone());
        best_cov
            .entry(key)
            .and_modify(|cur| {
                let better = r.score.boards_complete > cur.score.boards_complete
                    || (r.score.boards_complete == cur.score.boards_complete
                        && r.score.nets_routed > cur.score.nets_routed);
                if better {
                    *cur = r;
                }
            })
            .or_insert(r);
    }
    for ((board, sheet), r) in &best_cov {
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}/{}</td><td>{}/{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(board),
            esc(sheet),
            esc(&r.strategy),
            r.score.boards_complete,
            r.score.boards_total,
            r.score.nets_routed,
            r.score.nets_total,
            r.score.usb.fmt_maze(),
            r.score.power.fmt_maze(),
            r.score.ls.fmt_maze(),
            r.score.gnd.fmt_gnd()
        ));
    }
    html.push_str("</table><h2>Cases</h2><table><tr>");
    for h in [
        "board", "sheet", "S", "hall", "maze", "USB", "power", "LS", "GND", "boards", "L", "vias",
        "bends", "90°", "detour", "UΔ", "loose", "DRC", "gap",
    ] {
        html.push_str(&format!("<th>{h}</th>"));
    }
    html.push_str("</tr>");
    for r in rows {
        let s = &r.score;
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}/{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}/{}</td><td>{:.0}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.2}</td><td>{:.2}</td><td>{}</td><td>{}</td><td>{:.2}</td></tr>",
            esc(&r.board),
            esc(&r.sheet),
            esc(&r.strategy),
            if s.hall_ok { "ok" } else { "fail" },
            s.nets_routed,
            s.nets_total,
            s.usb.fmt_maze(),
            s.power.fmt_maze(),
            s.ls.fmt_maze(),
            s.gnd.fmt_gnd(),
            s.boards_complete,
            s.boards_total,
            s.l_routed,
            s.v_vias,
            s.bends,
            s.bends90,
            s.detour,
            s.u_delta_routed,
            s.loose_pairs,
            s.drc_violations,
            s.min_gap_mm
        ));
    }
    html.push_str("</table>");
    for r in rows {
        html.push_str(&format!(
            "<h2>{} · {} · {}</h2><p class=\"note\">{}</p>{}",
            esc(&r.board),
            esc(&r.sheet),
            esc(&r.strategy),
            esc(&r.note),
            r.svg
        ));
    }
    html.push_str("</body></html>");
    let _ = (Assign::default(), Problem::default());
    html
}
