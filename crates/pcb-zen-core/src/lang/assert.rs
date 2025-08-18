use starlark::environment::GlobalsBuilder;
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::values::Value;

use crate::lang::input::InputValue;

/// Miscellaneous built-in Starlark helpers used by Diode.
///
/// Currently this exposes:
///  • error(msg): unconditionally raises a runtime error with the provided message.
///  • check(cond, msg): raises an error with `msg` when `cond` is false.
///  • serialize(value): converts a value to InputValue JSON for debugging.
///  • deserialize(expected_type, json_str): converts InputValue JSON back to a value.
#[starlark_module]
pub(crate) fn assert_globals(builder: &mut GlobalsBuilder) {
    /// Raise a runtime error with the given message.
    fn error<'v>(#[starlark(require = pos)] msg: String) -> anyhow::Result<Value<'v>> {
        Err(anyhow::anyhow!(msg))
    }

    /// Check that a condition holds. If `cond` is false, raise an error with `msg`.
    fn check<'v>(
        #[starlark(require = pos)] cond: bool,
        #[starlark(require = pos)] msg: String,
    ) -> anyhow::Result<Value<'v>> {
        if cond {
            Ok(Value::new_none())
        } else {
            Err(anyhow::anyhow!(msg))
        }
    }

    /// Convert a value to InputValue JSON for debugging serialization.
    fn serialize<'v>(
        #[starlark(require = pos)] value: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let input_value = InputValue::from_value(value);
        let json = serde_json::to_string_pretty(&input_value)
            .map_err(|e| anyhow::anyhow!("Failed to serialize: {}", e))?;
        Ok(eval.heap().alloc_str(&json).to_value())
    }

    /// Deserialize InputValue JSON back to a Starlark value with optional type guidance.
    fn deserialize<'v>(
        #[starlark(require = pos)] expected_type: Value<'v>,
        #[starlark(require = pos)] json_str: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let input_value: InputValue = serde_json::from_str(&json_str)
            .map_err(|e| anyhow::anyhow!("Failed to parse JSON: {}", e))?;
        input_value.to_value(eval, Some(expected_type))
    }
}

#[cfg(test)]
mod tests {
    use starlark::assert::Assert;
    use starlark::environment::GlobalsBuilder;

    use crate::lang::component::component_globals;
    use crate::lang::interface::interface_globals;

    use super::assert_globals;

    #[test]
    fn test_serialize_deserialize_net() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            interface_globals(builder);
            assert_globals(builder);
        });

        a.pass(
            r#"
# Test Net serialize/deserialize round-trip
net1 = Net("TEST_NET")
json_str = serialize(net1)
net2 = deserialize(Net, json_str)
assert_eq(net1.name, net2.name)
"#,
        );
    }

    #[test]
    fn test_serialize_deserialize_simple_interface() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            interface_globals(builder);
            assert_globals(builder);
        });

        a.pass(
            r#"
# Test simple interface serialize/deserialize round-trip
Power = interface(NET = Net("VCC"))
power1 = Power("MAIN_VCC")
json_str = serialize(power1)
power2 = deserialize(Power, json_str)
assert_eq(power1.NET.name, power2.NET.name)
"#,
        );
    }

    #[test]
    fn test_serialize_deserialize_complex_interface() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            interface_globals(builder);
            assert_globals(builder);
        });

        a.pass(
            r#"
# Test interface with basic field types
Config = interface(
    enable = field(bool, True),
    count = field(int, 42),
    label = field(str, "default"),
    ratio = field(float, 3.14),
)
config1 = Config("TEST", enable=False, count=100, ratio=2.71)
json_str = serialize(config1)
config2 = deserialize(Config, json_str)
assert_eq(config1.enable, config2.enable)
assert_eq(config1.count, config2.count)
assert_eq(config1.label, config2.label)
assert_eq(config1.ratio, config2.ratio)
"#,
        );
    }

    #[test]
    fn test_serialize_nested_interface() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            interface_globals(builder);
            assert_globals(builder);
        });

        a.pass(
            r#"
# Test that nested interface serialization produces valid JSON
Power = interface(NET = Net("VCC"))
Uart = interface(TX = Net("TX"), RX = Net("RX"))
System = interface(power = Power(), uart = Uart(), debug = field(bool, False))
system1 = System("MAIN")
json_str = serialize(system1)
# Just verify JSON structure contains expected fields
assert_eq("Interface" in json_str, True)
assert_eq("fields" in json_str, True)
assert_eq("power" in json_str, True)
assert_eq("uart" in json_str, True)
assert_eq("debug" in json_str, True)
"#,
        );
    }

    #[test]
    fn test_serialize_json_structure() {
        let mut a = Assert::new();
        a.globals_add(|builder: &mut GlobalsBuilder| {
            component_globals(builder);
            interface_globals(builder);
            assert_globals(builder);
        });

        a.pass(
            r#"
# Test that serialized JSON has expected structure
net = Net("TEST")
net_json = serialize(net)
assert_eq("Net" in net_json, True)
assert_eq("id" in net_json, True)
assert_eq("name" in net_json, True)
assert_eq("TEST" in net_json, True)

Power = interface(NET = Net("VCC"))
power = Power()
power_json = serialize(power)
assert_eq("Interface" in power_json, True)
assert_eq("fields" in power_json, True)
assert_eq("NET" in power_json, True)
"#,
        );
    }
}
