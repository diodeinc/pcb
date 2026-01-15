"""
Tests for SOURCE-authoritative pad-net assignment.

The _build_pad_net_map() function looks up pad-net assignments from
BoardView.nets (SOURCE) rather than copying from existing pads (DEST).
This ensures the netlist is always the source of truth for connectivity.
"""

from unittest.mock import Mock

from ..types import (
    EntityId,
    FootprintView,
    BoardView,
    NetView,
)
from ..kicad_adapter import _build_pad_net_map


class MockNetInfo:
    """Mock KiCad NETINFO object."""

    def __init__(self, name: str, code: int):
        self.name = name
        self.code = code

    def GetNetname(self) -> str:
        return self.name

    def GetNetCode(self) -> int:
        return self.code


def make_mock_board(nets: dict[str, MockNetInfo]) -> Mock:
    """Create a mock KiCad board with FindNet support."""
    board = Mock()
    board.FindNet = lambda name: nets.get(name)
    return board


class TestBuildPadNetMap:
    """Tests for _build_pad_net_map() function."""

    def test_single_pin_single_net(self):
        """Single pin connected to single net."""
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
            nets={
                "VCC": NetView(
                    name="VCC",
                    connections=((entity_id, "1"),),
                ),
            },
        )

        net_info = MockNetInfo("VCC", 1)
        board = make_mock_board({"VCC": net_info})

        result = _build_pad_net_map(entity_id, view, board)

        assert "1" in result
        assert result["1"] == net_info

    def test_multiple_pins_multiple_nets(self):
        """Multiple pins connected to different nets."""
        entity_id = EntityId.from_string("Power.U1")

        view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="U1",
                    value="LM358",
                    fpid="SOIC-8",
                )
            },
            nets={
                "VCC": NetView(
                    name="VCC",
                    connections=((entity_id, "8"),),
                ),
                "GND": NetView(
                    name="GND",
                    connections=((entity_id, "4"),),
                ),
                "IN+": NetView(
                    name="IN+",
                    connections=((entity_id, "3"),),
                ),
            },
        )

        vcc_info = MockNetInfo("VCC", 1)
        gnd_info = MockNetInfo("GND", 2)
        in_info = MockNetInfo("IN+", 3)
        board = make_mock_board(
            {
                "VCC": vcc_info,
                "GND": gnd_info,
                "IN+": in_info,
            }
        )

        result = _build_pad_net_map(entity_id, view, board)

        assert result["8"] == vcc_info
        assert result["4"] == gnd_info
        assert result["3"] == in_info
        assert len(result) == 3

    def test_unconnected_pins_not_in_map(self):
        """Pins not in any net are not in the result map."""
        entity_id = EntityId.from_string("Power.R1")

        view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="R1",
                    value="10k",
                    fpid="R_0603",
                )
            },
            nets={
                "VCC": NetView(
                    name="VCC",
                    connections=((entity_id, "1"),),  # Only pin 1 connected
                ),
            },
        )

        vcc_info = MockNetInfo("VCC", 1)
        board = make_mock_board({"VCC": vcc_info})

        result = _build_pad_net_map(entity_id, view, board)

        assert "1" in result
        assert "2" not in result  # Pin 2 not connected

    def test_net_not_found_in_board(self):
        """Net exists in view but not in KiCad board (not added yet)."""
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
            nets={
                "NEW_NET": NetView(
                    name="NEW_NET",
                    connections=((entity_id, "1"),),
                ),
            },
        )

        # Board doesn't have NEW_NET yet
        board = make_mock_board({})

        result = _build_pad_net_map(entity_id, view, board)

        # Pin not in map because net not found
        assert "1" not in result

    def test_different_footprint_not_included(self):
        """Connections to other footprints are not included."""
        entity_id = EntityId.from_string("Power.C1")
        other_id = EntityId.from_string("Power.C2")

        view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="C1",
                    value="10uF",
                    fpid="C_0603",
                ),
                other_id: FootprintView(
                    entity_id=other_id,
                    reference="C2",
                    value="100nF",
                    fpid="C_0603",
                ),
            },
            nets={
                "VCC": NetView(
                    name="VCC",
                    connections=(
                        (entity_id, "1"),
                        (other_id, "1"),  # C2's pin 1 also on VCC
                    ),
                ),
            },
        )

        vcc_info = MockNetInfo("VCC", 1)
        board = make_mock_board({"VCC": vcc_info})

        # Query for C1 only
        result = _build_pad_net_map(entity_id, view, board)

        assert "1" in result
        assert result["1"] == vcc_info

    def test_empty_view_returns_empty_map(self):
        """Empty view returns empty map."""
        entity_id = EntityId.from_string("Power.C1")

        view = BoardView()
        board = make_mock_board({})

        result = _build_pad_net_map(entity_id, view, board)

        assert result == {}

    def test_no_nets_returns_empty_map(self):
        """View with footprints but no nets returns empty map."""
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
            nets={},
        )

        board = make_mock_board({})

        result = _build_pad_net_map(entity_id, view, board)

        assert result == {}

    def test_same_pin_in_multiple_nets_last_wins(self):
        """If same pin appears in multiple nets (invalid), last one wins.

        This is an edge case that shouldn't happen in valid netlists,
        but we test the behavior anyway.
        """
        entity_id = EntityId.from_string("Power.C1")

        # Invalid: pin 1 in two nets
        view = BoardView(
            footprints={
                entity_id: FootprintView(
                    entity_id=entity_id,
                    reference="C1",
                    value="10uF",
                    fpid="C_0603",
                )
            },
            nets={
                "NET_A": NetView(
                    name="NET_A",
                    connections=((entity_id, "1"),),
                ),
                "NET_B": NetView(
                    name="NET_B",
                    connections=((entity_id, "1"),),  # Same pin!
                ),
            },
        )

        net_a = MockNetInfo("NET_A", 1)
        net_b = MockNetInfo("NET_B", 2)
        board = make_mock_board({"NET_A": net_a, "NET_B": net_b})

        result = _build_pad_net_map(entity_id, view, board)

        # One of them wins (dict iteration order)
        assert "1" in result
        assert result["1"] in (net_a, net_b)
