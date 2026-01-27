"""
Property-based tests for lens operations using Hypothesis.

These tests verify the formal TLA+ lens laws hold for arbitrary inputs:

Law 1 (ViewConsistency): DOMAIN new_complement = new_view paths
    - Complement domains exactly match view domains after adapt_complement()

Law 2 (ComplementPreservation): Unchanged entities preserve complement
    - If entity existed AND FPID unchanged, complement is identical

Law 3 (Idempotence): sync(s, sync(s, d)) = sync(s, d)
    - Double sync equals single sync; no additions/removals on second pass

Law 4 (StructuralFidelity): No stale complements
    - No complements for entities not in view
    - No routing on unknown nets

Note: Renames (moved() paths) are now handled in Rust preprocessing.

Run with: pytest -v test_properties.py
Requires: hypothesis>=6.0
"""

import pytest

try:
    from hypothesis import given, settings, assume

    HYPOTHESIS_AVAILABLE = True
except ImportError:
    HYPOTHESIS_AVAILABLE = False

from ..types import (
    EntityId,
    Position,
    FootprintView,
    FootprintComplement,
    BoardView,
    BoardComplement,
    NetView,
)
from ..lens import adapt_complement, join, check_lens_invariants
from ..changeset import build_sync_changeset

if HYPOTHESIS_AVAILABLE:
    from .strategies import (
        board_view_strategy,
        board_complement_strategy,
        footprint_complement_strategy,
    )


pytestmark = pytest.mark.skipif(
    not HYPOTHESIS_AVAILABLE, reason="hypothesis not installed"
)


# ═══════════════════════════════════════════════════════════════════════════════
# Law 1: View Consistency
# ═══════════════════════════════════════════════════════════════════════════════


@pytest.mark.skipif(not HYPOTHESIS_AVAILABLE, reason="hypothesis not installed")
class TestViewConsistency:
    """
    Law 1: After sync, the view portion equals the new view from SOURCE.

    ∀ view, complement:
        join(view, adapt_complement(view, complement)).view == view
    """

    @given(
        view=board_view_strategy(),
        complement=board_complement_strategy(),
    )
    @settings(max_examples=100)
    def test_view_unchanged_after_sync(self, view, complement):
        """Board view equals input view after sync."""
        new_complement = adapt_complement(view, complement)
        board = join(view, new_complement)

        assert board.view == view

    @given(
        view=board_view_strategy(min_footprints=1, max_footprints=3),
    )
    @settings(max_examples=50)
    def test_view_unchanged_from_empty_complement(self, view):
        """View is unchanged even with empty complement."""
        empty_complement = BoardComplement()

        new_complement = adapt_complement(view, empty_complement)
        board = join(view, new_complement)

        assert board.view == view


# ═══════════════════════════════════════════════════════════════════════════════
# Law 2: Complement Preservation
# ═══════════════════════════════════════════════════════════════════════════════


@pytest.mark.skipif(not HYPOTHESIS_AVAILABLE, reason="hypothesis not installed")
class TestComplementPreservation:
    """
    Law 2: For entities with unchanged FPID, complement is preserved from DEST.

    ∀ entity in CommonEntities(s, d) where FPID unchanged:
        new_complement[entity] == old_complement[entity]
    """

    @given(
        view=board_view_strategy(min_footprints=1, max_footprints=3),
        complement=footprint_complement_strategy(),
    )
    @settings(max_examples=50)
    def test_single_footprint_complement_preserved(self, view, complement):
        """Single footprint preserves its complement."""
        assume(len(view.footprints) > 0)

        entity_id = next(iter(view.footprints.keys()))
        old_complement = BoardComplement(footprints={entity_id: complement})

        new_complement = adapt_complement(view, old_complement)

        assert entity_id in new_complement.footprints
        assert new_complement.footprints[entity_id] == complement


# ═══════════════════════════════════════════════════════════════════════════════
# Law 3: Idempotence
# ═══════════════════════════════════════════════════════════════════════════════


@pytest.mark.skipif(not HYPOTHESIS_AVAILABLE, reason="hypothesis not installed")
class TestIdempotence:
    """
    Law 3: Syncing twice with the same source equals syncing once.

    sync(s, sync(s, d)) = sync(s, d)
    """

    @given(
        view=board_view_strategy(min_footprints=1, max_footprints=3),
        complement=board_complement_strategy(),
    )
    @settings(max_examples=100)
    def test_double_sync_equals_single_sync(self, view, complement):
        """Applying sync twice produces same result as once."""
        # First sync
        complement1 = adapt_complement(view, complement)
        board1 = join(view, complement1)

        # Second sync
        complement2 = adapt_complement(view, complement1)
        board2 = join(view, complement2)

        assert board1 == board2

    @given(
        view=board_view_strategy(min_footprints=1, max_footprints=3),
        complement=board_complement_strategy(),
    )
    @settings(max_examples=100)
    def test_second_sync_detects_no_changes(self, view, complement):
        """Second sync should detect no additions/removals."""
        # First sync
        complement1 = adapt_complement(view, complement)

        # Second sync
        complement2 = adapt_complement(view, complement1)

        # No new additions on second sync
        changeset2 = build_sync_changeset(view, complement2, complement1)
        assert len(changeset2.added_footprints) == 0, (
            f"Unexpected additions: {changeset2.added_footprints}"
        )

        # No new removals on second sync
        assert len(changeset2.removed_footprints) == 0, (
            f"Unexpected removals: {changeset2.removed_footprints}"
        )


# ═══════════════════════════════════════════════════════════════════════════════
# Law 4: Structural Fidelity
# ═══════════════════════════════════════════════════════════════════════════════


@pytest.mark.skipif(not HYPOTHESIS_AVAILABLE, reason="hypothesis not installed")
class TestStructuralFidelity:
    """
    Law 4: After sync, the board contains exactly what's in the view.

    - No stale footprint complements
    - No routing on unknown nets
    - All footprints in view have complements
    """

    @given(
        view=board_view_strategy(min_footprints=1, max_footprints=5),
        complement=board_complement_strategy(),
    )
    @settings(max_examples=100)
    def test_no_stale_complements(self, view, complement):
        """After sync, no complements exist for entities not in view."""
        new_complement = adapt_complement(view, complement)

        # Every complement should have a view
        for entity_id in new_complement.footprints.keys():
            assert entity_id in view.footprints, (
                f"Stale complement for {entity_id}"
            )

        # Every view should have a complement
        for entity_id in view.footprints.keys():
            assert entity_id in new_complement.footprints, (
                f"Missing complement for {entity_id}"
            )

    @given(view=board_view_strategy(min_footprints=1, max_footprints=3))
    @settings(max_examples=50)
    def test_check_lens_invariants_passes_after_sync(self, view):
        """check_lens_invariants should pass after adapt_complement."""
        complement = BoardComplement()
        new_complement = adapt_complement(view, complement)

        # Should not raise
        check_lens_invariants(view, new_complement)


# ═══════════════════════════════════════════════════════════════════════════════
# Footprint-Specific Properties
# ═══════════════════════════════════════════════════════════════════════════════


class TestFootprintProperties:
    """Tests for footprint-specific properties."""

    def test_new_footprint_starts_unlocked(self):
        """New footprints start with locked=False."""
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

        new_complement = adapt_complement(new_view, BoardComplement())

        assert new_complement.footprints[entity_id].locked is False


# ═══════════════════════════════════════════════════════════════════════════════
# Group Membership Consistency
# ═══════════════════════════════════════════════════════════════════════════════


@pytest.mark.skipif(not HYPOTHESIS_AVAILABLE, reason="hypothesis not installed")
class TestGroupMembershipConsistency:
    """
    Tests for group membership view consistency.

    GroupView.member_ids should reference valid footprints.
    This is a VIEW property, not a complement property.
    """

    @given(view=board_view_strategy(min_footprints=2, max_footprints=4))
    @settings(max_examples=50)
    def test_group_members_reference_valid_footprints(self, view):
        """All group member_ids must reference footprints in the view."""
        footprint_ids = set(view.footprints.keys())

        for group_id, group_view in view.groups.items():
            for member_id in group_view.member_ids:
                assert member_id in footprint_ids, (
                    f"Group {group_id} references non-existent footprint {member_id}"
                )


# ═══════════════════════════════════════════════════════════════════════════════
# Invariant Checker Tests
# ═══════════════════════════════════════════════════════════════════════════════


class TestInvariantChecker:
    """Tests for the check_lens_invariants() function."""

    def test_valid_view_complement_passes(self):
        """A well-formed view/complement pair passes the invariant check."""
        entity_id = EntityId.from_string("Power.C1")

        view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="C1",
                    value="10uF",
                    fpid="C_0603",
                )
            },
            nets={"VCC": NetView(name="VCC", connections=((entity_id, "1"),))},
        )

        complement = BoardComplement(
            footprints={
                entity_id: FootprintComplement(
                    position=Position(x=0, y=0),
                    orientation=0.0,
                    layer="F.Cu",
                )
            },
        )

        # Should not raise
        check_lens_invariants(view, complement)

    def test_missing_footprint_complement_fails(self):
        """Missing footprint complement violates Law 1."""
        entity_id = EntityId.from_string("Power.C1")

        view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="C1",
                    value="10uF",
                    fpid="C_0603",
                )
            }
        )

        complement = BoardComplement(footprints={})  # Missing complement!

        with pytest.raises(AssertionError, match="Law 1 & 4"):
            check_lens_invariants(view, complement)

    def test_extra_footprint_complement_fails(self):
        """Extra footprint complement violates Law 4."""
        entity_id = EntityId.from_string("Power.C1")
        stale_id = EntityId.from_string("Stale.R1")

        view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="C1",
                    value="10uF",
                    fpid="C_0603",
                )
            }
        )

        complement = BoardComplement(
            footprints={
                entity_id: FootprintComplement(
                    position=Position(x=0, y=0),
                    orientation=0.0,
                    layer="F.Cu",
                ),
                stale_id: FootprintComplement(  # Stale complement!
                    position=Position(x=1000, y=0),
                    orientation=0.0,
                    layer="F.Cu",
                ),
            }
        )

        with pytest.raises(AssertionError, match="Law 1 & 4"):
            check_lens_invariants(view, complement)
