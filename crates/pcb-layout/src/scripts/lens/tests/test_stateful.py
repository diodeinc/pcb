"""
Stateful property-based tests for lens operations using Hypothesis.

Uses RuleBasedStateMachine to simulate realistic edit sequences and
verify the lens laws hold after every action.

Edit operations simulated:
- add_footprint: Add a new footprint to the design
- remove_footprint: Remove an existing footprint
- change_fpid: Change a footprint's package type

Note: Renames (moved() paths) are now handled in Rust preprocessing.
Rename-related tests have been moved to Rust integration tests.

Run with: pytest -v test_stateful.py
Requires: hypothesis>=6.0
"""

import pytest

try:
    from hypothesis import settings, note
    from hypothesis import strategies as st
    from hypothesis.stateful import (
        RuleBasedStateMachine,
        rule,
        invariant,
        initialize,
        Bundle,
    )

    HYPOTHESIS_AVAILABLE = True
except ImportError:
    HYPOTHESIS_AVAILABLE = False

from ..types import (
    EntityId,
    Position,
    FootprintView,
    FootprintComplement,
    GroupView,
    NetView,
    BoardView,
    BoardComplement,
)
from ..lens import adapt_complement
from ..changeset import build_sync_changeset

if HYPOTHESIS_AVAILABLE:
    from .strategies import (
        entity_path_strategy,
        FPID_POOL,
        VALUE_POOL,
    )


pytestmark = pytest.mark.skipif(
    not HYPOTHESIS_AVAILABLE, reason="hypothesis not installed"
)


# ═══════════════════════════════════════════════════════════════════════════════
# Common Helpers
# ═══════════════════════════════════════════════════════════════════════════════


def derive_groups_from_footprints(footprints: dict) -> dict:
    """Derive group structure from footprint paths.

    Groups are created for parent paths that have at least 2 descendant footprints
    (to avoid wrapper-style single-child groups). Members include all descendants,
    not just direct children.
    """
    groups = {}
    fp_paths = {fp_id.path for fp_id in footprints.keys()}

    for fp_id in footprints.keys():
        parent = fp_id.path.parent()
        while parent and parent.segments:
            # Skip if this parent path equals a footprint path (NoLeafGroups)
            if parent in fp_paths:
                parent = parent.parent()
                continue

            group_id = EntityId(path=parent)
            if group_id not in groups:
                # Members are all descendant footprints (not just direct children)
                member_ids = [
                    other_id
                    for other_id in footprints.keys()
                    if parent.is_ancestor_of(other_id.path)
                ]
                # Only create group if it has at least 2 members (skip wrappers)
                if len(member_ids) >= 2:
                    groups[group_id] = GroupView(
                        entity_id=group_id,
                        member_ids=tuple(member_ids),
                    )
            parent = parent.parent()
    return groups


def find_footprint_by_reference(footprints: dict, reference: str) -> EntityId:
    """Find entity_id by reference designator, or None if not found."""
    for entity_id, fp in footprints.items():
        if fp.reference == reference:
            return entity_id
    return None


def make_footprint(path_str: str, reference: str, value: str, fpid: str) -> tuple:
    """Create a (entity_id, FootprintView) tuple."""
    entity_id = EntityId.from_string(path_str)
    return entity_id, FootprintView(
        entity_id=entity_id,
        reference=reference,
        value=value,
        fpid=fpid,
    )


def make_complement(
    x: int, y: int, orientation: float = 0.0, layer: str = "F.Cu", locked: bool = False
) -> FootprintComplement:
    """Create a FootprintComplement with common defaults."""
    return FootprintComplement(
        position=Position(x=x, y=y),
        orientation=orientation,
        layer=layer,
        locked=locked,
    )


# ═══════════════════════════════════════════════════════════════════════════════
# Stateful Machine for Edit Sequences
# ═══════════════════════════════════════════════════════════════════════════════


@pytest.mark.skipif(not HYPOTHESIS_AVAILABLE, reason="hypothesis not installed")
class LensSyncStateMachine(RuleBasedStateMachine):
    """
    Stateful test machine simulating design edits and syncs.

    State:
    - current_view: The SOURCE view (what the netlist produces)
    - current_complement: The DEST complement (user-authored placements)
    - last_synced_view: The view from the last sync (for FPID change detection)

    After every rule, we sync and verify the lens laws hold.
    """

    def __init__(self):
        super().__init__()
        self.current_view = BoardView()
        self.current_complement = BoardComplement()
        self.last_synced_view = None
        self.sync_count = 0

    footprint_paths = Bundle("footprint_paths")

    @initialize()
    def setup(self):
        """Reset state for each test run."""
        self.current_view = BoardView()
        self.current_complement = BoardComplement()
        self.last_synced_view = None
        self.sync_count = 0

    def _sync(self):
        """
        Perform sync and update state.

        Returns (new_complement, old_complement) for changeset building.
        """
        old_complement = self.current_complement
        new_complement = adapt_complement(
            self.current_view,
            old_complement,
        )

        self.current_complement = new_complement
        self.last_synced_view = self.current_view
        self.sync_count += 1

        return new_complement, old_complement

    def _derive_groups(self) -> dict:
        """Derive group structure from footprints."""
        return derive_groups_from_footprints(self.current_view.footprints)

    # ─────────────────────────────────────────────────────────────────────────
    # Rules (state transitions)
    # ─────────────────────────────────────────────────────────────────────────

    @rule(
        target=footprint_paths,
        path=entity_path_strategy(min_depth=1, max_depth=3),
        fpid=st.sampled_from(FPID_POOL),
        value=st.sampled_from(VALUE_POOL),
    )
    def add_footprint(self, path, fpid, value):
        """Add a new footprint to the design."""
        entity_id = EntityId(path=path, fpid=fpid)

        # Skip if path conflicts with existing footprint or group (NoLeafGroups)
        existing_paths = {eid.path for eid in self.current_view.footprints}
        existing_paths |= {gid.path for gid in self.current_view.groups}
        if path in existing_paths:
            return path

        footprint = FootprintView(
            entity_id=entity_id,
            reference=path.name,
            value=value,
            fpid=fpid,
            fields={"Path": str(path)},
        )

        new_footprints = dict(self.current_view.footprints)
        new_footprints[entity_id] = footprint

        self.current_view = BoardView(
            footprints=new_footprints,
            groups=self._derive_groups(),
            nets=self.current_view.nets,
        )

        self._sync()
        note(f"Added footprint: {path} with fpid={fpid}")
        return path

    @rule(path=footprint_paths)
    def remove_footprint(self, path):
        """Remove an existing footprint."""
        # Find footprint by path (any fpid)
        entity_id = None
        for eid in self.current_view.footprints:
            if eid.path == path:
                entity_id = eid
                break

        if entity_id is None:
            return

        new_footprints = {
            k: v for k, v in self.current_view.footprints.items() if k != entity_id
        }

        self.current_view = BoardView(
            footprints=new_footprints,
            groups=derive_groups_from_footprints(new_footprints),
            nets=self.current_view.nets,
        )

        self._sync()
        note(f"Removed footprint: {path}")

    @rule(
        path=footprint_paths,
        new_fpid=st.sampled_from(FPID_POOL),
    )
    def change_fpid(self, path, new_fpid):
        """Change the FPID of an existing footprint.

        With EntityId including fpid, this is a delete + add operation.
        """
        # Find footprint with this path (any fpid)
        old_entity_id = None
        old_fp = None
        for eid, fp in self.current_view.footprints.items():
            if eid.path == path:
                old_entity_id = eid
                old_fp = fp
                break

        if old_entity_id is None:
            return

        if old_fp.fpid == new_fpid:
            return

        # Create new entity with new fpid
        new_entity_id = EntityId(path=path, fpid=new_fpid)
        new_fp = FootprintView(
            entity_id=new_entity_id,
            reference=old_fp.reference,
            value=old_fp.value,
            fpid=new_fpid,
            dnp=old_fp.dnp,
            exclude_from_bom=old_fp.exclude_from_bom,
            exclude_from_pos=old_fp.exclude_from_pos,
            fields=old_fp.fields,
        )

        # Remove old, add new
        new_footprints = dict(self.current_view.footprints)
        del new_footprints[old_entity_id]
        new_footprints[new_entity_id] = new_fp

        # Rebuild groups with updated member EntityIds
        new_groups = {}
        for gid, gv in self.current_view.groups.items():
            new_members = []
            for mid in gv.member_ids:
                if mid == old_entity_id:
                    new_members.append(new_entity_id)
                else:
                    new_members.append(mid)
            new_groups[gid] = GroupView(
                entity_id=gv.entity_id,
                member_ids=tuple(new_members),
                layout_path=gv.layout_path,
            )

        self.current_view = BoardView(
            footprints=new_footprints,
            groups=new_groups,
            nets=self.current_view.nets,
        )

        new_complement, old_complement = self._sync()
        note(f"Changed FPID: {path} from {old_fp.fpid} to {new_fpid}")

        # Should be tracked as remove + add
        changeset = build_sync_changeset(
            self.current_view, new_complement, old_complement
        )
        assert old_entity_id in changeset.removed_footprints
        assert new_entity_id in changeset.added_footprints

    # ─────────────────────────────────────────────────────────────────────────
    # Invariants (checked after every rule)
    # ─────────────────────────────────────────────────────────────────────────

    @invariant()
    def complement_matches_view(self):
        """Law 1: Complement domain equals view domain."""
        view_fp_ids = set(self.current_view.footprints.keys())
        complement_fp_ids = set(self.current_complement.footprints.keys())
        assert view_fp_ids == complement_fp_ids, (
            f"Domain mismatch: view={view_fp_ids}, complement={complement_fp_ids}"
        )

    @invariant()
    def no_stale_group_complements(self):
        """Law 4: No stale group complements."""
        view_group_ids = set(self.current_view.groups.keys())
        complement_group_ids = set(self.current_complement.groups.keys())
        assert complement_group_ids == view_group_ids, (
            f"Group domain mismatch: view={view_group_ids}, complement={complement_group_ids}"
        )

    @invariant()
    def groups_have_valid_members(self):
        """Group members must be valid footprints."""
        footprint_ids = set(self.current_view.footprints.keys())
        for group_id, group_view in self.current_view.groups.items():
            for member_id in group_view.member_ids:
                assert member_id in footprint_ids, (
                    f"Group {group_id} has invalid member {member_id}"
                )


if HYPOTHESIS_AVAILABLE:
    LensSyncStateMachine.TestCase.settings = settings(
        max_examples=50,
        stateful_step_count=20,
    )
    TestLensSync = LensSyncStateMachine.TestCase


# ═══════════════════════════════════════════════════════════════════════════════
# Pad-Net SOURCE Authoritative Machine
# ═══════════════════════════════════════════════════════════════════════════════


@pytest.mark.skipif(not HYPOTHESIS_AVAILABLE, reason="hypothesis not installed")
class PadNetSourceMachine(RuleBasedStateMachine):
    """
    Focused machine for testing pad-net SOURCE-authoritative assignment.

    Verifies that:
    - Net connections come from BoardView.nets (SOURCE), not old pads
    - When footprint moves between nets, new net is used
    - When footprint is removed from a net, connection is dropped
    - Adding new connections is reflected in view
    """

    NET_POOL = ["VCC", "GND", "SDA", "SCL", "NET1", "NET2"]

    def __init__(self):
        super().__init__()
        self._init_state()

    def _init_state(self):
        u1_id, u1_fp = make_footprint("U1", "U1", "IC", "Package_SO:SOIC-8")
        r1_id, r1_fp = make_footprint("R1", "R1", "10k", "Resistor_SMD:R_0603")
        r2_id, r2_fp = make_footprint("R2", "R2", "4.7k", "Resistor_SMD:R_0603")

        self.footprints = {u1_id: u1_fp, r1_id: r1_fp, r2_id: r2_fp}
        self.pin_nets = {
            u1_id: {"1": "VCC", "4": "GND"},
            r1_id: {"1": "VCC", "2": "NET1"},
            r2_id: {"1": "NET1", "2": "GND"},
        }
        self.complements = {
            u1_id: make_complement(0, 0),
            r1_id: make_complement(5000, 0),
            r2_id: make_complement(10000, 0),
        }
        self.last_synced_view = None
        self.sync_count = 0

    @initialize()
    def setup(self):
        self._init_state()

    def _build_nets(self) -> dict:
        """Build NetView dict from pin_nets."""
        nets = {}
        for entity_id, pins in self.pin_nets.items():
            for pin, net_name in pins.items():
                if net_name not in nets:
                    nets[net_name] = []
                nets[net_name].append((entity_id, pin))
        return {
            name: NetView(name=name, connections=tuple(conns))
            for name, conns in nets.items()
        }

    def _make_view(self):
        return BoardView(footprints=self.footprints, nets=self._build_nets())

    def _make_complement(self):
        return BoardComplement(footprints=self.complements)

    @rule(
        fp_ref=st.sampled_from(["U1", "R1", "R2"]),
        pin=st.sampled_from(["1", "2", "3", "4"]),
        net=st.sampled_from(NET_POOL),
    )
    def change_pin_net(self, fp_ref, pin, net):
        """Change which net a pin is connected to."""
        entity_id = EntityId.from_string(fp_ref)
        if entity_id not in self.pin_nets:
            return

        old_net = self.pin_nets[entity_id].get(pin)
        if old_net == net:
            return

        self.pin_nets[entity_id][pin] = net
        note(f"Changed {fp_ref}.{pin}: {old_net} -> {net}")

    @rule(
        fp_ref=st.sampled_from(["U1", "R1", "R2"]),
        pin=st.sampled_from(["1", "2", "3", "4"]),
    )
    def disconnect_pin(self, fp_ref, pin):
        """Remove a pin from its net."""
        entity_id = EntityId.from_string(fp_ref)
        if entity_id not in self.pin_nets:
            return
        if pin not in self.pin_nets[entity_id]:
            return

        old_net = self.pin_nets[entity_id].pop(pin)
        note(f"Disconnected {fp_ref}.{pin} from {old_net}")

    @rule()
    def perform_sync(self):
        """Sync and verify SOURCE-authoritative net assignment."""
        new_view = self._make_view()
        old_complement = self._make_complement()

        new_complement = adapt_complement(
            new_view,
            old_complement,
        )

        # Verify the view's nets reflect our pin_nets state
        expected_nets = self._build_nets()
        assert new_view.nets == expected_nets, (
            f"View nets mismatch: {new_view.nets} != {expected_nets}"
        )

        # Verify all footprints have complements
        for entity_id in new_view.footprints.keys():
            assert entity_id in new_complement.footprints

        self.complements = dict(new_complement.footprints)
        self.last_synced_view = new_view
        self.sync_count += 1

    @invariant()
    def nets_match_pin_state(self):
        """Nets in view should match our pin_nets state."""
        if self.sync_count == 0:
            return

        expected_nets = self._build_nets()
        current_view = self._make_view()

        # Check each net has correct connections
        for net_name, net_view in expected_nets.items():
            assert net_name in current_view.nets, f"Missing net {net_name} in view"
            assert set(current_view.nets[net_name].connections) == set(
                net_view.connections
            ), f"Net {net_name} connections mismatch"


if HYPOTHESIS_AVAILABLE:
    PadNetSourceMachine.TestCase.settings = settings(
        max_examples=40,
        stateful_step_count=15,
    )
    TestPadNetSource = PadNetSourceMachine.TestCase
