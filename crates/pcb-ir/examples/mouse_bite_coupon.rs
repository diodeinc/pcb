//! Reproducible geometry coupons, not fabrication-ready manufacturing documents.
//! Run: cargo run -p pcb-ir --example mouse_bite_coupon -- /tmp/coupons
use pcb_ir::geom::attachment::{BoundaryQuery, QueryTolerance, transform_region};
use pcb_ir::geom::mouse_bite::{Attachment, build};
use pcb_ir::geom::{Affine2, BBox, ContourSet, Point, Resolution, shapes};
use pcb_ir::render::svg_path_data;
use std::fmt::Write;

fn rect(x: f64, y: f64, w: f64, h: f64) -> ContourSet {
    ContourSet::rectangle(
        BBox::new(Point::new(x, y), Point::new(x + w, y + h)),
        Resolution::default().strict(),
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("supply output directory")?,
    );
    std::fs::create_dir_all(&out)?;
    let tolerance = QueryTolerance {
        boundary_mm: 0.015,
        numerical_mm: 1e-6,
    };
    for curved in [false, true] {
        let name = if curved { "curved" } else { "straight" };
        let board = if curved {
            transform_region(
                &ContourSet::from_filled_contours(
                    &[shapes::circle(20.0).unwrap()],
                    Resolution::default().strict(),
                )?,
                Affine2::translation(Point::new(0.0, -10.0)),
            )?
        } else {
            rect(-10.0, -20.0, 20.0, 20.0)
        };
        let support = rect(-10.0, 3.0, 20.0, 10.0);
        let stock = rect(-10.0, -20.0, 20.0, 33.0);
        let query = BoundaryQuery::new(&board, tolerance)?;
        let boundary = query.boundaries().next().unwrap();
        let station_mm = query.project(boundary, Point::ZERO)?.site.station_mm;
        let tab = build(Attachment {
            stock: &stock,
            board: &board,
            support: &support,
            boundary,
            station_mm,
            support_anchor: Point::new(0.0, 5.0),
            board_witness: Point::new(0.0, -5.0),
            tolerance,
        })?;
        let after = tab.after_break(
            0.01,
            &[Point::new(0.0, -5.0), Point::new(0.0, 5.0)],
            tolerance,
        )?;
        let mut drills = String::from("x_mm,y_mm,diameter_mm,plating\n");
        for hole in &tab.npth {
            writeln!(
                drills,
                "{:.9},{:.9},{:.9},NPTH",
                hole.center.x, hole.center.y, hole.diameter_mm
            )?;
        }
        std::fs::write(out.join(format!("{name}-npth.csv")), drills)?;
        for (suffix, viewbox) in [("coupon", "-11 -14 22 35"), ("detail", "-3 -4 6 5")] {
            let mut svg = format!(
                "<svg xmlns='http://www.w3.org/2000/svg' width='880' height='700' viewBox='{viewbox}'><rect x='-50' y='-50' width='100' height='100' fill='white'/><g transform='scale(1,-1)'>"
            );
            for (id, region, color) in [
                ("routed-removal", &tab.routed_removal, "#e5e7eb"),
                ("retained-substrate", &tab.retained_substrate, "#b5d9bd"),
                ("rounded-shoulders", &tab.shoulders, "#6eaf86"),
                ("npth", &tab.perforations, "#ffffff"),
            ] {
                writeln!(
                    svg,
                    "<path id='{id}' d='{}' fill='{color}' fill-rule='nonzero'/>",
                    svg_path_data(&region.to_contours())
                )?;
            }
            writeln!(
                svg,
                "<path d='{}' fill='none' stroke='#111827' stroke-width='.012' stroke-dasharray='.08 .04'/>",
                svg_path_data(&board.to_contours())
            )?;
            writeln!(
                svg,
                "<path id='break-row' d='{}' fill='none' stroke='#dc2626' stroke-width='.018'/>",
                svg_path_data(std::slice::from_ref(&tab.break_path))
            )?;
            svg.push_str("</g></svg>");
            std::fs::write(out.join(format!("{name}-{suffix}.svg")), svg)?;
        }
        // Separate, machine-readable polygon paths for downstream adapters.
        for (part, region) in [
            ("routed", &tab.routed_removal),
            ("retained", &tab.retained_substrate),
            ("after-virtual-break", &after.retained),
        ] {
            std::fs::write(
                out.join(format!("{name}-{part}.path")),
                svg_path_data(&region.to_contours()),
            )?;
        }
    }
    Ok(())
}
