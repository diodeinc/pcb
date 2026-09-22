use crate::types::Units;

/// Convert a value from the given units to millimeters (canonical internal unit)
///
/// All dimensions in the parsed IPC-2581 document are stored in millimeters.
/// This function converts from the source units specified in the XML to mm.
pub fn to_mm(value: f64, from_units: Units) -> f64 {
    match from_units {
        Units::Millimeter => value,
        Units::Inch => value * 25.4,
        Units::Mils => value * 0.0254,
        Units::Micron => value * 0.001,
    }
}

/// Convert a value from millimeters to the specified units
///
/// This is the inverse of `to_mm()` and is useful for exporting data.
pub fn from_mm(value: f64, to_units: Units) -> f64 {
    match to_units {
        Units::Millimeter => value,
        Units::Inch => value / 25.4,
        Units::Mils => value / 0.0254,
        Units::Micron => value / 0.001,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_each_unit_both_ways() {
        for (units, value, mm) in [
            (Units::Millimeter, 10.0, 10.0),
            (Units::Inch, 1.0, 25.4),
            (Units::Mils, 1.0, 0.0254),
            (Units::Micron, 1000.0, 1.0),
        ] {
            assert_eq!(to_mm(value, units), mm);
            assert_eq!(from_mm(mm, units), value);
        }
    }
}
