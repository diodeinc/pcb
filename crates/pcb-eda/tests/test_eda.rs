mod test_utils;

use test_utils::setup_symbol;

use pcb_eda::{Part, Symbol};
use std::collections::HashMap;

fn test_symbol_property(symbol_name: &str, property: impl Fn(&Symbol) -> String, expected: &str) {
    let symbol = setup_symbol(symbol_name);
    assert_eq!(property(&symbol), expected);
}

fn test_symbol_option_property(
    symbol_name: &str,
    property: impl Fn(&Symbol) -> Option<String>,
    expected: Option<&str>,
) {
    let symbol = setup_symbol(symbol_name);
    assert_eq!(property(&symbol), expected.map(String::from));
}

#[test]
fn test_pcm2903cdb_name() {
    test_symbol_property("PCM2903CDB", |s| s.name.clone(), "PCM2903CDB");
}

#[test]
fn test_pcm2903cdb_footprint() {
    test_symbol_property("PCM2903CDB", |s| s.footprint.clone(), "SOP65P780X200-28N");
}

#[test]
fn test_pcm2903cdb_in_bom() {
    let symbol = setup_symbol("PCM2903CDB");
    assert!(symbol.in_bom);
}

#[test]
fn test_pcm2903cdb_datasheet() {
    test_symbol_option_property(
        "PCM2903CDB",
        |s| s.datasheet.clone(),
        Some("http://www.ti.com/lit/gpn/pcm2903c"),
    );
}

#[test]
fn test_pcm2903cdb_pin_count() {
    let symbol = setup_symbol("PCM2903CDB");
    assert_eq!(symbol.pins.len(), 28);
}

#[test]
fn test_pcm2903cdb_pin_names() {
    let symbol = setup_symbol("PCM2903CDB");
    let pin_map: HashMap<_, _> = symbol
        .pins
        .iter()
        .map(|pin| (pin.number.clone(), pin.name.clone()))
        .collect();

    let expected_pins = [
        ("1", "D+"),
        ("2", "D-"),
        ("3", "VBUS"),
        ("4", "GNDU"),
        ("5", "HID0"),
        ("6", "HID1"),
        ("7", "HID2"),
        ("8", "SEL0"),
        ("9", "SEL1"),
        ("10", "VCCC"),
        ("11", "AGNDC"),
        ("12", "VIN_L"),
        ("13", "VIN_R"),
        ("14", "VCOM"),
        ("15", "VOUT_R"),
        ("16", "VOUT_L"),
        ("17", "VCCP1"),
        ("18", "AGNDP"),
        ("19", "VCCP2"),
        ("20", "XTO"),
        ("21", "XTI"),
        ("22", "AGNDX"),
        ("23", "VCCX"),
        ("24", "DIN"),
        ("25", "DOUT"),
        ("26", "DGND"),
        ("27", "VDD"),
        ("28", "~{SSPND}"),
    ];

    for (number, name) in expected_pins.iter() {
        assert_eq!(pin_map.get(*number), Some(&name.to_string()));
    }
}

#[test]
fn test_pcm2903cdb_manufacturer() {
    test_symbol_option_property(
        "PCM2903CDB",
        |s| s.manufacturer.clone(),
        Some("Texas Instruments"),
    );
}

#[test]
fn test_pcm2903cdb_mpn() {
    test_symbol_option_property("PCM2903CDB", |s| s.mpn.clone(), Some("PCM2903CDB"));
}

#[test]
fn test_pcm2903cdb_distributors() {
    let symbol = setup_symbol("PCM2903CDB");
    let expected_distributors: HashMap<String, Part> = vec![
        ("Mouser".to_string(), Part {
            part_number: "595-PCM2903CDB".to_string(),
            url: "https://www.mouser.co.uk/ProductDetail/Texas-Instruments/PCM2903CDB?qs=4whTb%2F0XQMi6hOJbi2eq4w%3D%3D".to_string(),
        }),
        ("Arrow".to_string(), Part {
            part_number: "PCM2903CDB".to_string(),
            url: "https://www.arrow.com/en/products/pcm2903cdb/texas-instruments?region=nac".to_string(),
        }),
    ]
    .into_iter()
    .collect();
    assert_eq!(symbol.distributors, expected_distributors);
}

#[test]
fn test_pcm2903cdb_description() {
    test_symbol_option_property(
        "PCM2903CDB",
        |s| s.description.clone(),
        Some("Stereo USB1.1 CODEC with line-out and S/PDIF I/O, Self-powered (HID Interface) "),
    );
}

// Tests for SN75176BD

#[test]
fn test_sn75176bd_name() {
    test_symbol_property("SN75176BD", |s| s.name.clone(), "SN75176BD");
}

#[test]
fn test_sn75176bd_footprint() {
    test_symbol_property("SN75176BD", |s| s.footprint.clone(), "SOIC127P600X175-8N");
}

#[test]
fn test_sn75176bd_in_bom() {
    let symbol = setup_symbol("SN75176BD");
    assert!(symbol.in_bom);
}

#[test]
fn test_sn75176bd_datasheet() {
    test_symbol_option_property(
        "SN75176BD",
        |s| s.datasheet.clone(),
        Some("http://www.ti.com/lit/ds/symlink/sn75176b.pdf"),
    );
}

#[test]
fn test_sn75176bd_pin_count() {
    let symbol = setup_symbol("SN75176BD");
    assert_eq!(symbol.pins.len(), 8);
}

#[test]
fn test_sn75176bd_pin_names() {
    let symbol = setup_symbol("SN75176BD");
    let pin_map: HashMap<_, _> = symbol
        .pins
        .iter()
        .map(|pin| (pin.number.clone(), pin.name.clone()))
        .collect();

    let expected_pins = [
        ("1", "R"),
        ("2", "~{RE}"),
        ("3", "DE"),
        ("4", "D"),
        ("5", "GND"),
        ("6", "A"),
        ("7", "B"),
        ("8", "VCC"),
    ];

    for (number, name) in expected_pins.iter() {
        assert_eq!(pin_map.get(*number), Some(&name.to_string()));
    }
}

#[test]
fn test_sn75176bd_manufacturer() {
    test_symbol_option_property(
        "SN75176BD",
        |s| s.manufacturer.clone(),
        Some("Texas Instruments"),
    );
}

#[test]
fn test_sn75176bd_mpn() {
    test_symbol_option_property("SN75176BD", |s| s.mpn.clone(), Some("SN75176BD"));
}

#[test]
fn test_sn75176bd_distributors() {
    let symbol = setup_symbol("SN75176BD");
    let expected_distributors: HashMap<String, Part> = vec![
        ("Mouser".to_string(), Part {
            part_number: "595-SN75176BD".to_string(),
            url: "https://www.mouser.co.uk/ProductDetail/Texas-Instruments/SN75176BD?qs=LzFo6vGRJ4sdA5%2FEVFfutw%3D%3D".to_string(),
        }),
        ("Arrow".to_string(), Part {
            part_number: "SN75176BD".to_string(),
            url: "https://www.arrow.com/en/products/sn75176bd/texas-instruments?region=nac".to_string(),
        }),
    ]
    .into_iter()
    .collect();
    assert_eq!(symbol.distributors, expected_distributors);
}

#[test]
fn test_sn75176bd_description() {
    test_symbol_option_property(
        "SN75176BD",
        |s| s.description.clone(),
        Some("SN75176BDG4, Line Transceiver Differential, 5V, 8-Pin SOIC"),
    );
}

#[test]
fn test_c146731_name() {
    test_symbol_property("C146731", |s| s.name.clone(), "NB3N551DG");
}

#[test]
fn test_c146731_footprint() {
    test_symbol_property(
        "C146731",
        |s| s.footprint.clone(),
        "SOIC-8_L4.9-W3.9-P1.27-LS6.0-BL",
    );
}

#[test]
fn test_c146731_pins() {
    let symbol = setup_symbol("C146731");
    assert_eq!(symbol.pins.len(), 8);
}

#[test]
fn test_c146731_pin_names() {
    let symbol = setup_symbol("C146731");
    let pin_map: HashMap<_, _> = symbol
        .pins
        .iter()
        .map(|pin| (pin.number.clone(), pin.name.clone()))
        .collect();

    let expected_pins = [
        ("1", "ICLK"),
        ("2", "Q1"),
        ("3", "Q2"),
        ("4", "Q3"),
        ("5", "Q4"),
        ("6", "GND"),
        ("7", "VDD"),
        ("8", "OE"),
    ];

    for (number, name) in expected_pins.iter() {
        assert_eq!(pin_map.get(*number), Some(&name.to_string()));
    }
}

#[test]
fn test_c146731_datasheet() {
    test_symbol_option_property(
        "C146731",
        |s| s.datasheet.clone(),
        Some("https://lcsc.com/product-detail/Logic-ICs_ON_NB3N551DG_NB3N551DG_C146731.html"),
    );
}

#[test]
fn test_c146731_mpn() {
    test_symbol_option_property("C146731", |s| s.mpn.clone(), Some("NB3N551DG"));
}

#[test]
fn test_lan9252ti_name() {
    test_symbol_property("LAN9252TI-PT", |s| s.name.clone(), "LAN9252TI_PT");
}

#[test]
fn test_lan9252ti_footprint() {
    test_symbol_property("LAN9252TI-PT", |s| s.footprint.clone(), "TQFP-EP64_PT_MCH");
}

#[test]
fn test_lan9252ti_pin_count() {
    let symbol = setup_symbol("LAN9252TI-PT");
    assert_eq!(symbol.pins.len(), 65);
}

#[test]
fn test_lan9252ti_pin_names() {
    let symbol = setup_symbol("LAN9252TI-PT");
    let pin_map: HashMap<_, _> = symbol
        .pins
        .iter()
        .map(|pin| (pin.number.clone(), pin.name.clone()))
        .collect();

    assert_eq!(pin_map.get("1"), Some(&"OSCI".to_string()));
    assert_eq!(pin_map.get("2"), Some(&"OSCO".to_string()));
    assert_eq!(pin_map.get("3"), Some(&"OSCVDD12".to_string()));
    assert_eq!(pin_map.get("4"), Some(&"OSCVSS".to_string()));
    assert_eq!(pin_map.get("5"), Some(&"VDD33".to_string()));
    assert_eq!(pin_map.get("6"), Some(&"VDDCR".to_string()));
    assert_eq!(pin_map.get("7"), Some(&"REG_EN".to_string()));
    assert_eq!(pin_map.get("8"), Some(&"FXLOSEN".to_string()));
    assert_eq!(pin_map.get("9"), Some(&"FXSDA/FXLOSA/FXSDENA".to_string()));
    assert_eq!(pin_map.get("10"), Some(&"FXSDB/FXLOSB/FXSDENB".to_string()));
    assert_eq!(pin_map.get("11"), Some(&"RST#".to_string()));
    assert_eq!(pin_map.get("12"), Some(&"D2/AD2/SOF/SIO2".to_string()));
    assert_eq!(pin_map.get("13"), Some(&"D1/AD1/EOF/SO/SIO1".to_string()));
    assert_eq!(pin_map.get("14"), Some(&"VDDIO".to_string()));
    assert_eq!(
        pin_map.get("15"),
        Some(&"D14/AD14/DIGIO8/GP18/GPO8/MII_TXD3/TX_SHIFT1".to_string())
    );
    assert_eq!(
        pin_map.get("16"),
        Some(&"D13/AD13/DIGIO7/GPI7/GPO7/MII_TXD2/TX_SHIFT0".to_string())
    );
    assert_eq!(
        pin_map.get("17"),
        Some(&"D0/AD0/WD_STATE/SI/SIO0".to_string())
    );
    assert_eq!(pin_map.get("18"), Some(&"SYNC1/LATCH1".to_string()));
    assert_eq!(pin_map.get("19"), Some(&"D9/AD9/LATCH_IN/SCK".to_string()));
    assert_eq!(pin_map.get("20"), Some(&"VDDIO".to_string()));
    assert_eq!(
        pin_map.get("21"),
        Some(&"D12/AD12/DIGIO6/GPI6/GPO6/MII_TXD1".to_string())
    );
    assert_eq!(
        pin_map.get("22"),
        Some(&"D11/AD11/DIGIO5/GPI5/GPO5/MII_TXD0".to_string())
    );
    assert_eq!(
        pin_map.get("23"),
        Some(&"D10/AD10/DIGIO4/GPI4/GPO4/MII_TXEN".to_string())
    );
    assert_eq!(pin_map.get("24"), Some(&"VDDCR".to_string()));
    assert_eq!(
        pin_map.get("25"),
        Some(&"A1/ALELO/OE_EXT/MII_CLK25".to_string())
    );
    assert_eq!(
        pin_map.get("26"),
        Some(&"A3/DIGIO11/GPI11/GPO11/MII_RXDV".to_string())
    );
    assert_eq!(
        pin_map.get("27"),
        Some(&"A4/DIGIO12/GPI12/GPO12/MII_RXD0".to_string())
    );
    assert_eq!(
        pin_map.get("28"),
        Some(&"CS/DIGIO13/GPI13/GPO13/MII_RXD1".to_string())
    );
    assert_eq!(
        pin_map.get("29"),
        Some(&"A2/ALEHI/DIGIO10/GPI10/GPO10/LINKACTLED2/MII_LINKPOL".to_string())
    );
    assert_eq!(
        pin_map.get("30"),
        Some(&"WR/ENB/DIGIO14/GPI14/GPO14/MII_RXD2".to_string())
    );
    assert_eq!(
        pin_map.get("31"),
        Some(&"RD/RD_WR/DIGIO15/GPI15/GPO15/MIIRXD3".to_string())
    );
    assert_eq!(pin_map.get("32"), Some(&"VDDIO".to_string()));
    assert_eq!(
        pin_map.get("33"),
        Some(&"A0/D15/AD15/DIGIO9/GPI9/GPO9/MII_RXER".to_string())
    );
    assert_eq!(pin_map.get("34"), Some(&"SYNC0/LATCH0".to_string()));
    assert_eq!(pin_map.get("35"), Some(&"D3/AD3/WD_TRIG/SIO3".to_string()));
    assert_eq!(
        pin_map.get("36"),
        Some(&"D6/AD6/DIGIO0/GPI0/GPO0/MII_RXCLK".to_string())
    );
    assert_eq!(pin_map.get("37"), Some(&"VDDIO".to_string()));
    assert_eq!(pin_map.get("38"), Some(&"VDDCR".to_string()));
    assert_eq!(
        pin_map.get("39"),
        Some(&"D7/AD7/DIGIO1/GPI1/GPO1/MII_MDC".to_string())
    );
    assert_eq!(
        pin_map.get("40"),
        Some(&"D8/AD8/DIGIO2/GPI2/GPO2/MII_MDIO".to_string())
    );
    assert_eq!(pin_map.get("41"), Some(&"TESTMODE".to_string()));
    assert_eq!(pin_map.get("42"), Some(&"EESDA/TMS".to_string()));
    assert_eq!(pin_map.get("43"), Some(&"EESCL/TCK".to_string()));
    assert_eq!(pin_map.get("44"), Some(&"IRQ".to_string()));
    assert_eq!(pin_map.get("45"), Some(&"RUNLED/E2PSIZE".to_string()));
    assert_eq!(
        pin_map.get("46"),
        Some(&"LINKACTLED1/TDI/CHIP_MODE1".to_string())
    );
    assert_eq!(pin_map.get("47"), Some(&"VDDIO".to_string()));
    assert_eq!(
        pin_map.get("48"),
        Some(&"LINKACTLED0/TDO/CHIP_MODE0".to_string())
    );
    assert_eq!(
        pin_map.get("49"),
        Some(&"D4/AD4/DIGIO3/GPI3/GPO3/MII_LINK".to_string())
    );
    assert_eq!(pin_map.get("50"), Some(&"D5/AD5/OUTVALID/SCS#".to_string()));
    assert_eq!(pin_map.get("51"), Some(&"VDD3TXRX1".to_string()));
    assert_eq!(pin_map.get("52"), Some(&"TXN1".to_string()));
    assert_eq!(pin_map.get("53"), Some(&"TXPA".to_string()));
    assert_eq!(pin_map.get("54"), Some(&"RXNA".to_string()));
    assert_eq!(pin_map.get("55"), Some(&"RXPA".to_string()));
    assert_eq!(pin_map.get("56"), Some(&"VDD12TX1".to_string()));
    assert_eq!(pin_map.get("57"), Some(&"RBIAS".to_string()));
    assert_eq!(pin_map.get("58"), Some(&"VDD33BIAS".to_string()));
    assert_eq!(pin_map.get("59"), Some(&"VDD12TX2".to_string()));
    assert_eq!(pin_map.get("60"), Some(&"RXPB".to_string()));
    assert_eq!(pin_map.get("61"), Some(&"RXNB".to_string()));
    assert_eq!(pin_map.get("62"), Some(&"TXPB".to_string()));
    assert_eq!(pin_map.get("63"), Some(&"TXNB".to_string()));
    assert_eq!(pin_map.get("64"), Some(&"VDD3TXRX2".to_string()));
    assert_eq!(pin_map.get("EPAD"), Some(&"VSS".to_string()));
}
