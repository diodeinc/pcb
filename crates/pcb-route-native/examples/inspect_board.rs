use std::env;
use std::fs;

fn main() {
    let path = env::args()
        .nth(1)
        .expect("usage: inspect_board <path.kicad_pcb>");
    let text = fs::read_to_string(&path).expect("failed to read file");
    let board = pcb_route_native::parse_board(&text).expect("failed to parse board");

    let total_pads: usize = board.footprints.iter().map(|f| f.pads.len()).sum();
    println!("footprints: {}", board.footprints.len());
    println!("pads: {}", total_pads);
    match board.outline {
        Some(o) => println!("outline: {:.2}mm x {:.2}mm", o.width(), o.height()),
        None => println!("outline: none (no Edge.Cuts found)"),
    }

    let grid = pcb_route_native::build_grid(&board, pcb_route_native::GridConfig::default());
    println!("grid: {} x {} cells", grid.cols, grid.rows);
}
