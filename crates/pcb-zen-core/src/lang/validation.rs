use anyhow::anyhow;

/// Validates that a name is a valid identifier for PCB components, nets, modules, etc.
///
/// Valid identifiers must:
/// - Not be empty
/// - Not contain whitespace
/// - Not contain dots
/// - Only contain ASCII characters
///
/// Returns an error with a descriptive message if validation fails.
pub fn validate_identifier_name(name: &str, context: &str) -> Result<(), starlark::Error> {
    // Check for empty names
    if name.is_empty() {
        return Err(starlark::Error::new_other(anyhow!(
            "{} cannot be empty",
            context
        )));
    }

    // Check for any whitespace
    if name.contains(char::is_whitespace) {
        return Err(starlark::Error::new_other(anyhow!(
            "{} cannot contain whitespace. Got: {:?}",
            context,
            name
        )));
    }

    // Check for dots
    if name.contains('.') {
        return Err(starlark::Error::new_other(anyhow!(
            "{} cannot contain dots. Got: {:?}",
            context,
            name
        )));
    }

    // Check for non-ASCII characters
    if !name.is_ascii() {
        return Err(starlark::Error::new_other(anyhow!(
            "{} must contain only ASCII characters. Got: {:?}",
            context,
            name
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_names() {
        let valid_names = vec![
            "R1",
            "LED_STATUS",
            "power_rail",
            "Component123",
            "_private",
            "A",
            "a",
            "TEST_NET_VCC",
            "1Component",
            "2LED",
            "component-1",
            "LED-STATUS",
            "power-rail",
            "VCC+",          // Plus signs allowed
            "GND-",          // Minus signs allowed
            "R@1",           // @ signs allowed
            "C#1",           // # signs allowed
            "L$1",           // $ signs allowed
            "Q&1",           // & signs allowed
            "U*1",           // * signs allowed
            "D(1)",          // Parentheses allowed
            "J[1]",          // Brackets allowed
            "SW{1}",         // Braces allowed
            "net=vcc",       // Equals allowed
            "pin:1",         // Colons allowed
            "path/to/file",  // Path separators now allowed
            "windows\\path", // Backslashes now allowed
        ];

        for name in valid_names {
            assert!(
                validate_identifier_name(name, "Test name").is_ok(),
                "Expected '{}' to be valid",
                name
            );
        }
    }

    #[test]
    fn test_invalid_names_with_whitespace() {
        let invalid_names = vec![
            "PS WITH SPACES",
            "TEST NET",
            "middle space",
            "multiple   spaces",
            "space at end ",
            " space at start",
            "  multiple  spaces  ",
            "tab\there",
            "newline\nhere",
        ];

        for name in invalid_names {
            let result = validate_identifier_name(name, "Test name");
            assert!(result.is_err(), "Expected '{}' to be invalid", name);
            let error_msg = format!("{}", result.unwrap_err());
            assert!(
                error_msg.contains("cannot contain whitespace"),
                "Expected whitespace error for '{}', got: {}",
                name,
                error_msg
            );
        }
    }

    #[test]
    fn test_invalid_names_with_dots() {
        let invalid_names = vec!["power.rail", "file.ext", "net.test", "comp.1"];

        for name in invalid_names {
            let result = validate_identifier_name(name, "Test name");
            assert!(result.is_err(), "Expected '{}' to be invalid", name);
            let error_msg = format!("{}", result.unwrap_err());
            assert!(
                error_msg.contains("cannot contain dots"),
                "Expected dot error for '{}', got: {}",
                name,
                error_msg
            );
        }
    }

    #[test]
    fn test_invalid_names_with_non_ascii() {
        let invalid_names = vec![
            "café",     // Non-ASCII é
            "test™",    // Trademark symbol
            "résistor", // Non-ASCII é
            "πr²",      // Greek pi
            "测试",     // Chinese characters
            "🚀rocket", // Emoji
        ];

        for name in invalid_names {
            let result = validate_identifier_name(name, "Test name");
            assert!(result.is_err(), "Expected '{}' to be invalid", name);
            let error_msg = format!("{}", result.unwrap_err());
            assert!(
                error_msg.contains("must contain only ASCII characters"),
                "Expected ASCII error for '{}', got: {}",
                name,
                error_msg
            );
        }
    }

    #[test]
    fn test_empty_names() {
        let result = validate_identifier_name("", "Test name");
        assert!(result.is_err());
        let error_msg = format!("{}", result.unwrap_err());
        assert!(error_msg.contains("cannot be empty"));
    }

    #[test]
    fn test_whitespace_only_names() {
        let invalid_names = vec!["   ", "\t", "\n", "  \t  \n  "];

        for name in invalid_names {
            let result = validate_identifier_name(name, "Test name");
            assert!(result.is_err(), "Expected '{}' to be invalid", name);
            let error_msg = format!("{}", result.unwrap_err());
            assert!(
                error_msg.contains("cannot contain whitespace"),
                "Expected whitespace error for '{}', got: {}",
                name,
                error_msg
            );
        }
    }

    #[test]
    fn test_context_in_error_messages() {
        let result = validate_identifier_name("invalid name", "Component name");
        assert!(result.is_err());
        let error_msg = format!("{}", result.unwrap_err());
        assert!(
            error_msg.contains("Component name"),
            "Expected context 'Component name' in error message: {}",
            error_msg
        );

        let result = validate_identifier_name("invalid.name", "Net name");
        assert!(result.is_err());
        let error_msg = format!("{}", result.unwrap_err());
        assert!(
            error_msg.contains("Net name"),
            "Expected context 'Net name' in error message: {}",
            error_msg
        );
    }
}
