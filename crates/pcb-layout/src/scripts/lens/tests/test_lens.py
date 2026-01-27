"""Tests for lens operations."""

from ..types import (
    EntityId,
    Position,
    FootprintView,
    FootprintComplement,
    GroupView,
    GroupComplement,
    BoardView,
    BoardComplement,
    Board,
)
from ..lens import (
    adapt_complement,
    join,
)
from ..changeset import build_sync_changeset


class TestAdaptComplement:
    """Tests for adapt_complement."""

    def test_preserve_existing_footprint(self):
        """Existing footprints should preserve their complement."""
        entity_id = EntityId.from_string("Power.C1")

        new_view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="C1",
                    value="10uF",
                    fpid="Capacitor_SMD:C_0603",
                )
            }
        )

        old_complement = BoardComplement(
            footprints={
                entity_id: FootprintComplement(
                    position=Position(x=5000, y=6000),
                    orientation=90.0,
                    layer="B.Cu",
                    locked=True,
                )
            }
        )

        new_complement = adapt_complement(new_view, old_complement)

        assert entity_id in new_complement.footprints
        assert new_complement.footprints[entity_id].position.x == 5000
        assert new_complement.footprints[entity_id].orientation == 90.0
        assert new_complement.footprints[entity_id].layer == "B.Cu"
        assert new_complement.footprints[entity_id].locked

        # Should not be tracked as added
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert entity_id not in changeset.added_footprints

    def test_new_footprint_gets_default_complement(self):
        """New footprints should get default complement."""
        entity_id = EntityId.from_string("Power.C1")

        new_view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="C1",
                    value="10uF",
                    fpid="Capacitor_SMD:C_0603",
                )
            }
        )

        old_complement = BoardComplement()  # Empty - no existing footprints

        new_complement = adapt_complement(new_view, old_complement)

        assert entity_id in new_complement.footprints
        # Should have default values
        assert new_complement.footprints[entity_id].position.x == 0
        assert new_complement.footprints[entity_id].position.y == 0
        assert new_complement.footprints[entity_id].layer == "F.Cu"

        # Should be tracked as added
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert entity_id in changeset.added_footprints

    def test_removed_footprint_tracked(self):
        """Removed footprints should be tracked but not in result."""
        old_id = EntityId.from_string("Power.C1")

        new_view = BoardView()  # Empty - footprint removed

        old_complement = BoardComplement(
            footprints={
                old_id: FootprintComplement(
                    position=Position(x=5000, y=6000),
                    orientation=90.0,
                    layer="F.Cu",
                )
            }
        )

        new_complement = adapt_complement(new_view, old_complement)

        # Should not be in result
        assert old_id not in new_complement.footprints

        # Should be tracked as removed
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert old_id in changeset.removed_footprints

    def test_fpid_change_is_remove_plus_add(self):
        """FPID change should be tracked as remove + add since EntityId includes fpid."""
        old_fpid = "Capacitor_SMD:C_0402"
        new_fpid = "Capacitor_SMD:C_0603"
        
        # EntityId now includes fpid, so these are different entities
        old_entity_id = EntityId.from_string("Power.C1", fpid=old_fpid)
        new_entity_id = EntityId.from_string("Power.C1", fpid=new_fpid)

        new_view = BoardView(
            footprints={
                new_entity_id: FootprintView(
                    entity_id=new_entity_id,
                    reference="C1",
                    value="10uF",
                    fpid=new_fpid,
                )
            }
        )

        old_complement = BoardComplement(
            footprints={
                old_entity_id: FootprintComplement(
                    position=Position(x=5000, y=6000),
                    orientation=90.0,
                    layer="F.Cu",
                )
            }
        )

        new_complement = adapt_complement(new_view, old_complement)

        # Should be tracked as remove (old) + add (new)
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert old_entity_id in changeset.removed_footprints
        assert new_entity_id in changeset.added_footprints
        
        # New entity gets default complement (HierPlace will handle position inheritance)
        assert new_entity_id in new_complement.footprints

    def test_group_complement_preserved(self):
        """Existing group complement should be preserved."""
        group_id = EntityId.from_string("Power")
        fp_id = EntityId.from_string("Power.C1")

        new_view = BoardView(
            footprints={
                fp_id: FootprintView(
                    entity_id=fp_id,
                    reference="C1",
                    value="10uF",
                    fpid="Capacitor_SMD:C_0603",
                )
            },
            groups={
                group_id: GroupView(
                    entity_id=group_id,
                    member_ids=(fp_id,),
                )
            },
        )

        old_complement = BoardComplement(
            footprints={
                fp_id: FootprintComplement(
                    position=Position(x=1000, y=2000),
                    orientation=0.0,
                    layer="F.Cu",
                )
            },
            groups={
                group_id: GroupComplement(
                    tracks=(),
                )
            },
        )

        new_complement = adapt_complement(new_view, old_complement)

        assert group_id in new_complement.groups
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert group_id not in changeset.added_groups

    def test_new_group_default_complement(self):
        """New groups should get default complement."""
        group_id = EntityId.from_string("Power")
        fp_id = EntityId.from_string("Power.C1")

        new_view = BoardView(
            footprints={
                fp_id: FootprintView(
                    entity_id=fp_id,
                    reference="C1",
                    value="10uF",
                    fpid="Capacitor_SMD:C_0603",
                )
            },
            groups={
                group_id: GroupView(
                    entity_id=group_id,
                    member_ids=(fp_id,),
                )
            },
        )

        old_complement = BoardComplement()

        new_complement = adapt_complement(new_view, old_complement)

        assert group_id in new_complement.groups
        assert new_complement.groups[group_id].is_empty
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert group_id in changeset.added_groups


class TestJoin:
    """Tests for join operation."""

    def test_combines_view_and_complement(self):
        entity_id = EntityId.from_string("Power.C1")

        view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="C1",
                    value="10uF",
                    fpid="Capacitor_SMD:C_0603",
                )
            }
        )

        complement = BoardComplement(
            footprints={
                entity_id: FootprintComplement(
                    position=Position(x=5000, y=6000),
                    orientation=45.0,
                    layer="F.Cu",
                )
            }
        )

        result = join(view, complement)

        assert isinstance(result, Board)
        assert result.view == view
        assert result.complement == complement

        # Can get combined footprint
        fp = result.get_footprint(entity_id)
        assert fp is not None
        assert fp.view.reference == "C1"
        assert fp.complement.position.x == 5000


class TestIdempotence:
    """Tests for sync idempotence property."""

    def test_adapt_complement_is_idempotent(self):
        """
        Adapting a complement twice with the same view should produce
        the same result.
        """
        entity_id = EntityId.from_string("Power.C1")

        view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="C1",
                    value="10uF",
                    fpid="Capacitor_SMD:C_0603",
                )
            }
        )

        original_complement = BoardComplement(
            footprints={
                entity_id: FootprintComplement(
                    position=Position(x=5000, y=6000),
                    orientation=45.0,
                    layer="F.Cu",
                )
            }
        )

        # First adaptation
        complement1 = adapt_complement(view, original_complement)

        # Second adaptation (using result of first)
        complement2 = adapt_complement(view, complement1)

        # Results should be equal
        assert complement1.footprints == complement2.footprints

        # Second run should detect no new additions
        changeset2 = build_sync_changeset(view, complement2, complement1)
        assert len(changeset2.added_footprints) == 0
