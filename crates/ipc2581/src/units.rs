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
