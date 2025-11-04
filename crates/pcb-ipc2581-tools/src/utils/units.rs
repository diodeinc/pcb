use crate::UnitFormat;

/// Convert millimeters to the requested unit format
pub fn convert_mm(mm: f64, format: UnitFormat) -> String {
    match format {
        UnitFormat::Mm => format!("{:.2}mm", mm),
        UnitFormat::Mil => format!("{:.1}mil", mm * 39.3701), // 1mm = 39.3701 mils
        UnitFormat::Inch => format!("{:.4}in", mm / 25.4),    // 1in = 25.4mm
    }
}

/// Format board dimensions (width x height)
pub fn format_board_size(width_mm: f64, height_mm: f64, format: UnitFormat) -> String {
    match format {
        UnitFormat::Mm => format!("{:.2}mm × {:.2}mm", width_mm, height_mm),
        UnitFormat::Mil => {
            let width_mil = width_mm * 39.3701;
            let height_mil = height_mm * 39.3701;
            format!("{:.1}mil × {:.1}mil", width_mil, height_mil)
        }
        UnitFormat::Inch => {
            let width_in = width_mm / 25.4;
            let height_in = height_mm / 25.4;
            format!("{:.4}in × {:.4}in", width_in, height_in)
        }
    }
}
