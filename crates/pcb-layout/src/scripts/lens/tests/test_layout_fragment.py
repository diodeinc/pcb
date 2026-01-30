"""
Tests for layout fragment footprint position extraction.

These tests verify that when a parent module has a layout_path,
new footprints can inherit their positions from the layout fragment.
"""

from ..types import (
    EntityId,
    Position,
    FootprintView,
    FootprintComplement,
    GroupView,
    GroupComplement,
    BoardView,
    BoardComplement,
)
from ..lens import (
    adapt_complement,
    FragmentData,
)
from ..changeset import build_sync_changeset


def make_footprint_view(
    path: str, reference: str = "", fpid: str = ""
) -> FootprintView:
    """Helper to create a FootprintView with sensible defaults."""
    actual_fpid = fpid or "Resistor_SMD:R_0603"
    entity_id = EntityId.from_string(path, fpid=actual_fpid)
    return FootprintView(
        entity_id=entity_id,
        reference=reference or path.split(".")[-1],
        value="1k",
        fpid=actual_fpid,
    )


def make_footprint_complement(
    x: int = 0, y: int = 0, layer: str = "F.Cu"
) -> FootprintComplement:
    """Helper to create a FootprintComplement."""
    return FootprintComplement(
        position=Position(x=x, y=y),
        orientation=0.0,
        layer=layer,
    )


class TestLayoutFragmentInAdaptComplement:
    """Tests for adapt_complement behavior with layout fragments.

    Note: Fragment position application has moved to HierPlace time in kicad_adapter.py.
    adapt_complement no longer applies fragment positions - it just returns default
    positions for new footprints. These tests verify that behavior.
    """

    def test_new_footprint_gets_default_position(self):
        """New footprints get default position (0,0) - fragment applied at HierPlace time."""
        power_group = EntityId.from_string("Power")
        r1_id = EntityId.from_string("Power.R1")

        new_view = BoardView(
            footprints={
                r1_id: make_footprint_view("Power.R1"),
            },
            groups={
                power_group: GroupView(
                    entity_id=power_group,
                    member_ids=(r1_id,),
                    layout_path="./power_layout",
                ),
            },
        )

        old_complement = BoardComplement()  # Empty - R1 is new

        new_complement = adapt_complement(new_view, old_complement)

        # New footprints get default position - fragment positions applied at HierPlace time
        assert r1_id in new_complement.footprints
        fp_comp = new_complement.footprints[r1_id]
        assert fp_comp.position.x == 0
        assert fp_comp.position.y == 0
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert r1_id in changeset.added_footprints

    def test_existing_footprint_preserves_position(self):
        """Existing footprints preserve their user-placed position."""
        power_group = EntityId.from_string("Power")
        r1_id = EntityId.from_string("Power.R1")

        new_view = BoardView(
            footprints={
                r1_id: make_footprint_view("Power.R1"),
            },
            groups={
                power_group: GroupView(
                    entity_id=power_group,
                    member_ids=(r1_id,),
                    layout_path="./power_layout",
                ),
            },
        )

        # R1 already exists at user-placed position
        old_complement = BoardComplement(
            footprints={
                r1_id: make_footprint_complement(x=1000000, y=2000000),
            },
        )

        new_complement = adapt_complement(new_view, old_complement)

        # Position should be preserved from old complement
        fp_comp = new_complement.footprints[r1_id]
        assert fp_comp.position.x == 1000000
        assert fp_comp.position.y == 2000000
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert r1_id not in changeset.added_footprints

    def test_new_footprint_without_layout_gets_default(self):
        """New footprints without layout fragment get default position."""
        power_group = EntityId.from_string("Power")
        r1_id = EntityId.from_string("Power.R1")

        new_view = BoardView(
            footprints={
                r1_id: make_footprint_view("Power.R1"),
            },
            groups={
                power_group: GroupView(
                    entity_id=power_group,
                    member_ids=(r1_id,),
                    layout_path="./power_layout",  # Path doesn't matter for adapt_complement
                ),
            },
        )

        old_complement = BoardComplement()

        new_complement = adapt_complement(new_view, old_complement)

        # Should get default position (0, 0)
        fp_comp = new_complement.footprints[r1_id]
        assert fp_comp.position.x == 0
        assert fp_comp.position.y == 0

    def test_multiple_new_footprints_get_default_positions(self):
        """Multiple new footprints all get default positions."""
        power_group = EntityId.from_string("Power")
        r1_id = EntityId.from_string("Power.R1")
        c1_id = EntityId.from_string("Power.C1")

        new_view = BoardView(
            footprints={
                r1_id: make_footprint_view("Power.R1"),
                c1_id: make_footprint_view("Power.C1"),
            },
            groups={
                power_group: GroupView(
                    entity_id=power_group,
                    member_ids=(r1_id, c1_id),
                    layout_path="./power_layout",
                ),
            },
        )

        old_complement = BoardComplement()

        new_complement = adapt_complement(new_view, old_complement)

        # Both get default positions - fragment applied at HierPlace time
        r1_comp = new_complement.footprints[r1_id]
        c1_comp = new_complement.footprints[c1_id]

        assert r1_comp.position.x == 0
        assert c1_comp.position.x == 0


class TestFragmentDataDataClass:
    """Tests for FragmentData dataclass."""

    def test_cache_stores_group_complement(self):
        """Cache stores the group complement for tracks/vias/zones."""
        group_comp = GroupComplement(
            tracks=(),
            vias=(),
            zones=(),
            graphics=(),
        )

        cache = FragmentData(
            group_complement=group_comp,
            footprint_complements={},
        )

        assert cache.group_complement == group_comp

    def test_cache_stores_footprint_complements(self):
        """Cache stores footprint complements by reference/path."""
        fp_comp = make_footprint_complement(x=1000, y=2000)

        cache = FragmentData(
            group_complement=GroupComplement(),
            footprint_complements={"R1": fp_comp, "Path.C1": fp_comp},
        )

        assert cache.footprint_complements["R1"] == fp_comp
        assert cache.footprint_complements["Path.C1"] == fp_comp
