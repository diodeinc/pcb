"""
Tests for SyncChangeset - the core interface between pure lens computation
and effectful application.

Note: Renames (moved() paths) are now handled in Rust preprocessing.
"""

from ..types import (
    EntityId,
    Position,
    FootprintView,
    FootprintComplement,
    BoardView,
    BoardComplement,
    default_footprint_complement,
)
from ..changeset import (
    SyncChangeset,
    build_sync_changeset,
)


def make_footprint_view(
    path: str, reference: str = "", value: str = "", fpid: str = ""
) -> FootprintView:
    """Helper to create a FootprintView with sensible defaults."""
    actual_fpid = fpid or "Resistor_SMD:R_0603"
    entity_id = EntityId.from_string(path, fpid=actual_fpid)
    return FootprintView(
        entity_id=entity_id,
        reference=reference or path.split(".")[-1],
        value=value or "1k",
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


class TestSyncChangesetBasic:
    """Basic tests for SyncChangeset structure and properties."""

    def test_empty_changeset(self):
        """An empty changeset should report is_empty=True."""
        changeset = SyncChangeset(
            view=BoardView(footprints={}, groups={}, nets={}),
            complement=BoardComplement(footprints={}, groups={}),
        )

        assert changeset.is_empty
        assert len(changeset.footprint_changes) == 0
        assert len(changeset.group_changes) == 0

    def test_changeset_with_added_footprint(self):
        """Changeset with added footprint should report correctly."""
        r1_id = EntityId.from_string("R1")

        changeset = SyncChangeset(
            view=BoardView(footprints={r1_id: make_footprint_view("R1")}),
            complement=BoardComplement(footprints={r1_id: make_footprint_complement()}),
            added_footprints={r1_id},
        )

        assert not changeset.is_empty
        assert len(changeset.footprint_changes) == 1

        change = changeset.footprint_changes[0]
        assert change.kind == "add"
        assert change.entity_id == r1_id

    def test_changeset_with_removed_footprint(self):
        """Changeset with removed footprint should report correctly."""
        r1_id = EntityId.from_string("R1")
        r1_comp = make_footprint_complement(x=1000, y=2000)

        changeset = SyncChangeset(
            view=BoardView(footprints={}),
            complement=BoardComplement(footprints={}),
            removed_footprints={r1_id: r1_comp},
        )

        assert not changeset.is_empty
        assert len(changeset.footprint_changes) == 1

        change = changeset.footprint_changes[0]
        assert change.kind == "remove"
        assert change.entity_id == r1_id


class TestSyncChangesetSerialization:
    """Tests for plaintext serialization."""

    def test_empty_changeset_serialization(self):
        """Empty changeset should serialize to empty string."""
        changeset = SyncChangeset(
            view=BoardView(),
            complement=BoardComplement(),
        )

        output = changeset.to_plaintext()
        assert output == ""

    def test_added_footprint_serialization(self):
        """Added footprint should serialize with FP_ADD command."""
        r1_id = EntityId.from_string("Power.R1")

        changeset = SyncChangeset(
            view=BoardView(
                footprints={
                    r1_id: make_footprint_view(
                        "Power.R1",
                        reference="R1",
                        value="10k",
                        fpid="Resistor_SMD:R_0603",
                    )
                },
            ),
            complement=BoardComplement(footprints={r1_id: make_footprint_complement()}),
            added_footprints={r1_id},
        )

        output = changeset.to_plaintext()
        assert "FP_ADD" in output
        assert "path=Power.R1" in output
        assert "ref=R1" in output
        assert "fpid=Resistor_SMD:R_0603" in output
        assert "value=10k" in output

    def test_removed_footprint_serialization(self):
        """Removed footprint should serialize with FP_REMOVE command."""
        r1_id = EntityId.from_string("Legacy.R_OLD")
        r1_comp = make_footprint_complement(x=1000, y=2000)

        changeset = SyncChangeset(
            view=BoardView(),
            complement=BoardComplement(),
            removed_footprints={r1_id: r1_comp},
        )

        output = changeset.to_plaintext()
        assert "FP_REMOVE path=Legacy.R_OLD" in output
        assert "x=1000" in output
        assert "y=2000" in output

    def test_serialization_is_sorted(self):
        """Changes should be sorted for deterministic output."""
        a_id = EntityId.from_string("A.R1")
        b_id = EntityId.from_string("B.R1")
        c_id = EntityId.from_string("C.R1")

        # Add in reverse order to test sorting
        changeset = SyncChangeset(
            view=BoardView(
                footprints={
                    a_id: make_footprint_view("A.R1"),
                    b_id: make_footprint_view("B.R1"),
                    c_id: make_footprint_view("C.R1"),
                },
            ),
            complement=BoardComplement(
                footprints={
                    a_id: make_footprint_complement(),
                    b_id: make_footprint_complement(),
                    c_id: make_footprint_complement(),
                },
            ),
            added_footprints={c_id, a_id, b_id},
        )

        output = changeset.to_plaintext()
        lines = output.strip().split("\n")

        # Find the FP lines
        fp_lines = [line for line in lines if line.startswith("FP_ADD")]
        assert len(fp_lines) == 3

        # Should be sorted: A, B, C
        assert "path=A.R1" in fp_lines[0]
        assert "path=B.R1" in fp_lines[1]
        assert "path=C.R1" in fp_lines[2]


class TestBuildSyncChangeset:
    """Tests for build_sync_changeset function."""

    def test_build_from_adapt_result_added(self):
        """build_sync_changeset should correctly detect added footprints."""
        r1_id = EntityId.from_string("R1")

        new_view = BoardView(
            footprints={r1_id: make_footprint_view("R1")},
        )

        new_complement = BoardComplement(
            footprints={r1_id: default_footprint_complement()},
        )

        # Empty old_complement means R1 is new
        old_complement = BoardComplement()

        changeset = build_sync_changeset(
            new_view=new_view,
            new_complement=new_complement,
            old_complement=old_complement,
        )

        assert r1_id in changeset.added_footprints
        assert changeset.view == new_view
        assert r1_id in changeset.complement.footprints


class TestSyncChangesetDiagnostics:
    """Tests for to_diagnostics() method."""

    def test_added_footprint_diagnostic(self):
        """Added footprint should produce info-level diagnostic."""
        r1_id = EntityId.from_string("Power.R1")

        changeset = SyncChangeset(
            view=BoardView(
                footprints={r1_id: make_footprint_view("Power.R1")},
            ),
            complement=BoardComplement(
                footprints={r1_id: make_footprint_complement()},
            ),
            added_footprints={r1_id},
        )

        diagnostics = changeset.to_diagnostics()
        assert len(diagnostics) == 1
        assert diagnostics[0]["kind"] == "layout.sync.missing_footprint"
        assert diagnostics[0]["severity"] == "info"
        assert "Power.R1" in diagnostics[0]["body"]

    def test_removed_footprint_diagnostic(self):
        """Removed footprint should produce warning-level diagnostic."""
        r1_id = EntityId.from_string("Legacy.R1")
        r1_comp = make_footprint_complement()

        changeset = SyncChangeset(
            view=BoardView(),
            complement=BoardComplement(),
            removed_footprints={r1_id: r1_comp},
        )

        diagnostics = changeset.to_diagnostics()
        assert len(diagnostics) == 1
        assert diagnostics[0]["kind"] == "layout.sync.extra_footprint"
        assert diagnostics[0]["severity"] == "warning"
