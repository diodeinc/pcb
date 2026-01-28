"""
Test scenarios from the sync proposal design document (Section 12).

These tests verify the lens-based sync implementation against the
documented test scenarios and edge cases.

Note: Renames (moved() paths) are now handled in Rust preprocessing before
the Python sync runs. Rename-related tests have been moved to Rust integration tests.
"""

from typing import List, Dict, Any

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
from ..lens import adapt_complement
from ..changeset import build_sync_changeset


def format_diagnostics(diagnostics: List[Dict[str, Any]]) -> str:
    """Format diagnostics as a stable string for snapshot comparison."""
    if not diagnostics:
        return "(no diagnostics)"
    lines = []
    for d in sorted(diagnostics, key=lambda x: (x.get("path", ""), x["kind"])):
        lines.append(f"{d['severity'].upper()}: {d['kind']} @ {d.get('path', '')}")
    return "\n".join(lines)


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

        # Verify diagnostics
        diagnostics = changeset.to_diagnostics()
        assert (
            format_diagnostics(diagnostics) == "INFO: layout.sync.missing_footprint @ C"
        )

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

        # Verify diagnostics
        diagnostics = changeset.to_diagnostics()
        assert (
            format_diagnostics(diagnostics)
            == "WARNING: layout.sync.extra_footprint @ C"
        )

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
        assert new_view.footprints[a_id].value == "4.7k"

        # No diagnostics for metadata-only changes
        assert format_diagnostics(changeset.to_diagnostics()) == "(no diagnostics)"

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

        # FPID change shows as both add and remove
        diagnostics = format_diagnostics(changeset.to_diagnostics())
        assert "INFO: layout.sync.missing_footprint @ A" in diagnostics
        assert "WARNING: layout.sync.extra_footprint @ A" in diagnostics

        # View has new FPID
        assert new_view.footprints[new_id].fpid == new_fpid


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

        # Diagnostics: footprint added (groups don't produce diagnostics)
        diagnostics = format_diagnostics(changeset.to_diagnostics())
        assert diagnostics == "INFO: layout.sync.missing_footprint @ Power.C1"

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

        # Diagnostics: footprint removed (groups don't produce diagnostics)
        diagnostics = format_diagnostics(changeset.to_diagnostics())
        assert diagnostics == "WARNING: layout.sync.extra_footprint @ Power.C1"


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
        assert format_diagnostics(changeset2.to_diagnostics()) == "(no diagnostics)"

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
        assert format_diagnostics(changeset2.to_diagnostics()) == "(no diagnostics)"


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

        # Diagnostics for removed footprint
        assert format_diagnostics(changeset.to_diagnostics()) == (
            "WARNING: layout.sync.extra_footprint @ A"
        )


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
        assert format_diagnostics(changeset.to_diagnostics()) == (
            f"INFO: layout.sync.missing_footprint @ {long_path}"
        )

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
        changeset = build_sync_changeset(view, new_complement, old_complement)
        assert format_diagnostics(changeset.to_diagnostics()) == (
            "INFO: layout.sync.missing_footprint @ Résistance_10kΩ"
        )

    def test_empty_path(self):
        """Test handling of empty path (should be skipped or handled gracefully)."""
        empty_path = EntityPath.from_string("")
        assert not empty_path  # Empty path is falsy
        assert empty_path.depth == 0


class TestUnmanagedFootprints:
    """
    Tests for detection of unmanaged footprints (duplicates/extras).

    Unmanaged footprints have a Path field but invalid/missing KIID_PATH,
    indicating they were manually copied or added outside the sync process.
    """

    def test_footprint_with_correct_kiid_path_is_managed(self):
        """Footprint with matching KIID_PATH should be extracted normally."""
        import uuid as uuid_module
        from unittest.mock import Mock
        from ..lens import extract

        path_str = "Power.R1"
        expected_uuid = str(uuid_module.uuid5(uuid_module.NAMESPACE_URL, path_str))
        expected_kiid_path = f"/{expected_uuid}/{expected_uuid}"

        # Mock footprint with correct KIID_PATH
        fp = Mock()
        fp.GetFieldByName.return_value = Mock(GetText=lambda: path_str)
        fp.GetPath.return_value = Mock(AsString=lambda: expected_kiid_path)
        fp.GetFPIDAsString.return_value = "Resistor_SMD:R_0603"
        fp.GetReference.return_value = "R1"
        fp.GetValue.return_value = "10k"
        fp.IsDNP.return_value = False
        fp.IsExcludedFromBOM.return_value = False
        fp.IsExcludedFromPosFiles.return_value = False
        fp.GetFields.return_value = []
        fp.GetPosition.return_value = Mock(x=1000, y=2000)
        fp.GetOrientation.return_value = Mock(AsDegrees=lambda: 0.0)
        fp.GetLayer.return_value = 0  # F.Cu
        fp.IsLocked.return_value = False
        fp.Reference.return_value = Mock(
            GetPosition=lambda: Mock(x=1000, y=2000), IsVisible=lambda: True
        )
        fp.Value.return_value = Mock(
            GetPosition=lambda: Mock(x=1000, y=2000), IsVisible=lambda: False
        )
        fp.Pads.return_value = []

        # Mock board
        board = Mock()
        board.GetFootprints.return_value = [fp]
        board.Groups.return_value = []

        # Mock pcbnew
        pcbnew = Mock()
        pcbnew.B_Cu = 31

        diagnostics: List[Dict[str, Any]] = []
        view, complement = extract(board, pcbnew, diagnostics)

        # Should be extracted as managed footprint
        assert len(view.footprints) == 1
        assert len(diagnostics) == 0

    def test_footprint_with_missing_kiid_path_is_unmanaged(self):
        """Footprint with empty KIID_PATH should be detected as unmanaged."""
        from unittest.mock import Mock
        from ..lens import extract

        path_str = "Power.R1"

        # Mock footprint with empty KIID_PATH (like a manual copy)
        fp = Mock()
        fp.GetFieldByName.return_value = Mock(GetText=lambda: path_str)
        fp.GetPath.return_value = Mock(AsString=lambda: "")  # Empty!
        fp.GetFPIDAsString.return_value = "Resistor_SMD:R_0603"
        fp.GetReference.return_value = "R1"
        fp.m_Uuid = Mock(AsString=lambda: "fake-uuid-1234")

        # Mock board
        board = Mock()
        board.GetFootprints.return_value = [fp]
        board.Groups.return_value = []

        # Mock pcbnew
        pcbnew = Mock()

        diagnostics: List[Dict[str, Any]] = []
        view, complement = extract(board, pcbnew, diagnostics)

        # Should NOT be extracted
        assert len(view.footprints) == 0

        # Should have warning diagnostic
        assert len(diagnostics) == 1
        assert diagnostics[0]["kind"] == "layout.sync.unmanaged_footprint"
        assert diagnostics[0]["severity"] == "warning"
        assert path_str in diagnostics[0]["body"]

    def test_footprint_with_wrong_kiid_path_is_unmanaged(self):
        """Footprint with mismatched KIID_PATH should be detected as unmanaged."""
        from unittest.mock import Mock
        from ..lens import extract

        path_str = "Power.R1"

        # Mock footprint with wrong KIID_PATH
        fp = Mock()
        fp.GetFieldByName.return_value = Mock(GetText=lambda: path_str)
        fp.GetPath.return_value = Mock(
            AsString=lambda: "/wrong-uuid/wrong-uuid"
        )  # Wrong!
        fp.GetFPIDAsString.return_value = "Resistor_SMD:R_0603"
        fp.GetReference.return_value = "R1"
        fp.m_Uuid = Mock(AsString=lambda: "fake-uuid-5678")

        # Mock board
        board = Mock()
        board.GetFootprints.return_value = [fp]
        board.Groups.return_value = []

        # Mock pcbnew
        pcbnew = Mock()

        diagnostics: List[Dict[str, Any]] = []
        view, complement = extract(board, pcbnew, diagnostics)

        # Should NOT be extracted
        assert len(view.footprints) == 0

        # Should have warning diagnostic
        assert len(diagnostics) == 1
        assert diagnostics[0]["kind"] == "layout.sync.unmanaged_footprint"

    def test_multiple_footprints_with_same_path_one_managed(self):
        """Only the footprint with correct KIID_PATH should be managed."""
        import uuid as uuid_module
        from unittest.mock import Mock
        from ..lens import extract

        path_str = "Power.R1"
        expected_uuid = str(uuid_module.uuid5(uuid_module.NAMESPACE_URL, path_str))
        expected_kiid_path = f"/{expected_uuid}/{expected_uuid}"

        # Mock managed footprint (correct KIID_PATH)
        fp_managed = Mock()
        fp_managed.GetFieldByName.return_value = Mock(GetText=lambda: path_str)
        fp_managed.GetPath.return_value = Mock(AsString=lambda: expected_kiid_path)
        fp_managed.GetFPIDAsString.return_value = "Resistor_SMD:R_0603"
        fp_managed.GetReference.return_value = "R1"
        fp_managed.GetValue.return_value = "10k"
        fp_managed.IsDNP.return_value = False
        fp_managed.IsExcludedFromBOM.return_value = False
        fp_managed.IsExcludedFromPosFiles.return_value = False
        fp_managed.GetFields.return_value = []
        fp_managed.GetPosition.return_value = Mock(x=1000, y=2000)
        fp_managed.GetOrientation.return_value = Mock(AsDegrees=lambda: 0.0)
        fp_managed.GetLayer.return_value = 0
        fp_managed.IsLocked.return_value = False
        fp_managed.Reference.return_value = Mock(
            GetPosition=lambda: Mock(x=1000, y=2000), IsVisible=lambda: True
        )
        fp_managed.Value.return_value = Mock(
            GetPosition=lambda: Mock(x=1000, y=2000), IsVisible=lambda: False
        )
        fp_managed.Pads.return_value = []

        # Mock duplicate footprint (empty KIID_PATH - manual copy)
        fp_duplicate = Mock()
        fp_duplicate.GetFieldByName.return_value = Mock(GetText=lambda: path_str)
        fp_duplicate.GetPath.return_value = Mock(AsString=lambda: "")  # Empty!
        fp_duplicate.GetFPIDAsString.return_value = "Resistor_SMD:R_0603"
        fp_duplicate.GetReference.return_value = "R1"
        fp_duplicate.m_Uuid = Mock(AsString=lambda: "duplicate-uuid")

        # Mock board with both footprints
        board = Mock()
        board.GetFootprints.return_value = [fp_managed, fp_duplicate]
        board.Groups.return_value = []

        # Mock pcbnew
        pcbnew = Mock()
        pcbnew.B_Cu = 31

        diagnostics: List[Dict[str, Any]] = []
        view, complement = extract(board, pcbnew, diagnostics)

        # Only managed footprint should be extracted
        assert len(view.footprints) == 1

        # Duplicate should generate warning
        assert len(diagnostics) == 1
        assert diagnostics[0]["kind"] == "layout.sync.unmanaged_footprint"
