//! End-to-end interposer POC driver: extract → pack → bundle → suppress →
//! Hall (fail-open board drop) → assign → route (R5) → score → report.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use pcb_interposer::extract::{board_bbox_from_kicad, extract_kicad_path, parse_zen_ict_map};
use pcb_interposer::instantiate::{self, Sheet};
use pcb_interposer::pattern::{PITCH_254, PatternKind, attach_pattern, generate_pattern_at};
use pcb_interposer::score::quality_score;
use pcb_interposer::types::{Assign, BoardId, Problem};
use pcb_interposer::viz::{self, CaseRow};
use pcb_interposer::{assign, bundle, hall, nets_from_assign, route_r5, score_g0, score_g1};

#[derive(Parser, Debug)]
#[command(name = "interposer-poc")]
struct Args {
    /// Directory of demo boards (each with a top-level .zen and layout/layout.kicad_pcb).
    #[arg(long)]
    corpus: PathBuf,
    /// Output directory for the HTML report and per-case SVGs.
    #[arg(long)]
    out: PathBuf,
    /// Directory of real board-array panels ({board}_{sheet}.xml from
    /// `pcbc ipc2581 board-array create`); when a panel exists for a case,
    /// its placements, tooling, and fiducials replace the synthetic pack.
    #[arg(long)]
    panels: Option<PathBuf>,
}

fn find_boards(root: &Path) -> Vec<(String, PathBuf, PathBuf)> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return out;
    };
    for e in entries.flatten() {
        let dir = e.path();
        if !dir.is_dir() {
            continue;
        }
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let zen = dir.join(format!("{name}.zen"));
        let pcb = dir.join("layout/layout.kicad_pcb");
        if zen.is_file() && pcb.is_file() {
            out.push((name, zen, pcb));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn run_one(
    board_name: &str,
    zen: &Path,
    pcb: &Path,
    sheet: Sheet,
    strategy: PatternKind,
    panels_dir: Option<&Path>,
    emit_dir: Option<&Path>,
) -> Result<CaseRow> {
    let zen_src = fs::read_to_string(zen)?;
    let ict_map = parse_zen_ict_map(&zen_src);
    let kicad_src = fs::read_to_string(pcb)?;
    let mut local = extract_kicad_path(pcb, BoardId(0), &ict_map)?;
    let (mut origin, mut bw, mut bh) = board_bbox_from_kicad(&kicad_src)
        .map(|(mn, mx)| {
            (
                mn,
                (mx[0] - mn[0]).abs().max(10.0),
                (mx[1] - mn[1]).abs().max(10.0),
            )
        })
        .unwrap_or(([0.0, 0.0], 40.0, 30.0));
    // Grow the cell so every TP sits inside it (injected pads may sit
    // slightly outside Edge.Cuts).
    if !local.is_empty() {
        let minx = local.iter().map(|c| c.xy[0]).fold(f64::INFINITY, f64::min);
        let miny = local.iter().map(|c| c.xy[1]).fold(f64::INFINITY, f64::min);
        let maxx = local
            .iter()
            .map(|c| c.xy[0])
            .fold(f64::NEG_INFINITY, f64::max);
        let maxy = local
            .iter()
            .map(|c| c.xy[1])
            .fold(f64::NEG_INFINITY, f64::max);
        let pad = 1.5;
        origin = [origin[0].min(minx - pad), origin[1].min(miny - pad)];
        let mx = (origin[0] + bw).max(maxx + pad);
        let my = (origin[1] + bh).max(maxy + pad);
        bw = (mx - origin[0]).max(10.0);
        bh = (my - origin[1]).max(10.0);
    }
    instantiate::localize(&mut local, origin);

    // Real generated panel when present; the synthetic pack otherwise.
    let panel_xml = panels_dir
        .map(|d| d.join(format!("{board_name}_{}.xml", sheet.name)))
        .filter(|p| p.is_file());
    let (places, sheet_w, sheet_h, inherited) = match &panel_xml {
        Some(p) => {
            let ap = pcb_interposer::arrayspec::parse_panel(p, origin)
                .with_context(|| format!("parse {}", p.display()))?;
            (ap.places, ap.sheet_w, ap.sheet_h, Some(ap.panel))
        }
        None => (instantiate::pack(sheet, bw, bh, 8), sheet.w, sheet.h, None),
    };
    let n_local = local.len();
    let n_boards = places.len();

    let label = strategy.name().to_string();
    if local.is_empty() {
        return Ok(CaseRow {
            board: board_name.into(),
            sheet: sheet.name.into(),
            strategy: label,
            score: Default::default(),
            svg: String::new(),
            note: format!(
                "no Ø 1 mm ict TestPoints on layout (zen map {})",
                ict_map.len()
            ),
        });
    }

    // SMT pogo pads (top) never collide with mate lands (bottom); a net's
    // single layer-change via is optional and sits in one of its own pads.
    // Real packings can exceed the constellation's capacity — keep the
    // largest board prefix whose demands still satisfy Hall; the rest of
    // the panel waits for another fixture insertion.
    let mut kept = n_boards;
    let (problem, pattern, hall_ok) = loop {
        // Spread the tested subset across the sheet: a clumped prefix puts
        // every net far from half the perimeter constellation.
        let pick: Vec<_> = (0..kept)
            .map(|i| places[i * n_boards / kept].clone())
            .collect();
        let contacts = instantiate::instantiate(&local, &pick);
        let mut problem: Problem = bundle(contacts)?;
        problem.panel = match &inherited {
            Some(spec) => {
                let mut spec = spec.clone();
                pcb_interposer::panel::add_a7_tile_tooling(&mut spec, sheet_w, sheet_h);
                spec
            }
            None => pcb_interposer::panel::panel_spec(sheet, &places, bw, bh),
        };
        let mut cents: BTreeMap<BoardId, (f64, f64, u32)> = BTreeMap::new();
        for c in problem.contacts.values() {
            let e = cents.entry(c.board).or_insert((0.0, 0.0, 0));
            e.0 += c.xy[0];
            e.1 += c.xy[1];
            e.2 += 1;
        }
        let centroids: Vec<[f64; 2]> = cents
            .values()
            .map(|(x, y, n)| [x / *n as f64, y / *n as f64])
            .collect();
        let mut pattern = generate_pattern_at(strategy, PITCH_254, &centroids);
        // The mate follows the ISO fold: rotated 90° on A6/A4-class sheets.
        pcb_interposer::pattern::orient_pattern(&mut pattern, sheet_w, sheet_h);
        attach_pattern(&mut problem, &pattern);
        let ok = hall(&problem).is_ok();
        if ok || kept == 1 {
            break (problem, pattern, ok);
        }
        kept -= 1;
    };
    let asg: Assign = if hall_ok {
        assign(&problem)
    } else {
        Assign::default()
    };
    let nets = if hall_ok {
        nets_from_assign(&problem, &asg)
    } else {
        Vec::new()
    };
    let route = if hall_ok {
        route_r5(sheet_w, sheet_h, &problem, &asg)
    } else {
        Default::default()
    };
    // Emit the committed strategy as a real KiCad board for external DRC.
    if let Some(dir) = emit_dir {
        fs::create_dir_all(dir)?;
        let stem = format!("{board_name}_{}", sheet.name);
        let pcb_path = dir.join(format!("{stem}.kicad_pcb"));
        fs::write(
            &pcb_path,
            pcb_interposer::emit::emit_kicad(sheet_w, sheet_h, &problem, &asg, &route),
        )?;
        fs::write(
            dir.join(format!("{stem}.kicad_pro")),
            pcb_interposer::emit::emit_project(),
        )?;
        fill_zones(&pcb_path);
    }
    let g0 = score_g0(&problem, &asg, &pattern, &nets);
    let mut score = score_g1(g0, &route, &nets);
    score.hall_ok = hall_ok;
    let board_rects: Vec<([f64; 2], f64, f64)> =
        places.iter().map(|p| (p.origin, bw, bh)).collect();
    let svg = viz::svg_panel(
        sheet_w,
        sheet_h,
        &problem,
        &pattern,
        &nets,
        &route,
        &board_rects,
        &format!(
            "{board_name} {} {label}  routed {}/{}  boards {}/{}",
            sheet.name,
            score.nets_routed,
            score.nets_total,
            score.boards_complete,
            score.boards_total
        ),
    );
    Ok(CaseRow {
        board: board_name.into(),
        sheet: sheet.name.into(),
        strategy: label,
        score,
        svg,
        note: format!(
            "local TPs={n_local}  {src} boards={n_boards} testable={kept}  zen ict={n_ict}  hall={h}",
            src = if panel_xml.is_some() { "panel" } else { "pack" },
            n_ict = ict_map.len(),
            h = if hall_ok { "ok" } else { "fail" },
        ),
    })
}

/// Fill the emitted board's zones in place with KiCad's own filler, so the
/// file opens with the pours already poured — and repair GND connectivity
/// with bridging vias where the pours split (see fill_zones.py). Skipped
/// when KiCad's bundled python isn't present.
fn fill_zones(pcb: &Path) {
    let py = Path::new(
        "/Applications/KiCad/KiCad.app/Contents/Frameworks/Python.framework/Versions/Current/bin/python3",
    );
    if !py.exists() {
        return;
    }
    let ok = std::process::Command::new(py)
        .arg("-c")
        .arg(include_str!("fill_zones.py"))
        .arg(pcb)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("warning: zone fill failed for {}", pcb.display());
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    fs::create_dir_all(&args.out)?;
    let boards = find_boards(&args.corpus);
    println!("corpus boards: {}", boards.len());
    let sheets = [instantiate::A7, instantiate::A6, instantiate::A5];
    let mut rows = Vec::new();
    for (name, zen, pcb) in &boards {
        for sheet in sheets {
            for strat in PatternKind::eval() {
                print!("  {name} {} {} ... ", sheet.name, strat.name());
                let emit_dir = (strat == PatternKind::S11).then(|| args.out.join("boards"));
                match run_one(
                    name,
                    zen,
                    pcb,
                    sheet,
                    strat,
                    args.panels.as_deref(),
                    emit_dir.as_deref(),
                ) {
                    Ok(row) => {
                        println!(
                            "maze {}/{} usb {} pwr {} ls {} gnd {} boards {}/{} | vias {} bends {} sharp {} detour {:.2} UΔ {:.2} loose {} drc {} gap {:.2} | Q {:.1} | {}",
                            row.score.nets_routed,
                            row.score.nets_total,
                            row.score.usb.fmt_maze(),
                            row.score.power.fmt_maze(),
                            row.score.ls.fmt_maze(),
                            row.score.gnd.fmt_gnd(),
                            row.score.boards_complete,
                            row.score.boards_total,
                            row.score.v_vias,
                            row.score.bends,
                            row.score.bends90,
                            row.score.detour,
                            row.score.u_delta_routed,
                            row.score.loose_pairs,
                            row.score.drc_violations,
                            row.score.min_gap_mm,
                            quality_score(&row.score),
                            row.note
                        );
                        rows.push(row);
                    }
                    Err(e) => {
                        println!("ERR {e}");
                        rows.push(CaseRow {
                            board: name.clone(),
                            sheet: sheet.name.into(),
                            strategy: strat.name().to_string(),
                            score: Default::default(),
                            svg: String::new(),
                            note: format!("error: {e}"),
                        });
                    }
                }
            }
        }
    }

    // Rank strategies by their worst panel's quality score: the winner is
    // the constellation we would actually commit to.
    let mut by_strat: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    for r in &rows {
        if r.score.nets_total == 0 {
            continue;
        }
        by_strat
            .entry(r.strategy.clone())
            .or_default()
            .push((format!("{}-{}", r.board, r.sheet), quality_score(&r.score)));
    }
    let mut ranking: Vec<(String, f64)> = by_strat
        .iter()
        .map(|(k, vs)| {
            let (case, q) = vs
                .iter()
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap();
            (format!("{k} (worst {case})"), *q)
        })
        .collect();
    ranking.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    let html = viz::html_report(&rows, &ranking);
    let report = args.out.join("interposer-poc-report.html");
    fs::write(&report, html).with_context(|| format!("write {}", report.display()))?;
    println!("wrote {}", report.display());

    if let Some((best, q)) = ranking.first() {
        println!("best-generalizing strategy (lowest worst-panel Q): {best}  Q={q:.1}");
    }
    let live: Vec<_> = rows.iter().filter(|r| r.score.boards_total > 0).collect();
    let (mut bok, mut bn, mut nok, mut nn) = (0, 0, 0, 0);
    for r in &live {
        bok += r.score.boards_complete;
        bn += r.score.boards_total;
        nok += r.score.nets_routed;
        nn += r.score.nets_total;
    }
    if bn > 0 {
        println!(
            "overall boards {:.1}% ({bok}/{bn})  maze {:.1}% ({nok}/{nn})",
            100.0 * bok as f64 / bn as f64,
            100.0 * nok as f64 / nn.max(1) as f64
        );
    }
    Ok(())
}
