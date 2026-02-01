"""
Tests for NotConnected net handling in lens sync.

NotConnected nets are special: they represent pads that should be
electrically isolated. Each NotConnected pad gets:
1. A unique unconnected-(...) net name in KiCad
2. The no_connect pintype

This is handled by:
- lens.get(): Populates not_connected_pads instead of nets
- kicad_adapter: Creates unique nets and sets pintype
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


class MockProperty:
    """Mock property for parts."""

    def __init__(self, name: str, value: str):
        self.name = name
        self.value = value


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

    def test_not_connected_net_populates_not_connected_pads(self):
        """NotConnected nets should populate not_connected_pads, not nets."""
        # Create a part with two pads
        part = MockPart(
            path="Power.C1",
            ref="C1",
            footprint="Capacitor_SMD:C_0603",
            value="10uF",
        )

        # Create a NotConnected net connecting both pads
        nc_net = MockNet(
            name="NC_TEST",
            nodes=[("C1", "1", "1"), ("C1", "2", "2")],
            kind="NotConnected",
        )

        netlist = MockNetlist(parts=[part], nets=[nc_net])

        view = get(netlist)

        # NotConnected net should NOT be in view.nets
        assert "NC_TEST" not in view.nets

        # Both pads should be in not_connected_pads
        assert len(view.not_connected_pads) == 2

        # Find the entity_id for C1
        c1_ids = [eid for eid in view.footprints if eid.path.name == "C1"]
        assert len(c1_ids) == 1
        c1_id = c1_ids[0]

        assert (c1_id, "1") in view.not_connected_pads
        assert (c1_id, "2") in view.not_connected_pads

    def test_regular_net_not_in_not_connected_pads(self):
        """Regular nets should populate nets, not not_connected_pads."""
        part = MockPart(
            path="Power.C1",
            ref="C1",
            footprint="Capacitor_SMD:C_0603",
            value="10uF",
        )

        # Regular net (kind="Net" or "Power" etc)
        vcc_net = MockNet(
            name="VCC",
            nodes=[("C1", "1", "1")],
            kind="Power",
        )

        netlist = MockNetlist(parts=[part], nets=[vcc_net])

        view = get(netlist)

        # Regular net should be in view.nets
        assert "VCC" in view.nets
        assert view.nets["VCC"].kind == "Power"

        # Should NOT be in not_connected_pads
        assert len(view.not_connected_pads) == 0

    def test_mixed_nets_separated_correctly(self):
        """Mix of regular and NotConnected nets should be separated."""
        part = MockPart(
            path="Power.U1",
            ref="U1",
            footprint="Package_SO:SOIC-8",
            value="LM358",
        )

        # Regular nets
        vcc_net = MockNet(name="VCC", nodes=[("U1", "8", "VCC")], kind="Power")
        gnd_net = MockNet(name="GND", nodes=[("U1", "4", "GND")], kind="Ground")

        # NotConnected net
        nc_net = MockNet(name="NC_PIN", nodes=[("U1", "3", "NC")], kind="NotConnected")

        netlist = MockNetlist(parts=[part], nets=[vcc_net, gnd_net, nc_net])

        view = get(netlist)

        # Regular nets in view.nets
        assert "VCC" in view.nets
        assert "GND" in view.nets
        assert len(view.nets) == 2

        # NotConnected in not_connected_pads
        assert len(view.not_connected_pads) == 1

        u1_ids = [eid for eid in view.footprints if eid.path.name == "U1"]
        assert len(u1_ids) == 1
        u1_id = u1_ids[0]

        assert (u1_id, "3") in view.not_connected_pads

    def test_not_connected_without_kind_defaults_to_net(self):
        """Nets without explicit kind should default to 'Net' (not NotConnected)."""
        part = MockPart(
            path="Power.R1",
            ref="R1",
            footprint="Resistor_SMD:R_0603",
            value="10k",
        )

        # Net without kind attribute
        net = MockNet(name="SIGNAL", nodes=[("R1", "1", "1")])
        del net.kind  # Remove kind attribute

        netlist = MockNetlist(parts=[part], nets=[net])

        view = get(netlist)

        # Should be treated as regular net
        assert "SIGNAL" in view.nets
        assert len(view.not_connected_pads) == 0

    def test_empty_not_connected_net(self):
        """NotConnected net with no connections should not add to not_connected_pads."""
        part = MockPart(
            path="Power.C1",
            ref="C1",
            footprint="Capacitor_SMD:C_0603",
        )

        # NotConnected net with no nodes (edge case)
        nc_net = MockNet(name="NC_EMPTY", nodes=[], kind="NotConnected")

        netlist = MockNetlist(parts=[part], nets=[nc_net])

        view = get(netlist)

        assert "NC_EMPTY" not in view.nets
        assert len(view.not_connected_pads) == 0
