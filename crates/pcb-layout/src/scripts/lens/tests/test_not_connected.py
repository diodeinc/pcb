"""
Tests for NotConnected net handling in lens sync.

NotConnected nets are special: they represent pads that should be
electrically isolated. Each NotConnected pad gets:
1. A unique unconnected-(...) net name in KiCad
2. The no_connect pintype

This is handled by:
- lens.get(): Explodes NotConnected nets into unique per-pad nets
- kicad_adapter: Sets pintype for NotConnected kind nets
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

    def test_not_connected_net_exploded_to_unique_nets(self):
        """NotConnected net with multiple pads becomes multiple unique nets."""
        part = MockPart(
            path="Power.C1",
            ref="C1",
            footprint="Capacitor_SMD:C_0603",
            value="10uF",
        )

        # NotConnected net connecting both pads
        nc_net = MockNet(
            name="NC_TEST",
            nodes=[("C1", "1", "1"), ("C1", "2", "2")],
            kind="NotConnected",
        )

        netlist = MockNetlist(parts=[part], nets=[nc_net])
        view = get(netlist)

        # Original net name should NOT exist
        assert "NC_TEST" not in view.nets

        # Should have two unique unconnected nets
        nc_nets = [n for n in view.nets.keys() if n.startswith("unconnected-")]
        assert len(nc_nets) == 2

        # Each should have kind="NotConnected" and single connection
        for net_name in nc_nets:
            net = view.nets[net_name]
            assert net.kind == "NotConnected"
            assert len(net.connections) == 1

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

        # NC net exploded to unique name
        assert "NC_PIN" not in view.nets
        nc_nets = [n for n in view.nets.keys() if n.startswith("unconnected-")]
        assert len(nc_nets) == 1
        assert view.nets[nc_nets[0]].kind == "NotConnected"

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
        nc_nets = [n for n in view.nets.keys() if n.startswith("unconnected-")]
        assert len(nc_nets) == 0

    def test_empty_not_connected_net(self):
        """NotConnected net with no connections creates no nets."""
        part = MockPart(
            path="Power.C1",
            ref="C1",
            footprint="Capacitor_SMD:C_0603",
        )

        nc_net = MockNet(name="NC_EMPTY", nodes=[], kind="NotConnected")

        netlist = MockNetlist(parts=[part], nets=[nc_net])
        view = get(netlist)

        assert "NC_EMPTY" not in view.nets
        nc_nets = [n for n in view.nets.keys() if n.startswith("unconnected-")]
        assert len(nc_nets) == 0
