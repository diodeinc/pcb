"""
Tests for kicad_adapter pure functions and helpers.

These tests verify the pure computation logic in the adapter layer
without requiring actual KiCad objects.
"""

from typing import Dict, Tuple

from ..types import (
    EntityPath,
    EntityId,
    Position,
    FootprintComplement,
    GroupComplement,
    TrackComplement,
    ViaComplement,
    ZoneComplement,
    default_footprint_complement,
)
from ..lens import (
    build_fragment_net_remap,
    FragmentData,
)


class TestBuildFragmentNetRemap:
    """Tests for the pure build_fragment_net_remap function."""

    def test_simple_single_pad_mapping(self):
        """Single pad maps fragment net to board net."""
        group_path = EntityPath.from_string("Power")
        member_paths = [EntityPath.from_string("Power.R1")]

        # Fragment: R1.1 is connected to "LOCAL_VCC"
        fragment_pad_net_map: Dict[Tuple[str, str], str] = {
            ("R1", "1"): "LOCAL_VCC",
        }

        # Board: Power.R1.1 is connected to "VCC_3V3"
        board_pad_net_map: Dict[Tuple[EntityId, str], str] = {
            (EntityId.from_string("Power.R1"), "1"): "VCC_3V3",
        }

        net_remap, warnings = build_fragment_net_remap(
            group_path, member_paths, fragment_pad_net_map, board_pad_net_map
        )

        assert net_remap == {"LOCAL_VCC": "VCC_3V3"}
        assert warnings == []

    def test_multiple_pads_same_net(self):
        """Multiple pads on same net should produce single mapping."""
        group_path = EntityPath.from_string("Power")
        member_paths = [
            EntityPath.from_string("Power.R1"),
            EntityPath.from_string("Power.R2"),
        ]

        fragment_pad_net_map: Dict[Tuple[str, str], str] = {
            ("R1", "1"): "LOCAL_GND",
            ("R2", "2"): "LOCAL_GND",
        }

        board_pad_net_map: Dict[Tuple[EntityId, str], str] = {
            (EntityId.from_string("Power.R1"), "1"): "GND",
            (EntityId.from_string("Power.R2"), "2"): "GND",
        }

        net_remap, warnings = build_fragment_net_remap(
            group_path, member_paths, fragment_pad_net_map, board_pad_net_map
        )

        assert net_remap == {"LOCAL_GND": "GND"}
        assert warnings == []

    def test_multiple_different_nets(self):
        """Multiple different nets should each get their own mapping."""
        group_path = EntityPath.from_string("Power")
        member_paths = [EntityPath.from_string("Power.R1")]

        fragment_pad_net_map: Dict[Tuple[str, str], str] = {
            ("R1", "1"): "LOCAL_VCC",
            ("R1", "2"): "LOCAL_GND",
        }

        board_pad_net_map: Dict[Tuple[EntityId, str], str] = {
            (EntityId.from_string("Power.R1"), "1"): "VCC_3V3",
            (EntityId.from_string("Power.R1"), "2"): "GND",
        }

        net_remap, warnings = build_fragment_net_remap(
            group_path, member_paths, fragment_pad_net_map, board_pad_net_map
        )

        assert net_remap == {"LOCAL_VCC": "VCC_3V3", "LOCAL_GND": "GND"}
        assert warnings == []

    def test_conflict_produces_warning(self):
        """Conflicting mappings should produce warnings."""
        group_path = EntityPath.from_string("Power")
        member_paths = [
            EntityPath.from_string("Power.R1"),
            EntityPath.from_string("Power.R2"),
        ]

        # Both pads have same fragment net but different board nets
        fragment_pad_net_map: Dict[Tuple[str, str], str] = {
            ("R1", "1"): "LOCAL_VCC",
            ("R2", "1"): "LOCAL_VCC",
        }

        board_pad_net_map: Dict[Tuple[EntityId, str], str] = {
            (EntityId.from_string("Power.R1"), "1"): "VCC_3V3",
            (EntityId.from_string("Power.R2"), "1"): "VCC_5V",  # Different!
        }

        net_remap, warnings = build_fragment_net_remap(
            group_path, member_paths, fragment_pad_net_map, board_pad_net_map
        )

        # First mapping wins
        assert net_remap == {"LOCAL_VCC": "VCC_3V3"}
        assert len(warnings) == 1
        assert "LOCAL_VCC" in warnings[0]
        assert "conflict" in warnings[0].lower()

    def test_unmapped_fragment_pad_ignored(self):
        """Pads not in fragment_pad_net_map are silently ignored."""
        group_path = EntityPath.from_string("Power")
        member_paths = [EntityPath.from_string("Power.R1")]

        fragment_pad_net_map: Dict[Tuple[str, str], str] = {
            # R1.1 is NOT in fragment map
        }

        board_pad_net_map: Dict[Tuple[EntityId, str], str] = {
            (EntityId.from_string("Power.R1"), "1"): "VCC_3V3",
        }

        net_remap, warnings = build_fragment_net_remap(
            group_path, member_paths, fragment_pad_net_map, board_pad_net_map
        )

        assert net_remap == {}
        assert warnings == []

    def test_nested_path_uses_relative_lookup(self):
        """Nested footprint paths should use relative path for fragment lookup."""
        group_path = EntityPath.from_string("TopModule.Power")
        member_paths = [EntityPath.from_string("TopModule.Power.R1")]

        # Fragment uses relative path "R1"
        fragment_pad_net_map: Dict[Tuple[str, str], str] = {
            ("R1", "1"): "LOCAL_VCC",
        }

        board_pad_net_map: Dict[Tuple[EntityId, str], str] = {
            (EntityId.from_string("TopModule.Power.R1"), "1"): "VCC_3V3",
        }

        net_remap, warnings = build_fragment_net_remap(
            group_path, member_paths, fragment_pad_net_map, board_pad_net_map
        )

        assert net_remap == {"LOCAL_VCC": "VCC_3V3"}
        assert warnings == []

    def test_empty_inputs_returns_empty(self):
        """Empty inputs should return empty results."""
        group_path = EntityPath.from_string("Power")

        net_remap, warnings = build_fragment_net_remap(group_path, [], {}, {})

        assert net_remap == {}
        assert warnings == []


class TestFragmentData:
    """Tests for FragmentData dataclass structure (pure Python, no KiCad objects)."""

    def test_has_required_fields(self):
        """FragmentData should have all required fields."""
        cache = FragmentData(
            group_complement=GroupComplement(),
            footprint_complements={"R1": default_footprint_complement()},
            pad_net_map={("R1", "1"): "VCC"},
        )

        assert cache.group_complement is not None
        assert "R1" in cache.footprint_complements
        assert ("R1", "1") in cache.pad_net_map

    def test_default_pad_net_map(self):
        """pad_net_map should default to empty dict."""
        cache = FragmentData(
            group_complement=GroupComplement(),
            footprint_complements={},
        )

        assert cache.pad_net_map == {}

    def test_stores_routing_in_group_complement(self):
        """Routing data should be stored in GroupComplement dataclasses."""
        gc = GroupComplement(
            tracks=(
                TrackComplement(
                    uuid="t1",
                    start=Position(0, 0),
                    end=Position(1000, 0),
                    width=200,
                    layer="F.Cu",
                    net_name="VCC",
                ),
            ),
            vias=(
                ViaComplement(
                    uuid="v1",
                    position=Position(500, 0),
                    diameter=800,
                    drill=400,
                    net_name="VCC",
                ),
            ),
        )

        cache = FragmentData(
            group_complement=gc,
            footprint_complements={},
        )

        assert len(cache.group_complement.tracks) == 1
        assert len(cache.group_complement.vias) == 1
        assert cache.group_complement.tracks[0].net_name == "VCC"


class TestGroupComplementRouting:
    """Tests for GroupComplement routing data handling."""

    def test_group_complement_empty(self):
        """Empty group complement should be detected."""
        gc = GroupComplement()
        assert gc.is_empty

    def test_group_complement_with_tracks(self):
        """Group complement with tracks is not empty."""
        gc = GroupComplement(
            tracks=(
                TrackComplement(
                    uuid="1234",
                    start=Position(0, 0),
                    end=Position(1000, 0),
                    width=200,
                    layer="F.Cu",
                    net_name="VCC",
                ),
            ),
        )
        assert not gc.is_empty

    def test_group_complement_with_vias(self):
        """Group complement with vias is not empty."""
        gc = GroupComplement(
            vias=(
                ViaComplement(
                    uuid="5678",
                    position=Position(500, 500),
                    diameter=800,
                    drill=400,
                    net_name="GND",
                ),
            ),
        )
        assert not gc.is_empty

    def test_group_complement_with_zones(self):
        """Group complement with zones is not empty."""
        gc = GroupComplement(
            zones=(
                ZoneComplement(
                    uuid="abcd",
                    name="GND_ZONE",
                    outline=(Position(0, 0), Position(1000, 0), Position(1000, 1000)),
                    layer="F.Cu",
                    priority=0,
                    net_name="GND",
                ),
            ),
        )
        assert not gc.is_empty


class TestFootprintComplementPlacement:
    """Tests for FootprintComplement placement data."""

    def test_with_position_creates_new_complement(self):
        """with_position should return new complement with updated position."""
        original = FootprintComplement(
            position=Position(0, 0),
            orientation=45.0,
            layer="F.Cu",
            locked=True,
        )

        new_pos = Position(1000, 2000)
        updated = original.with_position(new_pos)

        # Original unchanged
        assert original.position.x == 0
        assert original.position.y == 0

        # New has updated position
        assert updated.position.x == 1000
        assert updated.position.y == 2000

        # Other fields preserved
        assert updated.orientation == 45.0
        assert updated.layer == "F.Cu"
        assert updated.locked is True

    def test_with_locked_creates_new_complement(self):
        """with_locked should return new complement with updated lock state."""
        original = FootprintComplement(
            position=Position(100, 200),
            orientation=90.0,
            layer="B.Cu",
            locked=True,
        )

        unlocked = original.with_locked(False)

        # Original unchanged
        assert original.locked is True

        # New has updated lock state
        assert unlocked.locked is False

        # Other fields preserved
        assert unlocked.position.x == 100
        assert unlocked.orientation == 90.0
        assert unlocked.layer == "B.Cu"

    def test_back_layer_representation(self):
        """B.Cu layer should be stored correctly."""
        fc = FootprintComplement(
            position=Position(0, 0),
            orientation=0.0,
            layer="B.Cu",
        )

        assert fc.layer == "B.Cu"
        assert fc.layer.startswith("B.")
