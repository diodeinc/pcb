"""
Tests for layout fragment footprint position extraction.

These tests verify that when a parent module has a layout_path,
new footprints can inherit their positions from the layout fragment.
"""

from typing import Dict

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
    _get_fragment_footprint_complement,
)
from ..changeset import build_sync_changeset


def make_footprint_view(
    path: str, reference: str = "", fpid: str = ""
) -> FootprintView:
    """Helper to create a FootprintView with sensible defaults."""
    entity_id = EntityId.from_string(path)
    return FootprintView(
        entity_id=entity_id,
        reference=reference or path.split(".")[-1],
        value="1k",
        fpid=fpid or "Resistor_SMD:R_0603",
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


def make_fragment_loader(cache: Dict[str, FragmentData]):
    """Create a simple fragment loader from a dict cache."""
    def loader(layout_path: str) -> FragmentData:
        if layout_path not in cache:
            raise FileNotFoundError(f"Fragment not found: {layout_path}")
        return cache[layout_path]
    return loader


class TestFragmentData:
    """Tests for FragmentData lookup behavior."""

    def test_lookup_by_entity_name(self):
        """Footprint can be found by entity name (last segment)."""
        cache = FragmentData(
            group_complement=GroupComplement(),
            footprint_complements={
                "R1": make_footprint_complement(x=1000, y=2000),
                "C1": make_footprint_complement(x=3000, y=4000),
            },
        )

        power_group = EntityId.from_string("Power")
        new_view = BoardView(
            footprints={
                EntityId.from_string("Power.R1"): make_footprint_view("Power.R1"),
            },
            groups={
                power_group: GroupView(
                    entity_id=power_group,
                    member_ids=(),
                    layout_path="./power_layout",
                ),
            },
        )

        r1_id = EntityId.from_string("Power.R1")
        fragment_loader = make_fragment_loader({"./power_layout": cache})

        result = _get_fragment_footprint_complement(
            r1_id,
            new_view,
            fragment_loader,
        )

        assert result is not None
        assert result.position.x == 1000
        assert result.position.y == 2000

    def test_lookup_by_relative_path(self):
        """Footprint can be found by relative path from parent."""
        cache = FragmentData(
            group_complement=GroupComplement(),
            footprint_complements={
                "Sub.R1": make_footprint_complement(x=5000, y=6000),
            },
        )

        power_group = EntityId.from_string("Power")
        new_view = BoardView(
            footprints={
                EntityId.from_string("Power.Sub.R1"): make_footprint_view(
                    "Power.Sub.R1"
                ),
            },
            groups={
                power_group: GroupView(
                    entity_id=power_group,
                    member_ids=(),
                    layout_path="./power_layout",
                ),
            },
        )

        r1_id = EntityId.from_string("Power.Sub.R1")
        fragment_loader = make_fragment_loader({"./power_layout": cache})

        result = _get_fragment_footprint_complement(
            r1_id,
            new_view,
            fragment_loader,
        )

        assert result is not None
        assert result.position.x == 5000
        assert result.position.y == 6000

    def test_lookup_returns_none_when_not_found(self):
        """Returns None when footprint not found in cache."""
        cache = FragmentData(
            group_complement=GroupComplement(),
            footprint_complements={
                "R1": make_footprint_complement(x=1000, y=2000),
            },
        )

        power_group = EntityId.from_string("Power")
        new_view = BoardView(
            footprints={
                EntityId.from_string("Power.C1"): make_footprint_view("Power.C1"),
            },
            groups={
                power_group: GroupView(
                    entity_id=power_group,
                    member_ids=(),
                    layout_path="./power_layout",
                ),
            },
        )

        c1_id = EntityId.from_string("Power.C1")
        fragment_loader = make_fragment_loader({"./power_layout": cache})

        result = _get_fragment_footprint_complement(
            c1_id,
            new_view,
            fragment_loader,
        )

        assert result is None

    def test_no_parent_with_layout_path(self):
        """Returns None when no parent has layout_path."""
        r1_id = EntityId.from_string("R1")

        new_view = BoardView(
            footprints={
                r1_id: make_footprint_view("R1"),
            },
            groups={},  # No groups
        )

        # Even with a loader, returns None if no parent group with layout_path
        fragment_loader = make_fragment_loader({})

        result = _get_fragment_footprint_complement(
            r1_id,
            new_view,
            fragment_loader,
        )

        assert result is None


class TestLayoutFragmentInAdaptComplement:
    """Tests for layout fragment integration with adapt_complement."""

    def test_new_footprint_uses_layout_fragment_position(self):
        """New footprints should get positions from layout fragment."""
        power_group = EntityId.from_string("Power")
        r1_id = EntityId.from_string("Power.R1")

        cache = FragmentData(
            group_complement=GroupComplement(),
            footprint_complements={
                "R1": make_footprint_complement(x=15000000, y=25000000),  # 15mm, 25mm
            },
        )

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
        fragment_loader = make_fragment_loader({"./power_layout": cache})

        new_complement = adapt_complement(
            new_view,
            old_complement,
            fragment_loader=fragment_loader,
        )

        assert r1_id in new_complement.footprints
        fp_comp = new_complement.footprints[r1_id]
        assert fp_comp.position.x == 15000000
        assert fp_comp.position.y == 25000000
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert r1_id in changeset.added_footprints

    def test_existing_footprint_ignores_layout_fragment(self):
        """Existing footprints should NOT use layout fragment (preserve user position)."""
        power_group = EntityId.from_string("Power")
        r1_id = EntityId.from_string("Power.R1")

        cache = FragmentData(
            group_complement=GroupComplement(),
            footprint_complements={
                "R1": make_footprint_complement(x=15000000, y=25000000),
            },
        )

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

        # R1 already exists at different position
        old_complement = BoardComplement(
            footprints={
                r1_id: make_footprint_complement(x=1000000, y=2000000),  # User-placed
            },
        )

        fragment_loader = make_fragment_loader({"./power_layout": cache})

        new_complement = adapt_complement(
            new_view,
            old_complement,
            fragment_loader=fragment_loader,
        )

        # Position should be preserved from old complement, not layout fragment
        fp_comp = new_complement.footprints[r1_id]
        assert fp_comp.position.x == 1000000
        assert fp_comp.position.y == 2000000
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert r1_id not in changeset.added_footprints

    def test_new_footprint_without_layout_gets_default(self):
        """New footprints without layout fragment should get default position."""
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
                    layout_path="./power_layout",  # Has path but fragment not found
                ),
            },
        )

        old_complement = BoardComplement()
        
        # Loader that always raises (simulates missing fragment)
        def missing_loader(path: str) -> FragmentData:
            raise FileNotFoundError(f"Fragment not found: {path}")

        new_complement = adapt_complement(
            new_view,
            old_complement,
            fragment_loader=missing_loader,
        )

        # Should get default position (0, 0)
        fp_comp = new_complement.footprints[r1_id]
        assert fp_comp.position.x == 0
        assert fp_comp.position.y == 0

    def test_multiple_footprints_from_same_layout(self):
        """Multiple new footprints can get positions from same layout fragment."""
        power_group = EntityId.from_string("Power")
        r1_id = EntityId.from_string("Power.R1")
        c1_id = EntityId.from_string("Power.C1")

        cache = FragmentData(
            group_complement=GroupComplement(),
            footprint_complements={
                "R1": make_footprint_complement(x=10000000, y=10000000),
                "C1": make_footprint_complement(x=20000000, y=20000000),
            },
        )

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
        fragment_loader = make_fragment_loader({"./power_layout": cache})

        new_complement = adapt_complement(
            new_view,
            old_complement,
            fragment_loader=fragment_loader,
        )

        # Both should get their positions from cache
        r1_comp = new_complement.footprints[r1_id]
        c1_comp = new_complement.footprints[c1_id]

        assert r1_comp.position.x == 10000000
        assert c1_comp.position.x == 20000000


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
