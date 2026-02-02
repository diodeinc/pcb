pub fn string(s: &str) -> String {
    // JSON string escaping is compatible with Starlark string literals.
    serde_json::to_string(s).unwrap_or_else(|_| format!("\"{}\"", s.replace('\"', "\\\"")))
}

pub fn float(v: f64) -> String {
    // Keep a stable representation without excessive noise.
    let s = format!("{v:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}
