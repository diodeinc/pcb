//! End-to-end interposer POC driver.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use pcb_interposer::extract::{board_bbox_from_kicad, extract_kicad_path, parse_zen_ict_map};
use pcb_interposer::instantiate::{self, Sheet};
use pcb_interposer::pattern::{PITCH_254, PatternKind, attach_pattern, generate_pattern_at};
use pcb_interposer::route::{RouterKind, nets_from_assign, route};
use pcb_interposer::score::{rank_s1, score_g0, score_g1};
use pcb_interposer::types::{Assign, BoardId, Problem};
use pcb_interposer::viz::{self, CaseRow};
use pcb_interposer::{assign, bundle, hall};

#[derive(Parser, Debug)]
#[command(name = "interposer-poc")]
struct Args {
    /// Directory of demo boards (each with a top-level .zen and layout/layout.kicad_pcb).
    #[arg(long)]
    corpus: PathBuf,
    /// Output directory for the HTML report and per-case SVGs.
    #[arg(long)]
    out: PathBuf,
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
    router: RouterKind,
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
        let maxx = local.iter().map(|c| c.xy[0]).fold(f64::NEG_INFINITY, f64::max);
        let maxy = local.iter().map(|c| c.xy[1]).fold(f64::NEG_INFINITY, f64::max);
        let pad = 1.5;
        origin = [origin[0].min(minx - pad), origin[1].min(miny - pad)];
        let mx = (origin[0] + bw).max(maxx + pad);
        let my = (origin[1] + bh).max(maxy + pad);
        bw = (mx - origin[0]).max(10.0);
        bh = (my - origin[1]).max(10.0);
    }
    instantiate::localize(&mut local, origin);
    let places = instantiate::pack(sheet, bw, bh, 8);
    let contacts = instantiate::instantiate(&local, &places);
    let n_local = local.len();
    let n_boards = places.len();

    let label = format!("{}+{}", strategy.name(), router.name());
    if contacts.is_empty() {
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

    let mut problem: Problem = bundle(contacts)?;
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
    let pattern = generate_pattern_at(strategy, PITCH_254, &centroids);
    attach_pattern(&mut problem, &pattern);
    let hall_ok = hall(&problem).is_ok();
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
        route(router, sheet.w, sheet.h, &nets)
    } else {
        Default::default()
    };
    let g0 = score_g0(&problem, &asg, &pattern, &nets);
    let mut score = score_g1(g0, &route, &nets);
    score.hall_ok = hall_ok;
    let svg = viz::svg_panel(
        sheet.w,
        sheet.h,
        &problem,
        &pattern,
        &nets,
        &route,
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
            "local TPs={n_local}  packed={n_boards}  zen ict={}  hall={}  router={}  dropped={:?}",
            ict_map.len(),
            if hall_ok { "ok" } else { "fail" },
            router.name(),
            route.dropped_boards.iter().map(|b| b.0).collect::<Vec<_>>()
        ),
    })
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
                for router in RouterKind::eval() {
                    print!(
                        "  {name} {} {}+{} ... ",
                        sheet.name,
                        strat.name(),
                        router.name()
                    );
                    match run_one(name, zen, pcb, sheet, strat, router) {
                        Ok(row) => {
                            println!(
                                "maze {}/{} usb {} pwr {} ls {} gnd {} boards {}/{}  {}",
                                row.score.nets_routed,
                                row.score.nets_total,
                                row.score.usb.fmt_maze(),
                                row.score.power.fmt_maze(),
                                row.score.ls.fmt_maze(),
                                row.score.gnd.fmt_gnd(),
                                row.score.boards_complete,
                                row.score.boards_total,
                                row.note
                            );
                            rows.push(row);
                        }
                        Err(e) => {
                            println!("ERR {e}");
                            rows.push(CaseRow {
                                board: name.clone(),
                                sheet: sheet.name.into(),
                                strategy: format!("{}+{}", strat.name(), router.name()),
                                score: Default::default(),
                                svg: String::new(),
                                note: format!("error: {e}"),
                            });
                        }
                    }
                }
            }
        }
    }

    // Per strategy, take the worst panel (lowest board coverage, then most crossings).
    // Rank those worst-panel scores — this is "best worst-panel S1" from the note.
    let mut by_strat: BTreeMap<String, Vec<(String, pcb_interposer::Score)>> = BTreeMap::new();
    for r in &rows {
        if r.score.nets_total == 0 {
            continue;
        }
        by_strat
            .entry(r.strategy.clone())
            .or_default()
            .push((format!("{}-{}", r.board, r.sheet), r.score.clone()));
    }
    let flat: Vec<(String, pcb_interposer::Score)> = by_strat
        .iter()
        .map(|(k, vs)| {
            let worst = vs
                .iter()
                .max_by(|a, b| {
                    let cov = |s: &pcb_interposer::Score| {
                        if s.boards_total == 0 {
                            0.0
                        } else {
                            s.boards_complete as f64 / s.boards_total as f64
                        }
                    };
                    let ka = -cov(&a.1) * 100.0 + a.1.x_cross as f64 + a.1.a_mm * 0.01;
                    let kb = -cov(&b.1) * 100.0 + b.1.x_cross as f64 + b.1.a_mm * 0.01;
                    ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap();
            (format!("{} (worst {})", k, worst.0), worst.1.clone())
        })
        .collect();
    let ranking = rank_s1(&flat);

    let html = viz::html_report(&rows, &ranking);
    let report = args.out.join("interposer-poc-report.html");
    fs::write(&report, html).with_context(|| format!("write {}", report.display()))?;
    println!("wrote {}", report.display());

    if let Some((best, _, s1)) = ranking.first() {
        println!("best-generalizing key (lowest S1 on worst panel): {best}  S1={s1:.3}");
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
