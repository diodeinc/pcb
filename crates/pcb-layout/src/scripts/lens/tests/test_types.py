"""Tests for lens core types."""

from ..types import (
    EntityPath,
    EntityId,
    Position,
    FootprintView,
    FootprintComplement,
    BoardView,
    BoardComplement,
    Board,
    default_footprint_complement,
    default_group_complement,
)


class TestEntityPath:
    """Tests for EntityPath."""

    def test_from_string_simple(self):
        path = EntityPath.from_string("Power")
        assert path.segments == ("Power",)
        assert str(path) == "Power"

    def test_from_string_nested(self):
        path = EntityPath.from_string("Power.Regulator.C1")
        assert path.segments == ("Power", "Regulator", "C1")
        assert str(path) == "Power.Regulator.C1"

    def test_from_string_empty(self):
        path = EntityPath.from_string("")
        assert path.segments == ()
        assert str(path) == ""
        assert not path  # Empty path is falsy

    def test_parent(self):
        path = EntityPath.from_string("Power.Regulator.C1")
        parent = path.parent()
        assert parent is not None
        assert str(parent) == "Power.Regulator"

        grandparent = parent.parent()
        assert grandparent is not None
        assert str(grandparent) == "Power"

        assert grandparent.parent() is None  # Root has no parent

    def test_child(self):
        path = EntityPath.from_string("Power")
        child = path.child("Regulator")
        assert str(child) == "Power.Regulator"

    def test_is_ancestor_of(self):
        power = EntityPath.from_string("Power")
        reg = EntityPath.from_string("Power.Regulator")
        c1 = EntityPath.from_string("Power.Regulator.C1")
        other = EntityPath.from_string("Digital")

        assert power.is_ancestor_of(reg)
        assert power.is_ancestor_of(c1)
        assert reg.is_ancestor_of(c1)

        assert not reg.is_ancestor_of(power)
        assert not power.is_ancestor_of(other)
        assert not power.is_ancestor_of(power)  # Not ancestor of self

    def test_relative_to(self):
        power = EntityPath.from_string("Power")
        c1 = EntityPath.from_string("Power.Regulator.C1")

        rel = c1.relative_to(power)
        assert rel is not None
        assert str(rel) == "Regulator.C1"

        # Not an ancestor
        other = EntityPath.from_string("Digital")
        assert c1.relative_to(other) is None

    def test_name_property(self):
        path = EntityPath.from_string("Power.Regulator.C1")
        assert path.name == "C1"

        empty = EntityPath.from_string("")
        assert empty.name == ""

    def test_depth(self):
        assert EntityPath.from_string("").depth == 0
        assert EntityPath.from_string("Power").depth == 1
        assert EntityPath.from_string("Power.Regulator.C1").depth == 3

    def test_hashable(self):
        path1 = EntityPath.from_string("Power.C1")
        path2 = EntityPath.from_string("Power.C1")
        path3 = EntityPath.from_string("Power.C2")

        # Same path should hash the same
        assert hash(path1) == hash(path2)

        # Can be used in sets
        paths = {path1, path2, path3}
        assert len(paths) == 2


class TestEntityId:
    """Tests for EntityId."""

    def test_from_string(self):
        entity_id = EntityId.from_string("Power.C1")
        assert str(entity_id) == "Power.C1"
        assert entity_id.uuid  # Should have a UUID

    def test_deterministic_uuid(self):
        """UUID should be deterministic based on path."""
        id1 = EntityId.from_string("Power.C1")
        id2 = EntityId.from_string("Power.C1")
        id3 = EntityId.from_string("Power.C2")

        assert id1.uuid == id2.uuid
        assert id1.uuid != id3.uuid

    def test_equality(self):
        id1 = EntityId.from_string("Power.C1")
        id2 = EntityId.from_string("Power.C1")
        id3 = EntityId.from_string("Power.C2")

        assert id1 == id2
        assert id1 != id3

    def test_hashable(self):
        id1 = EntityId.from_string("Power.C1")
        id2 = EntityId.from_string("Power.C1")

        ids = {id1, id2}
        assert len(ids) == 1


class TestPosition:
    """Tests for Position."""

    def test_creation(self):
        pos = Position(x=1000, y=2000)
        assert pos.x == 1000
        assert pos.y == 2000

    def test_offset_by(self):
        pos = Position(x=1000, y=2000)
        new_pos = pos.offset_by(100, -50)
        assert new_pos.x == 1100
        assert new_pos.y == 1950
        # Original unchanged (immutable)
        assert pos.x == 1000

    def test_add(self):
        p1 = Position(x=100, y=200)
        p2 = Position(x=50, y=30)
        result = p1 + p2
        assert result.x == 150
        assert result.y == 230

    def test_sub(self):
        p1 = Position(x=100, y=200)
        p2 = Position(x=50, y=30)
        result = p1 - p2
        assert result.x == 50
        assert result.y == 170


class TestFootprintView:
    """Tests for FootprintView."""

    def test_creation(self):
        entity_id = EntityId.from_string("Power.C1")
        view = FootprintView(
            entity_id=entity_id,
            reference="C1",
            value="10uF",
            fpid="Capacitor_SMD:C_0603",
            dnp=False,
            fields={"MPN": "GRM188R71C104KA01"},
        )

        assert view.reference == "C1"
        assert view.value == "10uF"
        assert view.fpid == "Capacitor_SMD:C_0603"
        assert not view.dnp
        assert view.fields["MPN"] == "GRM188R71C104KA01"

    def test_path_property(self):
        entity_id = EntityId.from_string("Power.C1")
        view = FootprintView(
            entity_id=entity_id,
            reference="C1",
            value="10uF",
            fpid="Capacitor_SMD:C_0603",
        )

        assert str(view.path) == "Power.C1"


class TestFootprintComplement:
    """Tests for FootprintComplement."""

    def test_creation(self):
        comp = FootprintComplement(
            position=Position(x=1000, y=2000),
            orientation=45.0,
            layer="F.Cu",
            locked=True,
        )

        assert comp.position.x == 1000
        assert comp.orientation == 45.0
        assert comp.layer == "F.Cu"
        assert comp.locked

    def test_with_position(self):
        comp = FootprintComplement(
            position=Position(x=1000, y=2000),
            orientation=45.0,
            layer="F.Cu",
        )

        new_comp = comp.with_position(Position(x=5000, y=6000))

        assert new_comp.position.x == 5000
        assert new_comp.orientation == 45.0  # Preserved
        assert comp.position.x == 1000  # Original unchanged


class TestDefaults:
    """Tests for default factories."""

    def test_default_footprint_complement(self):
        comp = default_footprint_complement()
        assert comp.position.x == 0
        assert comp.position.y == 0
        assert comp.orientation == 0.0
        assert comp.layer == "F.Cu"
        assert not comp.locked

    def test_default_group_complement(self):
        comp = default_group_complement()
        assert comp.tracks == ()
        assert comp.vias == ()
        assert comp.zones == ()
        assert comp.graphics == ()
        assert comp.is_empty


class TestBoardView:
    """Tests for BoardView."""

    def test_creation(self):
        view = BoardView()
        assert len(view.footprints) == 0
        assert len(view.groups) == 0
        assert len(view.nets) == 0

    def test_get_footprint_by_path(self):
        entity_id = EntityId.from_string("Power.C1")
        fp_view = FootprintView(
            entity_id=entity_id,
            reference="C1",
            value="10uF",
            fpid="Capacitor_SMD:C_0603",
        )

        view = BoardView(footprints={entity_id: fp_view})

        path = EntityPath.from_string("Power.C1")
        found = view.get_footprint_by_path(path)
        assert found is not None
        assert found.reference == "C1"

        not_found = view.get_footprint_by_path(EntityPath.from_string("Power.C2"))
        assert not_found is None


class TestBoard:
    """Tests for Board (combined View + Complement)."""

    def test_get_footprint(self):
        entity_id = EntityId.from_string("Power.C1")

        fp_view = FootprintView(
            entity_id=entity_id,
            reference="C1",
            value="10uF",
            fpid="Capacitor_SMD:C_0603",
        )

        fp_comp = FootprintComplement(
            position=Position(x=1000, y=2000),
            orientation=45.0,
            layer="F.Cu",
        )

        view = BoardView(footprints={entity_id: fp_view})
        complement = BoardComplement(footprints={entity_id: fp_comp})

        board = Board(view=view, complement=complement)

        fp = board.get_footprint(entity_id)
        assert fp is not None
        assert fp.view.reference == "C1"
        assert fp.complement.position.x == 1000

    def test_get_footprint_uses_default_complement(self):
        entity_id = EntityId.from_string("Power.C1")

        fp_view = FootprintView(
            entity_id=entity_id,
            reference="C1",
            value="10uF",
            fpid="Capacitor_SMD:C_0603",
        )

        view = BoardView(footprints={entity_id: fp_view})
        complement = BoardComplement()  # No complement for this footprint

        board = Board(view=view, complement=complement)

        fp = board.get_footprint(entity_id)
        assert fp is not None
        assert fp.view.reference == "C1"
        # Should use default complement
        assert fp.complement.position.x == 0
        assert fp.complement.position.y == 0
