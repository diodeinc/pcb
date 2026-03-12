"""Tiny no-dependency KiCad symbol S-expression helper.

`kicad_sym` is intentionally small and Python-native:

- forms are plain Python ``list`` objects
- quoted strings are plain Python ``str``
- bare atoms are :class:`Sym`
- integers and floats stay native

The intended workflow is read -> modify -> write. There is no patch DSL and no
semantic object model hiding KiCad's real structure.
"""

from __future__ import annotations

from collections.abc import Callable, Iterable, Iterator, Sequence
from pathlib import Path
from typing import TypeAlias

__all__ = [
    "DEFAULT_FONT_SIZE",
    "DEFAULT_GENERATOR",
    "DEFAULT_GENERATOR_VERSION",
    "DEFAULT_LIB_VERSION",
    "DEFAULT_PIN_LENGTH",
    "Atom",
    "Form",
    "Node",
    "ParseError",
    "Sym",
    "at",
    "at_form",
    "children",
    "circle",
    "clone",
    "del_property",
    "direct_properties",
    "direct_symbols",
    "dumps",
    "effects",
    "fill",
    "find_all",
    "find_one",
    "font",
    "form",
    "get_nested_symbol",
    "get_property",
    "get_property_form",
    "get_symbol",
    "head",
    "is_form",
    "library",
    "load",
    "nested_symbol_names",
    "nested_symbols",
    "parse",
    "pin",
    "polyline",
    "property",
    "pts",
    "rectangle",
    "save",
    "set_property",
    "size",
    "stroke",
    "sym",
    "symbol",
    "symbol_name",
    "symbol_names",
    "text",
    "unit_symbol",
    "unit_symbol_name",
    "walk",
    "xy",
    "yesno",
]

__version__ = "0.1.0"

DEFAULT_LIB_VERSION = 20241209
DEFAULT_GENERATOR = "kicad_symbol_editor"
DEFAULT_GENERATOR_VERSION = "9.0"
DEFAULT_FONT_SIZE = (1.27, 1.27)
DEFAULT_PIN_LENGTH = 2.54


class ParseError(ValueError):
    """Raised when KiCad S-expression parsing fails."""


class Sym(str):
    """Bare symbol atom.

    Plain :class:`str` values are emitted as quoted KiCad strings. Use
    :class:`Sym` for unquoted atoms such as ``pin``, ``passive``, ``yes``, or
    ``hide``.
    """


Atom: TypeAlias = Sym | str | int | float | bool
Node: TypeAlias = Atom | list["Node"]
Form: TypeAlias = list[Node]


def sym(name: str) -> Sym:
    """Return a bare symbol atom."""

    return Sym(name)


def yesno(value: bool) -> Sym:
    """Return KiCad's conventional ``yes`` or ``no`` symbol atom."""

    return Sym("yes" if value else "no")


def form(head: str | Sym, *items: Node | None) -> Form:
    """Create an S-expression form.

    ``head`` may be a plain string and is converted to :class:`Sym`
    automatically. ``None`` items are ignored so callers can build forms
    conditionally.
    """

    out: Form = [head if isinstance(head, Sym) else Sym(head)]
    out.extend(item for item in items if item is not None)
    return out


def load(path: str | Path) -> Node:
    """Load and parse a KiCad S-expression file."""

    return parse(Path(path).read_text())


def save(path: str | Path, node: Node, *, pretty: bool = True) -> None:
    """Serialize and write a KiCad S-expression file."""

    Path(path).write_text(dumps(node, pretty=pretty))


def parse(text: str) -> Node:
    """Parse one KiCad-style S-expression tree from ``text``."""

    parser = _Parser(text)
    node = parser.parse()
    parser.skip_ws()
    if not parser.eof():
        msg = f"Unexpected trailing input at byte {parser.pos}"
        raise ParseError(msg)
    return node


def dumps(node: Node, *, pretty: bool = True, indent: str = "\t") -> str:
    """Serialize ``node`` to KiCad-style S-expression text."""

    text = _dump_pretty(node, 0, indent).rstrip() if pretty else _dump_dense(node)
    return text + "\n"


def clone(node: Node) -> Node:
    """Deep-copy a parsed tree using native Python containers."""

    if isinstance(node, list):
        return [clone(item) for item in node]
    return node


def head(node: Node) -> str | None:
    """Return the head symbol of a form, or ``None`` for non-forms."""

    if not isinstance(node, list) or not node:
        return None
    first = node[0]
    return str(first) if isinstance(first, Sym) else None


def is_form(node: Node, kind: str | None = None) -> bool:
    """Return whether ``node`` is a form, optionally with a matching head."""

    actual = head(node)
    return actual is not None and (kind is None or actual == kind)


def children(node: Node, kind: str | None = None) -> Iterator[Form]:
    """Yield direct child forms of ``node``.

    If ``kind`` is provided, only children with that head symbol are yielded.
    """

    if not isinstance(node, list):
        return
    for child in node[1:]:
        if isinstance(child, list) and is_form(child, kind):
            yield child


def walk(node: Node, kind: str | None = None) -> Iterator[Form]:
    """Yield forms in preorder from ``node``'s subtree."""

    if isinstance(node, list):
        if is_form(node, kind):
            yield node
        for child in node[1:]:
            yield from walk(child, kind)


def find_all(
    node: Node,
    kind: str | None = None,
    pred: Callable[[Form], bool] | None = None,
) -> list[Form]:
    """Return all matching forms in a subtree."""

    return [item for item in walk(node, kind) if pred is None or pred(item)]


def find_one(
    node: Node,
    kind: str | None = None,
    pred: Callable[[Form], bool] | None = None,
    default: Node | None = None,
) -> Form | Node | None:
    """Return the first matching form in a subtree."""

    for item in walk(node, kind):
        if pred is None or pred(item):
            return item
    return default


def library(
    *symbols: Node,
    version: int = DEFAULT_LIB_VERSION,
    generator: str = DEFAULT_GENERATOR,
    generator_version: str | None = DEFAULT_GENERATOR_VERSION,
    embedded_fonts: bool | None = None,
) -> Form:
    """Build a ``(kicad_symbol_lib ...)`` root form."""

    return form(
        "kicad_symbol_lib",
        form("version", version),
        form("generator", generator),
        form("generator_version", generator_version) if generator_version else None,
        *symbols,
        form("embedded_fonts", yesno(embedded_fonts)) if embedded_fonts is not None else None,
    )


def symbol(name: str, *items: Node) -> Form:
    """Build a top-level or nested ``(symbol ...)`` form."""

    return form("symbol", name, *items)


def symbol_name(symbol_node: Sequence[Node]) -> str:
    """Return the quoted name of a ``(symbol ...)`` form."""

    if head(symbol_node) != "symbol":
        raise ValueError("Expected `(symbol ...)` node")
    if len(symbol_node) < 2 or not isinstance(symbol_node[1], str):
        raise ValueError("Symbol is missing its quoted name")
    return symbol_node[1]


def unit_symbol_name(parent_name: str, unit: int, style: int) -> str:
    """Return KiCad's conventional nested symbol name for one unit/style."""

    return f"{parent_name}_{unit}_{style}"


def unit_symbol(parent_name: str, unit: int, style: int, *items: Node) -> Form:
    """Build a nested unit/style ``(symbol ...)`` form."""

    return symbol(unit_symbol_name(parent_name, unit, style), *items)


def direct_symbols(lib: Sequence[Node]) -> list[Form]:
    """Return direct top-level symbol forms from a library root."""

    if head(lib) != "kicad_symbol_lib":
        raise ValueError("Expected `(kicad_symbol_lib ...)` root")
    return [child for child in children(lib, "symbol")]


def symbol_names(lib: Sequence[Node]) -> list[str]:
    """Return the names of top-level symbols in a library."""

    return [symbol_name(node) for node in direct_symbols(lib)]


def get_symbol(lib: Sequence[Node], name: str | None = None) -> Form:
    """Return one top-level symbol from a library.

    If ``name`` is omitted, the library must contain exactly one top-level
    symbol.
    """

    symbols = direct_symbols(lib)
    if name is None:
        if len(symbols) != 1:
            msg = f"Expected a single-symbol library, found {len(symbols)} symbols"
            raise ValueError(msg)
        return symbols[0]
    for node in symbols:
        if symbol_name(node) == name:
            return node
    raise KeyError(f"Symbol `{name}` not found")


def nested_symbols(symbol_node: Sequence[Node]) -> list[Form]:
    """Return direct nested unit/style symbol forms from a top-level symbol."""

    if head(symbol_node) != "symbol":
        raise ValueError("Expected `(symbol ...)` node")
    return [child for child in children(symbol_node, "symbol")]


def nested_symbol_names(symbol_node: Sequence[Node]) -> list[str]:
    """Return direct nested unit/style symbol names from a top-level symbol."""

    return [symbol_name(node) for node in nested_symbols(symbol_node)]


def get_nested_symbol(symbol_node: Sequence[Node], name: str) -> Form:
    """Return one direct nested unit/style symbol by name."""

    for node in nested_symbols(symbol_node):
        if symbol_name(node) == name:
            return node
    raise KeyError(f"Nested symbol `{name}` not found")


def direct_properties(symbol_node: Sequence[Node]) -> list[Form]:
    """Return direct ``(property ...)`` forms from a symbol."""

    if head(symbol_node) != "symbol":
        raise ValueError("Expected `(symbol ...)` node")
    return [child for child in children(symbol_node, "property")]


def get_property_form(symbol_node: Sequence[Node], name: str) -> Form | None:
    """Return one direct ``(property ...)`` form by name."""

    for prop in direct_properties(symbol_node):
        if len(prop) >= 3 and prop[1] == name:
            return prop
    return None


def get_property(symbol_node: Sequence[Node], name: str, default: str | None = None) -> str | None:
    """Return the value of a direct property by name."""

    prop = get_property_form(symbol_node, name)
    if prop is None:
        return default
    value = prop[2] if len(prop) >= 3 else default
    return value if isinstance(value, str) else default


def set_property(
    symbol_node: Form,
    name: str,
    value: str,
    *,
    at: tuple[float | int, float | int, float | int] | None = None,
    effects_node: Node | None = None,
    hidden: bool | None = None,
    prop_id: int | None = None,
) -> Form:
    """Create or update one direct symbol property."""

    prop = get_property_form(symbol_node, name)
    if prop is None:
        prop = property(
            name,
            value,
            at=at,
            effects_node=effects_node,
            hidden=False if hidden is None else hidden,
            prop_id=prop_id,
        )
        _insert_before_nested_symbols(symbol_node, prop)
        return prop

    while len(prop) < 3:
        prop.append("")
    prop[2] = value

    if at is not None:
        _replace_or_append_child(prop, "at", at_form(*at))
    if effects_node is not None:
        _replace_or_append_child(prop, "effects", effects_node)
    elif hidden is not None:
        _set_hidden_in_effects(prop, hidden)
    if prop_id is not None:
        _replace_or_append_child(prop, "id", form("id", prop_id))
    return prop


def del_property(symbol_node: Form, name: str) -> bool:
    """Delete one direct property by name."""

    for idx, child in enumerate(symbol_node[1:], start=1):
        if (
            isinstance(child, list)
            and head(child) == "property"
            and len(child) >= 2
            and child[1] == name
        ):
            del symbol_node[idx]
            return True
    return False


def at_form(x: float | int, y: float | int, rot: float | int = 0) -> Form:
    """Build an ``(at x y rot)`` form."""

    return form("at", x, y, rot)


def at(x: float | int, y: float | int, rot: float | int = 0) -> Form:
    """Alias for :func:`at_form`."""

    return at_form(x, y, rot)


def xy(x: float | int, y: float | int) -> Form:
    """Build an ``(xy x y)`` point form."""

    return form("xy", x, y)


def pts(points: Iterable[tuple[float | int, float | int]]) -> Form:
    """Build a ``(pts ...)`` form from point tuples."""

    return form("pts", *(xy(x, y) for x, y in points))


def size(width: float | int, height: float | int) -> Form:
    """Build a ``(size width height)`` form."""

    return form("size", width, height)


def stroke(
    width: float | int | None = None,
    *,
    stroke_type: str | None = None,
    color: tuple[float | int, float | int, float | int, float | int] | None = None,
) -> Form:
    """Build a ``(stroke ...)`` form."""

    return form(
        "stroke",
        form("width", width) if width is not None else None,
        form("type", sym(stroke_type)) if stroke_type is not None else None,
        form("color", *color) if color is not None else None,
    )


def fill(
    fill_type: str | None = None,
    *,
    color: tuple[float | int, float | int, float | int, float | int] | None = None,
) -> Form:
    """Build a ``(fill ...)`` form."""

    return form(
        "fill",
        form("type", sym(fill_type)) if fill_type is not None else None,
        form("color", *color) if color is not None else None,
    )


def font(
    width: float | int = DEFAULT_FONT_SIZE[0],
    height: float | int = DEFAULT_FONT_SIZE[1],
    *,
    thickness: float | int | None = None,
    bold: bool = False,
    italic: bool = False,
) -> Form:
    """Build a KiCad ``(font ...)`` form."""

    return form(
        "font",
        size(width, height),
        form("thickness", thickness) if thickness is not None else None,
        form("bold") if bold else None,
        form("italic") if italic else None,
    )


def effects(
    *items: Node,
    font_node: Node | None = None,
    hidden: bool = False,
    justify: str | Sequence[str] | None = None,
) -> Form:
    """Build a KiCad ``(effects ...)`` form."""

    if font_node is None:
        font_node = font()

    justify_node = None
    if justify is not None:
        justify_items = [justify] if isinstance(justify, str) else list(justify)
        justify_node = form("justify", *(sym(item) for item in justify_items))

    return form(
        "effects",
        font_node,
        justify_node,
        form("hide", yesno(True)) if hidden else None,
        *items,
    )


def property(
    name: str,
    value: str,
    *,
    at: tuple[float | int, float | int, float | int] | None = (0, 0, 0),
    effects_node: Node | None = None,
    hidden: bool = False,
    prop_id: int | None = None,
) -> Form:
    """Build a direct symbol ``(property ...)`` form."""

    if effects_node is None:
        effects_node = effects(hidden=hidden)
    if at is None:
        at = (0, 0, 0)
    return form(
        "property",
        name,
        value,
        at_form(*at),
        form("id", prop_id) if prop_id is not None else None,
        effects_node,
    )


def pin(
    number: str | int,
    name: str,
    *,
    at: tuple[float | int, float | int, float | int],
    electrical: str = "passive",
    graphic: str = "line",
    length: float | int = DEFAULT_PIN_LENGTH,
    hide: bool = False,
    name_effects: Node | None = None,
    number_effects: Node | None = None,
) -> Form:
    """Build a KiCad ``(pin ...)`` form."""

    if name_effects is None:
        name_effects = effects()
    if number_effects is None:
        number_effects = effects()

    return form(
        "pin",
        sym(electrical),
        sym(graphic),
        at_form(*at),
        form("length", length),
        form("hide") if hide else None,
        form("name", str(name), name_effects),
        form("number", str(number), number_effects),
    )


def text(
    value: str,
    *,
    at: tuple[float | int, float | int, float | int],
    effects_node: Node | None = None,
) -> Form:
    """Build a KiCad ``(text ...)`` form."""

    if effects_node is None:
        effects_node = effects()
    return form("text", value, at_form(*at), effects_node)


def rectangle(
    start: tuple[float | int, float | int],
    end: tuple[float | int, float | int],
    *,
    stroke_node: Node | None = None,
    fill_node: Node | None = None,
) -> Form:
    """Build a KiCad ``(rectangle ...)`` form."""

    if stroke_node is None:
        stroke_node = stroke(0, stroke_type="default")
    if fill_node is None:
        fill_node = fill("none")
    return form("rectangle", form("start", *start), form("end", *end), stroke_node, fill_node)


def polyline(
    points: Iterable[tuple[float | int, float | int]],
    *,
    stroke_node: Node | None = None,
    fill_node: Node | None = None,
) -> Form:
    """Build a KiCad ``(polyline ...)`` form."""

    if stroke_node is None:
        stroke_node = stroke(0, stroke_type="default")
    if fill_node is None:
        fill_node = fill("none")
    return form("polyline", pts(points), stroke_node, fill_node)


def circle(
    center: tuple[float | int, float | int],
    radius: float | int,
    *,
    stroke_node: Node | None = None,
    fill_node: Node | None = None,
) -> Form:
    """Build a KiCad ``(circle ...)`` form."""

    if stroke_node is None:
        stroke_node = stroke(0, stroke_type="default")
    if fill_node is None:
        fill_node = fill("none")
    return form("circle", form("center", *center), form("radius", radius), stroke_node, fill_node)


def _insert_before_nested_symbols(symbol_node: Form, item: Node) -> None:
    for idx, child in enumerate(symbol_node[1:], start=1):
        if isinstance(child, list) and head(child) == "symbol":
            symbol_node.insert(idx, item)
            return
    symbol_node.append(item)


def _replace_or_append_child(parent: Form, kind: str, node: Node) -> None:
    for idx, child in enumerate(parent[1:], start=1):
        if isinstance(child, list) and head(child) == kind:
            parent[idx] = node
            return
    parent.append(node)


def _set_hidden_in_effects(node: Form, hidden: bool) -> None:
    effects_node = next(children(node, "effects"), None)
    if effects_node is None:
        node.append(effects(hidden=hidden))
        return

    hide_idx = next(
        (
            idx
            for idx, child in enumerate(effects_node[1:], start=1)
            if isinstance(child, list) and head(child) == "hide"
        ),
        None,
    )

    if hidden:
        hide_form = form("hide", yesno(True))
        if hide_idx is None:
            effects_node.append(hide_form)
        else:
            effects_node[hide_idx] = hide_form
    elif hide_idx is not None:
        del effects_node[hide_idx]


def _dump_pretty(node: Node, level: int, indent: str) -> str:
    if not isinstance(node, list):
        return _dump_atom(node)

    if not node:
        return "()"

    dense = _dump_dense(node)
    if len(dense) <= 88 and all(not isinstance(item, list) for item in node[1:]):
        return dense

    prefix: list[Node] = []
    rest: list[Node] = []
    hit_nested = False
    for item in node:
        if hit_nested or isinstance(item, list):
            hit_nested = True
            rest.append(item)
        else:
            prefix.append(item)

    if not prefix:
        prefix = [node[0]]
        rest = node[1:]

    pieces = ["(" + " ".join(_dump_atom(item) for item in prefix)]
    child_indent = indent * (level + 1)
    for item in rest:
        pieces.append("\n" + child_indent + _dump_pretty(item, level + 1, indent))
    pieces.append("\n" + indent * level + ")")
    return "".join(pieces)


def _dump_dense(node: Node) -> str:
    if not isinstance(node, list):
        return _dump_atom(node)
    return "(" + " ".join(_dump_dense(item) for item in node) + ")"


def _dump_atom(value: Node) -> str:
    if isinstance(value, Sym):
        return str(value)
    if isinstance(value, str):
        escaped = (
            value.replace("\\", "\\\\")
            .replace('"', '\\"')
            .replace("\n", "\\n")
            .replace("\t", "\\t")
        )
        return f'"{escaped}"'
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        return _format_float(value)
    raise TypeError(f"Unsupported atom type: {type(value)!r}")


def _format_float(value: float) -> str:
    if value == 0:
        return "0"
    text = format(value, ".15g")
    if "e" in text or "E" in text:
        text = format(value, ".15f").rstrip("0").rstrip(".")
    return "0" if text == "-0" else text


class _Parser:
    def __init__(self, text: str) -> None:
        self.text = text
        self.pos = 0

    def parse(self) -> Node:
        self.skip_ws()
        if self.eof():
            raise ParseError("Expected s-expression")
        return self.parse_value()

    def parse_value(self) -> Node:
        self.skip_ws()
        if self.eof():
            raise ParseError("Unexpected end of input")

        ch = self.text[self.pos]
        if ch == "(":
            return self.parse_list()
        if ch == '"':
            return self.parse_string()
        if ch == ")":
            msg = f"Unexpected `)` at byte {self.pos}"
            raise ParseError(msg)
        return self.parse_atom()

    def parse_list(self) -> Form:
        self.pos += 1
        out: Form = []
        while True:
            self.skip_ws()
            if self.eof():
                raise ParseError("Unclosed `(`")
            if self.text[self.pos] == ")":
                self.pos += 1
                return out
            out.append(self.parse_value())

    def parse_string(self) -> str:
        self.pos += 1
        out: list[str] = []
        while not self.eof():
            ch = self.text[self.pos]
            self.pos += 1
            if ch == '"':
                return "".join(out)
            if ch != "\\":
                out.append(ch)
                continue
            if self.eof():
                raise ParseError("Unclosed string escape")
            esc = self.text[self.pos]
            self.pos += 1
            out.append(
                {
                    "n": "\n",
                    "r": "\r",
                    "t": "\t",
                    '"': '"',
                    "\\": "\\",
                }.get(esc, esc)
            )
        raise ParseError("Unclosed string literal")

    def parse_atom(self) -> Node:
        start = self.pos
        while not self.eof() and self.text[self.pos] not in "() \t\r\n":
            self.pos += 1
        token = self.text[start : self.pos]
        if token == "":
            msg = f"Expected atom at byte {start}"
            raise ParseError(msg)
        return _coerce_atom(token)

    def skip_ws(self) -> None:
        while not self.eof() and self.text[self.pos].isspace():
            self.pos += 1

    def eof(self) -> bool:
        return self.pos >= len(self.text)


def _coerce_atom(token: str) -> Node:
    if token in {"+", "-"}:
        return Sym(token)

    try:
        return float(token) if any(ch in token for ch in ".eE") else int(token)
    except ValueError:
        return Sym(token)
