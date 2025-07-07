import { SchematicLayoutEngine } from "../LayoutEngine";
import { type Netlist, InstanceKind, NetKind } from "../types/NetlistTypes";
import { GraphImageRenderer } from "./GraphImageRenderer";
import * as fs from "fs";
import * as path from "path";

// Helper to build test netlists
class NetlistBuilder {
  private namespace: string;
  private instances: Record<string, any> = {};
  private nets: Record<string, any> = {};

  constructor(namespace = "test") {
    this.namespace = namespace;
  }

  private ref(path: string): string {
    return `${this.namespace}:${path}`;
  }

  addModule(name: string, parentPath: string | null = null): string {
    const path = parentPath ? `${parentPath}.${name}` : name;
    const ref = this.ref(path);
    this.instances[ref] = {
      type_ref: { source_path: this.namespace, module_name: name },
      kind: InstanceKind.MODULE,
      attributes: {},
      children: {},
    };
    if (parentPath) {
      const parentRef = this.ref(parentPath);
      if (!this.instances[parentRef]) {
        throw new Error(`Parent ${parentRef} not found`);
      }
      this.instances[parentRef].children[name] = ref;
    }
    return ref;
  }

  addComponent(
    parentPath: string,
    name: string,
    attributes: Record<string, any> = {}
  ): string {
    const path = `${parentPath}.${name}`;
    const ref = this.ref(path);
    this.instances[ref] = {
      type_ref: {
        source_path: this.namespace,
        module_name: attributes.module_name || name,
      },
      kind: InstanceKind.COMPONENT,
      attributes,
      children: {},
    };

    const parentRef = this.ref(parentPath);
    this.instances[parentRef].children[name] = ref;

    // Add ports for passive components
    if (
      attributes.type === "resistor" ||
      attributes.type === "capacitor" ||
      attributes.type === "inductor"
    ) {
      this.addPort(path, "P1");
      this.addPort(path, "P2");
    }

    return ref;
  }

  addPort(parentPath: string, name: string): string {
    const path = `${parentPath}.${name}`;
    const ref = this.ref(path);
    this.instances[ref] = {
      type_ref: { source_path: this.namespace, module_name: "Port" },
      kind: InstanceKind.PORT,
      attributes: {},
      children: {},
    };

    const parentRef = this.ref(parentPath);
    this.instances[parentRef].children[name] = ref;

    return ref;
  }

  connect(
    netName: string,
    portPaths: string[],
    kind: NetKind = NetKind.NORMAL
  ) {
    if (!this.nets[netName]) {
      this.nets[netName] = { kind, ports: [] };
    }
    this.nets[netName].ports.push(
      ...portPaths.map((p) =>
        p.startsWith(`${this.namespace}:`) ? p : this.ref(p)
      )
    );
  }

  build(): Netlist {
    return { instances: this.instances, nets: this.nets } as Netlist;
  }
}

// Mock ELK to provide deterministic layout for visual snapshots
jest.mock("elkjs/lib/elk-api.js", () => ({
  __esModule: true,
  default: class ELKStub {
    async layout(graph: any) {
      // Simple layout algorithm for testing
      const layoutGraph = JSON.parse(JSON.stringify(graph));

      // Layout nodes in a grid
      if (layoutGraph.children) {
        let x = 50;
        let y = 50;
        const spacing = 150;

        layoutGraph.children.forEach((node: any, index: number) => {
          node.x = x;
          node.y = y;

          // Set default sizes if not present
          if (!node.width) node.width = 100;
          if (!node.height) node.height = 60;

          // Move to next position
          x += spacing;
          if ((index + 1) % 3 === 0) {
            x = 50;
            y += spacing;
          }
        });
      }

      return layoutGraph;
    }
  },
}));

describe("LayoutEngine Visual Snapshots", () => {
  const snapshotDir = path.join(__dirname, "__snapshots__", "visual");

  beforeAll(() => {
    // Create snapshot directory if it doesn't exist
    if (!fs.existsSync(snapshotDir)) {
      fs.mkdirSync(snapshotDir, { recursive: true });
    }
  });

  test("simple resistor divider circuit", async () => {
    // Build a simple voltage divider circuit
    const builder = new NetlistBuilder("test");
    builder.addModule("Board");
    builder.addPort("Board", "VIN");
    builder.addPort("Board", "VOUT");
    builder.addPort("Board", "GND");
    builder.addComponent("Board", "R1", { type: "resistor", value: "10k" });
    builder.addComponent("Board", "R2", { type: "resistor", value: "10k" });

    // Connect: VIN -> R1 -> VOUT -> R2 -> GND
    builder.connect("net_vin", ["Board.VIN", "Board.R1.P1"]);
    builder.connect("net_vout", ["Board.R1.P2", "Board.VOUT", "Board.R2.P1"]);
    builder.connect("net_gnd", ["Board.R2.P2", "Board.GND"], NetKind.GROUND);

    const netlist = builder.build();
    const engine = new SchematicLayoutEngine(netlist);
    const layoutResult = await engine.layout("test:Board");

    // Render to image
    const renderer = new GraphImageRenderer(600, 400);
    const imageBuffer = renderer.render(layoutResult);

    // Save the image for visual inspection
    const imagePath = path.join(snapshotDir, "resistor-divider.png");
    fs.writeFileSync(imagePath, imageBuffer);

    // For snapshot testing, we'll check the buffer exists and has content
    expect(imageBuffer).toBeDefined();
    expect(imageBuffer.length).toBeGreaterThan(0);

    // Also snapshot the graph structure for regression testing
    expect(layoutResult).toMatchSnapshot();
  });

  test("RC filter circuit", async () => {
    // Build an RC low-pass filter
    const builder = new NetlistBuilder("test");
    builder.addModule("Filter");
    builder.addPort("Filter", "IN");
    builder.addPort("Filter", "OUT");
    builder.addPort("Filter", "GND");
    builder.addComponent("Filter", "R1", { type: "resistor", value: "1k" });
    builder.addComponent("Filter", "C1", { type: "capacitor", value: "100nF" });

    // Connect: IN -> R1 -> OUT, OUT -> C1 -> GND
    builder.connect("net_in", ["Filter.IN", "Filter.R1.P1"]);
    builder.connect("net_out", ["Filter.R1.P2", "Filter.OUT", "Filter.C1.P1"]);
    builder.connect("net_gnd", ["Filter.C1.P2", "Filter.GND"], NetKind.GROUND);

    const netlist = builder.build();
    const engine = new SchematicLayoutEngine(netlist);
    const layoutResult = await engine.layout("test:Filter");

    // Render to image
    const renderer = new GraphImageRenderer(600, 400);
    const imageBuffer = renderer.render(layoutResult);

    // Save the image
    const imagePath = path.join(snapshotDir, "rc-filter.png");
    fs.writeFileSync(imagePath, imageBuffer);

    expect(imageBuffer).toBeDefined();
    expect(imageBuffer.length).toBeGreaterThan(0);
    expect(layoutResult).toMatchSnapshot();
  });

  test("hierarchical module with sub-modules", async () => {
    // Build a hierarchical design
    const builder = new NetlistBuilder("test");

    // Top level
    builder.addModule("System");
    builder.addPort("System", "PWR");
    builder.addPort("System", "GND");

    // Power supply module
    builder.addModule("PowerSupply", "System");
    builder.addPort("System.PowerSupply", "VIN");
    builder.addPort("System.PowerSupply", "VOUT");
    builder.addPort("System.PowerSupply", "GND");
    builder.addComponent("System.PowerSupply", "C1", {
      type: "capacitor",
      value: "10uF",
    });
    builder.addComponent("System.PowerSupply", "C2", {
      type: "capacitor",
      value: "100nF",
    });

    // Sensor module
    builder.addModule("Sensor", "System");
    builder.addPort("System.Sensor", "VCC");
    builder.addPort("System.Sensor", "GND");
    builder.addPort("System.Sensor", "OUT");
    builder.addComponent("System.Sensor", "R1", {
      type: "resistor",
      value: "4.7k",
    });

    // Internal connections
    builder.connect("pwr_in", ["System.PWR", "System.PowerSupply.VIN"]);
    builder.connect("pwr_out", [
      "System.PowerSupply.VOUT",
      "System.Sensor.VCC",
    ]);
    builder.connect(
      "gnd",
      [
        "System.GND",
        "System.PowerSupply.GND",
        "System.Sensor.GND",
        "System.PowerSupply.C1.P2",
        "System.PowerSupply.C2.P2",
      ],
      NetKind.GROUND
    );

    // PowerSupply internal
    builder.connect("ps_vin", [
      "System.PowerSupply.VIN",
      "System.PowerSupply.C1.P1",
    ]);
    builder.connect("ps_vout", [
      "System.PowerSupply.VOUT",
      "System.PowerSupply.C2.P1",
    ]);

    // Sensor internal
    builder.connect("sensor_pull", [
      "System.Sensor.VCC",
      "System.Sensor.R1.P1",
    ]);
    builder.connect("sensor_out", ["System.Sensor.R1.P2", "System.Sensor.OUT"]);

    const netlist = builder.build();
    const engine = new SchematicLayoutEngine(netlist);
    const layoutResult = await engine.layout("test:System");

    // Render to image
    const renderer = new GraphImageRenderer(800, 600);
    const imageBuffer = renderer.render(layoutResult);

    // Save the image
    const imagePath = path.join(snapshotDir, "hierarchical-system.png");
    fs.writeFileSync(imagePath, imageBuffer);

    expect(imageBuffer).toBeDefined();
    expect(imageBuffer.length).toBeGreaterThan(0);
    expect(layoutResult).toMatchSnapshot();
  });
});
