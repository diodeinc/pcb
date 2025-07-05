import { SchematicRenderer, NodeType } from "../renderer";
import { Netlist, InstanceKind, NetKind } from "../types/NetlistTypes";

// Mock out heavy native & web-worker dependencies that aren't needed for unit testing.
// 1.   node-canvas – only used for simple text-measurement calls.  We replace it with a
//      stub that returns deterministic widths so layout maths keep working while under test.
jest.mock("canvas", () => ({
  createCanvas: () => ({
    getContext: () => ({
      // eslint-disable-next-line @typescript-eslint/no-unused-vars
      font: "",
      // Basic monospace approximation – each char = 7 px wide
      measureText: (text: string) => ({ width: text.length * 7 }),
    }),
  }),
}));

// 2.   elkjs – the actual ELK layout engine spins up a WebWorker which is unnecessary in unit
//      tests.  Here we stub it so `layout` just returns the graph that it was given.
jest.mock("elkjs/lib/elk-api.js", () => ({
  __esModule: true,
  default: class ELKStub {
    // Simply echo back the input graph so downstream code can keep running.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any,@typescript-eslint/no-unused-vars
    layout(graph: any, _options?: any) {
      return Promise.resolve(graph);
    }
  },
}));

// ---------------------------------------------------------------------------
// Test NetlistBuilder – utility for quickly constructing netlists
// ---------------------------------------------------------------------------

class NetlistBuilder {
  private namespace: string;
  private instances: Record<string, any> = {};
  private nets: Record<string, any> = {};

  constructor(namespace = "test") {
    this.namespace = namespace;
  }

  /** Convert a dot-separated path (e.g. "Board.IF.p1") into a fully-qualified
   *  instance reference with the namespace prefix – e.g. "test:Board.IF.p1". */
  private ref(path: string): string {
    return `${this.namespace}:${path}`;
  }

  /** Ensure an instance exists at a given ref, returning it. */
  private getInstance(ref: string) {
    const inst = this.instances[ref];
    if (!inst) throw new Error(`Instance ${ref} has not been defined`);
    return inst;
  }

  /** Helper used internally to register a child reference on its parent. */
  private addChild(
    parentRef: string | null,
    childName: string,
    childRef: string
  ) {
    if (!parentRef) return; // root node has no parent
    const parentInst = this.getInstance(parentRef);
    parentInst.children = parentInst.children || {};
    parentInst.children[childName] = childRef;
  }

  /** Add a module.  Returns the fully-qualified reference string. */
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
      this.addChild(this.ref(parentPath), name, ref);
    }
    return ref;
  }

  /** Add a plain Port under a module or interface. */
  addPort(parentPath: string, name: string): string {
    const path = `${parentPath}.${name}`;
    const ref = this.ref(path);
    this.instances[ref] = {
      type_ref: { source_path: this.namespace, module_name: "Port" },
      kind: InstanceKind.PORT,
      attributes: {},
      children: {},
    };
    this.addChild(this.ref(parentPath), name, ref);
    return ref;
  }

  /** Add a component (e.g. resistor).  By default we only need to pass a `type` attr */
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
    this.addChild(this.ref(parentPath), name, ref);
    return ref;
  }

  /** Add an Interface and its given sub-ports. */
  addInterface(parentPath: string, name: string, portNames: string[]): string {
    const path = `${parentPath}.${name}`;
    const ref = this.ref(path);
    this.instances[ref] = {
      type_ref: {
        source_path: this.namespace,
        module_name: "Iface" + Math.random(),
      },
      kind: InstanceKind.INTERFACE,
      attributes: {},
      children: {},
    };
    this.addChild(this.ref(parentPath), name, ref);

    for (const p of portNames) {
      this.addPort(path, p);
    }

    return ref;
  }

  /** Connect a set of port references (dot-separated paths under namespace) to a net. */
  connect(netName: string, portPaths: string[]) {
    if (!this.nets[netName]) {
      this.nets[netName] = { kind: NetKind.NORMAL, ports: [] };
    }
    this.nets[netName].ports.push(...portPaths.map((p) => this.ref(p)));
  }

  build(): Netlist {
    return { instances: this.instances, nets: this.nets } as Netlist;
  }
}

// ---------------------------------------------------------------------------
// Shared sample builders
// ---------------------------------------------------------------------------

/**
 * Builds a minimal sample netlist consisting of two resistors in series and
 * a single input port on a top-level `Board` module.
 */
const buildSampleNetlist = (): Netlist => {
  const b = new NetlistBuilder("test");
  const boardPath = "Board";
  b.addModule(boardPath);
  b.addPort(boardPath, "IN");
  b.addComponent(boardPath, "r1", { type: "resistor" });
  b.addComponent(boardPath, "r2", { type: "resistor" });

  // Connectivity
  b.connect("N1", ["Board.r1.p1", "Board.r2.p1"]);
  b.connect("N2", ["Board.r1.p2", "Board.IN"]);
  b.connect("N3", ["Board.r2.p2"]);

  return b.build();
};

// ---------------------------------------------------------------------------

describe("SchematicRenderer - basic graph construction", () => {
  // Uses the shared helper `buildSampleNetlist` defined above.

  test("_nodeForInstance adds Port children to module", () => {
    const renderer = new SchematicRenderer(buildSampleNetlist());
    const boardNode = renderer._nodeForInstance("test:Board")!;

    expect(boardNode.type).toBe(NodeType.MODULE);
    expect(boardNode.ports).toEqual(
      expect.arrayContaining([expect.objectContaining({ id: "test:Board.IN" })])
    );
  });

  test("resistor nodes expose exactly two ports", () => {
    const renderer = new SchematicRenderer(buildSampleNetlist());
    const r1Node = renderer._nodeForInstance("test:Board.r1")!;
    const r2Node = renderer._nodeForInstance("test:Board.r2")!;

    for (const node of [r1Node, r2Node]) {
      expect(node.type).toBe(NodeType.RESISTOR);
      expect(node.ports).toHaveLength(2);
    }
  });

  test("_graphForInstance wires up an edge between series resistors", () => {
    const renderer = new SchematicRenderer(buildSampleNetlist());
    const graph = renderer._graphForInstance("test:Board");

    // Find edge connecting the first ports of the two resistors
    const expectedEdgeId = "test:Board.r1.p1-test:Board.r2.p1";

    expect(graph.edges).toEqual(
      expect.arrayContaining([expect.objectContaining({ id: expectedEdgeId })])
    );
  });
});

describe("Interface aggregation behavior", () => {
  const buildInterfaceNetlist = (aggregate: boolean): Netlist => {
    // Build the top-level Board (namespace `test`)
    const testBuilder = new NetlistBuilder("test");
    const boardPath = "Board";
    testBuilder.addModule(boardPath);
    testBuilder.addInterface(boardPath, "IF", ["p1", "p2"]);

    // Build an external module `B` under namespace `ext`
    const extBuilder = new NetlistBuilder("ext");
    const moduleB = "B";
    extBuilder.addModule(moduleB);
    extBuilder.addInterface(moduleB, "J", ["q1", "q2"]);

    // Cross-namespace connectivity
    const nets: Record<string, any> = {
      N1: {
        kind: NetKind.NORMAL,
        ports: ["test:Board.IF.p1", "ext:B.J.q1"],
      },
      N2: {
        kind: NetKind.NORMAL,
        ports: ["test:Board.IF.p2", ...(aggregate ? ["ext:B.J.q2"] : [])],
      },
    };

    // Merge instances from both builders and return the combined netlist
    const instances = {
      ...testBuilder.build().instances,
      ...extBuilder.build().instances,
    } as Record<string, any>;

    return { instances, nets } as Netlist;
  };

  test("interface remains exploded when peer pin names differ", () => {
    const renderer = new SchematicRenderer(buildInterfaceNetlist(true));
    const boardNode = renderer._nodeForInstance("test:Board")!;

    const portIds = (boardNode.ports || []).map((p) => p.id);
    expect(portIds).toEqual(
      expect.arrayContaining(["test:Board.IF.p1", "test:Board.IF.p2"])
    );
    expect(portIds).not.toContain("test:Board.IF");
  });

  test("keeps individual interface ports when connected inconsistently", () => {
    const renderer = new SchematicRenderer(buildInterfaceNetlist(false));
    const boardNode = renderer._nodeForInstance("test:Board")!;

    const portIds = (boardNode.ports || []).map((p) => p.id);
    expect(portIds).not.toContain("test:Board.IF");
    expect(portIds).toEqual(
      expect.arrayContaining(["test:Board.IF.p1", "test:Board.IF.p2"])
    );
  });
});

// ---------------------------------------------------------------------------
// New tests – interface aggregation should still occur even with internal nets
// ---------------------------------------------------------------------------

describe("Interface aggregation with internal nets", () => {
  const buildFlashNetlist = (): Netlist => {
    const b = new NetlistBuilder("test");

    // Top-level board
    const board = "Board";
    b.addModule(board);

    // flash_a sub-module with QSPI interface & some internal sink ports
    const flashA = `${board}.flash_a`;
    b.addModule("flash_a", board);
    b.addInterface(flashA, "qspi", ["clk", "cs", "io0", "io1", "io2", "io3"]);
    // Add dummy internal ports to create internal-only nets
    ["clk", "cs", "io0", "io1", "io2", "io3"].forEach((p) => {
      b.addPort(flashA, `sink_${p}`);
      b.connect(`intA_${p}`, [`${flashA}.qspi.${p}`, `${flashA}.sink_${p}`]);
    });

    // flash_b module
    const flashB = `${board}.flash_b`;
    b.addModule("flash_b", board);
    b.addInterface(flashB, "qspi", ["clk", "cs", "io0", "io1", "io2", "io3"]);
    ["clk", "cs", "io0", "io1", "io2", "io3"].forEach((p) => {
      b.addPort(flashB, `sink_${p}`);
      b.connect(`intB_${p}`, [`${flashB}.qspi.${p}`, `${flashB}.sink_${p}`]);
    });

    // External connections between the two interfaces
    ["clk", "cs", "io0", "io1", "io2", "io3"].forEach((p) => {
      b.connect(`ext_${p}`, [`${flashA}.qspi.${p}`, `${flashB}.qspi.${p}`]);
    });

    return b.build();
  };

  test("qspi interface is aggregated despite additional internal nets", () => {
    const renderer = new SchematicRenderer(buildFlashNetlist());
    const flashANode = renderer._nodeForInstance("test:Board.flash_a")!;

    const portIds = (flashANode.ports || []).map((p) => p.id);
    // Aggregated port should exist
    expect(portIds).toContain("test:Board.flash_a.qspi");
    // And individual sub-ports should NOT be present
    expect(
      portIds.some((id) => id.startsWith("test:Board.flash_a.qspi."))
    ).toBeFalsy();
  });
});

// ---------------------------------------------------------------------------
// New test – interface should NOT aggregate when one of its nets fans out
// ---------------------------------------------------------------------------

describe("Interface aggregation with fan-out on subset of pins", () => {
  const buildFanoutNetlist = (): Netlist => {
    const b = new NetlistBuilder("root");

    // Parent module M with its own I2C interface
    const board = "M";
    b.addModule(board);
    b.addInterface(board, "i2c", ["scl", "sda"]);

    // test1 and test2 sub-modules with I2C interfaces
    const test1 = `${board}.test1`;
    b.addModule("test1", board);
    b.addInterface(test1, "i2c", ["scl", "sda"]);

    const test2 = `${board}.test2`;
    b.addModule("test2", board);
    b.addInterface(test2, "i2c", ["scl", "sda"]);

    // Whole-interface connection: test1.i2c <> M.i2c  (connect both pins)
    ["scl", "sda"].forEach((p) => {
      b.connect(`b_${p}`, [`${board}.i2c.${p}`, `${test1}.i2c.${p}`]);
    });

    // Partial connection: test2.i2c.scl -> test1.i2c.scl
    b.connect("fan_scl", [`${test2}.i2c.scl`, `${test1}.i2c.scl`]);

    return b.build();
  };

  test("test1.i2c interface should be exploded (not aggregated)", () => {
    const renderer = new SchematicRenderer(buildFanoutNetlist());

    const test1Node = renderer._nodeForInstance("root:M.test1")!;
    const portIds = (test1Node.ports || []).map((p) => p.id);

    // Expected: individual sub-ports present
    expect(portIds).toEqual(
      expect.arrayContaining(["root:M.test1.i2c.scl", "root:M.test1.i2c.sda"])
    );

    // Aggregated port should NOT be present
    expect(portIds).not.toContain("root:M.test1.i2c");
  });
});

// ---------------------------------------------------------------------------
// Cross-wired interface should NOT aggregate due to name mismatch
// ---------------------------------------------------------------------------

describe("Interface aggregation fails on pin-name mismatch", () => {
  const buildCrossWireNetlist = (): Netlist => {
    const b = new NetlistBuilder("cw");

    const modA = "A";
    const modB = "B";
    b.addModule(modA);
    b.addModule(modB);
    b.addInterface(modA, "uart", ["rx", "tx"]);
    b.addInterface(modB, "uart", ["rx", "tx"]);

    // Cross connect: A.rx <-> B.tx  and A.tx <-> B.rx
    b.connect("n1", ["cw:A.uart.rx", "cw:B.uart.tx"]);
    b.connect("n2", ["cw:A.uart.tx", "cw:B.uart.rx"]);

    return b.build();
  };

  test("uart interfaces should be exploded", () => {
    const renderer = new SchematicRenderer(buildCrossWireNetlist());
    const nodeA = renderer._nodeForInstance("cw:A")!;
    const portIds = (nodeA.ports || []).map((p) => p.id);

    expect(portIds).toEqual(
      expect.arrayContaining(["cw:A.uart.rx", "cw:A.uart.tx"])
    );
    expect(portIds).not.toContain("cw:A.uart");
  });
});
