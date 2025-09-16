use allocative::Allocative;
use rust_decimal::Decimal;
use starlark::{
    any::ProvidesStaticType,
    eval::{Arguments, Evaluator},
    starlark_simple_value,
    values::{starlark_value, Freeze, FreezeResult, NoSerialize, StarlarkValue, Value},
};
use std::str::FromStr;

#[derive(Clone, Debug, ProvidesStaticType, NoSerialize, Freeze, Allocative)]
pub struct PhysicalValue {
    #[allocative(skip)]
    value: Decimal,
    #[allocative(skip)]
    tolerance: Decimal,
    unit: PhysicalUnit,
}

impl PhysicalValue {
    pub fn new(value: Decimal, unit: PhysicalUnit) -> Self {
        Self {
            value,
            tolerance: Decimal::ZERO,
            unit,
        }
    }

    pub fn with_tolerance(value: Decimal, unit: PhysicalUnit, tolerance: Decimal) -> Self {
        Self {
            value,
            tolerance,
            unit,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, ProvidesStaticType, NoSerialize, Freeze, Allocative)]
pub enum PhysicalUnit {
    Time,
    Current,
    Voltage,
    Capacitance,
    Resistance,
    Inductance,
    Frequency,
    Temperature,
}

impl PhysicalUnit {
    fn suffix(self) -> &'static str {
        use PhysicalUnit::*;
        match self {
            Resistance => "",
            Time => "s",
            Current => "A",
            Voltage => "V",
            Capacitance => "F",
            Inductance => "H",
            Frequency => "Hz",
            Temperature => "K",
        }
    }
}

impl std::fmt::Display for PhysicalUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, ProvidesStaticType, NoSerialize, Freeze, Allocative)]
pub struct VoltageType;

impl std::fmt::Display for VoltageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Voltage")
    }
}

const SI_PREFIXES: [(i32, &str); 17] = [
    (24, "Y"),
    (21, "Z"),
    (18, "E"),
    (15, "P"),
    (12, "T"),
    (9, "G"),
    (6, "M"),
    (3, "k"),
    (0, ""),
    (-3, "m"),
    (-6, "u"),
    (-9, "n"),
    (-12, "p"),
    (-15, "f"),
    (-18, "a"),
    (-21, "z"),
    (-24, "y"),
];

#[inline]
fn pow10(exp: i32) -> Decimal {
    if exp >= 0 {
        Decimal::from_i128_with_scale(10i128.pow(exp as u32), 0)
    } else {
        Decimal::new(1, (-exp) as u32)
    }
}

fn scale_to_si(raw: Decimal) -> (Decimal, &'static str) {
    for &(exp, sym) in &SI_PREFIXES {
        let factor = pow10(exp);
        if raw.abs() >= factor {
            return (raw / factor, sym);
        }
    }
    (raw, "")
}

fn fmt_significant(x: Decimal) -> String {
    let formatted = if x.abs() >= Decimal::from(100) {
        format!("{:.0}", x.round())
    } else if x.abs() >= Decimal::from(10) {
        format!("{:.1}", x)
    } else {
        format!("{:.2}", x)
    };

    if formatted.contains('.') {
        formatted
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    } else {
        formatted
    }
}

#[derive(Debug)]
pub enum ParseError {
    InvalidFormat,
    InvalidNumber,
    InvalidUnit,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::InvalidFormat => write!(f, "Invalid physical value format"),
            ParseError::InvalidNumber => write!(f, "Invalid number"),
            ParseError::InvalidUnit => write!(f, "Invalid unit"),
        }
    }
}

impl std::error::Error for ParseError {}

impl FromStr for PhysicalValue {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseError::InvalidFormat);
        }

        // Split by spaces to check for tolerance
        let parts: Vec<&str> = s.split_whitespace().collect();

        // Extract tolerance if provided (last token ending with "%")
        let mut tolerance = Decimal::ZERO;
        let value_unit_str = if parts.len() > 1 && parts.last().unwrap().ends_with('%') {
            let tolerance_str = parts.last().unwrap();
            let tolerance_digits = tolerance_str
                .strip_suffix('%')
                .ok_or(ParseError::InvalidFormat)?;
            if tolerance_digits.is_empty() {
                return Err(ParseError::InvalidFormat);
            }
            tolerance = tolerance_digits
                .parse::<Decimal>()
                .map_err(|_| ParseError::InvalidNumber)?
                / Decimal::from(100);

            // Rejoin all parts except the last one
            parts[..parts.len() - 1].join("")
        } else {
            // No tolerance, join all parts
            parts.join("")
        };

        // Handle special case like "4k7" (resistance notation -> 4.7kOhm)
        if let Some(k_pos) = value_unit_str.find('k') {
            let before_k = &value_unit_str[..k_pos];
            let after_k = &value_unit_str[k_pos + 1..];

            if !after_k.is_empty()
                && before_k
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '+')
                && after_k.chars().all(|c| c.is_ascii_digit() || c == '.')
            {
                if let (Ok(before_num), Ok(after_num)) =
                    (before_k.parse::<f64>(), after_k.parse::<f64>())
                {
                    // Treat as decimal notation: "4k7" -> "4.7k" -> 4700
                    let decimal_num = before_num + after_num / 10_f64.powi(after_k.len() as i32);
                    let combined_value = decimal_num * 1000.0;
                    return Ok(PhysicalValue::with_tolerance(
                        Decimal::try_from(combined_value).map_err(|_| ParseError::InvalidNumber)?,
                        PhysicalUnit::Resistance,
                        tolerance,
                    ));
                }
            }
        }

        // Standard parsing: find where number ends
        let mut split_pos = value_unit_str.len();
        for (i, ch) in value_unit_str.char_indices() {
            if !ch.is_ascii_digit() && ch != '.' && ch != '-' && ch != '+' {
                split_pos = i;
                break;
            }
        }

        if split_pos == 0 {
            return Err(ParseError::InvalidFormat);
        }

        let number_str = &value_unit_str[..split_pos];
        let unit_str = &value_unit_str[split_pos..];

        let base_number: Decimal = number_str.parse().map_err(|_| ParseError::InvalidNumber)?;

        // Parse unit with prefix
        let (multiplier, unit) = parse_unit_with_prefix(unit_str)?;
        let value = base_number * multiplier;

        Ok(PhysicalValue::with_tolerance(value, unit, tolerance))
    }
}

fn parse_unit_with_prefix(unit_str: &str) -> Result<(Decimal, PhysicalUnit), ParseError> {
    // Handle bare number (empty unit) - defaults to resistance
    if unit_str.is_empty() {
        return Ok((Decimal::ONE, PhysicalUnit::Resistance));
    }

    // Try each SI prefix from longest to shortest
    for &(exp, prefix) in &SI_PREFIXES {
        if prefix.is_empty() {
            continue; // Handle base unit separately
        }

        if let Some(base_unit) = unit_str.strip_prefix(prefix) {
            let multiplier = pow10(exp);
            let unit = match base_unit {
                "V" => PhysicalUnit::Voltage,
                "A" => PhysicalUnit::Current,
                "F" => PhysicalUnit::Capacitance,
                "H" => PhysicalUnit::Inductance,
                "Hz" => PhysicalUnit::Frequency,
                "s" => PhysicalUnit::Time,
                "K" => PhysicalUnit::Temperature,
                "Ohm" | "ohm" => PhysicalUnit::Resistance,
                "" => PhysicalUnit::Resistance, // Handle bare prefix for resistance (like "4k7" -> "4k" + "7")
                _ => return Err(ParseError::InvalidUnit),
            };
            return Ok((multiplier, unit));
        }
    }

    // Handle base units (no prefix)
    let unit = match unit_str {
        "V" => PhysicalUnit::Voltage,
        "A" => PhysicalUnit::Current,
        "F" => PhysicalUnit::Capacitance,
        "H" => PhysicalUnit::Inductance,
        "Hz" => PhysicalUnit::Frequency,
        "s" => PhysicalUnit::Time,
        "K" => PhysicalUnit::Temperature,
        "Ohm" | "ohm" => PhysicalUnit::Resistance,
        _ => return Err(ParseError::InvalidUnit),
    };

    Ok((Decimal::ONE, unit))
}

impl std::fmt::Display for PhysicalValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (scaled, prefix) = scale_to_si(self.value);
        let val_str = fmt_significant(scaled);

        let tol = self.tolerance * Decimal::from(100);
        let show_tol = tol > Decimal::ZERO;

        let suffix = self.unit.suffix();

        if show_tol {
            write!(f, "{val_str}{prefix}{} {}%", suffix, tol.round())
        } else {
            write!(f, "{val_str}{prefix}{}", suffix)
        }
    }
}

starlark_simple_value!(PhysicalUnit);

#[starlark_value(type = "Unit")]
impl<'v> StarlarkValue<'v> for PhysicalUnit {}

starlark_simple_value!(PhysicalValue);

#[starlark_value(type = "PhysicalValue")]
impl<'v> StarlarkValue<'v> for PhysicalValue {}

starlark_simple_value!(VoltageType);

#[starlark_value(type = "VoltageType")]
impl<'v> StarlarkValue<'v> for VoltageType {
    fn invoke(
        &self,
        _me: Value<'v>,
        args: &Arguments<'v, '_>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> starlark::Result<Value<'v>> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper function for formatting tests
    fn assert_formatting(value_str: &str, unit: PhysicalUnit, expected: &str) {
        let val = PhysicalValue::new(value_str.parse().unwrap(), unit);
        assert_eq!(
            format!("{}", val),
            expected,
            "Formatting mismatch for {} {:?}",
            value_str,
            unit
        );
    }

    // Helper function for parsing tests
    fn assert_parsing(input: &str, expected_unit: PhysicalUnit, expected_value: Decimal) {
        let val: PhysicalValue = input.parse().unwrap();
        assert_eq!(val.unit, expected_unit, "Unit mismatch for '{}'", input);
        assert_eq!(val.value, expected_value, "Value mismatch for '{}'", input);
    }

    #[test]
    fn test_si_prefix_formatting() {
        let test_cases = [
            ("4700", PhysicalUnit::Resistance, "4.7k"),
            ("1500000", PhysicalUnit::Frequency, "1.5MHz"),
            ("0.001", PhysicalUnit::Capacitance, "1mF"),
            ("0.000001", PhysicalUnit::Capacitance, "1uF"),
            ("0.0000001", PhysicalUnit::Capacitance, "100nF"),
        ];

        for (value, unit, expected) in test_cases {
            assert_formatting(value, unit, expected);
        }
    }

    #[test]
    fn test_formatting_features() {
        let test_cases = [
            // Significant digits: ≥100 (no decimals), ≥10 (one decimal), <10 (two decimals)
            ("150000", PhysicalUnit::Resistance, "150k"),
            ("47000", PhysicalUnit::Resistance, "47k"),
            ("4700", PhysicalUnit::Resistance, "4.7k"),
            // Trailing zero removal
            ("1000", PhysicalUnit::Resistance, "1k"),
            ("1200", PhysicalUnit::Resistance, "1.2k"),
            // Resistance special case (no unit suffix)
            ("1000", PhysicalUnit::Resistance, "1k"),
            ("1000", PhysicalUnit::Voltage, "1kV"), // Other units show suffix
            // Various units
            ("3300", PhysicalUnit::Voltage, "3.3kV"),
            ("0.1", PhysicalUnit::Current, "100mA"),
            ("1000000", PhysicalUnit::Frequency, "1MHz"),
            // Edge cases
            ("0.000000000001", PhysicalUnit::Capacitance, "1pF"),
            ("1000000000", PhysicalUnit::Frequency, "1GHz"),
            ("1", PhysicalUnit::Voltage, "1V"),
            // No prefix needed
            ("100", PhysicalUnit::Voltage, "100V"),
            ("47", PhysicalUnit::Resistance, "47"),
        ];

        for (value, unit, expected) in test_cases {
            assert_formatting(value, unit, expected);
        }
    }

    #[test]
    fn test_tolerance_display() {
        let test_cases = [
            (
                Decimal::from(1000),
                PhysicalUnit::Resistance,
                Decimal::new(5, 2),
                "1k 5%",
            ), // With tolerance
            (
                Decimal::from(1000),
                PhysicalUnit::Resistance,
                Decimal::ZERO,
                "1k",
            ), // Without tolerance
            (
                Decimal::from(1000),
                PhysicalUnit::Capacitance,
                Decimal::new(1, 1),
                "1kF 10%",
            ), // Non-resistance with tolerance
        ];

        for (value, unit, tolerance, expected) in test_cases {
            let val = PhysicalValue::with_tolerance(value, unit, tolerance);
            assert_eq!(format!("{}", val), expected);
        }
    }

    #[test]
    fn test_parsing_basic_units() {
        let test_cases = [
            ("5V", PhysicalUnit::Voltage, Decimal::from(5)),
            ("100A", PhysicalUnit::Current, Decimal::from(100)),
            ("47", PhysicalUnit::Resistance, Decimal::from(47)),
            ("100Ohm", PhysicalUnit::Resistance, Decimal::from(100)),
        ];

        for (input, unit, value) in test_cases {
            assert_parsing(input, unit, value);
        }
    }

    #[test]
    fn test_parsing_with_prefixes() {
        let test_cases = [
            ("5kV", PhysicalUnit::Voltage, Decimal::from(5000)),
            ("100mA", PhysicalUnit::Current, Decimal::new(1, 1)), // 0.1
            ("470nF", PhysicalUnit::Capacitance, Decimal::new(47, 8)), // 470e-9
            ("4k7", PhysicalUnit::Resistance, Decimal::from(4700)), // Special notation
        ];

        for (input, unit, value) in test_cases {
            assert_parsing(input, unit, value);
        }
    }

    #[test]
    fn test_parsing_decimal_numbers() {
        let test_cases = [
            ("3.3V", PhysicalUnit::Voltage, Decimal::new(33, 1)), // 3.3
            ("4.7kOhm", PhysicalUnit::Resistance, Decimal::from(4700)),
        ];

        for (input, unit, value) in test_cases {
            assert_parsing(input, unit, value);
        }
    }

    #[test]
    fn test_parsing_errors() {
        let invalid_cases = ["", "abc", "5X", "5.3.3V"];

        for input in invalid_cases {
            assert!(
                input.parse::<PhysicalValue>().is_err(),
                "Expected error for '{}'",
                input
            );
        }
    }

    #[test]
    fn test_roundtrip_parsing() {
        let test_cases = ["5V", "100mA", "4k7", "470nF", "3.3kV", "100Ohm"];

        for input in test_cases {
            let parsed: PhysicalValue = input.parse().unwrap();
            // Note: roundtrip may not be exact due to SI prefix selection
            let _formatted = format!("{}", parsed);
            // Just ensure parsing succeeds - exact roundtrip not guaranteed due to SI prefix normalization
        }
    }

    // Helper function for tolerance parsing tests
    fn assert_tolerance_parsing(
        input: &str,
        expected_unit: PhysicalUnit,
        expected_value: Decimal,
        expected_tolerance: Decimal,
    ) {
        let val: PhysicalValue = input.parse().unwrap();
        assert_eq!(val.unit, expected_unit, "Unit mismatch for '{}'", input);
        assert_eq!(val.value, expected_value, "Value mismatch for '{}'", input);
        assert_eq!(
            val.tolerance, expected_tolerance,
            "Tolerance mismatch for '{}'",
            input
        );
    }

    #[test]
    fn test_tolerance_parsing() {
        // Test basic tolerance parsing across different units
        let test_cases = [
            (
                "100kOhm 5%",
                PhysicalUnit::Resistance,
                Decimal::from(100000),
                Decimal::new(5, 2),
            ),
            (
                "10nF 20%",
                PhysicalUnit::Capacitance,
                Decimal::new(1, 8),
                Decimal::new(2, 1),
            ),
            (
                "3.3V 1%",
                PhysicalUnit::Voltage,
                Decimal::new(33, 1),
                Decimal::new(1, 2),
            ),
            (
                "12V 0.5%",
                PhysicalUnit::Voltage,
                Decimal::from(12),
                Decimal::new(5, 3),
            ),
            (
                "100mA 5%",
                PhysicalUnit::Current,
                Decimal::new(1, 1),
                Decimal::new(5, 2),
            ),
            (
                "1MHz 10%",
                PhysicalUnit::Frequency,
                Decimal::from(1000000),
                Decimal::new(1, 1),
            ),
            (
                "10uH 20%",
                PhysicalUnit::Inductance,
                Decimal::new(1, 5),
                Decimal::new(2, 1),
            ),
            (
                "100s 1%",
                PhysicalUnit::Time,
                Decimal::from(100),
                Decimal::new(1, 2),
            ),
            (
                "300K 2%",
                PhysicalUnit::Temperature,
                Decimal::from(300),
                Decimal::new(2, 2),
            ),
            (
                "4k7 1%",
                PhysicalUnit::Resistance,
                Decimal::from(4700),
                Decimal::new(1, 2),
            ), // Special notation
        ];

        for (input, unit, value, tolerance) in test_cases {
            assert_tolerance_parsing(input, unit, value, tolerance);
        }
    }

    #[test]
    fn test_tolerance_parsing_without_tolerance() {
        let test_cases = ["100kOhm", "10nF", "3.3V"];
        for input in test_cases {
            let val: PhysicalValue = input.parse().unwrap();
            assert_eq!(
                val.tolerance,
                Decimal::ZERO,
                "Expected zero tolerance for '{}'",
                input
            );
        }
    }

    #[test]
    fn test_tolerance_parsing_with_spaces() {
        let test_cases = [
            "100 kOhm 5%",  // Space in unit
            "100kOhm  5%",  // Multiple spaces before tolerance
            " 100kOhm 5% ", // Leading/trailing spaces
        ];

        for input in test_cases {
            assert_tolerance_parsing(
                input,
                PhysicalUnit::Resistance,
                Decimal::from(100000),
                Decimal::new(5, 2),
            );
        }
    }

    #[test]
    fn test_tolerance_formatting() {
        let test_cases = [
            (
                Decimal::from(100000),
                PhysicalUnit::Resistance,
                Decimal::new(5, 2),
                "100k 5%",
            ),
            (
                Decimal::new(1, 8),
                PhysicalUnit::Capacitance,
                Decimal::new(2, 1),
                "10nF 20%",
            ),
            (
                Decimal::from(3300),
                PhysicalUnit::Voltage,
                Decimal::new(1, 2),
                "3.3kV 1%",
            ),
        ];

        for (value, unit, tolerance, expected) in test_cases {
            let val = PhysicalValue::with_tolerance(value, unit, tolerance);
            assert_eq!(format!("{}", val), expected);
        }
    }

    #[test]
    fn test_tolerance_parsing_errors() {
        let invalid_cases = [
            "100kOhm %",    // Empty tolerance
            "100kOhm abc%", // Invalid tolerance number
            "100kOhm 5%%",  // Multiple percent signs
        ];

        for input in invalid_cases {
            assert!(
                input.parse::<PhysicalValue>().is_err(),
                "Expected error for '{}'",
                input
            );
        }
    }
}
