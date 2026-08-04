//! Integration tests for the peripheral capability model. Behavior assertions
//! live in the `.zen` fixtures via `check()`; the Rust side asserts overall
//! success/failure, diagnostics, and emitted module properties. Pin/AF pairs
//! are illustrative, not datasheet-verified.

mod common;

use common::eval_zen;
use pcb_zen_core::WithDiagnostics;
use pcb_zen_core::lang::eval::EvalOutput;

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
"#;

const STM32: &str = r#"
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

#[test]
fn downgrade_and_poorest_instance() {
    let result = eval_with_fixtures(
        r#"
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Usart")

res = pin_solve(PERIPHS, [pin_request("DEBUG", Uart), pin_request("SC", Usart)])
a = res["assignment"]
check(a["DEBUG"]["instance"] == "USART2", "DEBUG got " + a["DEBUG"]["instance"])
check(a["SC"]["instance"] == "USART1", "SC got " + a["SC"]["instance"])
check(a["SC"]["signals"]["TX"]["pin"] == "PA9", "SC TX pin")
check(a["SC"]["signals"]["TX"]["af"] == 1, "SC TX af")
check("PB3" in res["free_pins"], "unclaimed candidate must be free")
check(not ("PA9" in res["free_pins"]), "claimed pin must not be free")
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
fn pinmap_cap_truncation_warns() {
    let result = eval_with_fixtures(
        r#"
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
load("./stm32.zen", "PERIPHS")
load("./ifaces.zen", "Uart", "Gpio")

DBG = Uart("DBG")
LED = Net("LED")

res = pin_solve(PERIPHS, [
    pin_request("COM", Uart, instance = "USART1"),
    pin_request("LED", Gpio, prefer = ["PB0"], lock = True),
])
m = pin_map(res["assignment"], {"COM": DBG, "LED": LED})
check(len(m) == 3, "expected 3 mapped pins, got " + str(len(m)))
check(m["PA9"] == DBG.TX, "PA9 must carry DBG.TX")
check(m["PA10"] == DBG.RX, "PA10 must carry DBG.RX")
check(m["PB0"] == LED, "PB0 must carry the LED net")
"#,
    );
    assert_ok(&result);
}

const MCU_AT: &str = r#"
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
m = pin_map(res["assignment"], named)

a = res["assignment"]
builtin.add_property("led_pin", a["LED"]["signals"]["PIN"]["pin"] if "LED" in a else "none")
builtin.add_property("vbat_pin", a["VBAT"]["signals"]["IN"]["pin"] if "VBAT" in a else "none")
builtin.add_property("map_size", str(len(m)))
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
    assert_eq!(get("map_size"), "2", "pin_map must cover both roles");
}

#[test]
fn attr_vocabulary_is_enforced() {
    let result = eval_with_fixtures(
        r#"
load("./ifaces.zen", "Uart")

peripheral("U9", provides = [Uart], rebind = "firmware",
    signals = {"TX": [pin("P1")], "RX": [pin("P2")]},
    attrs = {"baudMax": "8MHz"})
"#,
    );
    assert_fails_with(&result, "not declared by any provided interface");

    let result = eval_with_fixtures(
        r#"
load("./ifaces.zen", "Uart")

peripheral("U9", provides = [Uart], rebind = "firmware",
    signals = {"TX": [pin("P1")], "RX": [pin("P2")]},
    attrs = {"baud_max": "3.3V"})
"#,
    );
    assert_fails_with(&result, "expects");

    let result = eval_with_fixtures(
        r#"
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
load("./ifaces.zen", "I2c")

peripheral("B9", provides = [I2c], rebind = "fixed",
    signals = {"SDA": [pin("P1")], "SCL": [pin("P2")]},
    attrs = {"vio": "400kHz"})
"#,
    );
    assert_fails_with(&result, "expects");
}
