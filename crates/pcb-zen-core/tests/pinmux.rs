//! Integration tests for the peripheral capability model
//! (`pin`/`peripheral`/`pool`/`pin_request`/`pin_solve` + `interface(implies=)`).
//!
//! Behavior assertions live in the `.zen` fixtures themselves via `check()`;
//! the Rust side only asserts overall success/failure, diagnostics, and the
//! emitted module properties. Fixtures model a small STM32G030 subset (AF
//! matrix), an ESP32-C3 subset (GPIO matrix + IOMUX + strapping), and a dual
//! comparator (gate swap). Pin/AF pairs are illustrative fixtures, not
//! datasheet-verified data.

mod common;

use common::eval_zen;
use pcb_zen_core::WithDiagnostics;
use pcb_zen_core::lang::eval::EvalOutput;

const IFACES: &str = r#"
Uart = interface(TX = Net, RX = Net)
Usart = interface(TX = Net, RX = Net, CK = Net, implies = [Uart])
UartFlow = interface(TX = Net, RX = Net, RTS = Net, CTS = Net, implies = [Uart])
I2c = interface(SDA = Net, SCL = Net)
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
    // A plain Uart request must not steal USART1 from the Usart request
    // (implies closure + poorest-instance preference), and af= must be
    // carried into the assignment for firmware codegen.
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
"#,
    );
    assert_ok(&result);
    // The solved assignment must be persisted as module properties.
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
    // Two Usart requests, one capable instance: instance capacity forbids it.
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
    // Three Uart requests for two instances: pin availability alone would
    // allow it (USART1 has two full pin sets), instance capacity must not.
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
    // PA10 locked as GPIO: USART1 must mix TX=PA9 with RX=PB7.
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
    // USART1 forced onto its alternate pins (PA9/PA10 taken) collides with
    // I2C1 on PB6/PB7 — the joint matching must detect it, not route around it.
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
    // Physical-value attributes with plain unit-string comparison.
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
    // Once USART1 is taken by the Usart request, no instance satisfies the
    // where= floor: infeasible, not silently degraded.
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
    // PA5 claimed as GPIO: SPI1 falls back to PB3 for SCK. Locking PB3 too
    // makes SPI infeasible (pin-tier exclusivity).
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
    // Claiming UartFlow without RTS/CTS candidates is a declaration error.
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
    // ADC2 is unusable while wifi is active.
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
    // A TX-only debug UART claims the USART1 instance but not its RX pin:
    // PA10 stays available as GPIO (the RX *signal* is gone with the instance,
    // the *pin* is not).
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
