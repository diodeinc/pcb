/// Check if color output should be disabled
/// Respects NO_COLOR environment variable
pub fn should_disable_color() -> bool {
    std::env::var("NO_COLOR").is_ok()
}

/// Initialize colored crate based on NO_COLOR environment variable
pub fn init_color() {
    if should_disable_color() {
        colored::control::set_override(false);
    }
}
