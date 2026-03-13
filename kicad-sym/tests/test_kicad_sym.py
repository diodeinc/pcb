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


def test_symbol_name_on_dot_name_attribute() -> None:
    sym = ks.symbol("MyPart", ks.property("Reference", "U"))
    assert sym.name == "MyPart"
    assert ks.symbol_name(sym) == "MyPart"
    # Name is NOT in the list — node[1] is the first child
    assert ks.head(sym[1]) == "property"


def test_parsed_symbol_has_name_attribute() -> None:
    text = (
        '(kicad_symbol_lib (version 20241209) (symbol "R" (property "Reference" "R" (at 0 0 0))))'
    )
    lib = ks.parse(text)
    sym = ks.get_symbol(lib, "R")
    assert sym.name == "R"
    # First child should be the property, not the name string
    assert ks.head(sym[1]) == "property"


def test_form_symbol_normalizes_to_symbol_representation() -> None:
    built = ks.symbol("Demo", ks.property("Reference", "U"))
    raw = ks.form("symbol", "Demo", ks.property("Reference", "U"))

    assert raw.name == "Demo"
    assert ks.symbol_name(raw) == "Demo"
    assert raw == built
    assert ks.head(raw[1]) == "property"


def test_form_symbol_works_with_library_helpers() -> None:
    lib = ks.library(
        ks.form("symbol", "R", ks.property("Description", "Resistor")),
        ks.form("symbol", "C", ks.property("Description", "Capacitor")),
    )

    assert ks.symbol_names(lib) == ["R", "C"]
    assert ks.symbol_name(ks.get_symbol(lib, "R")) == "R"
    assert ks.properties(ks.get_symbol(lib, "C"))["Description"] == "Capacitor"


def test_nested_symbol_name_attribute() -> None:
    sym = ks.symbol(
        "Demo",
        ks.unit_symbol("Demo", 0, 1, ks.rectangle((-5, 5), (5, -5))),
        ks.unit_symbol("Demo", 1, 1, ks.pin("1", "IN", electrical="input", at=(0, 0, 0))),
    )
    gfx = ks.get_nested_symbol(sym, "Demo_0_1")
    assert gfx.name == "Demo_0_1"
    # First child is the rectangle, not the name
    assert ks.head(gfx[1]) == "rectangle"


def test_clear_subsymbol_children_preserves_name() -> None:
    """The exact bug from eval sessions 01 and 02: gfx[:] = [gfx[0]] dropped the name."""
    sym = ks.symbol(
        "PART",
        ks.unit_symbol(
            "PART",
            0,
            1,
            ks.rectangle((-5, 5), (5, -5)),
            ks.circle((0, 0), 1.0),
        ),
    )
    gfx = ks.get_nested_symbol(sym, "PART_0_1")
    assert gfx.name == "PART_0_1"

    # Clear all children, keep only head
    gfx[:] = [gfx[0]]

    # Name is preserved on the attribute
    assert gfx.name == "PART_0_1"
    assert ks.symbol_name(gfx) == "PART_0_1"

    # Add new content
    gfx.append(ks.rectangle((-10, 10), (10, -10)))

    # Verify serialization includes the name
    output = ks.dumps(ks.library(sym))
    assert '"PART_0_1"' in output


def test_insert_at_index_1_is_first_child() -> None:
    """The exact bug from eval sessions 02, 13, 14: sym.insert(1, ...) put form before name."""
    sym = ks.symbol(
        "DEMO",
        ks.form("exclude_from_sim", ks.yesno(False)),
        ks.property("Reference", "U"),
    )

    # insert(1, ...) should insert as the first child
    sym.insert(1, ks.form("pin_names", ks.form("offset", 0.508)))

    assert ks.head(sym[1]) == "pin_names"
    assert sym.name == "DEMO"

    # Should serialize correctly
    output = ks.dumps(ks.library(sym))
    assert '"DEMO"' in output
    assert "pin_names" in output


def test_insert_child_before_nested_symbols() -> None:
    sym = ks.symbol(
        "DEMO",
        ks.property("Reference", "U"),
        ks.unit_symbol("DEMO", 1, 1),
    )
    ks.insert_child(sym, ks.form("pin_names", ks.form("offset", 1.016)))

    output = ks.dumps(ks.library(sym))
    assert output.index("pin_names") < output.index("DEMO_1_1")


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


def test_clone_preserves_name() -> None:
    sym = ks.symbol(
        "Original",
        ks.unit_symbol("Original", 0, 1),
        ks.unit_symbol("Original", 1, 1),
    )
    cloned = ks.clone(sym)
    assert cloned.name == "Original"
    for nested in ks.nested_symbols(cloned):
        assert nested.name is not None
    assert ks.nested_symbol_names(cloned) == ["Original_0_1", "Original_1_1"]


def test_filter_children_without_name_concern() -> None:
    """Filtering node[1:] should just work — no name string to accidentally drop."""
    sym = ks.symbol(
        "PART",
        ks.unit_symbol(
            "PART",
            0,
            1,
            ks.rectangle((-5, 5), (5, -5)),
            ks.circle((0, 0), 1.0),
            ks.rectangle((-3, 3), (3, -3)),
        ),
    )
    gfx = ks.get_nested_symbol(sym, "PART_0_1")

    # Remove all rectangles — standard Python filtering
    gfx[1:] = [c for c in gfx[1:] if ks.head(c) != "rectangle"]

    assert gfx.name == "PART_0_1"
    assert len(gfx) == 2  # head + circle
    assert ks.head(gfx[1]) == "circle"


def test_symbols_with_different_names_are_not_equal() -> None:
    """Symbol name is part of identity — different names must not compare equal."""
    a = ks.symbol("A", ks.property("Reference", "U"))
    b = ks.symbol("B", ks.property("Reference", "U"))
    assert a != b

    # Same name, same children — should be equal
    a2 = ks.symbol("A", ks.property("Reference", "U"))
    assert a == a2

    # Nested symbols too
    u1 = ks.unit_symbol("PART", 0, 1)
    u2 = ks.unit_symbol("PART", 1, 1)
    assert u1 != u2
