use anyhow::{Result, bail};
use serde_json::Value as JsonValue;
use starlark::collections::SmallMap;

pub const CONFIG_ARG_HELP: &str = "Override root config() parameters. Repeat as needed.\n\
     Values are passed as strings and coerced by config() based on the declared parameter type.";

pub fn parse_config_overrides(raw_configs: &[String]) -> Result<SmallMap<String, JsonValue>> {
    let mut parsed = SmallMap::new();

    for raw in raw_configs {
        let Some((key, value)) = raw.split_once('=') else {
            bail!("Invalid --config '{raw}'. Expected key=value");
        };

        if key.is_empty() {
            bail!("Invalid --config '{raw}'. Key cannot be empty");
        }

        parsed.insert(key.to_string(), JsonValue::String(value.to_string()));
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::parse_config_overrides;
    use serde_json::Value as JsonValue;

    #[test]
    fn parse_config_overrides_preserves_values_as_strings() {
        let raw = vec![
            "enabled=true".to_string(),
            "count=42".to_string(),
            "ratio=3.14".to_string(),
            "voltage=5V".to_string(),
            "enum_value=YES".to_string(),
            "package=0402".to_string(),
        ];

        let parsed = parse_config_overrides(&raw).expect("config overrides should parse");

        for (key, value) in [
            ("enabled", "true"),
            ("count", "42"),
            ("ratio", "3.14"),
            ("voltage", "5V"),
            ("enum_value", "YES"),
            ("package", "0402"),
        ] {
            assert_eq!(
                parsed.get(key),
                Some(&JsonValue::String(value.to_string())),
                "expected {key} to stay stringly"
            );
        }
    }

    #[test]
    fn parse_config_overrides_requires_key_value_syntax() {
        let err = parse_config_overrides(&["missing_separator".to_string()])
            .expect_err("missing separator should fail");

        assert_eq!(
            err.to_string(),
            "Invalid --config 'missing_separator'. Expected key=value"
        );
    }
}
