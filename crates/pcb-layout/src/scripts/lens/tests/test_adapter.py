"""
Tests for kicad_adapter functions and helpers.

Most tests run without KiCad. Native replacement regressions run when pcbnew
is importable (on Debian, set PYTHONPATH=/usr/lib/python3/dist-packages).
"""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import Mock

import pytest

from .. import kicad_adapter
from ..lens import (
    FragmentData,
    build_fragment_net_remap,
)
from ..types import (
    BoardView,
    EntityId,
    EntityPath,
    FootprintComplement,
    GroupComplement,
    GroupView,
    Position,
    TrackComplement,
    ViaComplement,
    ZoneComplement,
    default_footprint_complement,
)


class MockPad:
    def __init__(self, pad_name: str):
        self._pad_name = pad_name
        self.net = None
        self.pin_type = None

    def GetPadName(self) -> str:
        return self._pad_name

    def SetNet(self, net) -> None:
        self.net = net

    def SetPinType(self, pin_type: str) -> None:
        self.pin_type = pin_type


class MockFootprint:
    def __init__(self, pads):
        self._pads = list(pads)

    def Pads(self):
        return list(self._pads)


class TestApplyPadAssignment:
    def test_applies_to_all_duplicate_named_pads(self):
        net_info = object()
        pads = [MockPad("1"), MockPad("1"), MockPad("2"), MockPad("1")]
        fp = MockFootprint(pads)

        applied = kicad_adapter._apply_pad_assignment(fp, "1", net_info, "no_connect")

        assert applied == 3
        assert pads[0].net is net_info
        assert pads[1].net is net_info
        assert pads[3].net is net_info
        assert pads[0].pin_type == "no_connect"
        assert pads[1].pin_type == "no_connect"
        assert pads[3].pin_type == "no_connect"
        assert pads[2].net is None
        assert pads[2].pin_type is None

    def test_can_assign_net_without_touching_pin_type(self):
        net_info = object()
        pads = [MockPad("1"), MockPad("1")]
        fp = MockFootprint(pads)

        applied = kicad_adapter._apply_pad_assignment(fp, "1", net_info)

        assert applied == 2
        assert pads[0].net is net_info
        assert pads[1].net is net_info
        assert pads[0].pin_type is None
        assert pads[1].pin_type is None


class _MockGroup:
    def __init__(self, name: str):
        self._name = name

    def GetName(self) -> str:
        return self._name


class _MockGroupedItem:
    def __init__(self, group: _MockGroup | None):
        self._group = group

    def GetParentGroup(self) -> _MockGroup | None:
        return self._group


class TestFragmentTargetGroup:
    def test_restores_relative_nested_group(self):
        root = _MockGroup("PORT0")
        nested = _MockGroup("PORT0.U_RCB")

        result = kicad_adapter._fragment_target_group(
            _MockGroupedItem(_MockGroup("U_RCB")),
            "PORT0",
            root,
            {"PORT0": root, "PORT0.U_RCB": nested},
        )

        assert result is nested

    def test_keeps_ungrouped_items_in_root_group(self):
        root = _MockGroup("PORT0")

        result = kicad_adapter._fragment_target_group(
            _MockGroupedItem(None), "PORT0", root, {"PORT0": root}
        )

        assert result is root

    def test_missing_nested_group_falls_back_to_root(self):
        root = _MockGroup("PORT0")

        result = kicad_adapter._fragment_target_group(
            _MockGroupedItem(_MockGroup("MANUAL")),
            "PORT0",
            root,
            {"PORT0": root},
        )

        assert result is root

    def test_fragment_copy_adds_track_to_its_nested_group(self, monkeypatch, tmp_path):
        layout_file = tmp_path / "layout.kicad_pcb"
        layout_file.touch()
        monkeypatch.setattr(
            kicad_adapter, "_discover_kicad_pcb_file", lambda _layout_dir: layout_file
        )

        source_group = Mock()
        source_group.GetName.return_value = "U_RCB"
        source_track = Mock()
        source_track.GetParentGroup.return_value = source_group
        source_track.GetClass.return_value = "PCB_TRACK"
        source_track.GetNet.return_value = None

        copied_track = Mock()
        copied_track.GetStart.return_value = SimpleNamespace(x=1, y=2)
        copied_track.GetEnd.return_value = SimpleNamespace(x=3, y=4)
        copied_track.GetWidth.return_value = 5
        copied_track.GetLayer.return_value = 0

        fragment_board = Mock()
        fragment_board.GetTracks.return_value = [source_track]
        fragment_board.Zones.return_value = []
        fragment_board.GetDrawings.return_value = []
        fragment_board.GetLayerName.return_value = "F.Cu"

        pcbnew = Mock()
        pcbnew.LoadBoard.return_value = fragment_board
        pcbnew.BOARD_ITEM.Duplicate.return_value = copied_track
        pcbnew.FOOTPRINT = type("MockFootprint", (), {})

        root = Mock()
        nested = Mock()
        target_board = Mock()
        entity_id = EntityId.from_string("PORT0")

        kicad_adapter._apply_fragment_routing(
            root,
            {"PORT0": root, "PORT0.U_RCB": nested},
            GroupView(entity_id=entity_id, member_ids=(), layout_path="package://test"),
            entity_id,
            FragmentData(footprint_complements={}),
            BoardView(),
            target_board,
            pcbnew,
            kicad_adapter.OpLog(),
            {"test": str(tmp_path)},
        )

        target_board.Add.assert_called_once_with(copied_track)
        nested.AddItem.assert_called_once_with(copied_track)
        root.AddItem.assert_not_called()


class TestBuildFragmentNetRemap:
    """Tests for the pure build_fragment_net_remap function."""

    def test_simple_single_pad_mapping(self):
        """Single pad maps fragment net to board net."""
        group_path = EntityPath.from_string("Power")
        member_paths = [EntityPath.from_string("Power.R1")]

        # Fragment: R1.1 is connected to "LOCAL_VCC"
        fragment_pad_net_map: dict[tuple[str, str], str] = {
            ("R1", "1"): "LOCAL_VCC",
        }

        # Board: Power.R1.1 is connected to "VCC_3V3"
        board_pad_net_map: dict[tuple[EntityId, str], str] = {
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

        fragment_pad_net_map: dict[tuple[str, str], str] = {
            ("R1", "1"): "LOCAL_GND",
            ("R2", "2"): "LOCAL_GND",
        }

        board_pad_net_map: dict[tuple[EntityId, str], str] = {
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

        fragment_pad_net_map: dict[tuple[str, str], str] = {
            ("R1", "1"): "LOCAL_VCC",
            ("R1", "2"): "LOCAL_GND",
        }

        board_pad_net_map: dict[tuple[EntityId, str], str] = {
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
        fragment_pad_net_map: dict[tuple[str, str], str] = {
            ("R1", "1"): "LOCAL_VCC",
            ("R2", "1"): "LOCAL_VCC",
        }

        board_pad_net_map: dict[tuple[EntityId, str], str] = {
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

        fragment_pad_net_map: dict[tuple[str, str], str] = {
            # R1.1 is NOT in fragment map
        }

        board_pad_net_map: dict[tuple[EntityId, str], str] = {
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
        fragment_pad_net_map: dict[tuple[str, str], str] = {
            ("R1", "1"): "LOCAL_VCC",
        }

        board_pad_net_map: dict[tuple[EntityId, str], str] = {
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
            footprint_complements={"R1": default_footprint_complement()},
            pad_net_map={("R1", "1"): "VCC"},
        )

        assert "R1" in cache.footprint_complements
        assert ("R1", "1") in cache.pad_net_map

    def test_default_pad_net_map(self):
        """pad_net_map should default to empty dict."""
        cache = FragmentData(
            footprint_complements={},
        )

        assert cache.pad_net_map == {}


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

    def test_back_layer_representation(self):
        """B.Cu layer should be stored correctly."""
        fc = FootprintComplement(
            position=Position(0, 0),
            orientation=0.0,
            layer="B.Cu",
        )

        assert fc.layer == "B.Cu"
        assert fc.layer.startswith("B.")


class TestFieldVisibility:
    """Tests for field visibility behavior during footprint creation and update."""

    def test_create_footprint_hides_value_and_custom_fields(self):
        """New footprints should have Value and custom fields hidden."""
        from unittest.mock import Mock

        from ..kicad_adapter import _create_footprint
        from ..types import EntityId, FootprintComplement, FootprintView, Position

        # Create view with custom fields
        entity_id = EntityId.from_string("Power.R1", fpid="Resistor_SMD:R_0603")
        view = FootprintView(
            entity_id=entity_id,
            reference="R1",
            value="10k",
            fpid="Resistor_SMD:R_0603",
            fields={"Path": "Power.R1", "Datasheet": "http://example.com"},
        )
        complement = FootprintComplement(
            position=Position(x=1000, y=2000),
            orientation=0.0,
            layer="F.Cu",
        )

        # Mock pcbnew and footprint
        mock_fp = Mock()
        mock_board = Mock()
        mock_pcbnew = Mock()

        # Track which fields had SetVisible called
        visibility_calls = {}
        # Track which fields exist (simulates KiCad behavior where custom fields don't exist until SetField)
        existing_fields = {"Reference", "Value", "Footprint"}  # Standard KiCad fields

        def make_field_mock(name):
            field = Mock()
            field.SetVisible = lambda v: visibility_calls.update({name: v})
            return field

        def get_field_by_name(name):
            if name in existing_fields:
                return make_field_mock(name)
            return None

        def set_field(name, value):
            existing_fields.add(name)

        mock_fp.GetFieldByName = get_field_by_name
        mock_fp.SetField = set_field
        mock_pcbnew.FootprintLoad.return_value = mock_fp
        mock_pcbnew.F_Cu = 0
        mock_pcbnew.B_Cu = 31
        mock_pcbnew.KIID_PATH = Mock(return_value=Mock())

        footprint_lib_map = {"Resistor_SMD": "/path/to/lib"}

        _create_footprint(
            view, complement, mock_board, mock_pcbnew, footprint_lib_map, {}, None
        )

        # Custom fields should be hidden
        assert visibility_calls.get("Path") is False
        assert visibility_calls.get("Datasheet") is False
        # Value field should be hidden
        assert visibility_calls.get("Value") is False

    def test_update_footprint_preserves_field_visibility(self):
        """Updating footprints should not change field visibility."""
        from unittest.mock import Mock

        from ..kicad_adapter import _update_footprint_view
        from ..types import EntityId, FootprintView

        entity_id = EntityId.from_string("Power.R1", fpid="Resistor_SMD:R_0603")
        view = FootprintView(
            entity_id=entity_id,
            reference="R1",
            value="10k",
            fpid="Resistor_SMD:R_0603",
            fields={"Path": "Power.R1", "Datasheet": "http://example.com"},
        )

        mock_fp = Mock()
        mock_pcbnew = Mock()

        # Track SetVisible calls
        set_visible_calls = []
        mock_field = Mock()
        mock_field.SetVisible = lambda v: set_visible_calls.append(v)
        mock_fp.GetFieldByName.return_value = mock_field

        _update_footprint_view(mock_fp, view, mock_pcbnew, {}, None)

        # No SetVisible calls should be made during update
        assert len(set_visible_calls) == 0


class FakeUuid:
    def __init__(self, value: str):
        self.value = value

    def Clone(self, other: FakeUuid) -> None:
        self.value = other.value


def fake_pad(uuid: str, number: str, x: int, y: int) -> SimpleNamespace:
    return SimpleNamespace(
        m_Uuid=FakeUuid(uuid),
        GetNumber=lambda: number,
        GetPosition=lambda: SimpleNamespace(x=x, y=y),
    )


def fake_footprint(uuid: str, pads: list[SimpleNamespace]) -> SimpleNamespace:
    return SimpleNamespace(m_Uuid=FakeUuid(uuid), Pads=lambda: pads)


def test_replacement_inherits_footprint_and_pad_uuids():
    """Same-numbered pads pair up closest first; added pads keep fresh UUIDs."""
    old = fake_footprint(
        "old-fp",
        [
            fake_pad("old-1", "1", 0, 0),
            fake_pad("old-5a", "5", 100, 0),
            fake_pad("old-5b", "5", 200, 0),
            fake_pad("old-removed", "9", 300, 0),
        ],
    )
    # 5a moved next to 5b and is listed first; 5b stayed put and must keep its UUID.
    new = fake_footprint(
        "new-fp",
        [
            fake_pad("new-5a", "5", 180, 0),
            fake_pad("new-5b", "5", 200, 0),
            fake_pad("new-1", "1", 50, 50),
            fake_pad("new-added", "7", 400, 0),
        ],
    )

    kicad_adapter._inherit_uuids(old, new)

    assert new.m_Uuid.value == "old-fp"
    assert [pad.m_Uuid.value for pad in new.Pads()] == [
        "old-5a",
        "old-5b",
        "old-1",
        "new-added",
    ]


@pytest.mark.parametrize(
    "sync_footprints", [True, False], ids=["forced", "fpid-change"]
)
@pytest.mark.parametrize("back", [False, True], ids=["front", "back"])
def test_replacement_preserves_fields_through_save_reload(
    tmp_path, sync_footprints, back
):
    """Exercise real transforms, field ownership and serialization, not SWIG mocks."""
    pcbnew = pytest.importorskip("pcbnew")
    from ..changeset import build_sync_changeset
    from ..lens import extract
    from ..types import BoardComplement, FootprintView

    library = tmp_path / "Test.pretty"
    library.mkdir()
    template = pcbnew.FOOTPRINT(None)
    template.SetField("LibraryOnly", "library text")
    template.SetField("Rating", "library rating")
    pad = pcbnew.PAD(template)
    pad.SetNumber("1")
    pad.SetAttribute(pcbnew.PAD_ATTRIB_SMD)
    pad.SetShape(pcbnew.PAD_SHAPE_RECT)
    pad.SetLayerSet(pcbnew.PAD.SMDMask())
    pad.SetPosition(pcbnew.VECTOR2I(700_000, -300_000))
    pad.SetSize(pcbnew.VECTOR2I(1_200_000, 800_000))
    template.Add(pad)
    model = pcbnew.FP_3DMODEL()
    model.m_Filename = "library.step"
    template.Models().push_back(model)
    for name in ("Original", "Replacement"):
        template.SetFPIDAsString(f"Test:{name}")
        pcbnew.PCB_IO_KICAD_SEXPR().FootprintSave(str(library), template)

    board = pcbnew.BOARD()
    old_id = EntityId.from_string("R", fpid="Test:Original")
    old = kicad_adapter._create_footprint(
        FootprintView(old_id, "R1", "old value", old_id.fpid, fields={"Path": "R"}),
        FootprintComplement(
            position=Position(31_000_000, 47_000_000),
            orientation=37.0,
            layer="B.Cu" if back else "F.Cu",
            locked=True,
        ),
        board,
        pcbnew,
        {"Test": str(library)},
        {},
        None,
    )
    board.Add(old)
    old.Remove(kicad_adapter.get_footprint_field(old, "LibraryOnly"))
    for name, text in {
        "Manufacturer": "board manufacturer",
        "Mpn": "board mpn",
        "Rating": "board rating",
        "Datasheet": "board.pdf",
    }.items():
        old.SetField(name, text)

    # Deliberately asymmetric, board-authored presentation on both built-ins
    # and custom fields, unlike the library defaults after rotation/flip.
    for i, name in enumerate(
        ("Reference", "Value", "Manufacturer", "Mpn", "Rating", "Datasheet")
    ):
        field = kicad_adapter.get_footprint_field(old, name)
        field.SetVisible(name != "Reference")
        field.SetLayer(pcbnew.B_SilkS if back else pcbnew.F_Fab)
        field.SetKeepUpright(False)
        field.SetPosition(
            pcbnew.VECTOR2I(29_000_000 + i * 300_000, 44_000_000 - i * 700_000)
        )
        field.SetTextAngle(pcbnew.EDA_ANGLE(13 + i * 19, pcbnew.DEGREES_T))
        field.SetTextSize(pcbnew.VECTOR2I(900_000 + i * 50_000, 1_300_000))
        field.SetTextThickness(170_000)
        field.SetBold(True)
        field.SetItalic(i % 2 == 0)
        field.SetMirrored(back)
        field.SetIsKnockout(i % 2 == 1)
        field.SetHorizJustify(pcbnew.GR_TEXT_H_ALIGN_LEFT)
        field.SetVertJustify(pcbnew.GR_TEXT_V_ALIGN_TOP)

    # Board geometry/models must NOT survive a library refresh.
    next(iter(old.Pads())).SetSize(pcbnew.VECTOR2I(3_000_000, 2_000_000))
    old.Models()[0].m_Filename = "board.step"
    board_file = tmp_path / "test.kicad_pcb"

    def reload_board():
        pcbnew.SaveBoard(str(board_file), board)
        return pcbnew.LoadBoard(str(board_file))

    def presentation(field):
        return (
            field.m_Uuid.AsString(),
            tuple(field.GetPosition()),
            field.GetTextAngle().AsDegrees(),
            field.GetLayer(),
            field.IsVisible(),
            tuple(field.GetTextSize()),
            field.GetTextThickness(),
            field.IsBold(),
            field.IsItalic(),
            field.IsMirrored(),
            field.IsKeepUpright(),
            field.IsKnockout(),
            field.GetHorizJustify(),
            field.GetVertJustify(),
        )

    board = reload_board()
    old = next(iter(board.GetFootprints()))
    expected_fields = {f.GetName(): presentation(f) for f in old.GetFields()}
    fp_uuid = old.m_Uuid.AsString()
    pad_uuid = next(iter(old.Pads())).m_Uuid.AsString()
    new_id = EntityId.from_string(
        "R", fpid="Test:Original" if sync_footprints else "Test:Replacement"
    )
    view = BoardView(
        footprints={
            new_id: FootprintView(
                new_id,
                "R9",
                "22k",
                new_id.fpid,
                dnp=True,
                exclude_from_bom=True,
                exclude_from_pos=True,
                fields={
                    "Path": "R",
                    "Rating": "source rating",
                    "SourceOnly": "new source field",
                    "Datasheet": "package://test/datasheet.pdf",
                },
            )
        }
    )

    def check_fields():
        fp = next(iter(board.GetFootprints()))
        fields = {f.GetName(): f for f in fp.GetFields()}
        assert len(fields) == len(list(fp.GetFields()))  # No duplicate named fields.
        assert {
            name: presentation(fields[name]) for name in expected_fields
        } == expected_fields
        assert {
            name: fields[name].GetText()
            for name in (
                "Reference",
                "Value",
                "Manufacturer",
                "Mpn",
                "Rating",
                "Datasheet",
                "LibraryOnly",
            )
        } == {
            "Reference": "R9",
            "Value": "22k",
            "Manufacturer": "board manufacturer",
            "Mpn": "board mpn",
            "Rating": "source rating",
            "Datasheet": "datasheet.pdf",
            "LibraryOnly": "library text",
        }
        assert fields["SourceOnly"].GetText() == "new source field"
        assert not fields["SourceOnly"].IsVisible()
        assert fp.m_Uuid.AsString() == fp_uuid
        assert fp.GetFPIDAsString() == new_id.fpid
        assert tuple(fp.GetPosition()) == (31_000_000, 47_000_000)
        assert fp.GetOrientation().AsDegrees() == 37.0
        assert fp.IsFlipped() == back
        assert fp.IsLocked()
        assert fp.IsDNP() and fp.IsExcludedFromBOM() and fp.IsExcludedFromPosFiles()
        pad = next(iter(fp.Pads()))
        assert pad.m_Uuid.AsString() == pad_uuid
        assert tuple(pad.GetSize()) == (1_200_000, 800_000)
        assert [m.m_Filename for m in fp.Models()] == ["library.step"]
        assert all(f.GetParent() == fp for f in fields.values())

    for iteration in range(3):
        _, complement = extract(board, pcbnew)
        old_comp = next(iter(complement.footprints.values()))
        changeset = build_sync_changeset(
            view,
            BoardComplement(footprints={new_id: old_comp}),
            complement,
            sync_footprints=sync_footprints or iteration > 0,
        )
        assert changeset.added_footprints == {new_id}
        kicad_adapter.apply_changeset(
            changeset,
            board,
            pcbnew,
            {"Test": str(library)},
            {"test": str(tmp_path)},
            board_file,
        )
        check_fields()
        board = reload_board()
        check_fields()
        # Fields introduced by this refresh must retain UUIDs/presentation too.
        fp = next(iter(board.GetFootprints()))
        expected_fields = {f.GetName(): presentation(f) for f in fp.GetFields()}
