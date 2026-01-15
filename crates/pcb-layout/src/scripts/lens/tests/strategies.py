"""
Hypothesis strategies for generating lens types.

These strategies enable property-based testing of the lens laws
by generating arbitrary but valid instances of View, Complement types.

Note: Renames (moved() paths) are now handled in Rust preprocessing.
Paths are already in their final form when the Python sync runs.
"""

from hypothesis import strategies as st
from hypothesis.strategies import composite

from ..types import (
    EntityPath,
    EntityId,
    Position,
    FootprintView,
    FootprintComplement,
    GroupView,
    GroupComplement,
    BoardView,
    BoardComplement,
)


# ═══════════════════════════════════════════════════════════════════════════════
# Primitive Strategies
# ═══════════════════════════════════════════════════════════════════════════════

# Valid segment characters (no dots or colons which are delimiters)
segment_alphabet = st.characters(
    blacklist_categories=("Cs",),  # No surrogates
    blacklist_characters=[".", ":", "\x00"],
)

# Path segment: 1-8 characters
segment_strategy = st.text(
    alphabet=segment_alphabet,
    min_size=1,
    max_size=8,
).filter(lambda s: s.strip() != "")  # No empty/whitespace-only segments


# ═══════════════════════════════════════════════════════════════════════════════
# EntityPath and EntityId Strategies
# ═══════════════════════════════════════════════════════════════════════════════


@composite
def entity_path_strategy(draw, min_depth: int = 1, max_depth: int = 4):
    """Generate a valid EntityPath with 1-4 segments."""
    n = draw(st.integers(min_value=min_depth, max_value=max_depth))
    segments = draw(st.lists(segment_strategy, min_size=n, max_size=n))
    return EntityPath(tuple(segments))


@composite
def entity_id_strategy(draw, min_depth: int = 1, max_depth: int = 4):
    """Generate a valid EntityId."""
    path = draw(entity_path_strategy(min_depth=min_depth, max_depth=max_depth))
    return EntityId(path=path)


@composite
def deep_entity_path_strategy(draw, min_depth: int = 5, max_depth: int = 10):
    """Generate deep paths for edge case testing."""
    return draw(entity_path_strategy(min_depth=min_depth, max_depth=max_depth))


@composite
def unicode_entity_path_strategy(draw):
    """Generate paths with unicode characters."""
    unicode_alphabet = st.characters(
        blacklist_categories=("Cs",),
        blacklist_characters=[".", ":", "\x00"],
        whitelist_categories=("L", "N"),  # Letters and numbers from any script
    )
    unicode_segment = st.text(
        alphabet=unicode_alphabet,
        min_size=1,
        max_size=8,
    ).filter(lambda s: s.strip() != "")

    n = draw(st.integers(min_value=1, max_value=3))
    segments = draw(st.lists(unicode_segment, min_size=n, max_size=n))
    return EntityPath(tuple(segments))


# ═══════════════════════════════════════════════════════════════════════════════
# Position Strategy
# ═══════════════════════════════════════════════════════════════════════════════


@composite
def position_strategy(draw, min_val: int = -100_000_000, max_val: int = 100_000_000):
    """Generate a Position in KiCad units (nanometers)."""
    x = draw(st.integers(min_value=min_val, max_value=max_val))
    y = draw(st.integers(min_value=min_val, max_value=max_val))
    return Position(x=x, y=y)


# ═══════════════════════════════════════════════════════════════════════════════
# FootprintView Strategy
# ═══════════════════════════════════════════════════════════════════════════════

FPID_POOL = [
    "Resistor_SMD:R_0402",
    "Resistor_SMD:R_0603",
    "Resistor_SMD:R_0805",
    "Capacitor_SMD:C_0402",
    "Capacitor_SMD:C_0603",
    "Capacitor_SMD:C_0805",
    "Package_SO:SOIC-8",
    "Package_QFP:TQFP-32",
]

VALUE_POOL = [
    "10k",
    "4.7k",
    "100R",
    "1M",
    "100nF",
    "10uF",
    "1uF",
    "22pF",
    "LM358",
    "ATmega328P",
    "TPS54331",
]


@composite
def footprint_view_strategy(draw, entity_id: EntityId = None):
    """Generate a valid FootprintView."""
    if entity_id is None:
        entity_id = draw(entity_id_strategy())

    reference = entity_id.path.name if entity_id.path.segments else "U1"
    value = draw(st.sampled_from(VALUE_POOL))
    fpid = draw(st.sampled_from(FPID_POOL))
    dnp = draw(st.booleans())
    exclude_from_bom = draw(st.booleans())
    exclude_from_pos = draw(st.booleans())

    return FootprintView(
        entity_id=entity_id,
        reference=reference,
        value=value,
        fpid=fpid,
        dnp=dnp,
        exclude_from_bom=exclude_from_bom,
        exclude_from_pos=exclude_from_pos,
        fields={"Path": str(entity_id.path)},
    )


# ═══════════════════════════════════════════════════════════════════════════════
# FootprintComplement Strategy
# ═══════════════════════════════════════════════════════════════════════════════


@composite
def footprint_complement_strategy(draw):
    """Generate a valid FootprintComplement."""
    position = draw(position_strategy())
    orientation = draw(st.floats(min_value=-180.0, max_value=180.0))
    layer = draw(st.sampled_from(["F.Cu", "B.Cu"]))
    locked = draw(st.booleans())

    # Optionally add reference/value positions
    ref_pos = draw(st.one_of(st.none(), position_strategy()))
    ref_visible = draw(st.booleans())
    val_pos = draw(st.one_of(st.none(), position_strategy()))
    val_visible = draw(st.booleans())

    return FootprintComplement(
        position=position,
        orientation=orientation,
        layer=layer,
        locked=locked,
        reference_position=ref_pos,
        reference_visible=ref_visible,
        value_position=val_pos,
        value_visible=val_visible,
    )


# ═══════════════════════════════════════════════════════════════════════════════
# GroupView and GroupComplement Strategies
# ═══════════════════════════════════════════════════════════════════════════════


@composite
def group_view_strategy(draw, entity_id: EntityId = None, member_ids: tuple = None):
    """Generate a valid GroupView."""
    if entity_id is None:
        entity_id = draw(entity_id_strategy(max_depth=2))

    if member_ids is None:
        member_ids = ()

    layout_path = draw(st.one_of(st.none(), st.just("./layout.kicad_pcb")))

    return GroupView(
        entity_id=entity_id,
        member_ids=member_ids,
        layout_path=layout_path,
    )


def group_complement_strategy():
    """Generate a valid GroupComplement (usually empty for tests)."""
    return st.just(
        GroupComplement(
            tracks=(),
            vias=(),
            zones=(),
            graphics=(),
        )
    )


# ═══════════════════════════════════════════════════════════════════════════════
# BoardView Strategy
# ═══════════════════════════════════════════════════════════════════════════════


@composite
def board_view_strategy(draw, min_footprints: int = 0, max_footprints: int = 6):
    """
    Generate a valid BoardView with footprints and groups.

    Groups are derived from footprint paths (parent paths become groups).
    """
    n = draw(st.integers(min_value=min_footprints, max_value=max_footprints))

    # Generate unique paths
    paths = draw(
        st.lists(
            entity_path_strategy(min_depth=1, max_depth=3),
            min_size=n,
            max_size=n,
            unique=True,
        )
    )

    # Create footprint views
    footprints = {}
    for path in paths:
        entity_id = EntityId(path=path)
        footprints[entity_id] = draw(footprint_view_strategy(entity_id=entity_id))

    # Derive groups from footprint paths
    # Skip groups whose path equals a footprint path (NoLeafGroups invariant)
    groups = {}
    fp_paths = {fp_id.path for fp_id in footprints.keys()}

    for fp_id in footprints.keys():
        parent = fp_id.path.parent()
        while parent and parent.segments:
            # Skip if this path is also a footprint path
            if parent in fp_paths:
                parent = parent.parent()
                continue

            group_id = EntityId(path=parent)
            if group_id not in groups:
                # Find all descendant footprints
                member_ids = [
                    other_id
                    for other_id in footprints.keys()
                    if parent.is_ancestor_of(other_id.path)
                ]

                # Only create group if it has members
                if member_ids:
                    groups[group_id] = GroupView(
                        entity_id=group_id,
                        member_ids=tuple(member_ids),
                    )
            parent = parent.parent()

    return BoardView(
        footprints=footprints,
        groups=groups,
        nets={},
    )


# ═══════════════════════════════════════════════════════════════════════════════
# BoardComplement Strategy
# ═══════════════════════════════════════════════════════════════════════════════


@composite
def board_complement_strategy(draw, view: BoardView = None):
    """
    Generate a BoardComplement.

    If view is provided, generates complements for entities in the view
    plus optionally some extra "stale" complements.
    """
    footprints = {}
    groups = {}

    if view is not None:
        # Create complements for entities in view
        for entity_id in view.footprints.keys():
            if draw(st.booleans()):  # Maybe we have a complement
                footprints[entity_id] = draw(footprint_complement_strategy())

        for entity_id in view.groups.keys():
            if draw(st.booleans()):
                groups[entity_id] = draw(group_complement_strategy())

    # Optionally add some extra "stale" complements
    n_extra = draw(st.integers(min_value=0, max_value=2))
    for _ in range(n_extra):
        extra_id = draw(entity_id_strategy())
        footprints[extra_id] = draw(footprint_complement_strategy())

    return BoardComplement(
        footprints=footprints,
        groups=groups,
    )
