pub fn string(s: &str) -> String {
    // JSON string escaping is compatible with Starlark string literals.
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{}\"", s.replace('\"', "\\\"")))
}

pub fn float(v: f64) -> String {
    // Keep a stable representation without excessive noise.
    let s = format!("{v:.6}");
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');

    if trimmed.is_empty() {
        return "0.0".to_string();
    }

    // Downstream config parsing expects a float literal (not an int), so ensure we
    // keep at least one fractional digit.
    if trimmed.contains('.') || trimmed.contains('e') || trimmed.contains('E') {
        return trimmed.to_string();
    }

    if trimmed
        .chars()
        .all(|c| c.is_ascii_digit() || c == '-' || c == '+')
    {
        format!("{trimmed}.0")
    } else {
        trimmed.to_string()
    }
}

pub fn bool(v: bool) -> &'static str {
    if v {
        "True"
    } else {
        "False"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_always_renders_a_float_literal() {
        assert_eq!(float(4.0), "4.0");
        assert_eq!(float(0.0), "0.0");
        assert_eq!(float(-2.0), "-2.0");
        assert_eq!(float(1.5), "1.5");
    }
}
