"""
Tests for NotConnected net handling in lens sync.

NotConnected nets are treated as regular nets for connectivity.
Any "no connect" behavior is expressed via pad pin type in the adapter.
"""

from ..lens import get


class MockNet:
    """Mock netlist net object."""

    def __init__(self, name: str, nodes: list, kind: str = "Net"):
        self.name = name
        self.nodes = nodes  # List of (ref, pin_num, pin_name) tuples
        self.kind = kind


class MockSheetPath:
    """Mock sheetpath for parts."""

    def __init__(self, names: str):
        self.names = names


class MockPart:
    """Mock netlist part object."""

    def __init__(self, path: str, ref: str, footprint: str, value: str = ""):
        self.sheetpath = MockSheetPath(path)
        self.ref = ref
        self.footprint = footprint
        self.value = value
        self.properties = []


class MockNetlist:
    """Mock netlist object for testing get()."""

    def __init__(self, parts: list = None, nets: list = None):
        self.parts = parts or []
        self.nets = nets or []


class TestNotConnectedInGet:
    """Tests for NotConnected handling in lens.get()."""

    def test_not_connected_net_not_exploded(self):
        """NotConnected net is kept as-is (no connectivity changes)."""
        part = MockPart(
            path="Power.C1",
            ref="C1",
            footprint="Capacitor_SMD:C_0603",
            value="10uF",
        )

        # NotConnected net connecting both pads
        nc_net = MockNet(
            name="NC_TEST",
            # Two different pads fanning out from the same logical pin/port.
            nodes=[("C1", "1", "NC"), ("C1", "2", "NC")],
            kind="NotConnected",
        )

        netlist = MockNetlist(parts=[part], nets=[nc_net])
        view = get(netlist)

        assert "NC_TEST" in view.nets
        assert view.nets["NC_TEST"].kind == "NotConnected"
        assert len(view.nets["NC_TEST"].connections) == 2
        assert view.nets["NC_TEST"].logical_ports == (("C1", "NC"),)

    def test_regular_net_unchanged(self):
        """Regular nets are added normally."""
        part = MockPart(
            path="Power.C1",
            ref="C1",
            footprint="Capacitor_SMD:C_0603",
            value="10uF",
        )

        vcc_net = MockNet(
            name="VCC",
            nodes=[("C1", "1", "1")],
            kind="Power",
        )

        netlist = MockNetlist(parts=[part], nets=[vcc_net])
        view = get(netlist)

        assert "VCC" in view.nets
        assert view.nets["VCC"].kind == "Power"
        assert len(view.nets["VCC"].connections) == 1

    def test_mixed_nets_handled_correctly(self):
        """Mix of regular and NotConnected nets."""
        part = MockPart(
            path="Power.U1",
            ref="U1",
            footprint="Package_SO:SOIC-8",
            value="LM358",
        )

        vcc_net = MockNet(name="VCC", nodes=[("U1", "8", "VCC")], kind="Power")
        nc_net = MockNet(name="NC_PIN", nodes=[("U1", "3", "NC")], kind="NotConnected")

        netlist = MockNetlist(parts=[part], nets=[vcc_net, nc_net])
        view = get(netlist)

        # Regular net exists
        assert "VCC" in view.nets

        # NC net exists with original name
        assert "NC_PIN" in view.nets
        assert view.nets["NC_PIN"].kind == "NotConnected"

    def test_not_connected_without_kind_defaults_to_net(self):
        """Nets without explicit kind default to 'Net'."""
        part = MockPart(
            path="Power.R1",
            ref="R1",
            footprint="Resistor_SMD:R_0603",
            value="10k",
        )

        net = MockNet(name="SIGNAL", nodes=[("R1", "1", "1")])
        del net.kind

        netlist = MockNetlist(parts=[part], nets=[net])
        view = get(netlist)

        assert "SIGNAL" in view.nets
        assert view.nets["SIGNAL"].kind == "Net"

    def test_empty_not_connected_net(self):
        """NotConnected net with no connections is represented as an empty net."""
        part = MockPart(
            path="Power.C1",
            ref="C1",
            footprint="Capacitor_SMD:C_0603",
        )

        nc_net = MockNet(name="NC_EMPTY", nodes=[], kind="NotConnected")

        netlist = MockNetlist(parts=[part], nets=[nc_net])
        view = get(netlist)

        assert "NC_EMPTY" in view.nets
        assert view.nets["NC_EMPTY"].kind == "NotConnected"
        assert len(view.nets["NC_EMPTY"].connections) == 0
