"""
Regression tests for SWIG pointer stability and group deletion bugs.

These tests use "invalidatable fakes" that simulate KiCad's SWIG behavior
where object pointers become invalid after board structural mutations.

Bug 1 (SWIG): Cached group/footprint references became stale after board
mutations, causing TypeError or crashes when accessed later.

Bug 2 (Group Deletion): Deleting a group was also deleting all its contents,
causing footprints to be unintentionally removed.
"""

from typing import Any, Dict, List
from unittest.mock import Mock

from ..types import (
    EntityPath,
    EntityId,
    FootprintView,
    BoardView,
    BoardComplement,
    default_footprint_complement,
)
from ..changeset import SyncChangeset


class StaleObjectError(Exception):
    """Raised when accessing an invalidated SWIG-like object."""

    pass


class _FakeFOOTPRINT:
    """Base class for fake footprints (defined early for inheritance)."""

    pass


class _FakePCB_GROUP:
    """Base class for fake groups (defined early for inheritance)."""

    pass


class InvalidatableFootprint(_FakeFOOTPRINT):
    """Fake KiCad footprint that becomes invalid after board mutations."""

    def __init__(self, entity_id: EntityId, reference: str, fpid: str, value: str):
        self._valid = True
        self._entity_id = entity_id
        self._reference = reference
        self._fpid = fpid
        self._value = value
        self._position = Mock()
        self._position.x = 0
        self._position.y = 0
        self._layer = 0  # F.Cu

    def _check_valid(self):
        if not self._valid:
            raise StaleObjectError(
                f"Stale SWIG pointer: footprint {self._reference} was invalidated"
            )

    def invalidate(self):
        self._valid = False

    def GetReference(self) -> str:
        self._check_valid()
        return self._reference

    def GetFPIDAsString(self) -> str:
        self._check_valid()
        return self._fpid

    def GetValue(self) -> str:
        self._check_valid()
        return self._value

    def SetReference(self, ref: str):
        self._check_valid()
        self._reference = ref

    def SetValue(self, value: str):
        self._check_valid()
        self._value = value

    def SetDNP(self, dnp: bool):
        self._check_valid()
        self._dnp = dnp

    def SetExcludedFromBOM(self, excluded: bool):
        self._check_valid()
        self._exclude_bom = excluded

    def SetExcludedFromPosFiles(self, excluded: bool):
        self._check_valid()
        self._exclude_pos = excluded

    def GetPosition(self):
        self._check_valid()
        return self._position

    def SetPosition(self, pos):
        self._check_valid()
        self._position = pos

    def GetLayer(self) -> int:
        self._check_valid()
        return self._layer

    def GetFieldByName(self, name: str):
        self._check_valid()
        if name == "Path":
            field = Mock()
            field.GetText = lambda: str(self._entity_id.path)
            return field
        return None

    def Pads(self):
        self._check_valid()
        return []

    def GetPath(self):
        self._check_valid()
        import uuid

        path_str = str(self._entity_id.path)
        expected_uuid = str(uuid.uuid5(uuid.NAMESPACE_URL, path_str))
        mock_path = Mock()
        mock_path.AsString = lambda: f"/{expected_uuid}/{expected_uuid}"
        return mock_path


class InvalidatableGroup(_FakePCB_GROUP):
    """Fake KiCad group that becomes invalid after board mutations."""

    def __init__(self, name: str):
        self._valid = True
        self._name = name
        self._items: List[Any] = []

    def _check_valid(self):
        if not self._valid:
            raise StaleObjectError(
                f"Stale SWIG pointer: group {self._name} was invalidated"
            )

    def invalidate(self):
        self._valid = False

    def GetName(self) -> str:
        self._check_valid()
        return self._name

    def SetName(self, name: str):
        self._check_valid()
        self._name = name

    def GetItems(self) -> List[Any]:
        self._check_valid()
        return list(self._items)

    def AddItem(self, item: Any):
        self._check_valid()
        if item not in self._items:
            self._items.append(item)

    def RemoveItem(self, item: Any):
        self._check_valid()
        if item in self._items:
            self._items.remove(item)


class InvalidatableBoard:
    """Fake KiCad board that invalidates cached objects on mutations."""

    def __init__(self):
        self._footprints: List[InvalidatableFootprint] = []
        self._groups: List[InvalidatableGroup] = []
        self._nets: Dict[str, Any] = {}
        self._deleted_footprints: List[InvalidatableFootprint] = []
        self._deleted_groups: List[InvalidatableGroup] = []

    def _invalidate_all_cached(self):
        """Simulate SWIG pointer invalidation after structural change."""
        for fp in self._footprints:
            fp.invalidate()
        for g in self._groups:
            g.invalidate()

    def _rebuild_fresh(self):
        """Create fresh wrappers (simulates what KiCad does on re-enumeration)."""
        # In real KiCad, iterating GetFootprints() after mutation gives fresh pointers
        new_fps = []
        for old_fp in self._footprints:
            if old_fp._valid or not old_fp._valid:  # Always make new wrapper
                new_fp = InvalidatableFootprint(
                    old_fp._entity_id, old_fp._reference, old_fp._fpid, old_fp._value
                )
                new_fp._position = old_fp._position
                new_fps.append(new_fp)
        self._footprints = new_fps

        new_groups = []
        for old_g in self._groups:
            new_g = InvalidatableGroup(old_g._name)
            new_g._items = list(old_g._items)
            new_groups.append(new_g)
        self._groups = new_groups

    def GetFootprints(self) -> List[InvalidatableFootprint]:
        # Return fresh wrappers each time (simulates real KiCad behavior)
        return list(self._footprints)

    def Groups(self) -> List[InvalidatableGroup]:
        # Return fresh wrappers each time
        return list(self._groups)

    def Add(self, item: Any):
        self._invalidate_all_cached()
        self._rebuild_fresh()

        if isinstance(item, InvalidatableFootprint):
            self._footprints.append(item)
        elif isinstance(item, InvalidatableGroup):
            self._groups.append(item)

    def Delete(self, item: Any):
        self._invalidate_all_cached()

        if isinstance(item, InvalidatableFootprint):
            # Remove footprint from board
            self._footprints = [
                fp for fp in self._footprints if fp._entity_id != item._entity_id
            ]
            self._deleted_footprints.append(item)
        elif isinstance(item, InvalidatableGroup):
            # Remove group container only - items stay on board
            self._groups = [g for g in self._groups if g._name != item._name]
            self._deleted_groups.append(item)

        self._rebuild_fresh()

    def FindNet(self, name: str):
        return self._nets.get(name)

    def GetLayerName(self, layer: int) -> str:
        return "F.Cu" if layer == 0 else "B.Cu"

    def add_footprint(self, entity_id: EntityId, reference: str, fpid: str, value: str):
        """Helper to add a footprint without triggering invalidation."""
        fp = InvalidatableFootprint(entity_id, reference, fpid, value)
        self._footprints.append(fp)
        return fp

    def add_group(self, name: str) -> InvalidatableGroup:
        """Helper to add a group without triggering invalidation."""
        g = InvalidatableGroup(name)
        self._groups.append(g)
        return g


class FakePcbnew:
    """Fake pcbnew module for isinstance checks."""

    FOOTPRINT = _FakeFOOTPRINT
    PCB_GROUP = _FakePCB_GROUP

    class NETINFO_ITEM:
        def __init__(self, board, name):
            self.name = name

    class VECTOR2I:
        def __init__(self, x, y):
            self.x = x
            self.y = y


class TestSwigPointerStability:
    """
    Regression test for Bug 1: SWIG pointer corruption.

    The bug: Cached group references in groups_by_name dict became stale
    after board mutations (Delete/Add), causing crashes in Phase 4 (membership
    rebuild) when iterating group.GetItems().

    The fix: Rebuild indices after structural changes.
    """

    def test_group_iteration_after_deletion(self):
        """
        Accessing a group after board.Delete() should use fresh pointer.

        SCENARIO:
        1. Board has group G1 containing footprint F1
        2. Sync removes group G2 (triggers SWIG invalidation)
        3. Phase 4 iterates G1.GetItems() - must use fresh pointer
        """
        from ..kicad_adapter import _build_groups_index

        board = InvalidatableBoard()

        f1_id = EntityId.from_string("G1.F1", fpid="R_0603")
        f1 = board.add_footprint(f1_id, "F1", "R_0603", "10k")

        g1 = board.add_group("G1")
        g1.AddItem(f1)

        g2 = board.add_group("G2")

        # Cache references BEFORE mutation (simulating old buggy code)
        old_groups_index = _build_groups_index(board)
        old_g1 = old_groups_index["G1"]

        # Delete G2 - this invalidates all cached pointers
        board.Delete(g2)

        # OLD BUG: Using old_g1 would crash
        try:
            old_g1.GetItems()
            assert False, "Should have raised StaleObjectError"
        except StaleObjectError:
            pass  # Expected - old pointer is stale

        # FIX: Rebuild index after mutation
        fresh_groups_index = _build_groups_index(board)
        fresh_g1 = fresh_groups_index["G1"]

        # Fresh pointer works
        items = fresh_g1.GetItems()
        assert len(items) == 1

    def test_footprint_access_after_addition(self):
        """
        Accessing footprints after board.Add() should use fresh pointers.
        """
        from ..kicad_adapter import _build_footprints_index

        board = InvalidatableBoard()

        f1_id = EntityId.from_string("F1", fpid="R_0603")
        board.add_footprint(f1_id, "F1", "R_0603", "10k")

        # Cache reference BEFORE mutation
        old_fps_index = _build_footprints_index(board)
        old_f1 = old_fps_index[f1_id]

        # Add new footprint - this invalidates all cached pointers
        f2_id = EntityId.from_string("F2", fpid="R_0603")
        f2 = InvalidatableFootprint(f2_id, "F2", "R_0603", "4k7")
        board.Add(f2)

        # OLD BUG: Using old_f1 would crash
        try:
            old_f1.GetReference()
            assert False, "Should have raised StaleObjectError"
        except StaleObjectError:
            pass  # Expected

        # FIX: Rebuild index after mutation
        fresh_fps_index = _build_footprints_index(board)
        fresh_f1 = fresh_fps_index[f1_id]

        # Fresh pointer works
        assert fresh_f1.GetReference() == "F1"


class TestGroupDeletionPreservesContents:
    """
    Regression test for Bug 2: Group deletion destroying footprints.

    The bug: When removing a group, the old code iterated group.GetItems()
    and called Delete() on each item, including footprints. This caused
    footprints to be deleted even if they weren't marked for removal.

    The fix: Only call board.Delete(group) - KiCad preserves contents.
    """

    def test_delete_group_preserves_footprints(self):
        """
        Deleting a group must NOT delete its member footprints.

        SCENARIO:
        1. Board has group G1 containing footprint F1
        2. Changeset removes G1 (but not F1)
        3. After apply, F1 should still exist on board
        """
        board = InvalidatableBoard()

        f1_id = EntityId.from_string("G1.F1", fpid="R_0603")
        f1 = board.add_footprint(f1_id, "F1", "R_0603", "10k")

        g1 = board.add_group("G1")
        g1.AddItem(f1)

        # Verify initial state
        assert len(board.GetFootprints()) == 1
        assert len(board.Groups()) == 1

        # Delete only the group (correct behavior)
        board.Delete(g1)

        # Group is gone
        assert len(board.Groups()) == 0

        # Footprint is preserved
        assert len(board.GetFootprints()) == 1
        fp = board.GetFootprints()[0]
        assert fp.GetReference() == "F1"

    def test_apply_changeset_group_removal_preserves_footprints(self):
        """
        Full integration: apply_changeset removing a group preserves footprints.

        This tests the actual fix in apply_changeset Phase 1a.
        """
        from ..kicad_adapter import apply_changeset

        board = InvalidatableBoard()

        # Create footprint F1 in group G1
        f1_id = EntityId.from_string("G1.F1", fpid="R_0603")
        f1 = board.add_footprint(f1_id, "F1", "R_0603", "10k")

        g1 = board.add_group("G1")
        g1.AddItem(f1)

        # Create changeset that removes G1 but keeps F1
        g1_group_id = EntityId(path=EntityPath.from_string("G1"))

        # View still has F1, but no groups
        view = BoardView(
            footprints={
                f1_id: FootprintView(
                    entity_id=f1_id,
                    reference="F1",
                    value="10k",
                    fpid="R_0603",
                )
            },
            groups={},
        )

        complement = BoardComplement(
            footprints={
                f1_id: default_footprint_complement(),
            },
            groups={},
        )

        changeset = SyncChangeset(
            view=view,
            complement=complement,
            removed_groups={g1_group_id},  # Remove group
            # F1 is NOT in removed_footprints
        )

        # Apply changeset
        oplog = apply_changeset(changeset, board, FakePcbnew, {}, package_roots={})

        # Verify: group removed, footprint preserved
        assert len(board.Groups()) == 0
        assert len(board.GetFootprints()) == 1

        fp = board.GetFootprints()[0]
        assert fp.GetReference() == "F1"

        # Check oplog recorded the group removal
        gr_remove_events = [e for e in oplog.events if e.kind == "GR_REMOVE"]
        assert len(gr_remove_events) == 1
        assert gr_remove_events[0].fields["path"] == "G1"
