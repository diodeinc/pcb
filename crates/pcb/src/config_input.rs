use anyhow::{Result, bail};
use serde_json::Value as JsonValue;
use starlark::collections::SmallMap;

pub const CONFIG_ARG_HELP: &str = "Override root config() parameters. Repeat as needed.\n\
     Values parse as true/false, bare ints, bare floats, otherwise strings.";

fn parse_config_value(raw: &str) -> JsonValue {
    if raw.eq_ignore_ascii_case("true") {
        return JsonValue::Bool(true);
    }

    if raw.eq_ignore_ascii_case("false") {
        return JsonValue::Bool(false);
    }

    if let Ok(value) = raw.parse::<i32>() {
        return JsonValue::Number(value.into());
    }

    if let Ok(value) = raw.parse::<f64>()
        && let Some(number) = serde_json::Number::from_f64(value)
    {
        return JsonValue::Number(number);
    }

    JsonValue::String(raw.to_string())
}

pub fn parse_config_overrides(raw_configs: &[String]) -> Result<SmallMap<String, JsonValue>> {
    let mut parsed = SmallMap::new();

    for raw in raw_configs {
        let Some((key, value)) = raw.split_once('=') else {
            bail!("Invalid --config '{raw}'. Expected key=value");
        };

        if key.is_empty() {
            bail!("Invalid --config '{raw}'. Key cannot be empty");
        }

        parsed.insert(key.to_string(), parse_config_value(value));
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::parse_config_overrides;
    use serde_json::Value as JsonValue;

    #[test]
    fn parse_config_overrides_converts_supported_scalars() {
        let raw = vec![
            "enabled=true".to_string(),
            "count=42".to_string(),
            "ratio=3.14".to_string(),
            "voltage=5V".to_string(),
            "enum_value=YES".to_string(),
        ];

        let parsed = parse_config_overrides(&raw).expect("config overrides should parse");

        assert_eq!(parsed.get("enabled"), Some(&JsonValue::Bool(true)));
        assert_eq!(parsed.get("count"), Some(&JsonValue::Number(42.into())));
        assert_eq!(
            parsed.get("ratio"),
            Some(&JsonValue::Number(
                serde_json::Number::from_f64(3.14).expect("finite float")
            ))
        );
        assert_eq!(
            parsed.get("voltage"),
            Some(&JsonValue::String("5V".to_string()))
        );
        assert_eq!(
            parsed.get("enum_value"),
            Some(&JsonValue::String("YES".to_string()))
        );
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
