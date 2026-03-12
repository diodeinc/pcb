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

    assert ks.properties(symbol)["Reference"] == "U"
    assert ks.properties(symbol)["Manufacturer"] == "Diode Inc."
    assert ks.del_property(symbol, "Reference") is True
    assert "Reference" not in ks.properties(symbol)


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


def test_effects_accepts_explicit_font_without_duplicate_default() -> None:
    fx = ks.effects(ks.font(2.0, 2.0), justify="left")
    fonts = [child for child in fx if isinstance(child, list) and ks.head(child) == "font"]

    assert len(fonts) == 1
    assert fonts[0] == ks.font(2.0, 2.0)
    assert fx[1] == ks.font(2.0, 2.0)
    assert ks.child(fx, "justify") == ks.form("justify", ks.sym("left"))


def test_property_and_text_accept_direct_justify() -> None:
    prop = ks.property("Reference", "U", justify="left")
    text = ks.text("DBG", at=(0, 0, 0), justify="right")

    assert ks.child(ks.child(prop, "effects"), "justify") == ks.form("justify", ks.sym("left"))
    assert ks.child(ks.child(text, "effects"), "justify") == ks.form("justify", ks.sym("right"))


def test_hidden_pin_emits_hide_yes() -> None:
    pin = ks.pin("NC", "NC", at=(0, 0, 0), hide=True)

    assert ks.child(pin, "hide") == ks.form("hide", ks.yesno(True))


def test_font_emits_bold_and_italic_yes() -> None:
    node = ks.font(1.27, 1.27, bold=True, italic=True)

    assert ks.child(node, "bold") == ks.form("bold", ks.yesno(True))
    assert ks.child(node, "italic") == ks.form("italic", ks.yesno(True))


def test_read_multi_symbol_library_by_name() -> None:
    lib = ks.library(
        ks.symbol("R", ks.property("Description", "Resistor")),
        ks.symbol("C", ks.property("Description", "Capacitor")),
    )

    assert ks.symbol_names(lib) == ["R", "C"]
    assert ks.symbol_name(ks.get_symbol(lib, "R")) == "R"
    assert ks.properties(ks.get_symbol(lib, "C"))["Description"] == "Capacitor"


def test_write_multi_symbol_library() -> None:
    lib = ks.library(
        ks.symbol("R_Custom", ks.property("Reference", "R")),
        ks.symbol("C_Custom", ks.property("Reference", "C")),
    )

    ks.set_property(ks.get_symbol(lib, "R_Custom"), "Description", "Example resistor")
    lib.append(ks.symbol("TP_Custom", ks.property("Reference", "TP")))

    reparsed = ks.parse(ks.dumps(lib))

    assert ks.symbol_names(reparsed) == ["R_Custom", "C_Custom", "TP_Custom"]
    assert ks.properties(ks.get_symbol(reparsed, "R_Custom"))["Description"] == "Example resistor"


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
    assert len(list(ks.pins(dual))) == 5
