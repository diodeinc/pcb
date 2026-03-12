import kicad_sym as ks


def test_parse_and_dump_round_trip() -> None:
    text = """(kicad_symbol_lib
    (version 20241209)
    (generator "kicad_symbol_editor")
    (symbol "Demo"
        (property "Reference" "U" (at 0 0 0))
        (symbol "Demo_1_1"
            (pin passive line
                (at 0 0 0)
                (length 2.54)
                (name "IN" (effects (font (size 1.27 1.27))))
                (number "1" (effects (font (size 1.27 1.27))))
            )
        )
    )
)"""
    parsed = ks.parse(text)

    assert ks.symbol_names(parsed) == ["Demo"]
    assert ks.nested_symbol_names(ks.get_symbol(parsed)) == ["Demo_1_1"]
    assert ks.parse(ks.dumps(parsed)) == parsed
    assert repr(parsed).startswith("(kicad_symbol_lib")
    assert str(ks.get_symbol(parsed)).startswith('(symbol "Demo"')
    assert repr(ks.sym("pin")) == "Sym('pin')"


def test_set_property_creates_and_updates_properties() -> None:
    lib = ks.library(ks.symbol("Demo"))
    symbol = ks.get_symbol(lib)

    ks.set_property(symbol, "Reference", "U", at=(0, 5.08, 0))
    ks.set_property(symbol, "Manufacturer", "Diode", hidden=True)
    ks.set_property(symbol, "Manufacturer", "Diode Inc.", hidden=False)

    assert ks.get_property(symbol, "Reference") == "U"
    assert ks.get_property(symbol, "Manufacturer") == "Diode Inc."
    assert ks.del_property(symbol, "Reference") is True
    assert ks.get_property(symbol, "Reference") is None


def test_child_properties_and_pins_helpers() -> None:
    lib = ks.library(
        ks.symbol(
            "Demo",
            ks.property("Reference", "U"),
            ks.property("Description", "Demo part"),
            ks.unit_symbol(
                "Demo",
                1,
                1,
                ks.pin("1", "IN", electrical="input", at=(-2.54, 0, 0)),
                ks.pin("2", "OUT", electrical="output", at=(2.54, 0, 180)),
            ),
        )
    )

    symbol = ks.get_symbol(lib)
    unit = ks.get_nested_symbol(symbol, "Demo_1_1")

    assert ks.child(symbol, "property") is not None
    assert ks.child(symbol, "missing") is None
    assert ks.properties(symbol) == {"Reference": "U", "Description": "Demo part"}
    assert [pin[1] for pin in ks.pins(unit)] == [ks.sym("input"), ks.sym("output")]


def test_read_multi_symbol_library_by_name() -> None:
    lib = ks.library(
        ks.symbol("R", ks.property("Description", "Resistor")),
        ks.symbol("C", ks.property("Description", "Capacitor")),
    )

    assert ks.symbol_names(lib) == ["R", "C"]
    assert ks.symbol_name(ks.get_symbol(lib, "R")) == "R"
    assert ks.get_property(ks.get_symbol(lib, "C"), "Description") == "Capacitor"


def test_write_multi_symbol_library() -> None:
    lib = ks.library(
        ks.symbol("R_Custom", ks.property("Reference", "R")),
        ks.symbol("C_Custom", ks.property("Reference", "C")),
    )

    ks.set_property(ks.get_symbol(lib, "R_Custom"), "Description", "Example resistor")
    lib.append(ks.symbol("TP_Custom", ks.property("Reference", "TP")))

    reparsed = ks.parse(ks.dumps(lib))

    assert ks.symbol_names(reparsed) == ["R_Custom", "C_Custom", "TP_Custom"]
    assert ks.get_property(ks.get_symbol(reparsed, "R_Custom"), "Description") == "Example resistor"


def test_build_multi_unit_symbol() -> None:
    dual = ks.symbol(
        "Demo_Dual_Opamp",
        ks.property("Reference", "U"),
        ks.unit_symbol(
            "Demo_Dual_Opamp",
            1,
            1,
            ks.pin("1", "-", electrical="input", at=(-7.62, 2.54, 0)),
            ks.pin("2", "+", electrical="input", at=(-7.62, -2.54, 0)),
            ks.pin("3", "~", electrical="output", at=(7.62, 0, 180)),
        ),
        ks.unit_symbol(
            "Demo_Dual_Opamp",
            3,
            0,
            ks.pin("4", "V-", electrical="power_in", at=(0, -7.62, 90)),
            ks.pin("8", "V+", electrical="power_in", at=(0, 7.62, 270)),
        ),
    )

    assert ks.symbol_name(dual) == "Demo_Dual_Opamp"
    assert ks.nested_symbol_names(dual) == ["Demo_Dual_Opamp_1_1", "Demo_Dual_Opamp_3_0"]
    assert len(ks.find_all(dual, "pin")) == 5
