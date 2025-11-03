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
#[allow(dead_code)]
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
    fn test_mm_to_mm() {
        assert_eq!(to_mm(10.0, Units::Millimeter), 10.0);
        assert_eq!(from_mm(10.0, Units::Millimeter), 10.0);
    }

    #[test]
    fn test_inch_to_mm() {
        assert_eq!(to_mm(1.0, Units::Inch), 25.4);
        assert_eq!(from_mm(25.4, Units::Inch), 1.0);
    }

    #[test]
    fn test_mils_to_mm() {
        assert_eq!(to_mm(1.0, Units::Mils), 0.0254);
        assert_eq!(from_mm(0.0254, Units::Mils), 1.0);
    }

    #[test]
    fn test_micron_to_mm() {
        assert_eq!(to_mm(1000.0, Units::Micron), 1.0);
        assert_eq!(from_mm(1.0, Units::Micron), 1000.0);
    }

    #[test]
    fn test_roundtrip() {
        let original = 42.0;
        for units in [Units::Millimeter, Units::Inch, Units::Mils, Units::Micron] {
            let converted = to_mm(original, units);
            let back = from_mm(converted, units);
            assert!(
                (back - original).abs() < 1e-10,
                "Roundtrip failed for {:?}",
                units
            );
        }
    }
}
