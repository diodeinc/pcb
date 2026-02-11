import pytest

from .. import kicad_adapter
from ..changeset import SyncChangeset
from ..oplog import OpLog
from ..types import BoardComplement, BoardView, EntityId, FootprintView, GroupView


def test_group_only_add_does_not_trigger_hierplace(monkeypatch: pytest.MonkeyPatch):
    """Regression: restoring a missing KiCad group must not move existing footprints.

    If a group wrapper is deleted but its child footprints already exist on the board,
    the sync will report GR_ADD but should not run HierPlace/fragment placement.
    """

    gid = EntityId.from_string("McuAndEstop.STM")
    fid = EntityId.from_string("McuAndEstop.STM.U1", fpid="Pkg:Footprint")

    view = BoardView(
        footprints={
            fid: FootprintView(
                entity_id=fid,
                reference="U1",
                value="MCU",
                fpid=fid.fpid,
            )
        },
        groups={
            gid: GroupView(
                entity_id=gid,
                member_ids=(fid,),
                layout_path="some/fragment/layout/path",
            )
        },
        nets={},
    )

    changeset = SyncChangeset(
        view=view,
        complement=BoardComplement(),
        added_footprints=set(),
        removed_footprints={},
        added_groups={gid},
        removed_groups=set(),
    )

    def _boom(*_args, **_kwargs):
        raise AssertionError(
            "_build_fragment_plan must not be called for group-only adds"
        )

    monkeypatch.setattr(kicad_adapter, "_build_fragment_plan", _boom)

    oplog = OpLog()
    placed_count = kicad_adapter._run_hierarchical_placement(
        changeset=changeset,
        board_view=view,
        fps_by_entity_id={},
        groups_by_name={},
        kicad_board=None,
        pcbnew=None,
        oplog=oplog,
        package_roots={},
    )

    assert placed_count == 0
    assert [e.kind for e in oplog.events if e.kind == "PLACE_GR"] == []


def test_fragment_plan_ignores_groups_with_existing_footprints(
    monkeypatch: pytest.MonkeyPatch,
):
    """Regression: fragment placement must not move existing footprints.

    If a fragment group is re-created (GR_ADD) but its child footprints already
    exist on the board, we must not treat the group as an authoritative fragment
    (otherwise a PLACE_GR would translate existing footprints).
    """

    gid = EntityId.from_string("McuAndEstop.STM")
    existing_fid = EntityId.from_string("McuAndEstop.STM.U1", fpid="Pkg:Existing")
    other_new_fid = EntityId.from_string("Other.NewThing", fpid="Pkg:New")

    view = BoardView(
        footprints={
            existing_fid: FootprintView(
                entity_id=existing_fid,
                reference="U1",
                value="MCU",
                fpid=existing_fid.fpid,
            ),
            other_new_fid: FootprintView(
                entity_id=other_new_fid,
                reference="X1",
                value="NEW",
                fpid=other_new_fid.fpid,
            ),
        },
        groups={
            gid: GroupView(
                entity_id=gid,
                member_ids=(existing_fid,),
                layout_path="some/fragment/layout/path",
            )
        },
        nets={},
    )

    changeset = SyncChangeset(
        view=view,
        complement=BoardComplement(),
        added_footprints={other_new_fid},
        removed_footprints={},
        added_groups={gid},
        removed_groups=set(),
    )

    def _boom(*_args, **_kwargs):
        raise AssertionError("Fragment loader must not run for group repairs")

    monkeypatch.setattr(kicad_adapter, "load_layout_fragment_with_footprints", _boom)

    plan = kicad_adapter._build_fragment_plan(
        changeset=changeset,
        board_view=view,
        pcbnew=None,
        oplog=OpLog(),
        placeable_footprints={other_new_fid},
        package_roots={},
    )

    assert plan.loaded == {}
