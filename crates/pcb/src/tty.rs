use std::io::IsTerminal;

/// Returns true if running in an interactive terminal (both stdin and stdout are TTYs).
pub fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}
