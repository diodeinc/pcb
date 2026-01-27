"""
Test scenarios from the sync proposal design document (Section 12).

These tests verify the lens-based sync implementation against the
documented test scenarios and edge cases.

Note: Renames (moved() paths) are now handled in Rust preprocessing before
the Python sync runs. Rename-related tests have been moved to Rust integration tests.
"""

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
from ..lens import (
    adapt_complement,
    join,
)
from ..changeset import build_sync_changeset


def make_footprint_view(
    path: str, reference: str = "", value: str = "", fpid: str = ""
) -> FootprintView:
    """Helper to create a FootprintView with sensible defaults."""
    entity_id = EntityId.from_string(path)
    return FootprintView(
        entity_id=entity_id,
        reference=reference or path.split(".")[-1],
        value=value or "1k",
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


class TestFootprintScenarios:
    """
    FP-01 through FP-05: Footprint test scenarios from Section 11.1.
    """

    def test_fp01_new_footprint_added(self):
        """
        FP-01: New footprint added

        BEFORE:
          SOURCE: {A, B}
          DEST:   {A, B} at positions (10, 10), (20, 20)

        CHANGE:
          SOURCE: {A, B, C}

        EXPECTED:
          DEST: {A, B, C}
          - A at (10, 10) [preserved]
          - B at (20, 20) [preserved]
          - C at (0, 0) [default placement]
        """
        a_id = EntityId.from_string("A")
        b_id = EntityId.from_string("B")
        c_id = EntityId.from_string("C")

        # New view with C added
        new_view = BoardView(
            footprints={
                a_id: make_footprint_view("A"),
                b_id: make_footprint_view("B"),
                c_id: make_footprint_view("C"),
            }
        )

        # Old complement (A and B exist)
        old_complement = BoardComplement(
            footprints={
                a_id: make_footprint_complement(x=10, y=10),
                b_id: make_footprint_complement(x=20, y=20),
            }
        )

        new_complement = adapt_complement(new_view, old_complement)

        # A preserved
        assert a_id in new_complement.footprints
        assert new_complement.footprints[a_id].position.x == 10

        # B preserved
        assert b_id in new_complement.footprints
        assert new_complement.footprints[b_id].position.x == 20

        # C added with default
        assert c_id in new_complement.footprints
        assert new_complement.footprints[c_id].position.x == 0
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert c_id in changeset.added_footprints

    def test_fp02_footprint_removed(self):
        """
        FP-02: Footprint removed

        BEFORE:
          SOURCE: {A, B, C}
          DEST:   {A, B, C}

        CHANGE:
          SOURCE: {A, B}

        EXPECTED:
          DEST: {A, B}
          - C deleted
        """
        a_id = EntityId.from_string("A")
        b_id = EntityId.from_string("B")
        c_id = EntityId.from_string("C")

        # New view without C
        new_view = BoardView(
            footprints={
                a_id: make_footprint_view("A"),
                b_id: make_footprint_view("B"),
            }
        )

        # Old complement with C
        old_complement = BoardComplement(
            footprints={
                a_id: make_footprint_complement(x=10, y=10),
                b_id: make_footprint_complement(x=20, y=20),
                c_id: make_footprint_complement(x=30, y=30),
            }
        )

        new_complement = adapt_complement(new_view, old_complement)

        # A and B preserved
        assert a_id in new_complement.footprints
        assert b_id in new_complement.footprints

        # C removed
        assert c_id not in new_complement.footprints
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert c_id in changeset.removed_footprints

    def test_fp03_footprint_metadata_changed(self):
        """
        FP-03: Footprint metadata changed

        BEFORE:
          SOURCE: A with reference="R1", value="10k"
          DEST:   A at (50, 50), reference="R1", value="10k"

        CHANGE:
          SOURCE: A with reference="R1", value="4.7k"

        EXPECTED:
          DEST: A at (50, 50), reference="R1", value="4.7k"
          - Position preserved
          - Value updated (in View)
        """
        a_id = EntityId.from_string("A")

        # New view with updated value
        new_view = BoardView(
            footprints={
                a_id: FootprintView(
                    entity_id=a_id,
                    reference="R1",
                    value="4.7k",  # Changed
                    fpid="Resistor_SMD:R_0603",
                ),
            }
        )

        old_complement = BoardComplement(
            footprints={
                a_id: make_footprint_complement(x=50, y=50),
            }
        )

        new_complement = adapt_complement(new_view, old_complement)

        # Position preserved
        assert new_complement.footprints[a_id].position.x == 50
        assert new_complement.footprints[a_id].position.y == 50

        # No structural changes
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert a_id not in changeset.added_footprints
        assert a_id not in changeset.removed_footprints

        # Value change is in the View, not Complement
        board = join(new_view, new_complement)
        fp = board.get_footprint(a_id)
        assert fp.view.value == "4.7k"

    def test_fp04_fpid_changed(self):
        """
        FP-04: FPID changed (footprint type)

        With EntityId including fpid, this becomes a remove + add operation.
        Position inheritance will be handled by HierPlace.

        BEFORE:
          SOURCE: A with fpid="R_0402"
          DEST:   A at (50, 50) with fpid="R_0402"

        CHANGE:
          SOURCE: A with fpid="R_0603"

        EXPECTED:
          - Old entity (A, R_0402) removed
          - New entity (A, R_0603) added
          - HierPlace will inherit position from old entity (same path)
        """
        old_fpid = "Resistor_SMD:R_0402"
        new_fpid = "Resistor_SMD:R_0603"
        
        # EntityId now includes fpid
        old_id = EntityId.from_string("A", fpid=old_fpid)
        new_id = EntityId.from_string("A", fpid=new_fpid)

        # New view with new FPID
        new_view = BoardView(
            footprints={
                new_id: FootprintView(
                    entity_id=new_id,
                    reference="R1",
                    value="10k",
                    fpid=new_fpid,
                ),
            }
        )

        old_complement = BoardComplement(
            footprints={
                old_id: make_footprint_complement(x=50, y=50),
            }
        )

        new_complement = adapt_complement(new_view, old_complement)

        # Tracked as remove + add
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert old_id in changeset.removed_footprints
        assert new_id in changeset.added_footprints

        # New entity exists in complement
        assert new_id in new_complement.footprints

        # View has new FPID
        board = join(new_view, new_complement)
        fp = board.get_footprint(new_id)
        assert fp.view.fpid == new_fpid


class TestGroupScenarios:
    """
    GR-01 through GR-05: Group test scenarios from Section 11.3.
    """

    def test_gr01_new_group_created(self):
        """
        GR-01: New group/module created

        EXPECTED:
          - Group created
          - Members added to group
        """
        power_id = EntityId.from_string("Power")
        c1_id = EntityId.from_string("Power.C1")

        new_view = BoardView(
            footprints={
                c1_id: make_footprint_view("Power.C1"),
            },
            groups={
                power_id: GroupView(
                    entity_id=power_id,
                    member_ids=(c1_id,),
                ),
            },
        )

        old_complement = BoardComplement()

        new_complement = adapt_complement(new_view, old_complement)

        assert power_id in new_complement.groups
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert power_id in changeset.added_groups

    def test_gr04_group_removed(self):
        """
        GR-04: Group/module removed

        EXPECTED:
          - Group deleted
          - All members deleted
        """
        power_id = EntityId.from_string("Power")
        c1_id = EntityId.from_string("Power.C1")

        new_view = BoardView()  # Empty

        old_complement = BoardComplement(
            footprints={
                c1_id: make_footprint_complement(x=10, y=10),
            },
            groups={
                power_id: GroupComplement(),
            },
        )

        new_complement = adapt_complement(new_view, old_complement)

        assert power_id not in new_complement.groups
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert power_id in changeset.removed_groups


class TestIdempotence:
    """
    CX-05: Idempotence verification

    sync(S, D') -> D''
    D'' == D'
    """

    def test_idempotence_simple(self):
        """Second sync should produce identical result."""
        a_id = EntityId.from_string("A")
        b_id = EntityId.from_string("B")

        view = BoardView(
            footprints={
                a_id: make_footprint_view("A"),
                b_id: make_footprint_view("B"),
            }
        )

        original_complement = BoardComplement(
            footprints={
                a_id: make_footprint_complement(x=10, y=10),
                b_id: make_footprint_complement(x=20, y=20),
            }
        )

        # First sync
        complement1 = adapt_complement(view, original_complement)

        # Second sync (same view, result of first)
        complement2 = adapt_complement(view, complement1)

        # Should be identical
        assert complement1.footprints == complement2.footprints

        # No additions/removals in second run
        changeset2 = build_sync_changeset(view, complement2, complement1)
        assert len(changeset2.added_footprints) == 0
        assert len(changeset2.removed_footprints) == 0

    def test_idempotence_with_new_footprint(self):
        """After adding a footprint, re-sync should be idempotent."""
        a_id = EntityId.from_string("A")

        view = BoardView(
            footprints={
                a_id: make_footprint_view("A"),
            }
        )

        # First sync: empty -> A added
        empty_complement = BoardComplement()

        complement1 = adapt_complement(view, empty_complement)
        changeset1 = build_sync_changeset(view, complement1, empty_complement)
        assert a_id in changeset1.added_footprints

        # Second sync: A exists -> no changes
        complement2 = adapt_complement(view, complement1)

        changeset2 = build_sync_changeset(view, complement2, complement1)
        assert len(changeset2.added_footprints) == 0
        assert len(changeset2.removed_footprints) == 0


class TestComplexScenarios:
    """
    CX-01 through CX-04: Complex scenarios from Section 11.6.
    """

    def test_cx04_sync_with_empty_source(self):
        """
        CX-04: Sync with empty SOURCE

        EXPECTED:
          - All footprints deleted
          - All groups deleted
        """
        a_id = EntityId.from_string("A")
        power_id = EntityId.from_string("Power")

        new_view = BoardView()  # Empty

        old_complement = BoardComplement(
            footprints={
                a_id: make_footprint_complement(x=10, y=10),
            },
            groups={
                power_id: GroupComplement(),
            },
        )

        new_complement = adapt_complement(new_view, old_complement)

        # All removed
        assert len(new_complement.footprints) == 0
        assert len(new_complement.groups) == 0
        changeset = build_sync_changeset(new_view, new_complement, old_complement)
        assert a_id in changeset.removed_footprints
        assert power_id in changeset.removed_groups


class TestEdgeCases:
    """
    EC-01 through EC-07: Edge cases from Section 11.7.
    """

    def test_ec04_very_long_path(self):
        """EC-04: Very long hierarchical path."""
        long_path = ".".join([f"Level{i}" for i in range(20)])
        entity_id = EntityId.from_string(long_path)

        view = BoardView(
            footprints={
                entity_id: make_footprint_view(long_path),
            }
        )

        old_complement = BoardComplement()

        new_complement = adapt_complement(view, old_complement)

        assert entity_id in new_complement.footprints
        changeset = build_sync_changeset(view, new_complement, old_complement)
        assert entity_id in changeset.added_footprints

    def test_ec05_unicode_in_names(self):
        """EC-05: Unicode in names."""
        path = "Résistance_10kΩ"
        entity_id = EntityId.from_string(path)

        view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="R1",
                    value="10kΩ",
                    fpid="Résistance:R_0603",
                ),
            }
        )

        old_complement = BoardComplement()

        new_complement = adapt_complement(view, old_complement)

        assert entity_id in new_complement.footprints

    def test_empty_path(self):
        """Test handling of empty path (should be skipped or handled gracefully)."""
        empty_path = EntityPath.from_string("")
        assert not empty_path  # Empty path is falsy
        assert empty_path.depth == 0
