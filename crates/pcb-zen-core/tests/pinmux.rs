//! Integration tests for the peripheral capability model. Behavior assertions
//! live in the `.zen` fixtures via `check()`; the Rust side asserts overall
//! success/failure, diagnostics, and emitted module properties. Pin/AF pairs
//! are illustrative, not datasheet-verified.

mod common;

use common::eval_zen;
use pcb_zen_core::WithDiagnostics;
use pcb_zen_core::lang::eval::EvalOutput;
use pcb_zen_core::lang::eval::{EvalContext, EvalContextConfig, EvalSession};
use std::path::PathBuf;
use std::sync::Arc;

const IFACES: &str = r#"
Frequency = 1 / builtin.Time
Uart = interface(TX = Net, RX = Net, attrs = {"baud_max": Frequency})
Usart = interface(TX = Net, RX = Net, CK = Net, implies = [Uart])
UartFlow = interface(TX = Net, RX = Net, RTS = Net, CTS = Net, implies = [Uart])
I2c = interface(SDA = Net, SCL = Net, attrs = {"clk_max": Frequency, "vio": Voltage})
Spi = interface(SCK = Net, MISO = Net, MOSI = Net)
Gpio = interface(PIN = Net)
AdcIn = interface(IN = Net)
Comparator = interface(INP = Net, INN = Net, OUT = Net)

DiffPair = interface(P = Net, N = Net, impedance = field(int, default = 0))
# Nested as an instance (the stdlib spelling) and as a type.
Usb2 = interface(D = DiffPair(impedance = 90), VBUS = Net)
Lvds = interface(CLK = DiffPair, DATA = DiffPair)
# `D` flattens onto the sibling `D_P`.
Ambiguous = interface(D = DiffPair, D_P = Net)
"#;

const STM32: &str = r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pool")
load("./ifaces.zen", "Uart", "Usart", "UartFlow", "I2c", "Spi", "Gpio", "AdcIn")

USART1 = peripheral(
    "USART1",
    provides = [Usart, UartFlow],
    rebind = "firmware",
    signals = {
        "TX": [pin("PA9", data = {"af": 1}), pin("PB6", data = {"af": 0})],
        "RX": [pin("PA10", data = {"af": 1}), pin("PB7", data = {"af": 0})],
        "CK": [pin("PA8", data = {"af": 1})],
        "RTS": [pin("PA12", data = {"af": 1})],
        "CTS": [pin("PA11", data = {"af": 1})],
    },
    attrs = {"baud_max": "8MHz"},
)

USART2 = peripheral(
    "USART2",
    provides = [Uart],
    rebind = "firmware",
    signals = {
        "TX": [pin("PA2", data = {"af": 1}), pin("PA14", data = {"af": 1})],
        "RX": [pin("PA3", data = {"af": 1}), pin("PA15", data = {"af": 1})],
    },
    attrs = {"baud_max": "4MHz"},
)

SPI1 = peripheral(
    "SPI1",
    provides = [Spi],
    rebind = "firmware",
    signals = {
        "SCK": [pin("PA5", data = {"af": 0}), pin("PB3", data = {"af": 0})],
        "MISO": [pin("PA6", data = {"af": 0}), pin("PB4", data = {"af": 0})],
        "MOSI": [pin("PA7", data = {"af": 0}), pin("PB5", data = {"af": 0})],
    },
)

I2C1 = peripheral(
    "I2C1",
    provides = [I2c],
    rebind = "firmware",
    signals = {
        "SDA": [pin("PB7", data = {"af": 6})],
        "SCL": [pin("PB6", data = {"af": 6})],
    },
)

ADC1_IN0 = peripheral("ADC1_IN0", provides = [AdcIn], rebind = "fixed", signals = {"IN": [pin("PA0")]})
ADC1_IN1 = peripheral("ADC1_IN1", provides = [AdcIn], rebind = "fixed", signals = {"IN": [pin("PA1")]})

GPIO_POOL = pool(
    "GPIO",
    provides = [Gpio],
    pins = [
        "PA0", "PA1", "PA2", "PA3", "PA4", "PA5", "PA6", "PA7", "PA8",
        "PA9", "PA10", "PA11", "PA12", "PA15",
        "PB0", "PB1", "PB3", "PB4", "PB5", "PB6", "PB7",
    ],
)

PERIPHS = [USART1, USART2, SPI1, I2C1, ADC1_IN0, ADC1_IN1, GPIO_POOL]
"#;

const ESP32C3: &str = r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pool")
load("./ifaces.zen", "Uart", "Gpio", "AdcIn")

_MATRIX = [
    "GPIO0", "GPIO1", "GPIO2", "GPIO3", "GPIO4", "GPIO5", "GPIO6", "GPIO7",
    "GPIO8", "GPIO9", "GPIO10", "GPIO18", "GPIO19", "GPIO20", "GPIO21",
]
_STRAP = ["GPIO2", "GPIO8", "GPIO9"]

def _matrix(exclude):
    return [pin(n, cost = 1, strap = n in _STRAP) for n in _MATRIX if n not in exclude]

UART0 = peripheral(
    "UART0",
    provides = [Uart],
    rebind = "firmware",
    signals = {
        "TX": [pin("GPIO21", cost = 0, data = {"iomux_func": 0})] + _matrix(["GPIO21"]),
        "RX": [pin("GPIO20", cost = 0, data = {"iomux_func": 0})] + _matrix(["GPIO20"]),
    },
)

UART1 = peripheral(
    "UART1",
    provides = [Uart],
    rebind = "firmware",
    signals = {"TX": _matrix([]), "RX": _matrix([])},
)

ADC1_CH0 = peripheral("ADC1_CH0", provides = [AdcIn], rebind = "fixed", signals = {"IN": [pin("GPIO0")]})
ADC1_CH1 = peripheral("ADC1_CH1", provides = [AdcIn], rebind = "fixed", signals = {"IN": [pin("GPIO1")]})
ADC1_CH2 = peripheral("ADC1_CH2", provides = [AdcIn], rebind = "fixed", signals = {"IN": [pin("GPIO2")]})
ADC1_CH3 = peripheral("ADC1_CH3", provides = [AdcIn], rebind = "fixed", signals = {"IN": [pin("GPIO3")]})
ADC1_CH4 = peripheral("ADC1_CH4", provides = [AdcIn], rebind = "fixed", signals = {"IN": [pin("GPIO4")]})
ADC2_CH0 = peripheral("ADC2_CH0", provides = [AdcIn], rebind = "fixed", signals = {"IN": [pin("GPIO5")]}, unless = "wifi")

GPIO_POOL = pool(
    "GPIO",
    provides = [Gpio],
    pins = [pin(n, strap = n in _STRAP) for n in _MATRIX],
)

PERIPHS = [UART0, UART1, ADC1_CH0, ADC1_CH1, ADC1_CH2, ADC1_CH3, ADC1_CH4, ADC2_CH0, GPIO_POOL]
"#;

const COMPARATOR: &str = r#"
load("@stdlib/pinmux.zen", "peripheral", "pin")
load("./ifaces.zen", "Comparator")

UNIT_A = peripheral("A", provides = [Comparator], rebind = "none",
    signals = {"INP": [pin("1")], "INN": [pin("2")], "OUT": [pin("3")]})
UNIT_B = peripheral("B", provides = [Comparator], rebind = "none",
    signals = {"INP": [pin("5")], "INN": [pin("6")], "OUT": [pin("7")]})

PERIPHS = [UNIT_A, UNIT_B]
"#;

fn eval_with_fixtures(main: &str) -> WithDiagnostics<EvalOutput> {
    eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/stm32.zen".to_string(), STM32.to_string()),
        ("/esp32c3.zen".to_string(), ESP32C3.to_string()),
        ("/comparator.zen".to_string(), COMPARATOR.to_string()),
        ("/test.zen".to_string(), main.to_string()),
    ])
}

fn assert_ok(result: &WithDiagnostics<EvalOutput>) {
    if !result.is_success() {
        panic!(
            "expected success, got diagnostics: {:#?}",
            result.diagnostics
        );
    }
}

fn diag_text(result: &WithDiagnostics<EvalOutput>) -> String {
    format!("{:#?}", result.diagnostics)
}

fn assert_fails_with(result: &WithDiagnostics<EvalOutput>, needle: &str) {
    assert!(
        !result.is_success(),
        "expected failure containing {needle:?}, but evaluation succeeded"
    );
    let text = diag_text(result);
    assert!(
        text.contains(needle),
        "expected diagnostics to contain {needle:?}, got:\n{text}"
    );
}

fn json_property(result: &WithDiagnostics<EvalOutput>, key: &str) -> serde_json::Value {
    let props = result.output.as_ref().unwrap().sch_module.properties();
    let value = props
        .get(key)
        .unwrap_or_else(|| panic!("missing module property {key:?}"));
    let text = value
        .to_value()
        .unpack_str()
        .unwrap_or_else(|| panic!("module property {key:?} is not a string"));
    serde_json::from_str(text).unwrap()
}

#[test]
fn downgrade_and_poorest_instance() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve", "pin_map")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Usart")

res = pin_solve(PERIPHS, [pin_request("DEBUG", Uart), pin_request("SC", Usart)])
a = res["assignment"]
check(a["DEBUG"]["instance"] == "USART2", "DEBUG got " + a["DEBUG"]["instance"])
check(a["SC"]["instance"] == "USART1", "SC got " + a["SC"]["instance"])
check(a["SC"]["signals"]["TX"]["pin"] == "PA9", "SC TX pin")
check(a["SC"]["signals"]["TX"]["af"] == 1, "SC TX af")
m = pin_map(res["assignment"], {"DEBUG": Uart("D"), "SC": Usart("S")})
check("PB3" in m, "unclaimed candidate must be tied off")
"#,
    );
    assert_ok(&result);
    let props = result.output.as_ref().unwrap().sch_module.properties();
    assert!(
        props.contains_key("pin_assignment"),
        "pin_assignment property missing"
    );
    assert!(
        props.contains_key("swap_classes"),
        "swap_classes property missing"
    );
}

#[test]
fn upgrade_is_impossible() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Usart")

pin_solve(PERIPHS, [pin_request("SC1", Usart), pin_request("SC2", Usart)])
"#,
    );
    assert_fails_with(&result, "no feasible assignment");
}

#[test]
fn instance_exclusive_even_on_disjoint_pins() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart")

pin_solve(PERIPHS, [
    pin_request("U1", Uart),
    pin_request("U2", Uart),
    pin_request("U3", Uart),
])
"#,
    );
    assert_fails_with(&result, "no feasible assignment");
}

#[test]
fn intra_instance_pin_mixing() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Gpio")

res = pin_solve(PERIPHS, [
    pin_request("BTN", Gpio, prefer = ["PA10"], lock = True),
    pin_request("COM", Uart, instance = "USART1"),
])
a = res["assignment"]
check(a["BTN"]["signals"]["PIN"]["pin"] == "PA10", "BTN pin")
check(a["COM"]["signals"]["TX"]["pin"] == "PA9", "COM TX " + a["COM"]["signals"]["TX"]["pin"])
check(a["COM"]["signals"]["RX"]["pin"] == "PB7", "COM RX " + a["COM"]["signals"]["RX"]["pin"])
"#,
    );
    assert_ok(&result);
}

#[test]
fn joint_contention_is_infeasible() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "I2c", "Gpio")

pin_solve(PERIPHS, [
    pin_request("BTN1", Gpio, prefer = ["PA9"], lock = True),
    pin_request("BTN2", Gpio, prefer = ["PA10"], lock = True),
    pin_request("BUS", I2c),
    pin_request("COM", Uart, instance = "USART1"),
])
"#,
    );
    assert_fails_with(&result, "no feasible assignment");
}

#[test]
fn where_predicate_on_unit_attrs() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart")

def fast(a):
    return a["baud_max"] >= "6MHz"

res = pin_solve(PERIPHS, [pin_request("FAST", Uart, where = fast)])
check(res["assignment"]["FAST"]["instance"] == "USART1", "FAST got " + res["assignment"]["FAST"]["instance"])
"#,
    );
    assert_ok(&result);
}

#[test]
fn where_predicate_starves() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Usart")

def fast(a):
    return a["baud_max"] >= "6MHz"

pin_solve(PERIPHS, [
    pin_request("SC", Usart),
    pin_request("FAST", Uart, where = fast),
])
"#,
    );
    assert_fails_with(&result, "no feasible assignment");
}

#[test]
fn gpio_vs_peripheral_exclusivity() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Spi", "Gpio")

res = pin_solve(PERIPHS, [
    pin_request("LED", Gpio, prefer = ["PA5"], lock = True),
    pin_request("FLASH", Spi),
])
check(res["assignment"]["FLASH"]["signals"]["SCK"]["pin"] == "PB3", "SCK fallback")
"#,
    );
    assert_ok(&result);

    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Spi", "Gpio")

pin_solve(PERIPHS, [
    pin_request("LED", Gpio, prefer = ["PA5"], lock = True),
    pin_request("LED2", Gpio, prefer = ["PB3"], lock = True),
    pin_request("FLASH", Spi),
])
"#,
    );
    assert_fails_with(&result, "no feasible assignment");
}

#[test]
fn deterministic_and_stable_across_builds() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Usart", "Gpio", "AdcIn")

reqs = [pin_request("DEBUG", Uart), pin_request("LED", Gpio), pin_request("MES", AdcIn)]
r1 = pin_solve(PERIPHS, reqs)
r2 = pin_solve(PERIPHS, reqs)
check(r1["assignment"] == r2["assignment"], "two identical runs diverged")

r3 = pin_solve(PERIPHS, reqs + [pin_request("SC", Usart)], previous = r1["assignment"])
check(r3["assignment"]["DEBUG"] == r1["assignment"]["DEBUG"], "DEBUG reshuffled")
check(r3["assignment"]["LED"]["signals"] == r1["assignment"]["LED"]["signals"], "LED reshuffled")
check(r3["assignment"]["MES"]["signals"] == r1["assignment"]["MES"]["signals"], "MES reshuffled")
"#,
    );
    assert_ok(&result);
}

#[test]
fn declaration_cannot_overstate() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin")
load("./ifaces.zen", "UartFlow")

peripheral("UARTX", provides = [UartFlow], rebind = "firmware",
    signals = {"TX": [pin("P1")], "RX": [pin("P2")]})
"#,
    );
    assert_fails_with(&result, "no candidate for signal");
}

#[test]
fn esp32_iomux_preferred_over_matrix() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./esp32c3.zen", "PERIPHS")
load("./ifaces.zen", "Uart")

res = pin_solve(PERIPHS, [pin_request("CONSOLE", Uart), pin_request("GPS", Uart)])
a = res["assignment"]
check(a["CONSOLE"]["instance"] == "UART0", "console instance")
check(a["CONSOLE"]["signals"]["TX"]["pin"] == "GPIO21", "console TX iomux")
check(a["CONSOLE"]["signals"]["RX"]["pin"] == "GPIO20", "console RX iomux")
check(a["CONSOLE"]["signals"]["TX"]["iomux_func"] == 0, "iomux realization data")
check(a["GPS"]["instance"] == "UART1", "gps instance")
check(not (a["GPS"]["signals"]["TX"]["pin"] in ["GPIO2", "GPIO8", "GPIO9"]), "gps TX strap avoided")
check(not (a["GPS"]["signals"]["RX"]["pin"] in ["GPIO2", "GPIO8", "GPIO9"]), "gps RX strap avoided")
check(len(a["GPS"]["alternates"]["TX"]) > 0, "matrix alternates missing")
"#,
    );
    assert_ok(&result);
}

#[test]
fn esp32_conditional_capability() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./esp32c3.zen", "PERIPHS")
load("./ifaces.zen", "AdcIn")

pin_solve(
    PERIPHS,
    [pin_request("A" + str(i), AdcIn) for i in range(6)],
    config = {"wifi": True},
)
"#,
    );
    assert_fails_with(&result, "no feasible assignment");

    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./esp32c3.zen", "PERIPHS")
load("./ifaces.zen", "AdcIn")

res = pin_solve(
    PERIPHS,
    [pin_request("A" + str(i), AdcIn) for i in range(6)],
    config = {"wifi": False},
)
insts = [res["assignment"]["A" + str(i)]["instance"] for i in range(6)]
check("ADC2_CH0" in insts, "ADC2 not used: " + str(insts))
"#,
    );
    assert_ok(&result);
}

#[test]
fn forced_strap_pin_warns() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./esp32c3.zen", "PERIPHS")
load("./ifaces.zen", "Gpio")

res = pin_solve(PERIPHS, [pin_request("BOOT_BTN", Gpio, prefer = ["GPIO9"], lock = True)])
check(res["assignment"]["BOOT_BTN"]["signals"]["PIN"]["pin"] == "GPIO9", "locked pin honored")
"#,
    );
    assert_ok(&result);
    let text = diag_text(&result);
    assert!(
        text.contains("strapping pin"),
        "expected a strapping-pin warning, got:\n{text}"
    );
}

#[test]
fn hard_lock_single_pin_on_multi_signal_role() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart")

res = pin_solve(PERIPHS, [pin_request("DEBUG", Uart, prefer = ["PA2"], lock = True)])
a = res["assignment"]["DEBUG"]
check(a["instance"] == "USART2", "expected USART2, got " + a["instance"])
check(a["signals"]["TX"]["pin"] == "PA2", "TX must land on the locked pin")
"#,
    );
    assert_ok(&result);
}

#[test]
fn empty_uses_rejected() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart")

pin_solve(PERIPHS, [pin_request("X", Uart, uses = [])])
"#,
    );
    assert_fails_with(&result, "at least one signal");
}

#[test]
fn infeasible_after_truncation_mentions_the_cap() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")

WIDE = peripheral(
    "WIDE",
    provides = [Uart],
    rebind = "firmware",
    signals = {
        "TX": [pin("P" + str(i)) for i in range(24)],
        "RX": [pin("P" + str(i)) for i in range(24)],
    },
)

pin_solve([WIDE], [pin_request("A", Uart), pin_request("B", Uart)])
"#,
    );
    assert_fails_with(&result, "no feasible assignment");
    assert_fails_with(&result, "capped at");
}

#[test]
fn locked_pin_beyond_the_cap_is_still_found() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")

P = peripheral(
    "P",
    provides = [Gpio],
    rebind = "firmware",
    signals = {"PIN": [pin("P" + str(i)) for i in range(600)]},
)

res = pin_solve([P], [pin_request("R", Gpio, prefer = ["P599"], lock = True)])
check(res["assignment"]["R"]["signals"]["PIN"]["pin"] == "P599", "locked pin must win over the cap")
"#,
    );
    assert_ok(&result);
}

#[test]
fn mandatory_pin_beyond_the_cap_is_still_found() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")

W = peripheral(
    "W",
    provides = [Uart],
    rebind = "firmware",
    signals = {
        "TX": [pin("T" + str(i)) for i in range(30)],
        "RX": [pin("R" + str(i)) for i in range(30)],
    },
)

res = pin_solve([W], [pin_request("U", Uart, prefer = ["T29"], lock = True)])
check(res["assignment"]["U"]["signals"]["TX"]["pin"] == "T29", "mandatory pin must win over the cap")
"#,
    );
    assert_ok(&result);
}

#[test]
fn failed_lock_names_the_pins_in_the_rejection() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Gpio")

pin_solve(PERIPHS, [pin_request("LED", Gpio, prefer = ["NOPE"], lock = True)])
"#,
    );
    assert_fails_with(
        &result,
        "signal `PIN` has no candidate among the locked pins `NOPE`",
    );
}

#[test]
fn at_constraint_survives_solve_before_io() {
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/stm32.zen".to_string(), STM32.to_string()),
        (
            "/mcu_early.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
load("./stm32.zen", "PERIPHS")

res = pin_solve(PERIPHS, [pin_request("IO0", Gpio, if_connected = True)])
a = res["assignment"]
builtin.add_property("io0_pin", a["IO0"]["signals"]["PIN"]["pin"] if "IO0" in a else "none")

IO0 = io(Net, optional = True)
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
Mcu = Module("/mcu_early.zen")
Mcu(name = "M1", IO0 = at(Net("LED"), "PA10"))
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
    assert_eq!(io0_pin(&result), "PA10");
}

#[test]
fn duplicate_uses_rejected() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart")

pin_solve(PERIPHS, [pin_request("X", Uart, uses = ["TX", "TX"])])
"#,
    );
    assert_fails_with(&result, "duplicate signal");
}

#[test]
fn raw_pin_dict_with_reserved_data_key_rejected() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")

forged = {"kind": "pin", "name": "X1", "data": {"pin": "LIE"}, "cost": 0, "input_only": False, "strap": False}
P = peripheral("P", provides = [Gpio], rebind = "fixed", signals = {"PIN": [forged]})
pin_solve([P], [pin_request("R", Gpio)])
"#,
    );
    assert_fails_with(&result, "reserved");
}

#[test]
fn exclusivity_spans_solves_in_one_module() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")

P = peripheral("P", provides = [Gpio], rebind = "fixed", signals = {"PIN": [pin("X1")]})
pin_solve([P], [pin_request("A", Gpio)])
pin_solve([P], [pin_request("B", Gpio)])
"#,
    );
    assert_fails_with(&result, "already claimed by an earlier pin_solve");

    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve", "pool")
load("./ifaces.zen", "Gpio")

POOL = pool("GPIO", provides = [Gpio], pins = ["X1", "X2", "X3"])
r1 = pin_solve(POOL, [pin_request("A", Gpio)])
r2 = pin_solve(POOL, [pin_request("B", Gpio)])
p1 = r1["assignment"]["A"]["signals"]["PIN"]["pin"]
p2 = r2["assignment"]["B"]["signals"]["PIN"]["pin"]
check(p1 != p2, "second solve must avoid the claimed pin, got " + p1 + " twice")
"#,
    );
    assert_ok(&result);
}

#[test]
fn residual_freedom_excludes_prior_solve_claims() {
    // Tied-off pads and pool spare_pins must not list a pin an earlier solve owns.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve", "pool", "pin_map")
load("./ifaces.zen", "Gpio")

POOL = pool("GPIO", provides = [Gpio], pins = ["X1", "X2", "X3"])
r1 = pin_solve(POOL, [pin_request("A", Gpio)])
r2 = pin_solve(POOL, [pin_request("B", Gpio)])
p1 = r1["assignment"]["A"]["signals"]["PIN"]["pin"]
m2 = pin_map(r2["assignment"], {"B": Net("NB")})
check(not (p1 in m2), "the pin claimed by the first solve must not be tied off here")
pools = [c for c in r2["swap_classes"] if c["granularity"] == "pin"]
check(len(pools) == 1, "expected one pool class")
check(p1 not in pools[0]["spare_pins"], "spare_pins must exclude the pin claimed by the first solve")
"#,
    );
    assert_ok(&result);

    // Per-signal alternates must not offer a pin an earlier solve owns.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")

P1 = peripheral("P1", provides = [Gpio], rebind = "firmware",
    signals = {"PIN": [pin("X1"), pin("X2")]})
P2 = peripheral("P2", provides = [Gpio], rebind = "firmware",
    signals = {"PIN": [pin("X1"), pin("X2")]})
r1 = pin_solve([P1, P2], [pin_request("A", Gpio)])
r2 = pin_solve([P1, P2], [pin_request("B", Gpio)])
p1 = r1["assignment"]["A"]["signals"]["PIN"]["pin"]
alts = r2["assignment"]["B"]["alternates"]
check(p1 not in alts.get("PIN", []), "alternates must exclude the pin claimed by the first solve")
"#,
    );
    assert_ok(&result);

    // Cluster spare_units must not offer a unit an earlier solve owns: with
    // both comparator units taken across two solves, no swap class remains.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./comparator.zen", "PERIPHS")
load("./ifaces.zen", "Comparator")

pin_solve(PERIPHS, [pin_request("CMP1", Comparator)])
r2 = pin_solve(PERIPHS, [pin_request("CMP2", Comparator)])
clusters = [c for c in r2["swap_classes"] if c["granularity"] == "cluster"]
check(len(clusters) == 0, "no residual cluster freedom once both units are claimed, got " + str(clusters))
"#,
    );
    assert_ok(&result);
}

#[test]
fn misused_previous_warns() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart")

r1 = pin_solve(PERIPHS, [pin_request("DEBUG", Uart)])
pin_solve(PERIPHS, [pin_request("DEBUG", Uart)], previous = r1)
"#,
    );
    assert_ok(&result);
    let text = diag_text(&result);
    assert!(
        text.contains("no usable assignment entry"),
        "expected a previous= misuse warning, got:\n{text}"
    );
}

#[test]
fn conflicting_attr_dims_rejected() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin")
load("./ifaces.zen", "Frequency")

A = interface(X = Net, attrs = {"m": Frequency})
B = interface(Y = Net, attrs = {"m": builtin.Time})

peripheral("P", provides = [A, B], rebind = "fixed",
    signals = {"X": [pin("P1")], "Y": [pin("P2")]})
"#,
    );
    assert_fails_with(&result, "conflicting dimensions");
}

#[test]
fn two_solves_merge_into_the_module_properties() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Gpio")

pin_solve(PERIPHS, [pin_request("DEBUG", Uart)])
pin_solve(PERIPHS, [pin_request("LED", Gpio)])
"#,
    );
    assert_ok(&result);
    let props = result.output.as_ref().unwrap().sch_module.properties();
    let assignment = format!("{:?}", props.get("pin_assignment"));
    assert!(
        assignment.contains("DEBUG") && assignment.contains("LED"),
        "both solves must reach the property, got:\n{assignment}"
    );
}

#[test]
fn swap_classes_property_merges_pool_solves() {
    // Two solves on one pool merge into a single property class whose spares
    // reflect every claim in the module, not just the last solve's.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve", "pool")
load("./ifaces.zen", "Gpio")

POOL = pool("GPIO", provides = [Gpio], pins = ["X1", "X2", "X3", "X4"])
pin_solve(POOL, [pin_request("A", Gpio)])
pin_solve(POOL, [pin_request("B", Gpio)])
"#,
    );
    assert_ok(&result);
    let swaps = json_property(&result, "swap_classes");
    let classes = swaps.as_array().unwrap();
    assert_eq!(classes.len(), 1, "one merged pool class, got {swaps}");
    let members = classes[0]["members"].as_array().unwrap();
    let requests: Vec<&str> = members
        .iter()
        .map(|m| m["request"].as_str().unwrap())
        .collect();
    assert_eq!(
        requests,
        ["A", "B"],
        "members from both solves, got {swaps}"
    );
    let member_pins: Vec<&str> = members.iter().map(|m| m["pin"].as_str().unwrap()).collect();
    let spares: Vec<&str> = classes[0]["spare_pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert_eq!(spares.len(), 2, "two of four pool pins left, got {swaps}");
    assert!(
        spares.iter().all(|s| !member_pins.contains(s)),
        "spares must exclude claimed pins, got {swaps}"
    );
}

#[test]
fn resolving_one_member_keeps_its_siblings_in_the_property() {
    // Re-solving A must not drop B's membership from the shared class.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve", "pool")
load("./ifaces.zen", "Gpio")

POOL = pool("GPIO", provides = [Gpio], pins = ["X1", "X2"])
pin_solve(POOL, [pin_request("A", Gpio), pin_request("B", Gpio)])
pin_solve(POOL, [pin_request("A", Gpio)])
"#,
    );
    assert_ok(&result);
    let swaps = json_property(&result, "swap_classes");
    let classes = swaps.as_array().unwrap();
    assert_eq!(classes.len(), 1, "one merged pool class, got {swaps}");
    let requests: Vec<&str> = classes[0]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["request"].as_str().unwrap())
        .collect();
    assert_eq!(
        requests,
        ["A", "B"],
        "the sibling survives the re-solve, got {swaps}"
    );
}

#[test]
fn cluster_property_merges_and_refreshes_spares_across_solves() {
    // Cluster classes from separate solves over the same silicon merge, and
    // spare units drop out as later solves occupy them.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Comparator")

UNIT_A = peripheral("A", provides = [Comparator], rebind = "none",
    signals = {"INP": [pin("1")], "INN": [pin("2")], "OUT": [pin("3")]})
UNIT_B = peripheral("B", provides = [Comparator], rebind = "none",
    signals = {"INP": [pin("5")], "INN": [pin("6")], "OUT": [pin("7")]})
UNIT_C = peripheral("C", provides = [Comparator], rebind = "none",
    signals = {"INP": [pin("8")], "INN": [pin("9")], "OUT": [pin("10")]})
PERIPHS = [UNIT_A, UNIT_B, UNIT_C]

pin_solve(PERIPHS, [pin_request("CMP1", Comparator)])
pin_solve(PERIPHS, [pin_request("CMP2", Comparator)])
"#,
    );
    assert_ok(&result);
    let swaps = json_property(&result, "swap_classes");
    let classes = swaps.as_array().unwrap();
    assert_eq!(classes.len(), 1, "one merged cluster class, got {swaps}");
    let members = classes[0]["members"].as_array().unwrap();
    let requests: Vec<&str> = members
        .iter()
        .map(|m| m["request"].as_str().unwrap())
        .collect();
    assert_eq!(
        requests,
        ["CMP1", "CMP2"],
        "members from both solves, got {swaps}"
    );
    let member_units: Vec<&str> = members
        .iter()
        .map(|m| m["instance"].as_str().unwrap())
        .collect();
    let spares: Vec<&str> = classes[0]["spare_units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert_eq!(spares.len(), 1, "one unit left, got {swaps}");
    assert!(
        spares.iter().all(|s| !member_units.contains(s)),
        "spare units must exclude occupied units, got {swaps}"
    );
}

#[test]
fn reserved_interface_kwargs_misuse_mentions_reservation() {
    let result = eval_with_fixtures("A = interface(X = Net, attrs = Net)\n");
    assert_fails_with(&result, "reserved for capability metadata");

    let result = eval_with_fixtures("B = interface(X = Net, implies = Net)\n");
    assert_fails_with(&result, "reserved for capability metadata");

    // A field slipped into the container form is caught by its own contents.
    let result = eval_with_fixtures("C = interface(X = Net, attrs = {\"a\": Net})\n");
    assert_fails_with(&result, "must map to a physical value type");

    let result = eval_with_fixtures("D = interface(X = Net, implies = [Net])\n");
    assert_fails_with(&result, "entries must be interface types");
}

#[test]
fn unconsumed_at_failure_keeps_module_warnings() {
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/esp32c3.zen".to_string(), ESP32C3.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./esp32c3.zen", "PERIPHS")
load("./ifaces.zen", "Gpio")

IO0 = io(Net, optional = True)
pin_solve(PERIPHS, [pin_request("BOOT_BTN", Gpio, prefer = ["GPIO9"], lock = True)])
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
Mcu = Module("/mcu.zen")
Mcu(name = "M1", IO0 = at(Net("LED"), "GPIO4"))
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(&result, "never consumed");
    let text = diag_text(&result);
    assert!(
        text.contains("strapping pin"),
        "module warnings must survive the failure, got:\n{text}"
    );
}

#[test]
fn unconsumed_hard_at_fails_the_build() {
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/plain.zen".to_string(),
            "IO0 = io(Net, optional = True)\n".to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
Mcu = Module("/plain.zen")
Mcu(name = "M1", IO0 = at(Net("LED"), "PA10"))
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(&result, "never consumed");
}

#[test]
fn an_at_inside_a_list_config_is_not_dropped() {
    // Only the dict-of-roles shape reaches a request, so a wrapper handed in a
    // list is reported rather than silently forgotten.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/plain.zen".to_string(),
            "GPIO = config(\"gpio\", list, optional = True)\n".to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
Mcu = Module("/plain.zen")
Mcu(name = "M1", gpio = [at(Net("LED"), "PA10")])
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(&result, "never consumed");
}

#[test]
fn unconsumed_soft_at_is_tolerated() {
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/plain.zen".to_string(),
            "IO0 = io(Net, optional = True)\n".to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
Mcu = Module("/plain.zen")
Mcu(name = "M1", IO0 = at(Net("LED"), "PA10", soft = True))
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
}

#[test]
fn a_factory_built_capability_is_fresh_per_binding() {
    // An interface() minted inside a helper is keyed to the module that binds
    // it, so two callers hold two capabilities. The solve says so instead of
    // quietly failing to match.
    let result = eval_zen(vec![
        (
            "/mk.zen".to_string(),
            "def mk():\n    return interface(PIN = Net)\n".to_string(),
        ),
        (
            "/chip.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pool")
load("./mk.zen", "mk")
Cap = mk()
PERIPHS = [pool("GPIO", provides = [Cap], pins = ["PA0", "PA1"])]
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./mk.zen", "mk")
load("./chip.zen", "PERIPHS")
Cap = mk()
r = pin_solve(PERIPHS, [pin_request("A", Cap)])
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(
        &result,
        "provides a different Cap — capability types are per interface() declaration",
    );
}

#[test]
fn an_unnamed_factory_capability_stands_on_its_own() {
    // No top-level binding to key on, so each interface() call is its own
    // capability — two widths from one helper never pass for each other.
    let result = eval_zen(vec![
        (
            "/mk.zen".to_string(),
            "def mk(n):\n    return interface(PIN = Net, width = field(int, default = n))\n"
                .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./mk.zen", "mk")
CAPS = [mk(8), mk(16)]
P = [pool("GPIO", provides = [CAPS[0]], pins = ["PA0", "PA1"])]
r = pin_solve(P, [pin_request("A", CAPS[1])])
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(&result, "capability types are per interface() declaration");
}

/// The module makes a pin mandatory; the caller only wishes for another.
fn soft_over_lock(arg: &str, module: &str) -> WithDiagnostics<EvalOutput> {
    eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/mcu.zen".to_string(), module.to_string()),
        (
            "/test.zen".to_string(),
            format!(
                r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio", "Uart")
Mcu = Module("/mcu.zen")
Mcu(name = "U1", {arg})
"#
            ),
        ),
    ])
}

fn solved_pin(result: &WithDiagnostics<EvalOutput>) -> String {
    assert_ok(result);
    result
        .output
        .as_ref()
        .and_then(|o| {
            o.module_tree()
                .values()
                .filter_map(|m| m.properties().get("led"))
                .filter_map(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
                .next()
        })
        .unwrap_or_default()
}

const LOCKED_GPIO: &str = r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA5", "PA6"])
LED = io(Gpio)
res = pin_solve([P], [pin_request("LED", Gpio, prefer = ["PA6"], lock = True)])
builtin.add_property("led", res["assignment"]["LED"]["signals"]["PIN"]["pin"])
"#;

#[test]
fn a_soft_at_cannot_void_a_lock_the_module_set() {
    // A wish must not achieve what a hard at() could not: PA6 is mandatory,
    // so wishing elsewhere drops the wish, never the requirement.
    let none = soft_over_lock("LED = Gpio(\"L\")", LOCKED_GPIO);
    assert_eq!(solved_pin(&none), "PA6", "the module's own pin");

    let absent = soft_over_lock("LED = at(Gpio(\"L\"), \"PA9\", soft = True)", LOCKED_GPIO);
    assert_eq!(
        solved_pin(&absent),
        "PA6",
        "a wish for a pad the part lacks"
    );

    let elsewhere = soft_over_lock("LED = at(Gpio(\"L\"), \"PA5\", soft = True)", LOCKED_GPIO);
    assert_eq!(
        solved_pin(&elsewhere),
        "PA6",
        "a wish for another of its pads"
    );

    // A hard at() still overrides, as documented.
    let hard = soft_over_lock("LED = at(Gpio(\"L\"), \"PA5\")", LOCKED_GPIO);
    assert_eq!(solved_pin(&hard), "PA5", "a hard caller at() wins");
}

const UNLOCKED_GPIO: &str = r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA4", "PA5", "PA6"])
LED = io(Gpio)
res = pin_solve([P], [pin_request("LED", Gpio, prefer = ["PA6"])])
builtin.add_property("led", res["assignment"]["LED"]["signals"]["PIN"]["pin"])
"#;

#[test]
fn a_soft_at_falls_back_to_what_the_request_asked_for() {
    // Nothing is mandatory here, so the wish outranks the request's own
    // preference — but only where it can be met.
    let none = soft_over_lock("LED = Gpio(\"L\")", UNLOCKED_GPIO);
    assert_eq!(solved_pin(&none), "PA6", "the request's own preference");

    let absent = soft_over_lock("LED = at(Gpio(\"L\"), \"PA9\", soft = True)", UNLOCKED_GPIO);
    assert_eq!(
        solved_pin(&absent),
        "PA6",
        "an unmeetable wish leaves it standing"
    );

    let real = soft_over_lock("LED = at(Gpio(\"L\"), \"PA5\", soft = True)", UNLOCKED_GPIO);
    assert_eq!(solved_pin(&real), "PA5", "a wish that can be met wins");
}

const LOCKED_UART: &str = r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")
U = peripheral("U", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("PA5"), pin("PA6")], "RX": [pin("PA7")]})
LED = io(Uart)
res = pin_solve([U], [pin_request("LED", Uart, prefer = {"TX": ["PA5", "PA6"]}, lock = True)])
builtin.add_property("led", res["assignment"]["LED"]["signals"]["TX"]["pin"])
"#;

#[test]
fn a_soft_at_still_chooses_within_the_locked_pins() {
    // Both pads are mandatory-set members, so the wish is what decides.
    let none = soft_over_lock("LED = Uart(\"L\")", LOCKED_UART);
    assert_eq!(solved_pin(&none), "PA5");

    let wish = soft_over_lock(
        "LED = at(Uart(\"L\"), {\"TX\": [\"PA6\"]}, soft = True)",
        LOCKED_UART,
    );
    assert_eq!(solved_pin(&wish), "PA6", "the wish picks among them");
}

#[test]
fn an_io_that_consumed_an_at_records_the_bare_value() {
    // Once io() has handed the constraint to the store, the module keeps what
    // the caller meant to connect — a PinAt left here would reach anything
    // that reads module inputs.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA0", "PA1"])
LED = io(Gpio)
builtin.add_property("kind", str(type(LED)))
res = pin_solve([P], [pin_request("LED", Gpio)])
builtin.add_property("led", res["assignment"]["LED"]["signals"]["PIN"]["pin"])
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
Mcu = Module("/mcu.zen")
Mcu(name = "U1", LED = at(Gpio("L"), "PA1"))
"#
            .to_string(),
        ),
    ]);
    let inputs: Vec<String> = result
        .output
        .as_ref()
        .map(|o| {
            o.module_tree()
                .values()
                .flat_map(|m| {
                    m.inputs()
                        .iter()
                        .map(|(k, v)| format!("{k}:{}", v.to_value().get_type()))
                        .collect::<Vec<_>>()
                })
                .collect()
        })
        .unwrap_or_default();
    assert_ok(&result);
    assert_eq!(inputs, ["LED:InterfaceValue"], "got {inputs:?}");
}

#[test]
fn a_part_cannot_be_configured_two_ways() {
    // The tie-off list carries pads across solves, so an axis that flipped
    // would make the component's pin table depend on which solve ran last.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Gpio")
A = peripheral("A", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA0")]}, unless = "wifi")
B = peripheral("B", provides = [Gpio], rebind = "fixed", signals = {"PIN": [pin("PA1")]})
r1 = pin_solve([A, B], [pin_request("X", Gpio, instance = "B")], config = {"wifi": False})
r2 = pin_solve([A, B], [], config = {"wifi": True})
"#,
    );
    assert_fails_with(
        &result,
        "config axis `wifi` was false for this part in an earlier solve",
    );

    // …even when the later table leaves the gated peripheral out but still
    // speaks about its axis.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
A = peripheral("A", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA0")]}, unless = "wifi")
B = peripheral("B", provides = [Gpio], rebind = "fixed", signals = {"PIN": [pin("PA1")]})
pin_solve([A, B], [pin_request("X", Gpio, instance = "B")], config = {"wifi": False})
pin_solve([B], [], config = {"wifi": True})
"#,
    );
    assert_fails_with(&result, "config axis `wifi` was false for this part");

    // Said the same way twice, the gated pad stays out of the tie-off.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Gpio")
A = peripheral("A", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA0")]}, unless = "wifi")
B = peripheral("B", provides = [Gpio], rebind = "fixed", signals = {"PIN": [pin("PA1")]})
r1 = pin_solve([A, B], [pin_request("X", Gpio, instance = "B")], config = {"wifi": True})
r2 = pin_solve([A, B], [], config = {"wifi": True})
m = pin_map(r1["assignment"], {"X": Net("N")})
check(sorted(m.keys()) == ["PA1"], "the wifi pad stays out, got " + str(sorted(m.keys())))
"#,
    );
    assert_ok(&result);
}

#[test]
fn a_role_dict_records_the_net_not_the_wrapper() {
    // The constraint goes to the store; what the module records is the
    // connection, or the netlist would carry a placeholder for that role.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA0", "PA1"])
GPIO = config("gpio", dict, optional = True)
res = pin_solve([P], [pin_request(k, Gpio, bind = v) for k, v in (GPIO or {}).items()])
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
Mcu = Module("/mcu.zen")
Mcu(name = "U1", gpio = {"LED": at(Gpio("L"), "PA1")})
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);

    let recorded: Vec<String> = result
        .output
        .as_ref()
        .map(|o| {
            o.module_tree()
                .values()
                .flat_map(|m| {
                    m.signature()
                        .iter()
                        .filter(|p| p.name == "gpio")
                        .filter_map(|p| p.actual_value.as_ref())
                        .map(|v| v.to_value().to_string())
                        .collect::<Vec<_>>()
                })
                .collect()
        })
        .unwrap_or_default();
    assert!(
        recorded.iter().all(|v| !v.contains("at(")),
        "the role holds its net, got {recorded:?}"
    );
    assert!(
        recorded.iter().any(|v| v.contains("LED")),
        "and the net is still there, got {recorded:?}"
    );
}

#[test]
fn two_ats_on_one_net_read_the_same_either_side_of_io() {
    // Each input carries its own constraint, so which side of the solve the
    // io() calls sit on must not change the answer.
    for (label, body) in [
        (
            "solve after io()",
            r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["P1", "P2", "P3"])
A = io(Gpio)
B = io(Gpio)
res = pin_solve([P], [pin_request("A", Gpio), pin_request("B", Gpio)])
builtin.add_property("a", res["assignment"]["A"]["signals"]["PIN"]["pin"])
builtin.add_property("b", res["assignment"]["B"]["signals"]["PIN"]["pin"])
"#,
        ),
        (
            "solve before io()",
            r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["P1", "P2", "P3"])
res = pin_solve([P], [pin_request("A", Gpio), pin_request("B", Gpio)])
A = io(Gpio)
B = io(Gpio)
builtin.add_property("a", res["assignment"]["A"]["signals"]["PIN"]["pin"])
builtin.add_property("b", res["assignment"]["B"]["signals"]["PIN"]["pin"])
"#,
        ),
    ] {
        let result = eval_zen(vec![
            ("/ifaces.zen".to_string(), IFACES.to_string()),
            ("/mcu.zen".to_string(), body.to_string()),
            (
                "/test.zen".to_string(),
                r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
N = Gpio("SHARED")
Mcu = Module("/mcu.zen")
Mcu(name = "U1", A = at(N, "P2"), B = at(N, "P3"))
"#
                .to_string(),
            ),
        ]);
        assert_ok(&result);
        let pins: Vec<String> = result
            .output
            .as_ref()
            .map(|o| {
                o.module_tree()
                    .values()
                    .flat_map(|m| {
                        ["a", "b"].iter().filter_map(move |k| {
                            m.properties()
                                .get(*k)
                                .and_then(|v| v.to_value().unpack_str().map(|s| format!("{k}={s}")))
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(pins, ["a=P2", "b=P3"], "{label}, got {pins:?}");
    }
}

#[test]
fn alternates_leave_out_pads_the_request_could_not_take() {
    // An output request never lands on an input-only pad, so its reported
    // freedom must not offer one either.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = peripheral("P", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA0"), pin("PA1", input_only = True), pin("PA2")]})
res = pin_solve([P], [pin_request("A", Gpio, direction = "output")])
alts = res["assignment"]["A"]["alternates"]["PIN"]
check(alts == ["PA2"], "input-only pads are not alternates, got " + str(alts))
"#,
    );
    assert_ok(&result);
}

#[test]
fn an_assignment_serving_nobody_says_so() {
    // Nothing was served, so the map has no part to infer; the message must
    // point at that rather than claim the assignment spans components.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Gpio")
A = pool("GPIO", part = "U1", provides = [Gpio], pins = ["PA0"])
B = pool("GPIO", part = "U2", provides = [Gpio], pins = ["PB0"])
pin_solve([A], [pin_request("X", Gpio)])
r = pin_solve([B], [])
m = pin_map(r["assignment"], {})
"#,
    );
    assert_fails_with(
        &result,
        "serves no request and the module solved `U1`, `U2`",
    );
}

#[test]
fn mapping_a_part_twice_yields_the_same_table() {
    // Mapping in several calls is legitimate, so a second call must repeat the
    // first: same pads, and the tie-offs reuse the nets the first call minted.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA0", "PA1", "PA2"])
r = pin_solve([P], [pin_request("A", Gpio)])
BUS = Gpio("N")
m1 = pin_map(r["assignment"], {"A": BUS})
m2 = pin_map(r["assignment"], {"A": BUS})
check(sorted(m1.keys()) == sorted(m2.keys()), "both calls yield the same table")
check(sorted(m1.keys()) == ["PA0", "PA1", "PA2"], "and it is the whole table")
"#,
    );
    assert_ok(&result);
}

#[test]
fn a_locked_list_as_long_as_the_signals_is_claimed_in_full() {
    // As many pins as signals, so "every pin ends up on some signal" and "each
    // signal takes one of these" are the same requirement: pads are exclusive.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")
U = peripheral("U", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("PA9"), pin("PB6")], "RX": [pin("PA10")]})
res = pin_solve([U], [pin_request("A", Uart, prefer = ["PA9", "PB6"], lock = True)])
"#,
    );
    // RX reaches neither pin, so the second one could never be used at all.
    assert_fails_with(
        &result,
        "signal `RX` has no candidate among the locked pins `PA9`, `PB6`",
    );

    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")
U = peripheral("U", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("PA9"), pin("PB6")], "RX": [pin("PA10"), pin("PB6")]})
res = pin_solve([U], [pin_request("A", Uart, prefer = ["PA9", "PB6"], lock = True)])
sig = res["assignment"]["A"]["signals"]
check(sorted([sig["TX"]["pin"], sig["RX"]["pin"]]) == ["PA9", "PB6"],
      "both locked pins are used, got " + str(sig))
"#,
    );
    assert_ok(&result);
}

#[test]
fn a_list_borne_at_is_not_claimed_by_a_like_named_request() {
    // The list shape reaches no request, and the request named like the config
    // cannot pair with it either — so it is reported, never quietly applied.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA0", "PA1"])
led_list = config("led", list, optional = True)
LED = io(Gpio)
res = pin_solve([P], [pin_request("led", Gpio)])
builtin.add_property("led_pin", res["assignment"]["led"]["signals"]["PIN"]["pin"])
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
Mcu = Module("/mcu.zen")
Mcu(name = "U1", led = [at(Gpio("L"), "PA1")], LED = Gpio("M"))
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(
        &result,
        "at() pin constraint on input `led` was never consumed",
    );
    let pin: Vec<String> = result
        .output
        .as_ref()
        .map(|o| {
            o.module_tree()
                .values()
                .filter_map(|m| m.properties().get("led_pin"))
                .filter_map(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        pin,
        ["PA0"],
        "the request solved unconstrained, got {pin:?}"
    );
}

#[test]
fn three_solves_keep_one_class_per_pool() {
    // The third solve places elsewhere, so the pool's class rides along
    // whole rather than splitting off a second one.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio", "AdcIn")
P = pool("GPIO", provides = [Gpio], pins = ["PA0", "PA1", "PA2", "PA3"])
Q = pool("ADC", provides = [AdcIn], pins = ["PB0", "PB1"])
a = pin_solve([P, Q], [pin_request("A", Gpio)])
b = pin_solve([P, Q], [pin_request("B", Gpio)])
c = pin_solve([P, Q], [pin_request("C", AdcIn)])
"#,
    );
    assert_ok(&result);

    let swaps = json_property(&result, "swap_classes");
    let classes = swaps.as_array().unwrap();
    let mut pools: Vec<&str> = classes
        .iter()
        .map(|c| c["pool"].as_str().unwrap())
        .collect();
    pools.sort();
    assert_eq!(pools, ["ADC", "GPIO"], "one class per pool, got {swaps}");
    let gpio = classes.iter().find(|c| c["pool"] == "GPIO").unwrap();
    assert_eq!(
        gpio["members"].as_array().unwrap().len(),
        2,
        "both requests in it, got {swaps}"
    );
}

#[test]
fn a_bound_soft_at_cannot_void_the_requests_own_lock() {
    // Whichever path delivers the connection, a wish chooses among the pins
    // the request made mandatory rather than replacing them.
    let solve = |bind: &str| {
        eval_with_fixtures(&format!(
            r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve", "at")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA5", "PA6"])
res = pin_solve([P], [pin_request("LED", Gpio, prefer = ["PA6"], lock = True, bind = {bind})])
builtin.add_property("led", res["assignment"]["LED"]["signals"]["PIN"]["pin"])
"#
        ))
    };
    let pin = |r: &WithDiagnostics<EvalOutput>| {
        assert_ok(r);
        r.output
            .as_ref()
            .and_then(|o| {
                o.module_tree()
                    .values()
                    .filter_map(|m| m.properties().get("led"))
                    .filter_map(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
                    .next()
            })
            .unwrap_or_default()
    };

    let soft = solve("at(Gpio(\"D\"), \"PA5\", soft = True)");
    assert_eq!(pin(&soft), "PA6", "the lock stands against a wish");
    let hard = solve("at(Gpio(\"D\"), \"PA5\")");
    assert_eq!(pin(&hard), "PA5", "a hard bound at() still overrides");
}

#[test]
fn a_forwarded_at_on_a_taken_pad_is_reported_not_forced() {
    // An earlier solve holds PA7, so no request here can honour the pinned
    // pad. Judging one "usable" would lock it into an infeasible solve and
    // blame the request; the constraint is what could not be met.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio", "AdcIn")
G = pool("GPIO", provides = [Gpio], pins = ["PA5", "PA7"])
A = peripheral("A", provides = [AdcIn], rebind = "fixed",
    signals = {"IN": [pin("PA1"), pin("PA2")]})
first = pin_solve([G, A], [pin_request("PRE", Gpio, prefer = ["PA7"], lock = True)])
ADC = io(AdcIn)
LED = io(Gpio)
res = pin_solve([G, A], [pin_request("ADC", AdcIn), pin_request("LED", Gpio)])
builtin.add_property("led", res["assignment"]["LED"]["signals"]["PIN"]["pin"])
"#
            .to_string(),
        ),
        (
            "/som.zen".to_string(),
            r#"
load("./ifaces.zen", "Gpio", "AdcIn")
RAIL = io(Gpio)
Mcu = Module("./mcu.zen")
Mcu(name = "U1", LED = RAIL, ADC = AdcIn(IN = RAIL.PIN))
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
Som = Module("./som.zen")
Som(name = "SOM", RAIL = at(Gpio("RAIL_NET"), "PA7"))
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(&result, "was never consumed");
}

#[test]
fn an_at_on_a_component_pin_says_where_it_belongs() {
    // The wrapper only means something on a module input; a pin sees a value
    // it cannot read, and the message says so rather than naming a type.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "at")
Component(
    name = "R1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": at(Net("LED"), "PA10"), "P2": Net("GND")},
    skip_bom = True,
)
"#,
    );
    assert_fails_with(
        &result,
        "at() constrains a module input, not a component pin",
    );
}

#[test]
fn alternates_leave_out_pads_the_lock_forbids() {
    // TX is nailed to PA9, so it has nowhere to move; RX is free and says so.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")
U = peripheral("U", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("PA9"), pin("PB6")], "RX": [pin("PA10"), pin("PB7")]})
res = pin_solve([U], [pin_request("A", Uart, prefer = {"TX": ["PA9"]}, lock = True)])
alts = res["assignment"]["A"]["alternates"]
check("TX" not in alts, "a locked signal has no alternate, got " + str(alts))
check(alts["RX"] == ["PB7"], "the free one keeps its own, got " + str(alts))
"#,
    );
    assert_ok(&result);

    // A bare list shorter than the signal count restricts nothing per signal,
    // but must be claimed in full — so TX cannot leave PA9 either.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")
U = peripheral("U", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("PA9"), pin("PB6")], "RX": [pin("PA10"), pin("PB7")]})
res = pin_solve([U], [pin_request("A", Uart, prefer = ["PA9"], lock = True)])
check(res["assignment"]["A"]["signals"]["TX"]["pin"] == "PA9", "TX holds the locked pin")
alts = res["assignment"]["A"]["alternates"]
check("TX" not in alts, "leaving PA9 would drop it, got " + str(alts))
check(alts["RX"] == ["PB7"], "RX is free, got " + str(alts))
"#,
    );
    assert_ok(&result);
}

#[test]
fn a_role_named_like_a_config_still_gets_its_at() {
    // Role names come from caller data and may collide with a config() name;
    // the role carries its own value, so the collision must not disarm it.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/stm32.zen".to_string(), STM32.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Gpio")

LED = config("LED", str, optional = True)
GPIO = config("gpio", dict, optional = True)
res = pin_solve(PERIPHS, [pin_request(k, Gpio, bind = v) for k, v in (GPIO or {}).items()])
if "LED" in res["assignment"]:
    check(res["assignment"]["LED"]["signals"]["PIN"]["pin"] == "PA10",
          "the role\'s at() must apply, got " + res["assignment"]["LED"]["signals"]["PIN"]["pin"])
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
Mcu = Module("/mcu.zen")
Mcu(name = "M1", gpio = {"LED": at(Net("L"), "PA10")})
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
}

#[test]
fn bind_at_overrides_request_prefer() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "at", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Gpio")

res = pin_solve(PERIPHS, [
    pin_request("LED", Gpio, prefer = ["PB0"], bind = at(Net("LED"), "PA10")),
])
check(res["assignment"]["LED"]["signals"]["PIN"]["pin"] == "PA10", "caller at() must win over request prefer")
"#,
    );
    assert_ok(&result);
}

#[test]
fn symmetric_parameter_is_rejected() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral")
load("./ifaces.zen", "Gpio")

peripheral("P", provides = [Gpio], rebind = "none", signals = {"PIN": ["X1"]}, symmetric = [["A"]])
"#,
    );
    assert_fails_with(&result, "symmetric");
}

#[test]
fn pin_data_above_i64_roundtrips_as_an_integer() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
BIG = 18446744073709551615
P = peripheral("P", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("X1", data = {"big": BIG})]})
res = pin_solve([P], [pin_request("R", Gpio)])
got = res["assignment"]["R"]["signals"]["PIN"]["big"]
check(got == BIG, "must round-trip, got " + str(got))
check(type(got) == "int", "and stay an int, got " + type(got))
"#,
    );
    assert_ok(&result);
}

#[test]
fn pin_data_large_int_roundtrip() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")

P = peripheral(
    "P",
    provides = [Gpio],
    rebind = "fixed",
    signals = {"PIN": [pin("X1", data = {"big": 5000000000})]},
)

res = pin_solve([P], [pin_request("R", Gpio)])
check(res["assignment"]["R"]["signals"]["PIN"]["big"] == 5000000000, "large data value must round-trip")
"#,
    );
    assert_ok(&result);
}

#[test]
fn pinmap_cap_truncation_warns() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")

WIDE = peripheral(
    "WIDE",
    provides = [Uart],
    rebind = "firmware",
    signals = {
        "TX": [pin("P" + str(i)) for i in range(24)],
        "RX": [pin("P" + str(i)) for i in range(24)],
    },
)

res = pin_solve([WIDE], [pin_request("LINK", Uart)])
check(res["assignment"]["LINK"]["instance"] == "WIDE", "assigned despite cap")
"#,
    );
    assert_ok(&result);
    let text = diag_text(&result);
    assert!(
        text.contains("capped at"),
        "expected a pin-combination cap warning, got:\n{text}"
    );
}

#[test]
fn gpio_pool_swap_class() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./esp32c3.zen", "PERIPHS")
load("./ifaces.zen", "Gpio")

res = pin_solve(PERIPHS, [
    pin_request("LED1", Gpio),
    pin_request("LED2", Gpio),
    pin_request("BTN", Gpio),
])
pools = [c for c in res["swap_classes"] if c["granularity"] == "pin"]
check(len(pools) == 1, "expected one pool class, got " + str(len(pools)))
check(len(pools[0]["members"]) == 3, "members")
check(len(pools[0]["spare_pins"]) > 0, "spares")
check(pools[0]["rebind"] == "firmware", "rebind")
"#,
    );
    assert_ok(&result);
}

#[test]
fn gate_swap_cluster_class() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./comparator.zen", "PERIPHS")
load("./ifaces.zen", "Comparator")

res = pin_solve(PERIPHS, [pin_request("CMP1", Comparator), pin_request("CMP2", Comparator)])
clusters = [c for c in res["swap_classes"] if c["granularity"] == "cluster"]
check(len(clusters) == 1, "expected one cluster class")
check(len(clusters[0]["members"]) == 2, "cluster members")
check(clusters[0]["rebind"] == "none", "cluster rebind")
"#,
    );
    assert_ok(&result);

    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./comparator.zen", "PERIPHS")
load("./ifaces.zen", "Comparator")

pin_solve(PERIPHS, [pin_request("C" + str(i), Comparator) for i in range(3)])
"#,
    );
    assert_fails_with(&result, "no feasible assignment");
}

#[test]
fn unconnected_signals_leave_pins_free() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Gpio")

res = pin_solve(PERIPHS, [
    pin_request("DBG", Uart, instance = "USART1", uses = ["TX"]),
    pin_request("BTN", Gpio, prefer = ["PA10"], lock = True),
])
a = res["assignment"]
check(a["DBG"]["signals"]["TX"]["pin"] == "PA9", "DBG TX")
check(not ("RX" in a["DBG"]["signals"]), "RX should not be assigned")
check(a["BTN"]["signals"]["PIN"]["pin"] == "PA10", "BTN on freed pin")
"#,
    );
    assert_ok(&result);
}

#[test]
fn optimal_not_merely_greedy() {
    // Greedy first-feasible would let A grab (P1, X, cost 0) and force B onto
    // (P2, Z, cost 10) — total 10. Branch-and-bound must return the global
    // optimum (total 1): A on P2/X, B on P1/Y.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")

P1 = peripheral("P1", provides = [Gpio], rebind = "firmware",
    signals = {"PIN": [pin("X"), pin("Y", cost = 1)]})
P2 = peripheral("P2", provides = [Gpio], rebind = "firmware",
    signals = {"PIN": [pin("X"), pin("Z", cost = 10)]})

res = pin_solve([P1, P2], [pin_request("A", Gpio), pin_request("B", Gpio)])
a = res["assignment"]
pins = [a["A"]["signals"]["PIN"]["pin"], a["B"]["signals"]["PIN"]["pin"]]
check(not ("Z" in pins), "suboptimal: Z (cost 10) used while X+Y (cost 1) was feasible: " + str(pins))
"#,
    );
    assert_ok(&result);
}

const MCU_SLOTS: &str = r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart", "Usart")
load("./stm32.zen", "PERIPHS")

DEBUG = io(Uart, optional = True)
SC = io(Usart, optional = True)

res = pin_solve(PERIPHS, [
    pin_request("DEBUG", Uart, if_connected = True),
    pin_request("SC", Usart, if_connected = True),
])
builtin.add_property("dbg_served", str("DEBUG" in res["assignment"]))
builtin.add_property("sc_served", str("SC" in res["assignment"]))
"#;

#[test]
fn if_connected_serves_only_connected_slots() {
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/stm32.zen".to_string(), STM32.to_string()),
        ("/mcu.zen".to_string(), MCU_SLOTS.to_string()),
        (
            "/test.zen".to_string(),
            r#"
load("./ifaces.zen", "Uart")
Mcu = Module("/mcu.zen")
Mcu(name = "MCU1", DEBUG = Uart("DBG"))
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
    let tree = result.output.as_ref().unwrap().module_tree();
    let child = tree
        .values()
        .find(|m| m.properties().contains_key("dbg_served"))
        .expect("child MCU module with pinmux properties not found");
    let get = |key: &str| {
        child
            .properties()
            .get(key)
            .and_then(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
            .unwrap_or_default()
    };
    assert_eq!(get("dbg_served"), "True", "connected slot must be served");
    assert_eq!(
        get("sc_served"),
        "False",
        "unconnected slot must be dropped"
    );
}

#[test]
fn pin_map_builds_component_pins() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_map", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Gpio")

DBG = Uart("DBG")
LED = Net("LED")

res = pin_solve(PERIPHS, [
    pin_request("COM", Uart, instance = "USART1"),
    pin_request("LED", Gpio, prefer = ["PB0"], lock = True),
])
m = pin_map(res["assignment"], {"COM": DBG, "LED": LED})
check(m["PA9"] == DBG.TX and m["PA10"] == DBG.RX, "mapped pins present")
check(m["PA9"] == DBG.TX, "PA9 must carry DBG.TX")
check(m["PA10"] == DBG.RX, "PA10 must carry DBG.RX")
check(m["PB0"] == LED, "PB0 must carry the LED net")
"#,
    );
    assert_ok(&result);
}

const MCU_AT: &str = r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
load("./stm32.zen", "PERIPHS")

IO0 = io(Net, optional = True)

res = pin_solve(PERIPHS, [pin_request("IO0", Gpio, if_connected = True)])
a = res["assignment"]
builtin.add_property("io0_pin", a["IO0"]["signals"]["PIN"]["pin"] if "IO0" in a else "none")
"#;

fn eval_mcu_at(parent: &str) -> WithDiagnostics<EvalOutput> {
    eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/stm32.zen".to_string(), STM32.to_string()),
        ("/mcu_at.zen".to_string(), MCU_AT.to_string()),
        ("/test.zen".to_string(), parent.to_string()),
    ])
}

fn io0_pin(result: &WithDiagnostics<EvalOutput>) -> String {
    let tree = result.output.as_ref().unwrap().module_tree();
    tree.values()
        .filter_map(|m| m.properties().get("io0_pin").cloned())
        .filter_map(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
        .find(|s| s != "none")
        .unwrap_or_default()
}

#[test]
fn at_constrains_pin_on_the_connection() {
    let result = eval_mcu_at(
        r#"
load("@stdlib/pinmux.zen", "at")
Mcu = Module("/mcu_at.zen")
Mcu(name = "M1", IO0 = at(Net("LED"), "PA10"))
"#,
    );
    assert_ok(&result);
    assert_eq!(io0_pin(&result), "PA10");
}

#[test]
fn at_hard_constraint_fails_loudly() {
    // PA13 is deliberately absent from the fixture's GPIO pool.
    let result = eval_mcu_at(
        r#"
load("@stdlib/pinmux.zen", "at")
Mcu = Module("/mcu_at.zen")
Mcu(name = "M1", IO0 = at(Net("LED"), "PA13"))
"#,
    );
    assert_fails_with(&result, "has no candidate");
}

#[test]
fn at_soft_constraint_falls_back() {
    let result = eval_mcu_at(
        r#"
load("@stdlib/pinmux.zen", "at")
Mcu = Module("/mcu_at.zen")
Mcu(name = "M1", IO0 = at(Net("LED"), "PA13", soft = True))
"#,
    );
    assert_ok(&result);
    let pin = io0_pin(&result);
    assert!(
        !pin.is_empty() && pin != "PA13",
        "soft must fall back, got {pin:?}"
    );
}

const MCU_ROLES: &str = r#"
load("@stdlib/pinmux.zen", "pin_map", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio", "AdcIn")
load("./stm32.zen", "PERIPHS")

gpio = config(dict, default = {})
adc = config(dict, default = {})

res = pin_solve(
    PERIPHS,
    [pin_request(n, Gpio, bind = gpio[n]) for n in gpio]
    + [pin_request(n, AdcIn, bind = adc[n]) for n in adc],
)
named = {}
named.update(gpio)
named.update(adc)
a = res["assignment"]
m = pin_map(res["assignment"], named)

builtin.add_property("led_pin", a["LED"]["signals"]["PIN"]["pin"] if "LED" in a else "none")
builtin.add_property("vbat_pin", a["VBAT"]["signals"]["IN"]["pin"] if "VBAT" in a else "none")
_led = a["LED"]["signals"]["PIN"]["pin"] if "LED" in a else "?"
_vbat = a["VBAT"]["signals"]["IN"]["pin"] if "VBAT" in a else "?"
builtin.add_property("map_roles", str(int(_led in m) + int(_vbat in m)))
"#;

#[test]
fn dict_of_roles_demands() {
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/stm32.zen".to_string(), STM32.to_string()),
        ("/mcu_roles.zen".to_string(), MCU_ROLES.to_string()),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
Mcu = Module("/mcu_roles.zen")
Mcu(name = "M1", gpio = {"LED": at(Net("LED"), "PA10")}, adc = {"VBAT": Net("VBAT")})
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
    let tree = result.output.as_ref().unwrap().module_tree();
    let child = tree
        .values()
        .find(|m| m.properties().contains_key("led_pin"))
        .expect("child module with role properties not found");
    let get = |key: &str| {
        child
            .properties()
            .get(key)
            .and_then(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
            .unwrap_or_default()
    };
    assert_eq!(get("led_pin"), "PA10", "at() constraint through the dict");
    assert_eq!(get("vbat_pin"), "PA0", "ADC role served");
    assert_eq!(get("map_roles"), "2", "pin_map must cover both roles");
}

#[test]
fn attr_vocabulary_is_enforced() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin")
load("./ifaces.zen", "Uart")

peripheral("U9", provides = [Uart], rebind = "firmware",
    signals = {"TX": [pin("P1")], "RX": [pin("P2")]},
    attrs = {"baudMax": "8MHz"})
"#,
    );
    assert_fails_with(&result, "not declared by any provided interface");

    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin")
load("./ifaces.zen", "Uart")

peripheral("U9", provides = [Uart], rebind = "firmware",
    signals = {"TX": [pin("P1")], "RX": [pin("P2")]},
    attrs = {"baud_max": "3.3V"})
"#,
    );
    assert_fails_with(&result, "expects");

    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin")
load("./ifaces.zen", "Spi")

peripheral("S9", provides = [Spi], rebind = "firmware",
    signals = {"SCK": [pin("P1")], "MISO": [pin("P2")], "MOSI": [pin("P3")]},
    attrs = {"clk_max": "10MHz"})
"#,
    );
    assert_fails_with(&result, "declare no attrs");
}

#[test]
fn vio_attr_selects_fixed_level_provider() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "I2c")

B3V3 = peripheral("B3V3", provides = [I2c], rebind = "fixed",
    signals = {"SDA": [pin("P1")], "SCL": [pin("P2")]},
    attrs = {"vio": "3.3V"})
B5V = peripheral("B5V", provides = [I2c], rebind = "fixed",
    signals = {"SDA": [pin("P3")], "SCL": [pin("P4")]},
    attrs = {"vio": "5V"})

def lv(a):
    return a["vio"] <= "3.3V"

res = pin_solve([B3V3, B5V], [pin_request("BUS", I2c, where = lv)])
check(res["assignment"]["BUS"]["instance"] == "B3V3", "3V3 provider expected")
"#,
    );
    assert_ok(&result);

    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin")
load("./ifaces.zen", "I2c")

peripheral("B9", provides = [I2c], rebind = "fixed",
    signals = {"SDA": [pin("P1")], "SCL": [pin("P2")]},
    attrs = {"vio": "400kHz"})
"#,
    );
    assert_fails_with(&result, "expects");
}

// --- Signals are the net-typed leaves, not every interface field -------------

#[test]
fn metadata_field_is_not_a_signal() {
    // Neither demanded of the declaration nor consuming a pin in the solve.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "DiffPair")

LVDS0 = peripheral("LVDS0", provides = [DiffPair], rebind = "fixed",
    signals = {"P": [pin("A1")], "N": [pin("A2")]})

res = pin_solve([LVDS0], [pin_request("D", DiffPair)])
sigs = res["assignment"]["D"]["signals"]
check(len(sigs) == 2, "impedance must not consume a pin, got " + str(len(sigs)))
check(sigs["P"]["pin"] == "A1", "P -> A1")
check(sigs["N"]["pin"] == "A2", "N -> A2")
"#,
    );
    assert_ok(&result);
}

#[test]
fn metadata_field_cannot_be_named_in_uses() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request")
load("./ifaces.zen", "DiffPair")
pin_request("D", DiffPair, uses = ["impedance"])
"#,
    );
    assert_fails_with(&result, "`impedance` is not a signal of DiffPair");
}

#[test]
fn nested_interface_flattens_into_signals() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_map", "pin_request", "pin_solve")
load("./ifaces.zen", "Usb2")

USB0 = peripheral("USB0", provides = [Usb2], rebind = "fixed",
    signals = {"D_P": [pin("PA12")], "D_N": [pin("PA11")], "VBUS": [pin("PA9")]})

BUS = Usb2("BUS")
res = pin_solve([USB0], [pin_request("U", Usb2)])
sigs = res["assignment"]["U"]["signals"]
check(len(sigs) == 3, "expected 3 signals, got " + str(len(sigs)))

m = pin_map(res["assignment"], {"U": BUS})
check(m["PA12"] == BUS.D.P, "PA12 must carry the nested D.P net")
check(m["PA11"] == BUS.D.N, "PA11 must carry the nested D.N net")
check(m["PA9"] == BUS.VBUS, "PA9 must carry VBUS")
check(type(m["PA12"]) == "Net", "a mapped pin must be a Net, got " + type(m["PA12"]))
"#,
    );
    assert_ok(&result);
}

#[test]
fn nested_interface_type_field_flattens_too() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_map", "pin_request", "pin_solve")
load("./ifaces.zen", "Lvds")

LV = peripheral("LV", provides = [Lvds], rebind = "fixed",
    signals = {
        "CLK_P": [pin("B1")], "CLK_N": [pin("B2")],
        "DATA_P": [pin("B3")], "DATA_N": [pin("B4")],
    })

LINK = Lvds("LINK")
res = pin_solve([LV], [pin_request("L", Lvds)])
m = pin_map(res["assignment"], {"L": LINK})
check(m["B1"] == LINK.CLK.P, "B1 must carry CLK.P")
check(m["B4"] == LINK.DATA.N, "B4 must carry DATA.N")
"#,
    );
    assert_ok(&result);
}

#[test]
fn nested_interface_declared_as_one_signal_names_the_flattened_signal() {
    // The pre-fix spelling: `D` as if the whole pair were one pin.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin")
load("./ifaces.zen", "Usb2")
peripheral("USB0", provides = [Usb2], rebind = "fixed",
    signals = {"D": [pin("PA12")], "VBUS": [pin("PA9")]})
"#,
    );
    assert_fails_with(
        &result,
        "peripheral `USB0` claims Usb2 but has no candidate for signal `D_P`",
    );
}

#[test]
fn uses_can_select_a_flattened_signal() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Usb2")

USB0 = peripheral("USB0", provides = [Usb2], rebind = "fixed",
    signals = {"D_P": [pin("PA12")], "D_N": [pin("PA11")], "VBUS": [pin("PA9")]})

res = pin_solve([USB0], [pin_request("U", Usb2, uses = ["D_P", "D_N"])])
sigs = res["assignment"]["U"]["signals"]
check(len(sigs) == 2, "VBUS must stay free, got " + str(len(sigs)))
check("VBUS" not in sigs, "VBUS must not be assigned")
"#,
    );
    assert_ok(&result);
}

#[test]
fn flattened_signal_name_collision_is_rejected() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_request")
load("./ifaces.zen", "Ambiguous")
pin_request("X", Ambiguous)
"#,
    );
    assert_fails_with(
        &result,
        "nested fields flatten to two signals named `D_P` — rename one of them",
    );
}

#[test]
fn pin_map_error_lists_the_available_signals() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_map", "pin_request", "pin_solve")
load("./ifaces.zen", "Usb2", "DiffPair")

USB0 = peripheral("USB0", provides = [Usb2], rebind = "fixed",
    signals = {"D_P": [pin("PA12")], "D_N": [pin("PA11")], "VBUS": [pin("PA9")]})

res = pin_solve([USB0], [pin_request("U", Usb2)])
pin_map(res["assignment"], {"U": DiffPair("WRONG")})
"#,
    );
    assert_fails_with(&result, "it carries `P`, `N`");
}

#[test]
fn stdlib_differential_interface_backs_a_capability_table() {
    // Real stdlib shapes: `impedance` is a nullable physical, not the int above.
    let result = eval_zen(vec![(
        "/test.zen".to_string(),
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_map", "pin_request", "pin_solve")
load("@stdlib/interfaces.zen", "Usb2")

USB_OTG = peripheral("USB_OTG", provides = [Usb2], rebind = "fixed",
    signals = {"D_P": [pin("PA12")], "D_N": [pin("PA11")]})

BUS = Usb2("BUS")
res = pin_solve([USB_OTG], [pin_request("U", Usb2)])
m = pin_map(res["assignment"], {"U": BUS})
check(len(m) == 2, "expected 2 mapped pins, got " + str(len(m)))
check(m["PA12"] == BUS.D.P, "PA12 must carry D.P")

Component(
    name = "U1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"PA11": "1", "PA12": "2"},
    pins = m,
    skip_bom = True,
)
"#
        .to_string(),
    )]);
    assert_ok(&result);
}

// --- Capability identity is per declaration, not per evaluation --------------

const ONE_IFACE: &str = r#"
Uart = interface(TX = Net, RX = Net)
"#;

const ONE_PERIPH: &str = r#"
load("@stdlib/pinmux.zen", "peripheral", "pin")
load("./ifaces.zen", "Uart")
PERIPHS = [peripheral("U0", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("A1")], "RX": [pin("A2")]})]
"#;

#[test]
fn capability_identity_crosses_file_boundaries() {
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), ONE_IFACE.to_string()),
        ("/lib.zen".to_string(), ONE_PERIPH.to_string()),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./lib.zen", "PERIPHS")
load("./ifaces.zen", "Uart")
res = pin_solve(PERIPHS, [pin_request("C", Uart)])
check(res["assignment"]["C"]["instance"] == "U0", "must match across files")
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
}

#[test]
fn separately_declared_interfaces_stay_distinct() {
    // Matching is nominal: identical text in two files is two capabilities.
    // Also pins down that identity keys on the declaring file, not on nothing.
    let result = eval_zen(vec![
        ("/a.zen".to_string(), ONE_IFACE.to_string()),
        ("/b.zen".to_string(), ONE_IFACE.to_string()),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./a.zen", "Uart")
load("./b.zen", Uart2 = "Uart")
P = [peripheral("U0", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("A1")], "RX": [pin("A2")]})]
pin_solve(P, [pin_request("C", Uart2)])
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(&result, "U0: rejected — provides a different Uart");
}

#[test]
fn a_shared_request_name_keeps_the_remap_guard() {
    // Same shape as the guard above, but a second component also has a request
    // named `COM`. The name no longer answers for one part — the entry does.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_map", "pin_request", "pin_solve", "pool")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Gpio")

OTHER = pool("GPIO", part = "U2", provides = [Gpio], pins = ["PB0", "PB1"])
BUS = Uart("BUS")
LED = Net("LED")

r1 = pin_solve(PERIPHS, [pin_request("COM", Uart, instance = "USART1")])
r2 = pin_solve(PERIPHS, [pin_request("COM", Uart, instance = "USART2")])
r3 = pin_solve(PERIPHS, [pin_request("LED", Gpio, prefer = ["PA9"], lock = True)])
r4 = pin_solve([OTHER], [pin_request("COM", Gpio)])

pin_map(r3["assignment"], {"LED": LED})
pin_map(r1["assignment"], {"COM": BUS})
"#,
    );
    assert_fails_with(&result, "already mapped to net `LED` in this module");
}

#[test]
fn superseded_assignment_cannot_remap_a_pin() {
    // Re-solving `COM` releases its claims, so a later solve may take PA9 —
    // mapping the stale result would put two nets on it.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_map", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Gpio")

BUS = Uart("BUS")
LED = Net("LED")

r1 = pin_solve(PERIPHS, [pin_request("COM", Uart, instance = "USART1")])
r2 = pin_solve(PERIPHS, [pin_request("COM", Uart, instance = "USART2")])
r3 = pin_solve(PERIPHS, [pin_request("LED", Gpio, prefer = ["PA9"], lock = True)])

pin_map(r3["assignment"], {"LED": LED})
pin_map(r1["assignment"], {"COM": BUS})
"#,
    );
    assert_fails_with(&result, "already mapped to net `LED` in this module");
}

#[test]
fn same_request_mapped_to_two_nets_across_solves_is_rejected() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_map", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart")

r1 = pin_solve(PERIPHS, [pin_request("COM", Uart, instance = "USART1")])
r2 = pin_solve(PERIPHS, [pin_request("COM", Uart, instance = "USART1")])
pin_map(r1["assignment"], {"COM": Uart("A")})
pin_map(r2["assignment"], {"COM": Uart("B")})
"#,
    );
    assert_fails_with(&result, "already mapped to net");
}

#[test]
fn remapping_the_same_net_stays_allowed() {
    // The `previous=` widening pattern: the stable requests land on the same
    // pins and the same nets, so mapping either result is harmless.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_map", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Usart")

BUS = Uart("BUS")
reqs = [pin_request("DEBUG", Uart)]
r1 = pin_solve(PERIPHS, reqs)
r2 = pin_solve(PERIPHS, reqs + [pin_request("SC", Usart)], previous = r1["assignment"])
pin_map(r1["assignment"], {"DEBUG": BUS})
pin_map(r2["assignment"], {"DEBUG": BUS})
"#,
    );
    assert_ok(&result);
}

#[test]
fn unconsumed_at_names_the_requests_that_were_solved() {
    // The at() pairing is nominal: io("debug_uart") is not served by
    // pin_request("DEBUG"), and the message must show that mismatch.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/stm32.zen".to_string(), STM32.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pin_map", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart")
debug_uart = io("debug_uart", Uart)
res = pin_solve(PERIPHS, [pin_request("DEBUG", Uart)])
pin_map(res["assignment"], {"DEBUG": debug_uart})
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Uart")
Mcu = Module("./mcu.zen")
Mcu(name = "U1", debug_uart = at(Uart("BUS"), ["PA9", "PA10"]))
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(
        &result,
        "constraint on input `debug_uart` was never consumed: no pin_request named `debug_uart` reached a pin_solve",
    );
}

#[test]
fn a_forwarded_at_goes_to_the_request_that_can_take_it() {
    // The pinned rail reaches both requests, but only the GPIO one has PA7.
    // Claiming by declaration order would lock the ADC to a pad it has no
    // candidate for and fail a design that has an answer.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio", "AdcIn")
G = pool("GPIO", provides = [Gpio], pins = ["PA5", "PA7"])
A = peripheral("A", provides = [AdcIn], rebind = "fixed", signals = {"IN": [pin("PA1")]})
ADC = io(AdcIn)
LED = io(Gpio)
res = pin_solve([G, A], [pin_request("ADC", AdcIn), pin_request("LED", Gpio)])
builtin.add_property("led", res["assignment"]["LED"]["signals"]["PIN"]["pin"])
builtin.add_property("adc", res["assignment"]["ADC"]["signals"]["IN"]["pin"])
"#
            .to_string(),
        ),
        (
            "/som.zen".to_string(),
            r#"
load("./ifaces.zen", "Gpio", "AdcIn")
RAIL = io(Gpio)
Mcu = Module("./mcu.zen")
Mcu(name = "U1", LED = RAIL, ADC = AdcIn(IN = RAIL.PIN))
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
Som = Module("./som.zen")
Som(name = "SOM", RAIL = at(Gpio("RAIL_NET"), "PA7"))
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
    let props: Vec<String> = result
        .output
        .as_ref()
        .map(|o| {
            o.module_tree()
                .values()
                .flat_map(|m| {
                    ["led", "adc"].iter().filter_map(move |p| {
                        m.properties()
                            .get(*p)
                            .and_then(|v| v.to_value().unpack_str().map(|s| format!("{p}={s}")))
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(props, ["led=PA7", "adc=PA1"], "got {props:?}");
}

#[test]
fn a_forwarded_at_wanted_by_two_children_is_reported() {
    // Both children solve on the pinned net. Children elaborate in parallel,
    // so first-come-wins would hand the pin to a different one each build.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve", "pool")
load("./ifaces.zen", "Gpio")
POOL = pool("GPIO", provides = [Gpio], pins = ["PA5", "PA6", "PA7"])
LED = io("LED", Gpio)
res = pin_solve([POOL], [pin_request("LED", Gpio)])
builtin.add_property("led_pin", res["assignment"]["LED"]["signals"]["PIN"]["pin"])
"#
            .to_string(),
        ),
        (
            "/som.zen".to_string(),
            r#"
load("./ifaces.zen", "Gpio")
LED = io("LED", Gpio)
Mcu = Module("./mcu.zen")
Mcu(name = "U1", LED = LED)
Mcu(name = "U2", LED = LED)
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
Som = Module("./som.zen")
Som(name = "SOM", LED = at(Gpio("LED_NET"), "PA7"))
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(
        &result,
        "is wanted by the solves of `SOM.U1`, `SOM.U2`: a pin name answers for one component",
    );
    // The report hangs off the at() itself, not off whichever solve lost.
    let t = diag_text(&result);
    assert!(t.contains("pinmux.contended_at"), "got {t}");
}

#[test]
fn at_reaches_a_solve_through_a_forwarding_module() {
    // The intermediate owns no solve and just forwards the value; the
    // constraint rides the net down to the leaf that does solve.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve", "pool")
load("./ifaces.zen", "Gpio")
POOL = pool("GPIO", provides = [Gpio], pins = ["PA5", "PA6", "PA7"])
LED = io("LED", Gpio)
res = pin_solve([POOL], [pin_request("LED", Gpio)])
builtin.add_property("led_pin", res["assignment"]["LED"]["signals"]["PIN"]["pin"])
"#
            .to_string(),
        ),
        (
            "/som.zen".to_string(),
            r#"
load("./ifaces.zen", "Gpio")
LED = io("LED", Gpio)
Mcu = Module("./mcu.zen")
Mcu(name = "U1", LED = LED)
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
Som = Module("./som.zen")
Som(name = "SOM", LED = at(Gpio("LED_NET"), "PA7"))
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
    let tree = result.output.as_ref().unwrap().module_tree();
    let pin = tree
        .values()
        .filter_map(|m| m.properties().get("led_pin").cloned())
        .filter_map(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
        .next()
        .unwrap_or_default();
    assert_eq!(pin, "PA7", "at() must reach the leaf solve two levels down");
}

#[test]
fn soft_at_reaches_a_solve_through_a_forwarding_module() {
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve", "pool")
load("./ifaces.zen", "Gpio")
POOL = pool("GPIO", provides = [Gpio], pins = ["PA5", "PA6", "PA7"])
LED = io("LED", Gpio)
res = pin_solve([POOL], [pin_request("LED", Gpio)])
builtin.add_property("led_pin", res["assignment"]["LED"]["signals"]["PIN"]["pin"])
"#
            .to_string(),
        ),
        (
            "/som.zen".to_string(),
            r#"
load("./ifaces.zen", "Gpio")
LED = io("LED", Gpio)
Mcu = Module("./mcu.zen")
Mcu(name = "U1", LED = LED)
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
Som = Module("./som.zen")
Som(name = "SOM", LED = at(Gpio("LED_NET"), "PA6", soft = True))
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
    let tree = result.output.as_ref().unwrap().module_tree();
    let pin = tree
        .values()
        .filter_map(|m| m.properties().get("led_pin").cloned())
        .filter_map(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
        .next()
        .unwrap_or_default();
    assert_eq!(
        pin, "PA6",
        "a soft at() must bias the leaf solve, not vanish"
    );
}

#[test]
fn if_connected_ignores_a_config_of_the_same_name() {
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/stm32.zen".to_string(), STM32.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Gpio")
gpio = config("gpio", str)
res = pin_solve(PERIPHS, [pin_request("gpio", Gpio, if_connected = True)])
builtin.add_property("served", "yes" if "gpio" in res["assignment"] else "no")
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
Mcu = Module("./mcu.zen")
Mcu(name = "U1", gpio = "not a connection")
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
    let served = result
        .output
        .as_ref()
        .unwrap()
        .module_tree()
        .values()
        .filter_map(|m| m.properties().get("served").cloned())
        .filter_map(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
        .next()
        .unwrap_or_default();
    assert_eq!(served, "no", "a config() must not count as a connection");
}

#[test]
fn pin_map_warns_about_a_solved_request_it_was_not_given() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pin_map", "pin_request", "pin_solve")
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Gpio")

res = pin_solve(PERIPHS, [pin_request("COM", Uart), pin_request("LED", Gpio)])
pin_map(res["assignment"], {"COM": Uart("BUS")})
"#,
    );
    assert_ok(&result);
    assert!(
        diag_text(&result).contains("solved request(s) `LED` are absent from the ifaces dict"),
        "expected a warning naming LED, got:\n{}",
        diag_text(&result)
    );
}

// --- Locked pins: a list must be claimed in full, a dict bounds one signal ---

#[test]
fn locked_list_that_starves_a_signal_names_it() {
    // Both pins are TX-only candidates, so RX is left with nothing.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")

U = peripheral("U0", provides = [Uart], rebind = "firmware",
    signals = {"TX": [pin("PA9"), pin("PB6")], "RX": [pin("PA10")]})
pin_solve([U], [pin_request("COM", Uart, prefer = ["PA9", "PB6"], lock = True)])
"#,
    );
    assert_fails_with(
        &result,
        "signal `RX` has no candidate among the locked pins `PA9`, `PB6`",
    );
}

#[test]
fn locked_dict_picks_one_of_several_pins_for_one_signal() {
    // The intent a bare list could not express: either pin for TX, RX free.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")

U = peripheral("U0", provides = [Uart], rebind = "firmware",
    signals = {"TX": [pin("PA9"), pin("PB6")], "RX": [pin("PA10")]})
res = pin_solve([U], [pin_request("COM", Uart, prefer = {"TX": ["PA9", "PB6"]}, lock = True)])
sigs = res["assignment"]["COM"]["signals"]
check(sigs["TX"]["pin"] in ["PA9", "PB6"], "TX must take one of the named pins")
check(sigs["RX"]["pin"] == "PA10", "RX stays free, got " + sigs["RX"]["pin"])
"#,
    );
    assert_ok(&result);
}

#[test]
fn locked_dict_rejects_a_pin_outside_the_named_set() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")

U = peripheral("U0", provides = [Uart], rebind = "firmware",
    signals = {"TX": [pin("PA9")], "RX": [pin("PA10")]})
pin_solve([U], [pin_request("COM", Uart, prefer = {"TX": ["PB6"]}, lock = True)])
"#,
    );
    assert_fails_with(
        &result,
        "signal `TX` has no candidate among the locked pins `PB6`",
    );
}

#[test]
fn at_accepts_a_per_signal_dict() {
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")
U = peripheral("U0", provides = [Uart], rebind = "firmware",
    signals = {"TX": [pin("PA9"), pin("PB6")], "RX": [pin("PA10")]})
COM = io("COM", Uart)
res = pin_solve([U], [pin_request("COM", Uart)])
builtin.add_property("tx", res["assignment"]["COM"]["signals"]["TX"]["pin"])
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Uart")
Mcu = Module("./mcu.zen")
Mcu(name = "U1", COM = at(Uart("BUS"), {"TX": "PB6"}))
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
    let tx = result
        .output
        .as_ref()
        .unwrap()
        .module_tree()
        .values()
        .filter_map(|m| m.properties().get("tx").cloned())
        .filter_map(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
        .next()
        .unwrap_or_default();
    assert_eq!(tx, "PB6", "at() dict must pin the named signal");
}

#[test]
fn two_parts_pinning_one_shared_net_each_get_their_own() {
    let part = |p: &str| {
        format!(
            r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = peripheral("P0", provides = [Gpio], rebind = "fixed",
    signals = {{"PIN": [pin("{p}"), pin("OTHER")]}})
BUS = io("BUS", Gpio)
res = pin_solve([P], [pin_request("BUS", Gpio)])
builtin.add_property("chosen", res["assignment"]["BUS"]["signals"]["PIN"]["pin"])
"#
        )
    };
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/a.zen".to_string(), part("PB7")),
        ("/b.zen".to_string(), part("P3")),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
A = Module("./a.zen")
B = Module("./b.zen")
bus = Gpio("SHARED")
A(name = "A", BUS = at(bus, "PB7"))
B(name = "B", BUS = at(bus, "P3"))
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
    let chosen: Vec<(String, String)> = result
        .output
        .as_ref()
        .unwrap()
        .module_tree()
        .iter()
        .filter_map(|(path, m)| {
            let v = m.properties().get("chosen")?;
            Some((
                path.segments.join("."),
                v.to_value().unpack_str()?.to_owned(),
            ))
        })
        .collect();
    assert_eq!(
        chosen,
        [
            ("A".to_string(), "PB7".to_string()),
            ("B".to_string(), "P3".to_string())
        ],
        "each part must get the pin named at its own call site"
    );
}

#[test]
fn where_predicate_failure_rejects_only_that_candidate() {
    // B9 declares no `baud_max`, so the predicate raises on it; the solve must
    // fall through to the provider that does instead of aborting.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")

NOATTR = peripheral("NOATTR", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("P1")], "RX": [pin("P2")]})
WITHATTR = peripheral("WITHATTR", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("P3")], "RX": [pin("P4")]},
    attrs = {"baud_max": "8MHz"})

def fast(a):
    return a["baud_max"] >= "1MHz"

res = pin_solve([NOATTR, WITHATTR], [pin_request("COM", Uart, where = fast)])
check(res["assignment"]["COM"]["instance"] == "WITHATTR", "must skip the attr-less provider")
"#,
    );
    assert_ok(&result);
}

/// A hard `at()` left unconsumed by the first design must not fail the second.
/// No `prepare_for_root_eval` here on purpose: each root owns its constraint
/// store, so the isolation holds by construction rather than by a reset.
#[test]
fn a_failed_design_does_not_poison_the_next_one() {
    let mut files = common::stdlib_test_files();
    files.insert(
        "/leaf.zen".to_string(),
        "IO0 = io(\"IO0\", Net, optional = True)\n".to_string(),
    );
    files.insert(
        "/a.zen".to_string(),
        r#"
load("@stdlib/pinmux.zen", "at")
Leaf = Module("./leaf.zen")
Leaf(name = "L", IO0 = at(Net("LED"), "PA9"))
"#
        .to_string(),
    );
    files.insert("/b.zen".to_string(), "N = Net(\"B\")\n".to_string());

    let file_provider: Arc<dyn pcb_zen_core::FileProvider> =
        Arc::new(common::InMemoryFileProvider::new(files));
    let resolution = Arc::new(common::test_resolution());
    let session = EvalSession::default();

    let eval_root = |path: &str| {
        EvalContext::from_session_and_config(
            session.clone(),
            EvalContextConfig::new(file_provider.clone(), resolution.clone()),
        )
        .set_source_path(PathBuf::from(path))
        .eval()
    };

    let a = eval_root("/a.zen");
    assert!(
        !a.is_success(),
        "a.zen must fail on its own unconsumed at()"
    );
    let b = eval_root("/b.zen");
    assert!(
        b.is_success(),
        "b.zen must build clean, got: {:#?}",
        b.diagnostics
    );
}

const TWO_CHIPS: &str = r#"
load("@stdlib/pinmux.zen", "peripheral", "pin")
load("./ifaces.zen", "Comparator")

# Same part number twice, so both chips carry the same pin numbering.
def quad(ref):
    return [
        peripheral(ref + ".A", part = ref, provides = [Comparator], rebind = "none",
            signals = {"INP": [pin("7")], "INN": [pin("6")], "OUT": [pin("1")]}),
        peripheral(ref + ".B", part = ref, provides = [Comparator], rebind = "none",
            signals = {"INP": [pin("5")], "INN": [pin("4")], "OUT": [pin("2")]}),
    ]
"#;

fn eval_two_chips(main: &str) -> WithDiagnostics<EvalOutput> {
    eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/chip.zen".to_string(), TWO_CHIPS.to_string()),
        ("/test.zen".to_string(), main.to_string()),
    ])
}

#[test]
fn a_second_part_is_not_blocked_by_the_first_parts_pin_names() {
    let result = eval_two_chips(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./chip.zen", "quad")
load("./ifaces.zen", "Comparator")

r1 = pin_solve(quad("U1"), [pin_request("OV", Comparator)])
r2 = pin_solve(quad("U2"), [pin_request("UV", Comparator)])
check(r1["assignment"]["OV"]["instance"] == "U1.A", "U1 must take its first unit")
check(r2["assignment"]["UV"]["instance"] == "U2.A", "U2 must take its own first unit")
"#,
    );
    assert_ok(&result);
}

#[test]
fn a_fully_used_part_does_not_exhaust_a_second_one() {
    let result = eval_two_chips(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./chip.zen", "quad")
load("./ifaces.zen", "Comparator")

pin_solve(quad("U1"), [pin_request("OV", Comparator), pin_request("UV", Comparator)])
r2 = pin_solve(quad("U2"), [pin_request("TEMP", Comparator)])
check(r2["assignment"]["TEMP"]["instance"] == "U2.A", "U2 must still be free")
"#,
    );
    assert_ok(&result);
}

#[test]
fn exclusivity_still_holds_within_one_part() {
    let result = eval_two_chips(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./chip.zen", "quad")
load("./ifaces.zen", "Comparator")

U = quad("U1")
r1 = pin_solve(U, [pin_request("OV", Comparator)])
r2 = pin_solve(U, [pin_request("UV", Comparator)])
check(r1["assignment"]["OV"]["instance"] == "U1.A", "first solve takes unit A")
check(r2["assignment"]["UV"]["instance"] == "U1.B", "second must move to unit B")
"#,
    );
    assert_ok(&result);
}

const TWO_SLOTS: &str = r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
PA = peripheral("PA", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("P1"), pin("P2")]})
PB = peripheral("PB", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("P1"), pin("P2")]})
A = io("A", Gpio)
B = io("B", Gpio)
res = pin_solve([PA, PB], [pin_request("B", Gpio), pin_request("A", Gpio)])
builtin.add_property("A", res["assignment"]["A"]["signals"]["PIN"]["pin"])
builtin.add_property("B", res["assignment"]["B"]["signals"]["PIN"]["pin"])
"#;

#[test]
fn an_unconstrained_io_does_not_steal_a_siblings_at() {
    // Both inputs carry the same net; only `A` is pinned.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/mcu.zen".to_string(), TWO_SLOTS.to_string()),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
Mcu = Module("./mcu.zen")
n = Gpio("SHARED")
Mcu(name = "U1", A = at(n, "P2"), B = n)
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
    let pin = |k: &str| {
        result
            .output
            .as_ref()
            .unwrap()
            .module_tree()
            .values()
            .filter_map(|m| m.properties().get(k).cloned())
            .filter_map(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
            .next()
            .unwrap_or_default()
    };
    assert_eq!(pin("A"), "P2", "A keeps the pin the caller named");
    assert_eq!(pin("B"), "P1", "B was never constrained");
}

#[test]
fn a_lock_naming_an_unknown_signal_is_rejected() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")
U = peripheral("U0", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("PA9")], "RX": [pin("PA10")]})
pin_solve([U], [pin_request("COM", Uart, prefer = {"TXX": "PA9"}, lock = True)])
"#,
    );
    assert_fails_with(
        &result,
        "pin constraint names signal `TXX`, which this request does not use (it uses `TX`, `RX`)",
    );
}

#[test]
fn a_lock_naming_a_signal_outside_uses_is_rejected() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")
U = peripheral("U0", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("PA9")], "RX": [pin("PA10")]})
pin_solve([U], [pin_request("COM", Uart, uses = ["TX"], prefer = {"RX": "PA10"})])
"#,
    );
    assert_fails_with(
        &result,
        "names signal `RX`, which this request does not use",
    );
}

#[test]
fn an_at_naming_an_unknown_signal_is_rejected() {
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart")
U = peripheral("U0", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("PA9")], "RX": [pin("PA10")]})
COM = io("COM", Uart)
pin_solve([U], [pin_request("COM", Uart)])
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Uart")
Mcu = Module("./mcu.zen")
Mcu(name = "U1", COM = at(Uart("BUS"), {"TXX": "PA9"}))
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(
        &result,
        "at() on input `COM`: pin constraint names signal `TXX`",
    );
}

#[test]
fn a_second_parts_pins_do_not_eat_the_firsts_spares() {
    // Two chips, overlapping pin numbering. U2 taking its `B` must not strike
    // `B` off U1's spare list in the merged property.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P1 = pool("U1.GPIO", part = "U1", provides = [Gpio], pins = ["A", "B", "C"])
P2 = pool("U2.GPIO", part = "U2", provides = [Gpio], pins = ["B", "D"])
pin_solve([P1], [pin_request("L1", Gpio)])
pin_solve([P2], [pin_request("L2", Gpio)])
"#,
    );
    assert_ok(&result);
    let swaps = json_property(&result, "swap_classes");
    let u1 = swaps
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["pool"] == "U1.GPIO")
        .expect("U1 class present");
    let spares: Vec<&str> = u1["spare_pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap())
        .collect();
    assert_eq!(spares, ["B", "C"], "U1 keeps both of its own spares");
}

#[test]
fn undeclared_silicon_shares_one_pad_namespace() {
    // Without `part=` there is one namespace, which is why the solver refuses
    // `A` to L2 — so `B` is gone from L1's alternates too. `part=` is how a
    // table says the pads sit on different components.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
U1 = peripheral("U1.P", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("A"), pin("B")]})
U2 = peripheral("U2.P", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("A"), pin("B")]})
pin_solve([U1], [pin_request("L1", Gpio)])
pin_solve([U2], [pin_request("L2", Gpio)])
"#,
    );
    assert_ok(&result);

    let a = json_property(&result, "pin_assignment");
    assert_eq!(a["L2"]["signals"]["PIN"]["pin"], "B", "got {a}");
    assert!(
        a["L1"]["alternates"].as_object().unwrap().is_empty(),
        "got {a}"
    );
}

#[test]
fn a_second_parts_pins_do_not_eat_the_firsts_alternates() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
U1 = peripheral("U1.P", part = "U1", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("A"), pin("B")]})
U2 = peripheral("U2.P", part = "U2", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("B")]})
pin_solve([U1], [pin_request("L1", Gpio)])
pin_solve([U2], [pin_request("L2", Gpio)])
"#,
    );
    assert_ok(&result);
    let a = json_property(&result, "pin_assignment");
    let alts = &a["L1"]["alternates"]["PIN"];
    assert_eq!(
        alts.as_array().map(|v| v.len()),
        Some(1),
        "U1 keeps its own alternate, got {a}"
    );
}

#[test]
fn pin_map_allows_two_declared_parts_to_share_a_pin_name() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Gpio")
U1 = peripheral("U1.P", part = "U1", provides = [Gpio], rebind = "fixed", signals = {"PIN": [pin("7")]})
U2 = peripheral("U2.P", part = "U2", provides = [Gpio], rebind = "fixed", signals = {"PIN": [pin("7")]})
r1 = pin_solve([U1], [pin_request("A", Gpio)])
r2 = pin_solve([U2], [pin_request("B", Gpio)])
m1 = pin_map(r1["assignment"], {"A": Net("NA")})
m2 = pin_map(r2["assignment"], {"B": Net("NB")})
check(m1["7"] != m2["7"], "each part keeps its own pad 7")
"#,
    );
    assert_ok(&result);
}

const QUAD: &str = r#"
load("@stdlib/pinmux.zen", "peripheral", "pin")
load("./ifaces.zen", "Comparator")
def quad(ref):
    return [
        peripheral(ref + ".A", part = ref, provides = [Comparator], rebind = "none",
            signals = {"INP": [pin("7")], "INN": [pin("6")], "OUT": [pin("1")]}),
        peripheral(ref + ".B", part = ref, provides = [Comparator], rebind = "none",
            signals = {"INP": [pin("5")], "INN": [pin("4")], "OUT": [pin("2")]}),
    ]
"#;

fn eval_quad(main: &str) -> WithDiagnostics<EvalOutput> {
    eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        ("/chip.zen".to_string(), QUAD.to_string()),
        ("/test.zen".to_string(), main.to_string()),
    ])
}

#[test]
fn declared_parts_spread_requests_over_both_components() {
    let result = eval_quad(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve", "pin_map")
load("./chip.zen", "quad")
load("./ifaces.zen", "Comparator")

roles = ["R1", "R2", "R3", "R4"]
res = pin_solve(quad("U1") + quad("U2"), [pin_request(k, Comparator) for k in roles])
check(res["assignment"]["R1"]["instance"] == "U1.A", "R1 on U1.A")
check(res["assignment"]["R3"]["instance"] == "U2.A", "R3 must reach the second chip")

ifaces = {k: Comparator(k) for k in roles}
m1 = pin_map(res["assignment"], ifaces, part = "U1")
m2 = pin_map(res["assignment"], ifaces, part = "U2")
check(sorted(m1.keys()) == sorted(m2.keys()), "both chips carry the same pad names")
check(m1["7"] != m2["7"], "but they are different pads")
"#,
    );
    assert_ok(&result);
}

#[test]
fn mapping_a_multi_part_assignment_needs_a_part() {
    let result = eval_quad(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve", "pin_map")
load("./chip.zen", "quad")
load("./ifaces.zen", "Comparator")
roles = ["R1", "R2", "R3", "R4"]
res = pin_solve(quad("U1") + quad("U2"), [pin_request(k, Comparator) for k in roles])
pin_map(res["assignment"], {k: Comparator(k) for k in roles})
"#,
    );
    assert_fails_with(
        &result,
        "spans several parts; pass part= to map one component at a time",
    );
}

#[test]
fn a_pool_inside_a_flat_list_is_still_one_part() {
    // `pool()` yields a list, so nesting is how pools compose: it must not be
    // read as a part boundary.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Uart", "Gpio")
U = peripheral("U0", provides = [Uart], rebind = "fixed",
    signals = {"TX": [pin("P1")], "RX": [pin("P2")]})
G = pool("GPIO", provides = [Gpio], pins = ["P1", "P3"])
res = pin_solve([U, G], [pin_request("COM", Uart), pin_request("LED", Gpio)])
check(res["assignment"]["LED"]["signals"]["PIN"]["pin"] == "P3", "P1 is taken by TX")
"#,
    );
    assert_ok(&result);
}

/// `pin_map` ties unclaimed pads off; a design may still override them, which
/// is how the LM339 example straps its unused comparator units.
#[test]
fn unclaimed_pads_are_tied_off_and_can_be_overridden() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Comparator")

def unit(n, p, m, o):
    return peripheral(n, provides = [Comparator], rebind = "none",
        signals = {"INP": [pin(p)], "INN": [pin(m)], "OUT": [pin(o)]})
UNITS = [unit("A", "IN1P", "IN1N", "OUT1"), unit("B", "IN2P", "IN2N", "OUT2")]
VCC = Net("VCC")
GND = Net("GND")
role = Comparator("OV")

res = pin_solve(UNITS, [pin_request("OV", Comparator, bind = role)])
pins = pin_map(res["assignment"], {"OV": role})

# The served unit carries its role...
check(pins["IN1P"] == role.INP, "IN1P carries the role")
check(pins["OUT1"] == role.OUT, "OUT1 carries the role")
# ...the spare unit's pads come back open, and a design may strap them.
check(pins["OUT2"].name == "", "an unclaimed pad is open")
pins["IN2P"] = VCC
pins["IN2N"] = GND
check(pins["IN2P"] == VCC, "an open pad can be overridden")
"#,
    );
    assert_ok(&result);
}

#[test]
fn an_ancestors_constraint_survives_a_second_solve() {
    // The at() came from the board and was forwarded down, so the leaf can
    // only read it from the constraint store — re-solving must not lose it.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA5", "PA6", "PA7"])
LED = io("LED", Gpio)
reqs = [pin_request("LED", Gpio)]
r1 = pin_solve([P], reqs)
r2 = pin_solve([P], reqs)
builtin.add_property("first", r1["assignment"]["LED"]["signals"]["PIN"]["pin"])
builtin.add_property("second", r2["assignment"]["LED"]["signals"]["PIN"]["pin"])
"#
            .to_string(),
        ),
        (
            "/som.zen".to_string(),
            r#"
load("./ifaces.zen", "Gpio")
LED = io("LED", Gpio)
Mcu = Module("./mcu.zen")
Mcu(name = "U1", LED = LED)
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
Som = Module("./som.zen")
Som(name = "SOM", LED = at(Gpio("LED_NET"), "PA7"))
"#
            .to_string(),
        ),
    ]);
    assert_ok(&result);
    let pin = |k: &str| {
        result
            .output
            .as_ref()
            .unwrap()
            .module_tree()
            .values()
            .filter_map(|m| m.properties().get(k).cloned())
            .filter_map(|v| v.to_value().unpack_str().map(|s| s.to_owned()))
            .next()
            .unwrap_or_default()
    };
    assert_eq!(pin("first"), "PA7", "the forwarded at() applies");
    assert_eq!(pin("second"), "PA7", "and still applies on a re-solve");
}

#[test]
fn an_idle_part_keeps_its_pads_when_another_part_uses_the_same_names() {
    // Both requests land on U1; U2 exposes the very same pad names and must
    // still come back tied off, or Component() rejects it as unconnected.
    let result = eval_quad(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve", "pin_map")
load("./chip.zen", "quad")
load("./ifaces.zen", "Comparator")

res = pin_solve(quad("U1") + quad("U2"),
                [pin_request("R1", Comparator), pin_request("R2", Comparator)])
check(res["assignment"]["R1"]["instance"] == "U1.A", "R1 on U1.A")
check(res["assignment"]["R2"]["instance"] == "U1.B", "R2 on U1.B")

ifaces = {"R1": Comparator("a"), "R2": Comparator("b")}
u2 = pin_map(res["assignment"], ifaces, part = "U2")
check(sorted(u2.keys()) == ["1", "2", "4", "5", "6", "7"], "U2 ties off all six pads, got " + str(sorted(u2.keys())))
"#,
    );
    assert_ok(&result);
}

#[test]
fn an_earlier_solve_on_one_part_leaves_the_other_free() {
    // Same table solved twice: U1's claimed pads must not block U2's, which
    // carry identical names.
    let result = eval_quad(
        r#"
load("@stdlib/pinmux.zen", "pin_request", "pin_solve")
load("./chip.zen", "quad")
load("./ifaces.zen", "Comparator")

ALL = quad("U1") + quad("U2")
pin_solve(ALL, [pin_request("R1", Comparator), pin_request("R2", Comparator)])
r = pin_solve(ALL, [pin_request("R3", Comparator)])
check(r["assignment"]["R3"]["instance"] == "U2.A", "R3 must reach the idle chip")
"#,
    );
    assert_ok(&result);
}

#[test]
fn the_unsolvable_hint_points_at_advice_that_works() {
    let undeclared = r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Comparator")
def u(ref):
    return peripheral(ref, provides = [Comparator], rebind = "none",
        signals = {"INP": [pin("7")], "INN": [pin("6")], "OUT": [pin("1")]})
pin_solve([u("U1"), u("U2")], [pin_request("R1", Comparator), pin_request("R2", Comparator)])
"#;
    let result = eval_with_fixtures(undeclared);
    assert_fails_with(
        &result,
        "give each `peripheral(part = ...)` so its pads stay its own",
    );

    // And taking the advice actually solves it.
    let declared = undeclared.replace(
        "provides = [Comparator]",
        "part = ref, provides = [Comparator]",
    );
    let result = eval_with_fixtures(&format!(
        r#"{declared}
"#
    ));
    assert_ok(&result);
}

#[test]
fn two_parts_may_name_a_resource_alike() {
    // `part=` separates pad namespaces; resource names may then repeat, so
    // instance exclusivity has to be per part too — and the assignment says
    // which part each one is on.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
U1 = pool("GPIO", part = "U1", provides = [Gpio], pins = ["PA0"])
U2 = pool("GPIO", part = "U2", provides = [Gpio], pins = ["PA0"])

a = pin_solve([U1, U2], [pin_request("A", Gpio)])
b = pin_solve([U1, U2], [pin_request("B", Gpio)])
check(a["assignment"]["A"]["instance"] == "GPIO.PA0", "A on the shared name")
check(b["assignment"]["B"]["instance"] == "GPIO.PA0", "B too")
check(a["assignment"]["A"]["part"] != b["assignment"]["B"]["part"],
      "but on different parts, got " + str(a["assignment"]["A"]["part"]))
"#,
    );
    assert_ok(&result);
}

#[test]
fn an_inherited_entry_keeps_the_alternates_of_its_own_part() {
    // X sits on U2, whose P2 stays free; the pad U1 took in the second solve
    // has the same name and must not shrink X's freedom.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio", "AdcIn")
U1A = peripheral("A", part = "U1", provides = [Gpio], rebind = "none",
    signals = {"PIN": [pin("P2")]})
U2A = peripheral("A", part = "U2", provides = [AdcIn], rebind = "none",
    signals = {"IN": [pin("P1"), pin("P2")]})

x = pin_solve([U1A, U2A], [pin_request("X", AdcIn)])
y = pin_solve([U1A, U2A], [pin_request("Y", Gpio)])
"#,
    );
    assert_ok(&result);

    let assign = json_property(&result, "pin_assignment");
    assert_eq!(
        assign["X"]["alternates"]["IN"].as_array().unwrap(),
        &["P2"],
        "got {assign}"
    );
}

#[test]
fn a_pool_class_merged_across_solves_keeps_its_part() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio", "AdcIn")
U1 = pool("GPIO", part = "U1", provides = [Gpio], pins = ["PA0", "PA1"])
U2 = pool("GPIO", part = "U2", provides = [AdcIn], pins = ["PB0", "PB1"])

a = pin_solve([U1, U2], [pin_request("A", Gpio)])
b = pin_solve([U1, U2], [pin_request("B", AdcIn)])
"#,
    );
    assert_ok(&result);

    let swaps = json_property(&result, "swap_classes");
    let classes = swaps.as_array().unwrap();
    assert_eq!(classes.len(), 2, "one class per component, got {swaps}");
    for c in classes {
        let part = c["part"].as_str().unwrap_or("?");
        let spare = c["spare_pins"][0].as_str().unwrap_or("?");
        let expect = if part == "U1" { "PA1" } else { "PB1" };
        assert_eq!(spare, expect, "spares stay on their component, got {swaps}");
    }
}

#[test]
fn a_class_merged_across_solves_keeps_its_part() {
    // The module property folds every solve together; a prior class must not
    // land on the class of the same-named units of another component.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Comparator")
def unit(name, part, pads):
    return peripheral(name, part = part, provides = [Comparator], rebind = "none",
        signals = {"INP": [pin(pads[0])], "INN": [pin(pads[1])], "OUT": [pin(pads[2])]})

P = [unit("A", "U1", ["1", "2", "3"]), unit("B", "U1", ["5", "6", "7"]),
     unit("A", "U2", ["1", "2", "3"]), unit("B", "U2", ["5", "6", "7"])]

x = pin_solve(P, [pin_request("X", Comparator)])
y = pin_solve(P, [pin_request("Y", Comparator)])
"#,
    );
    assert_ok(&result);

    let swaps = json_property(&result, "swap_classes");
    let classes = swaps.as_array().unwrap();
    assert_eq!(classes.len(), 2, "one class per component, got {swaps}");
    let mut parts: Vec<&str> = classes
        .iter()
        .map(|c| c["part"].as_str().unwrap_or("?"))
        .collect();
    parts.sort();
    assert_eq!(
        parts,
        ["U1", "U2"],
        "each class names its component, got {swaps}"
    );
}

#[test]
fn a_gate_swap_class_never_spans_two_parts() {
    // Same story for gate swap: X may move to U1's spare comparator, never to
    // the identically named one on U2.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve")
load("./ifaces.zen", "Comparator")
def unit(name, part, pads):
    return peripheral(name, part = part, provides = [Comparator], rebind = "none",
        signals = {"INP": [pin(pads[0])], "INN": [pin(pads[1])], "OUT": [pin(pads[2])]})

U1A = unit("A", "U1", ["1", "2", "3"])
U1B = unit("B", "U1", ["5", "6", "7"])
U2A = unit("A", "U2", ["1", "2", "3"])
U2B = unit("B", "U2", ["5", "6", "7"])

r = pin_solve([U1A, U1B, U2A, U2B], [pin_request("X", Comparator), pin_request("Y", Comparator)])
"#,
    );
    assert_ok(&result);

    let swaps = json_property(&result, "swap_classes");
    let classes = swaps.as_array().unwrap();
    assert_eq!(classes.len(), 2, "one class per component, got {swaps}");
    for c in classes {
        assert_eq!(c["members"].as_array().unwrap().len(), 1, "got {swaps}");
        assert_eq!(c["spare_units"].as_array().unwrap(), &["B"], "got {swaps}");
    }
    let parts: Vec<&str> = classes
        .iter()
        .map(|c| c["part"].as_str().unwrap_or("?"))
        .collect();
    assert!(
        parts.contains(&"U1") && parts.contains(&"U2"),
        "each class names its component, got {swaps}"
    );
}

#[test]
fn a_clobbered_result_property_is_reported() {
    // pin_solve owns these names; overwriting one would otherwise cost the
    // earlier solves their data with no word said.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA0", "PA1"])
builtin.add_property("pin_assignment", "mine")
r = pin_solve([P], [pin_request("A", Gpio)])
"#,
    );
    assert_fails_with(&result, "written by pin_solve");

    // ...and after it, where the merge would have kept no trace at all.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA0", "PA1"])
r = pin_solve([P], [pin_request("A", Gpio)])
builtin.add_property("swap_classes", "mine")
"#,
    );
    assert_fails_with(&result, "written by pin_solve");
}

#[test]
fn one_request_name_may_serve_two_parts() {
    // A name identifies a request per component, not per module: solving `LED`
    // on U2 must not release what `LED` took on U1.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Gpio")
U1 = pool("GPIO", part = "U1", provides = [Gpio], pins = ["PA0", "PA1"])
U2 = pool("GPIO", part = "U2", provides = [Gpio], pins = ["PA0", "PA1"])

a = pin_solve([U1], [pin_request("LED", Gpio)])
b = pin_solve([U2], [pin_request("LED", Gpio)])
c = pin_solve([U1], [pin_request("BTN", Gpio)])

led = a["assignment"]["LED"]["signals"]["PIN"]["pin"]
check(c["assignment"]["BTN"]["signals"]["PIN"]["pin"] != led,
      "BTN took the pad LED holds on U1")
m = pin_map(a["assignment"], {"LED": Gpio("L")}, part = "U1")
check(led in m, "U1 still maps the pad its own LED took, got " + str(sorted(m.keys())))
"#,
    );
    assert_ok(&result);
}

#[test]
fn a_swap_class_never_spans_two_parts() {
    // Two components whose pools carry the same name: the residual freedom of
    // a request on one is not freedom to move onto the other's pads.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio", "AdcIn")
U1 = pool("GPIO", part = "U1", provides = [Gpio], pins = ["PA0", "PA1"])
U2 = pool("GPIO", part = "U2", provides = [AdcIn], pins = ["PA0", "PA1"])

r = pin_solve([U1, U2], [pin_request("A", Gpio), pin_request("B", AdcIn)])
"#,
    );
    assert_ok(&result);

    let swaps = json_property(&result, "swap_classes");
    let classes = swaps.as_array().unwrap();
    assert_eq!(classes.len(), 2, "one class per component, got {swaps}");
    for c in classes {
        assert_eq!(c["members"].as_array().unwrap().len(), 1, "got {swaps}");
        assert_eq!(c["spare_pins"].as_array().unwrap().len(), 1, "got {swaps}");
    }
    let parts: Vec<&str> = classes
        .iter()
        .map(|c| c["part"].as_str().unwrap_or("?"))
        .collect();
    assert!(
        parts.contains(&"U1") && parts.contains(&"U2"),
        "each class names its component, got {swaps}"
    );
}

/// Re-analysing files on one `EvalContext` — the editor path — must not carry
/// one design's unmet `at()` into the next one's diagnostics.
#[test]
fn repeated_analysis_keeps_designs_apart() {
    let mut files = common::stdlib_test_files();
    files.insert(
        "/leaf.zen".to_string(),
        "IO0 = io(\"IO0\", Net, optional = True)\n".to_string(),
    );
    files.insert(
        "/bad.zen".to_string(),
        r#"
load("@stdlib/pinmux.zen", "at")
Leaf = Module("./leaf.zen")
Leaf(name = "L", IO0 = at(Net("LED"), "PA9"))
"#
        .to_string(),
    );
    files.insert("/good.zen".to_string(), "N = Net(\"OK\")\n".to_string());

    let file_provider: Arc<dyn pcb_zen_core::FileProvider> =
        Arc::new(common::InMemoryFileProvider::new(files.clone()));
    let ctx = EvalContext::from_session_and_config(
        EvalSession::default(),
        EvalContextConfig::new(file_provider, Arc::new(common::test_resolution())),
    );

    let analyze = |name: &str| ctx.parse_and_analyze_file(PathBuf::from(name), files[name].clone());
    assert!(!analyze("/bad.zen").is_success(), "bad.zen owns its error");
    for _ in 0..2 {
        let r = analyze("/good.zen");
        assert!(
            r.is_success(),
            "good.zen must stay clean, got: {:#?}",
            r.diagnostics
        );
    }
}

#[test]
fn an_at_in_a_role_dict_nobody_reads_is_reported() {
    // The module wires only the roles it knows, so a mistyped key would
    // otherwise drop its pin constraint without a word.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA0", "PA1", "PA8"])
gpio = config("gpio", dict, default = {})
reqs = [pin_request("LED", Gpio, bind = gpio["LED"])] if "LED" in gpio else []
pin_solve([P], reqs)
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "at")
load("./ifaces.zen", "Gpio")
Mcu = Module("./mcu.zen")
Mcu(name = "U1", gpio = {"LED": at(Gpio("L"), "PA8"), "TYPO": at(Gpio("T"), "PA1")})
"#
            .to_string(),
        ),
    ]);
    assert_fails_with(
        &result,
        "at() pin constraint on input `TYPO` was never consumed",
    );
}

#[test]
fn free_pads_accumulate_across_solves_of_one_part() {
    // Two tables, one anonymous part: the second solve must not erase the
    // first table's unclaimed pads, or they end up wired nowhere.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Gpio")
P1 = peripheral("P1", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA0"), pin("PA1")]})
P2 = peripheral("P2", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PB0"), pin("PB1")]})
r1 = pin_solve([P1], [pin_request("A", Gpio)])
pin_solve([P2], [pin_request("B", Gpio)])

m = pin_map(r1["assignment"], {"A": Net("NA")})
check("PA1" in m, "P1's own unclaimed pad survives the second solve")
check(not ("PB0" in m), "a pad the second solve claimed is not tied off")
"#,
    );
    assert_ok(&result);
}

#[test]
fn declared_parts_keep_their_free_pads_apart() {
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Gpio")
P1 = peripheral("P1", part = "U1", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA0"), pin("PA1")]})
P2 = peripheral("P2", part = "U2", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PB0"), pin("PB1")]})
r1 = pin_solve([P1], [pin_request("A", Gpio)])
pin_solve([P2], [pin_request("B", Gpio)])

m = pin_map(r1["assignment"], {"A": Net("NA")})
check(sorted(m.keys()) == ["PA0", "PA1"], "U1 keeps only its own, got " + str(sorted(m.keys())))
"#,
    );
    assert_ok(&result);
}

#[test]
fn a_pad_a_resolve_gave_back_is_tied_off_again() {
    // R moves from P1's table to P2's. Nobody holds PA0 any more, and the
    // component still has to wire it, though the later table never named it.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Gpio")
P1 = peripheral("P1", part = "U1", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA0"), pin("PA1")]})
P2 = peripheral("P2", part = "U1", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA2"), pin("PA3")]})
r1 = pin_solve([P1], [pin_request("R", Gpio)])
r2 = pin_solve([P2], [pin_request("R", Gpio)])

m = pin_map(r2["assignment"], {"R": Net("NA")}, part = "U1")
check(sorted(m.keys()) == ["PA0", "PA1", "PA2", "PA3"],
      "the whole pin table of U1, got " + str(sorted(m.keys())))
"#,
    );
    assert_ok(&result);
}

#[test]
fn a_named_part_solved_over_two_subsets_keeps_every_pad() {
    // One component, two tables: pads reachable only through the table absent
    // from the later solve must still reach the component's pin dict.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Gpio")
P1 = peripheral("P1", part = "U1", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA0"), pin("PA1")]})
P2 = peripheral("P2", part = "U1", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA2"), pin("PA3")]})
r1 = pin_solve([P1], [pin_request("A", Gpio)])
pin_solve([P2], [pin_request("B", Gpio)])

m = pin_map(r1["assignment"], {"A": Net("NA")})
check(sorted(m.keys()) == ["PA0", "PA1", "PA3"],
      "every unclaimed pad of U1 is tied off, got " + str(sorted(m.keys())))
"#,
    );
    assert_ok(&result);
}

#[test]
fn if_connected_rejects_a_config_declared_after_the_solve() {
    // `configs` only knows declarations seen so far, so the gate can read a
    // later config as a connection — caught once the signature is complete.
    let result = eval_zen(vec![
        ("/ifaces.zen".to_string(), IFACES.to_string()),
        (
            "/mcu.zen".to_string(),
            r#"
load("@stdlib/pinmux.zen", "pool", "pin_request", "pin_solve")
load("./ifaces.zen", "Gpio")
P = pool("GPIO", provides = [Gpio], pins = ["PA0"])
pin_solve([P], [pin_request("gpio", Gpio, if_connected = True)])
gpio = config("gpio", str)
"#
            .to_string(),
        ),
        (
            "/test.zen".to_string(),
            "Mcu = Module(\"./mcu.zen\")\nMcu(name = \"U1\", gpio = \"hello\")\n".to_string(),
        ),
    ]);
    assert_fails_with(
        &result,
        "was served because the caller passed `gpio`, but that input is a config()",
    );
}

#[test]
fn a_pad_shared_by_two_tables_of_one_part_is_claimed_once() {
    // P1 and P2 belong to the same (anonymous) part and both expose PA0.
    // Solved separately, the second must not be handed the pad the first took.
    let result = eval_with_fixtures(
        r#"
load("@stdlib/pinmux.zen", "peripheral", "pin", "pin_request", "pin_solve", "pin_map")
load("./ifaces.zen", "Gpio")
P1 = peripheral("P1", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA0")]})
P2 = peripheral("P2", provides = [Gpio], rebind = "fixed",
    signals = {"PIN": [pin("PA0"), pin("PA1")]})
a = pin_solve([P1], [pin_request("A", Gpio)])
b = pin_solve([P2], [pin_request("B", Gpio)])
check(a["assignment"]["A"]["signals"]["PIN"]["pin"] == "PA0", "A takes PA0")
check(b["assignment"]["B"]["signals"]["PIN"]["pin"] == "PA1",
      "B must avoid PA0, got " + b["assignment"]["B"]["signals"]["PIN"]["pin"])
m = pin_map(b["assignment"], {"B": Net("NB")})
check(not ("PA0" in m), "and PA0 must not be tied off as free")
"#,
    );
    assert_ok(&result);
}
