import ELK from "elkjs/lib/elk-api.js";
import type { ELK as ELKType } from "elkjs/lib/elk-api";
import { InstanceKind, NetKind } from "./types/NetlistTypes";
import type { Netlist, AttributeValue } from "./types/NetlistTypes";
import { createCanvas } from "canvas";
import { getKicadSymbolInfo } from "./renderer/kicad_sym";

export enum NodeType {
  META = "meta",
  MODULE = "module",
  COMPONENT = "component",
  RESISTOR = "resistor",
  CAPACITOR = "capacitor",
  INDUCTOR = "inductor",
  NET_REFERENCE = "net_reference",
  NET_JUNCTION = "net_junction",
  SYMBOL = "symbol",
}

export enum NetReferenceType {
  NORMAL = "normal",
  GROUND = "ground",
  VDD = "vdd",
}

export interface ElkNode {
  id: string;
  width?: number;
  height?: number;
  x?: number;
  y?: number;
  ports?: ElkPort[];
  labels?: ElkLabel[];
  properties?: Record<string, string>;
  type: NodeType;
  netId?: string; // Only used for net reference nodes
  netReferenceType?: NetReferenceType; // Only used for net reference nodes
  children?: ElkNode[];
  edges?: ElkEdge[];
}

export interface ElkPort {
  id: string;
  width?: number;
  height?: number;
  x?: number;
  y?: number;
  labels?: ElkLabel[];
  properties?: Record<string, string>;
  netId?: string;
}

export interface ElkLabel {
  text: string;
  x?: number;
  y?: number;
  width?: number;
  height?: number;
  textAlign?: "left" | "right" | "center";
  properties?: Record<string, string>;
}

export interface ElkEdge {
  id: string;
  netId: string;
  sources: string[];
  targets: string[];
  sourceComponentRef: string;
  targetComponentRef: string;
  labels?: ElkLabel[];
  junctionPoints?: { x: number; y: number }[];
  sections?: {
    id: string;
    startPoint: { x: number; y: number };
    endPoint: { x: number; y: number };
    bendPoints?: { x: number; y: number }[];
  }[];
  properties?: Record<string, string>;
}

export interface ElkGraph {
  id: string;
  children: ElkNode[];
  edges: ElkEdge[];
}

export interface NodeSizeConfig {
  module: {
    width: number;
    height: number;
  };
  component: {
    width: number;
    height: number;
  };
  resistor: {
    width: number;
    height: number;
  };
  capacitor: {
    width: number;
    height: number;
  };
  inductor: {
    width: number;
    height: number;
  };
  netReference: {
    width: number;
    height: number;
  };
  netJunction: {
    width: number;
    height: number;
  };
  ground: {
    width: number;
    height: number;
  };
  vdd: {
    width: number;
    height: number;
  };
  symbol: {
    width: number;
    height: number;
  };
}

export interface SchematicConfig {
  // Node size configuration
  nodeSizes: NodeSizeConfig;

  // Layout configuration - we'll add more options here later
  layout: {
    // Direction of the layout - will be passed to ELK
    direction: "LEFT" | "RIGHT" | "UP" | "DOWN";
    // Spacing between nodes
    spacing: number;
    // Padding around the entire layout
    padding: number;
    // Whether to explode modules into their component parts
    explodeModules: boolean;
    // Smart net reference positioning - position net references based on connected port side
    smartNetReferencePositioning?: boolean;
    // Smart edge splitting - replace direct edges between blocks with net references
    smartEdgeSplitting?: boolean;
  };

  // Visual configuration - we'll add more options here later
  visual: {
    // Whether to show port labels
    showPortLabels: boolean;
    // Whether to show component values
    showComponentValues: boolean;
    // Whether to show footprints
    showFootprints: boolean;
  };
}

export const DEFAULT_CONFIG: SchematicConfig = {
  nodeSizes: {
    module: {
      width: 256,
      height: 128,
    },
    component: {
      width: 256,
      height: 128,
    },
    resistor: {
      width: 40,
      height: 30,
    },
    capacitor: {
      width: 40,
      height: 20,
    },
    inductor: {
      width: 40,
      height: 40,
    },
    netReference: {
      width: 10,
      height: 10,
    },
    netJunction: {
      width: 10,
      height: 10,
    },
    ground: {
      width: 30,
      height: 50,
    },
    vdd: {
      width: 30,
      height: 10,
    },
    symbol: {
      width: 100,
      height: 100,
    },
  },
  layout: {
    direction: "LEFT",
    spacing: 10,
    padding: 20,
    explodeModules: true,
    smartNetReferencePositioning: true,
    smartEdgeSplitting: true,
  },
  visual: {
    showPortLabels: true,
    showComponentValues: true,
    showFootprints: true,
  },
};

// Add this helper function before the SchematicRenderer class
function calculateTextDimensions(
  text: string,
  fontSize: number,
  fontFamily: string = "monospace",
  fontWeight: string = "normal"
): { width: number; height: number } {
  // Create a canvas for text measurement
  const canvas = createCanvas(1, 1);
  const context = canvas.getContext("2d");

  // Set font properties
  context.font = `${fontWeight} ${fontSize}px ${fontFamily}`;

  // For multiline text, split by newline and find the widest line
  const lines = text.split("\n");
  const lineHeight = fontSize * 1.2; // Standard line height multiplier
  const width = Math.max(
    ...lines.map((line) => context.measureText(line).width)
  );
  const height = lineHeight * lines.length;

  return { width, height };
}

export class SchematicRenderer {
  netlist: Netlist;
  elk: ELKType;
  nets: Map<string, Set<string>>;
  config: SchematicConfig;
  netNames: Map<string, string>;
  /**
   * Helper mapping used when collapsing interface ports.  Key is the
   *  original sub-port id, value is the aggregated interface port id.
   */
  private interfacePortCollapse: Map<string, string> = new Map();

  constructor(netlist: Netlist, config: Partial<SchematicConfig> = {}) {
    this.netlist = netlist;
    this.elk = new ELK({
      workerFactory: function (url) {
        const { Worker } = require("elkjs/lib/elk-worker.js"); // non-minified
        return new Worker(url);
      },
    });
    this.nets = this._generateNets();
    this.netNames = this._generateUniqueNetNames();
    // Merge provided config with defaults
    this.config = {
      ...DEFAULT_CONFIG,
      ...config,
      // Deep merge for nested objects
      nodeSizes: {
        ...DEFAULT_CONFIG.nodeSizes,
        ...config.nodeSizes,
      },
      layout: {
        ...DEFAULT_CONFIG.layout,
        ...config.layout,
      },
      visual: {
        ...DEFAULT_CONFIG.visual,
        ...config.visual,
      },
    };
  }

  _generateNets(): Map<string, Set<string>> {
    const nets = new Map<string, Set<string>>();

    if (!this.netlist.nets) {
      return nets;
    }

    for (const [netId, net] of Object.entries(this.netlist.nets)) {
      nets.set(netId, new Set(net.ports));
    }

    return nets;
  }

  getNets(): Map<string, Set<string>> {
    return this.nets;
  }

  _getAttributeValue(attr: AttributeValue | string | undefined): string | null {
    if (!attr) return null;
    if (typeof attr === "string") return attr;
    if (attr.String) return attr.String;
    if (attr.Boolean !== undefined) return String(attr.Boolean);
    if (attr.Number !== undefined) return String(attr.Number);
    return null;
  }

  _renderValue(value: string | AttributeValue | undefined): string | undefined {
    if (typeof value === "string") return value;
    if (value?.String) return value.String;
    if (value?.Number !== undefined) return String(value.Number);
    if (value?.Boolean !== undefined) return String(value.Boolean);
    if (value?.Physical !== undefined) return String(value.Physical);

    return undefined;
  }

  _isGndNet(net: Set<string>): boolean {
    const netId = Array.from(this.nets.entries()).find(
      ([, set]) => set === net
    )?.[0];
    if (netId && this.netlist.nets[netId]) {
      return this.netlist.nets[netId].kind === NetKind.GROUND;
    }
    return false;
  }

  _isPowerNet(net: Set<string>): boolean {
    const netId = Array.from(this.nets.entries()).find(
      ([, set]) => set === net
    )?.[0];
    if (netId && this.netlist.nets[netId]) {
      return this.netlist.nets[netId].kind === NetKind.POWER;
    }
    return false;
  }

  _getPortConnections(instance_ref: string): {
    p1: { isGnd: boolean; isPower: boolean };
    p2: { isGnd: boolean; isPower: boolean };
  } {
    const { p1Name, p2Name } = this._getPassivePortNames(instance_ref);
    const connections = {
      p1: { isGnd: false, isPower: false },
      p2: { isGnd: false, isPower: false },
    };

    // Check each net for connections to our ports
    for (const net of this.nets.values()) {
      const p1Port = `${instance_ref}.${p1Name}`;
      const p2Port = `${instance_ref}.${p2Name}`;

      if (net.has(p1Port)) {
        connections.p1.isGnd = this._isGndNet(net);
        connections.p1.isPower = this._isPowerNet(net);
      }
      if (net.has(p2Port)) {
        connections.p2.isGnd = this._isGndNet(net);
        connections.p2.isPower = this._isPowerNet(net);
      }
    }

    return connections;
  }

  _determinePortSides(instance_ref: string): {
    p1Side: "NORTH" | "SOUTH";
    p2Side: "NORTH" | "SOUTH";
  } {
    // Reuse connectivity helper which already accounts for correct port IDs
    const connections = this._getPortConnections(instance_ref);

    // Default orientation
    let p1Side: "NORTH" | "SOUTH" = "NORTH";
    let p2Side: "NORTH" | "SOUTH" = "SOUTH";

    // Handle various cases
    if (connections.p1.isGnd && !connections.p2.isGnd) {
      // If p1 is ground and p2 isn't, p1 should be south
      p1Side = "SOUTH";
      p2Side = "NORTH";
    } else if (connections.p2.isGnd && !connections.p1.isGnd) {
      // If p2 is ground and p1 isn't, p2 should be south
      p1Side = "NORTH";
      p2Side = "SOUTH";
    } else if (connections.p1.isPower && !connections.p2.isPower) {
      // If p1 is power and p2 isn't, p1 should be north
      p1Side = "NORTH";
      p2Side = "SOUTH";
    } else if (connections.p2.isPower && !connections.p1.isPower) {
      // If p2 is power and p1 isn't, p2 should be north
      p1Side = "SOUTH";
      p2Side = "NORTH";
    }
    // In all other cases (including both ground, both power, or neither),
    // we keep the default orientation

    return { p1Side, p2Side };
  }

  _resistorNode(instance_ref: string): ElkNode {
    const instance = this.netlist.instances[instance_ref];
    const { p1Name, p2Name } = this._getPassivePortNames(instance_ref);
    const footprint =
      this._getAttributeValue(instance.attributes["package"]) ||
      this._getAttributeValue(instance.attributes["Package"]) ||
      this._getAttributeValue(instance.attributes["footprint"]);

    const value =
      this._renderValue(instance.attributes["value"]) ||
      this._renderValue(instance.attributes["Val"]);
    const showValue = this.config.visual.showComponentValues && value;
    const showFootprint = this.config.visual.showFootprints && footprint;

    // Get reference designator if available
    const refDes = instance.reference_designator;

    const { p1Side, p2Side } = this._determinePortSides(instance_ref);

    return {
      id: instance_ref,
      type: NodeType.RESISTOR,
      width: this.config.nodeSizes.resistor.width,
      height: this.config.nodeSizes.resistor.height,
      labels: [
        // Add reference designator label if available
        ...(refDes
          ? [
              {
                text: refDes,
                x: -15, // Position to the left of the component
                y: 10,
                width: 20,
                height: 10,
                textAlign: "right" as const,
              },
            ]
          : []),
        {
          text: `${showValue ? value : ""}${
            showFootprint ? `\n${footprint}` : ""
          }`,
          x: 35,
          y: 4,
          width: 128,
          height: 25,
          textAlign: "left" as const,
        },
      ],
      ports: [
        {
          id: `${instance_ref}.${p1Name}`,
          properties: {
            "port.side": p1Side,
            "port.index": "0",
            "port.anchor": "CENTER",
            "port.alignment": "CENTER",
          },
        },
        {
          id: `${instance_ref}.${p2Name}`,
          properties: {
            "port.side": p2Side,
            "port.index": "0",
            "port.anchor": "CENTER",
            "port.alignment": "CENTER",
          },
        },
      ],
      properties: {
        "elk.padding": "[top=10, left=10, bottom=10, right=10]",
        "elk.portConstraints": "FIXED_SIDE",
        "elk.nodeSize.minimum": "(40, 30)",
        "elk.nodeSize.constraints": "MINIMUM_SIZE",
        "elk.nodeLabels.placement": "INSIDE",
      },
    };
  }

  _capacitorNode(instance_ref: string): ElkNode {
    const instance = this.netlist.instances[instance_ref];
    const { p1Name, p2Name } = this._getPassivePortNames(instance_ref);
    const value =
      this._renderValue(instance.attributes["value"]) ||
      this._renderValue(instance.attributes["Val"]);
    const footprint =
      this._getAttributeValue(instance.attributes["package"]) ||
      this._getAttributeValue(instance.attributes["Package"]) ||
      this._getAttributeValue(instance.attributes["footprint"]);

    const showValue = this.config.visual.showComponentValues && value;
    const showFootprint = this.config.visual.showFootprints && footprint;

    const { p1Side, p2Side } = this._determinePortSides(instance_ref);

    return {
      id: instance_ref,
      type: NodeType.CAPACITOR,
      width: this.config.nodeSizes.capacitor.width,
      height: this.config.nodeSizes.capacitor.height,
      labels: [
        // Add reference designator label if available
        ...(instance.reference_designator
          ? [
              {
                text: instance.reference_designator,
                x: -20, // Position to the left of the component
                y: 7,
                width: 20,
                height: 10,
                textAlign: "right" as const,
              },
            ]
          : []),
        {
          text: `${showValue ? value : ""}${
            showFootprint ? `\n${footprint}` : ""
          }`,
          x: 40,
          y: 2,
          width: 128,
          height: 20,
          textAlign: "left" as const,
        },
      ],
      ports: [
        {
          id: `${instance_ref}.${p1Name}`,
          properties: {
            "port.side": p1Side,
            "port.index": "0",
            "port.anchor": "CENTER",
            "port.alignment": "CENTER",
          },
        },
        {
          id: `${instance_ref}.${p2Name}`,
          properties: {
            "port.side": p2Side,
            "port.index": "0",
            "port.anchor": "CENTER",
            "port.alignment": "CENTER",
          },
        },
      ],
      properties: {
        "elk.padding": "[top=10, left=10, bottom=10, right=10]",
        "elk.portConstraints": "FIXED_SIDE",
        "elk.nodeSize.minimum": "(40, 20)",
        "elk.nodeSize.constraints": "MINIMUM_SIZE",
        "elk.nodeLabels.placement": "",
      },
    };
  }

  _inductorNode(instance_ref: string): ElkNode {
    const instance = this.netlist.instances[instance_ref];
    const { p1Name, p2Name } = this._getPassivePortNames(instance_ref);
    const value =
      this._renderValue(instance.attributes["value"]) ||
      this._renderValue(instance.attributes["Val"]);
    const footprint =
      this._getAttributeValue(instance.attributes["package"]) ||
      this._getAttributeValue(instance.attributes["Package"]) ||
      this._getAttributeValue(instance.attributes["footprint"]);

    const showValue = this.config.visual.showComponentValues && value;
    const showFootprint = this.config.visual.showFootprints && footprint;

    // Get reference designator if available
    const refDes = instance.reference_designator;

    const { p1Side, p2Side } = this._determinePortSides(instance_ref);

    return {
      id: instance_ref,
      type: NodeType.INDUCTOR,
      width: this.config.nodeSizes.inductor.width,
      height: this.config.nodeSizes.inductor.height,
      labels: [
        // Add reference designator label if available
        ...(refDes
          ? [
              {
                text: refDes,
                x: -20, // Position to the left of the component
                y: 5,
                width: 15,
                height: 10,
                textAlign: "right" as const,
              },
            ]
          : []),
        {
          text: `${showValue ? value : ""}${
            showFootprint ? `\n${footprint}` : ""
          }`,
          x: 45,
          y: 0,
          width: 128,
          height: 40,
          textAlign: "left" as const,
        },
      ],
      ports: [
        {
          id: `${instance_ref}.${p1Name}`,
          properties: {
            "port.side": p1Side,
            "port.index": "0",
            "port.anchor": "CENTER",
            "port.alignment": "CENTER",
          },
        },
        {
          id: `${instance_ref}.${p2Name}`,
          properties: {
            "port.side": p2Side,
            "port.index": "0",
            "port.anchor": "CENTER",
            "port.alignment": "CENTER",
          },
        },
      ],
      properties: {
        "elk.padding": "[top=10, left=10, bottom=10, right=10]",
        "elk.portConstraints": "FIXED_SIDE",
        "elk.nodeSize.minimum": "(40, 40)",
        "elk.nodeSize.constraints": "MINIMUM_SIZE",
        "elk.nodeLabels.placement": "",
      },
    };
  }

  _symbolNode(instance_ref: string): ElkNode | null {
    const instance = this.netlist.instances[instance_ref];
    if (!instance) return null;

    // Get the symbol_path attribute
    const symbolPath = this._getAttributeValue(instance.attributes.symbol_path);
    if (!symbolPath || !this.netlist.symbols[symbolPath]) {
      // Fall back to regular component node if no symbol
      return this._moduleOrComponentNode(instance_ref);
    }

    // Get the symbol content
    const symbolContent = this.netlist.symbols[symbolPath];

    // Extract the symbol name from the instance
    const symbolName =
      this._getAttributeValue(instance.attributes.symbol_name) ||
      instance.type_ref.module_name;

    try {
      // Get symbol info including bounding box and pin endpoints
      const symbolInfo = getKicadSymbolInfo(symbolContent, symbolName, {
        unit: 1,
        bodyStyle: 1,
        tightBounds: false, // Include pins in the bounding box
      });

      // Calculate node size based on symbol bounding box
      // Scale factor to convert from symbol units to schematic units
      const scale = 10; // Adjust this based on your needs
      const nodeWidth = Math.max(
        symbolInfo.bbox.w * scale,
        this.config.nodeSizes.symbol.width
      );
      const nodeHeight = Math.max(
        symbolInfo.bbox.h * scale,
        this.config.nodeSizes.symbol.height
      );

      console.log("Symbol node size calculation:", {
        symbolBBox: symbolInfo.bbox,
        scale,
        nodeWidth,
        nodeHeight,
        configMinWidth: this.config.nodeSizes.symbol.width,
        configMinHeight: this.config.nodeSizes.symbol.height,
        pinEndpoints: symbolInfo.pinEndpoints.map((pin) => ({
          name: pin.name,
          position: pin.position,
        })),
      });

      // Get reference designator and value
      const refDes = instance.reference_designator;
      const value = this._renderValue(instance.attributes.value);
      const footprint =
        this._getAttributeValue(instance.attributes.footprint) ||
        this._getAttributeValue(instance.attributes.package);

      // Create the node
      const node: ElkNode = {
        id: instance_ref,
        type: NodeType.SYMBOL,
        width: nodeWidth,
        height: nodeHeight,
        ports: [],
        labels: [
          // Reference designator
          ...(refDes
            ? [
                {
                  text: refDes,
                  x: -20,
                  y: nodeHeight / 2 - 10,
                  width: 20,
                  height: 10,
                  textAlign: "right" as const,
                },
              ]
            : []),
          // Value
          ...(value && this.config.visual.showComponentValues
            ? [
                {
                  text: value,
                  x: nodeWidth + 5,
                  y: nodeHeight / 2 - 10,
                  width: 50,
                  height: 10,
                  textAlign: "left" as const,
                },
              ]
            : []),
          // Footprint
          ...(footprint && this.config.visual.showFootprints
            ? [
                {
                  text: footprint,
                  x: nodeWidth / 2 - 25,
                  y: nodeHeight + 5,
                  width: 50,
                  height: 10,
                  textAlign: "center" as const,
                },
              ]
            : []),
        ],
        properties: {
          "elk.portConstraints": "FIXED_POS",
          "elk.nodeSize.constraints": "MINIMUM_SIZE",
          "elk.nodeSize.minimum": `(${nodeWidth}, ${nodeHeight})`,
          symbolPath: symbolPath,
          symbolName: symbolName,
        },
      };

      // Create ports based on pin endpoints
      for (const pinEndpoint of symbolInfo.pinEndpoints) {
        // Find the corresponding port in the instance children
        let portName = pinEndpoint.name;
        let portRef = `${instance_ref}.${portName}`;

        // Check if this port exists in the instance children
        // Sometimes the port names might be different (e.g., PA0 vs p1)
        const childNames = Object.keys(instance.children || {});
        const matchingChild = childNames.find((name) => {
          // Try exact match first
          if (name === portName) return true;
          // Try case-insensitive match
          if (name.toLowerCase() === portName.toLowerCase()) return true;
          // Try matching by pin number
          const childInstance = this.netlist.instances[instance.children[name]];
          if (childInstance && childInstance.kind === InstanceKind.PORT) {
            // Check if pin number matches
            const pinNumber = this._getAttributeValue(
              childInstance.attributes.pin_number
            );
            return pinNumber === pinEndpoint.number;
          }
          return false;
        });

        if (matchingChild) {
          portName = matchingChild;
          portRef = `${instance_ref}.${matchingChild}`;
        }

        // Calculate port position relative to node
        // The pin endpoints are in symbol coordinates, we need to transform them
        const portX = (pinEndpoint.position.x - symbolInfo.bbox.x) * scale;
        const portY = (pinEndpoint.position.y - symbolInfo.bbox.y) * scale;

        // Determine which side the port is on
        const distToLeft = portX;
        const distToRight = nodeWidth - portX;
        const distToTop = portY;
        const distToBottom = nodeHeight - portY;
        const minDist = Math.min(
          distToLeft,
          distToRight,
          distToTop,
          distToBottom
        );

        let side: "WEST" | "EAST" | "NORTH" | "SOUTH";
        if (minDist === distToLeft) side = "WEST";
        else if (minDist === distToRight) side = "EAST";
        else if (minDist === distToTop) side = "NORTH";
        else side = "SOUTH";

        // Add the port
        node.ports?.push({
          id: portRef,
          x: portX,
          y: portY,
          width: 0,
          height: 0,
          labels: this.config.visual.showPortLabels
            ? [
                {
                  text: portName,
                  width: calculateTextDimensions(portName, 10).width,
                  height: calculateTextDimensions(portName, 10).height,
                },
              ]
            : [],
          properties: {
            "port.side": side,
            "port.alignment": "CENTER",
            pinNumber: pinEndpoint.number,
            pinType: pinEndpoint.type,
          },
        });
      }

      return node;
    } catch (error) {
      console.error(`Failed to create symbol node for ${instance_ref}:`, error);
      // Fall back to regular component node
      return this._moduleOrComponentNode(instance_ref);
    }
  }

  _netReferenceNode(
    ref_id: string,
    name: string,
    side: "NORTH" | "WEST" | "SOUTH" | "EAST" = "WEST",
    netReferenceType: NetReferenceType = NetReferenceType.NORMAL
  ): ElkNode {
    const sizes =
      netReferenceType === NetReferenceType.GROUND
        ? this.config.nodeSizes.ground
        : netReferenceType === NetReferenceType.VDD
        ? this.config.nodeSizes.vdd
        : this.config.nodeSizes.netReference;

    // Use the actual net name for all net references
    const displayName = name;

    // Calculate label dimensions
    const fontSize = 12; // Base font size
    const labelDimensions = calculateTextDimensions(displayName, fontSize);

    // For VDD and normal nets, we want the label to be visible
    // For ground symbols, we don't show a label
    const labels =
      netReferenceType === NetReferenceType.GROUND
        ? []
        : [
            {
              text: displayName,
              width:
                netReferenceType === NetReferenceType.VDD
                  ? labelDimensions.width
                  : 0,
              height:
                netReferenceType === NetReferenceType.VDD
                  ? labelDimensions.height
                  : 0,
              // Position the label above the node for VDD and opposite to the port side for normal nets
              x:
                netReferenceType === NetReferenceType.VDD
                  ? (sizes.width - labelDimensions.width) / 2 // Center horizontally
                  : side === "EAST"
                  ? -labelDimensions.width - 5 // Label on left when port is on east
                  : sizes.width + 5, // Label on right when port is on west
              y:
                netReferenceType === NetReferenceType.VDD
                  ? -labelDimensions.height - 5 // 5px above the node
                  : (sizes.height - labelDimensions.height) / 2, // Center vertically
            },
          ];

    // For VDD nodes, adjust the node height to account for the label if needed
    const nodeHeight =
      netReferenceType === NetReferenceType.VDD
        ? sizes.height + labelDimensions.height + 5 // Add label height plus padding
        : sizes.height;

    // For normal nets, adjust width to account for label if needed
    const nodeWidth =
      netReferenceType === NetReferenceType.NORMAL
        ? sizes.width + labelDimensions.width + 10 // Add label width plus padding
        : sizes.width;

    // Calculate port position - it should be centered on its side
    let portX = 0;
    let portY = nodeHeight / 2;

    switch (side) {
      case "EAST":
        portX = nodeWidth;
        break;
      case "WEST":
        portX = 0;
        break;
      case "NORTH":
        portX = nodeWidth / 2;
        portY = 0;
        break;
      case "SOUTH":
        portX = nodeWidth / 2;
        portY = nodeHeight;
        break;
    }

    return {
      id: ref_id,
      type: NodeType.NET_REFERENCE,
      width: nodeWidth,
      height: nodeHeight,
      netId: name,
      netReferenceType,
      labels,
      ports: [
        {
          id: `${ref_id}.port`,
          width: 0,
          height: 0,
          x: portX,
          y: portY,
          properties: {
            "port.alignment": "CENTER",
            "port.side": side,
          },
        },
      ],
      properties: {
        "elk.padding": "[top=0, left=0, bottom=0, right=0]",
        "elk.portConstraints": "FIXED_POS",
        "elk.nodeSize.constraints": "MINIMUM_SIZE",
        "elk.nodeSize.minimum": `(${nodeWidth}, ${nodeHeight})`,
        "elk.nodeLabels.placement":
          netReferenceType === NetReferenceType.VDD
            ? "OUTSIDE H_CENTER V_TOP"
            : "",
      },
    };
  }

  _moduleOrComponentNode(instance_ref: string): ElkNode {
    let instance = this.netlist.instances[instance_ref];
    if (!instance) {
      throw new Error(`Instance ${instance_ref} not found`);
    }

    const sizes =
      instance.kind === InstanceKind.MODULE
        ? this.config.nodeSizes.module
        : this.config.nodeSizes.component;

    // Calculate main label dimensions
    const instanceName = instance_ref.split(".").pop() || "";
    const mpn = this._getAttributeValue(instance.attributes.mpn);
    const mainLabelDimensions = calculateTextDimensions(instanceName, 12);
    const refDesLabelDimensions = calculateTextDimensions(
      instance.reference_designator || "",
      12
    );
    const mpnLabelDimensions = calculateTextDimensions(mpn || "", 12);

    // Initialize minimum width and height based on label dimensions
    let minWidth = Math.max(sizes.width, mainLabelDimensions.width + 20); // Add padding
    let minHeight = Math.max(sizes.height, mainLabelDimensions.height + 20); // Add padding

    let node: ElkNode = {
      id: instance_ref,
      type: NodeType.MODULE,
      // width: minWidth,
      // height: minHeight,
      ports: [],
      labels: [
        {
          text: instanceName,
          width: mainLabelDimensions.width,
          height: mainLabelDimensions.height,
          textAlign: "left" as const,
          properties: {
            "elk.nodeLabels.placement": "OUTSIDE H_LEFT V_TOP",
          },
        },
        ...(instance.reference_designator
          ? [
              {
                text: instance.reference_designator,
                width: refDesLabelDimensions.width,
                height: refDesLabelDimensions.height,
                textAlign: "right" as const,
                properties: {
                  "elk.nodeLabels.placement": "OUTSIDE H_RIGHT V_TOP",
                },
              },
            ]
          : []),
        ...(mpn
          ? [
              {
                text: mpn,
                width: mpnLabelDimensions.width,
                height: mpnLabelDimensions.height,
                textAlign: "left" as const,
                properties: {
                  "elk.nodeLabels.placement": "OUTSIDE H_LEFT V_BOTTOM",
                },
              },
            ]
          : []),
      ],
      properties: {},
    };

    // Add a port on this node for (a) every child of type Port, and (b) every Port of an Interface.
    for (let [child_name, child_ref] of Object.entries(instance.children)) {
      let child_instance = this.netlist.instances[child_ref];
      if (!child_instance) {
        throw new Error(`Child ${child_ref} not found`);
      }

      if (child_instance.kind === InstanceKind.PORT) {
        const port_ref = `${instance_ref}.${child_name}`;
        // Show all ports (including ground-connected ones)

        // Calculate port label dimensions
        const portLabelDimensions = calculateTextDimensions(child_name, 10);

        node.ports?.push({
          id: port_ref,
          labels: [
            {
              text: child_name,
              width: portLabelDimensions.width,
              height: portLabelDimensions.height,
            },
          ],
        });

        // Update minimum width/height to accommodate port labels
        minWidth = Math.max(minWidth, portLabelDimensions.width * 2 + 60); // Extra space for ports on both sides
        minHeight = Math.max(
          minHeight,
          mainLabelDimensions.height + portLabelDimensions.height * 2 + 40
        );
      } else if (child_instance.kind === InstanceKind.INTERFACE) {
        // Determine if we can aggregate this interface into a single port.
        const iface_can_collapse = this._canAggregateInterface(
          child_ref,
          instance_ref
        );

        if (iface_can_collapse) {
          const agg_port_id = `${instance_ref}.${child_name}`;

          // Record mapping for all sub-ports so connectivity code can collapse.
          for (let subPort of Object.keys(child_instance.children)) {
            const sub_id = `${instance_ref}.${child_name}.${subPort}`;
            this.interfacePortCollapse.set(sub_id, agg_port_id);
          }

          // Add aggregate port once.
          const portLabelDimensions = calculateTextDimensions(child_name, 10);
          node.ports?.push({
            id: agg_port_id,
            labels: [
              {
                text: child_name,
                width: portLabelDimensions.width,
                height: portLabelDimensions.height,
              },
            ],
          });

          minWidth = Math.max(minWidth, portLabelDimensions.width * 2 + 60);
          minHeight = Math.max(
            minHeight,
            mainLabelDimensions.height + portLabelDimensions.height * 2 + 40
          );

          // Skip adding individual sub-ports below.
          continue;
        }

        // Not collapsible – create individual ports.
        for (let port_name of Object.keys(child_instance.children)) {
          const full_port_ref = `${instance_ref}.${child_name}.${port_name}`;
          // Show all ports (including ground-connected ones)

          // Calculate port label dimensions for interface ports
          const portLabel = `${child_name}.${port_name}`;
          const portLabelDimensions = calculateTextDimensions(portLabel, 10);

          node.ports?.push({
            id: full_port_ref,
            labels: [
              {
                text: portLabel,
                width: portLabelDimensions.width,
                height: portLabelDimensions.height,
              },
            ],
          });

          // Update minimum width/height to accommodate interface port labels
          minWidth = Math.max(minWidth, portLabelDimensions.width * 2 + 60);
          minHeight = Math.max(
            minHeight,
            mainLabelDimensions.height + portLabelDimensions.height * 2 + 40
          );
        }
      }
    }

    // Update final node dimensions
    node.width = minWidth;
    node.height = minHeight;

    if (instance.kind === InstanceKind.COMPONENT) {
      node.type = NodeType.COMPONENT;
      node.properties = {
        ...node.properties,
        "elk.portConstraints": "FIXED_ORDER",
      };

      // Helper function for natural sort comparison
      const naturalCompare = (a: string, b: string): number => {
        const splitIntoNumbersAndStrings = (str: string) => {
          return str
            .split(/(\d+)/)
            .filter(Boolean)
            .map((part) => (/^\d+$/.test(part) ? parseInt(part, 10) : part));
        };

        const aParts = splitIntoNumbersAndStrings(a);
        const bParts = splitIntoNumbersAndStrings(b);

        for (let i = 0; i < Math.min(aParts.length, bParts.length); i++) {
          if (typeof aParts[i] !== typeof bParts[i]) {
            return typeof aParts[i] === "number" ? -1 : 1;
          }
          if (aParts[i] < bParts[i]) return -1;
          if (aParts[i] > bParts[i]) return 1;
        }
        return aParts.length - bParts.length;
      };

      node.ports?.sort((a, b) => {
        // Extract the port name from the ID (everything after the last dot)
        const aName = a.id.split(".").pop() || "";
        const bName = b.id.split(".").pop() || "";
        return naturalCompare(aName, bName);
      });

      node.ports?.forEach((port, index) => {
        const totalPorts = node.ports?.length || 0;
        const halfLength = Math.floor(totalPorts / 2);
        const isFirstHalf = index < halfLength;

        port.properties = {
          ...port.properties,
          "port.side": isFirstHalf ? "WEST" : "EAST",
          "port.index": isFirstHalf
            ? `${halfLength - 1 - (index % halfLength)}`
            : `${index % halfLength}`,
        };
      });
    }

    return node;
  }

  public _nodeForInstance(instance_ref: string): ElkNode | null {
    const instance = this.netlist.instances[instance_ref];
    if (!instance) {
      throw new Error(`Instance ${instance_ref} not found`);
    }

    if ([InstanceKind.MODULE, InstanceKind.COMPONENT].includes(instance.kind)) {
      // Check if this component has a symbol_path
      const symbolPath = this._getAttributeValue(
        instance.attributes.symbol_path
      );
      if (
        symbolPath &&
        this.netlist.symbols &&
        this.netlist.symbols[symbolPath]
      ) {
        return this._symbolNode(instance_ref);
      }

      // Get the type attribute value
      const typeAttr = instance.attributes.type;
      const type =
        typeof typeAttr === "string"
          ? typeAttr
          : typeAttr?.String || // Handle AttributeValue::String
            (typeAttr?.Boolean !== undefined
              ? String(typeAttr.Boolean) // Handle AttributeValue::Boolean
              : typeAttr?.Number !== undefined
              ? String(typeAttr.Number) // Handle AttributeValue::Number
              : null); // Handle other cases

      if (type === "resistor") {
        return this._resistorNode(instance_ref);
      } else if (type === "capacitor") {
        return this._capacitorNode(instance_ref);
      } else if (type === "inductor") {
        return this._inductorNode(instance_ref);
      } else {
        return this._moduleOrComponentNode(instance_ref);
      }
    }

    return null;
  }

  _metaNode(
    nodes: ElkNode[],
    edges: ElkEdge[],
    exposedPortIds: Set<string>
  ): ElkNode {
    // Rewrite ports on `nodes` and `edges` that are in `exposedPortIds`
    let newNodes = nodes.map((node) => {
      return {
        ...node,
        ports: node.ports?.map((port) => {
          return {
            ...port,
            id: exposedPortIds.has(port.id) ? port.id + "_internal" : port.id,
          };
        }),
      };
    });

    let newEdges = edges.map((edge) => {
      return {
        ...edge,
        sources: edge.sources.map((source) =>
          exposedPortIds.has(source) ? source + "_internal" : source
        ),
        targets: edge.targets.map((target) =>
          exposedPortIds.has(target) ? target + "_internal" : target
        ),
      };
    });

    // Return node with updated ports
    return {
      id: `${nodes.map((node) => node.id).join("_")}_meta`,
      type: NodeType.META,
      children: newNodes,
      edges: newEdges,
      ports: Array.from(exposedPortIds).map((port) => ({
        id: port,
        properties: {
          fromPortId: `${port}_internal`,
          fromNodeId:
            nodes.find((node) => node.ports?.some((p) => p.id === port))?.id ??
            "",
        },
      })),
      properties: {
        "elk.padding": "[top=0, left=0, bottom=0, right=0]",
        "elk.direction": "DOWN",
        "elk.layered.spacing.nodeNodeBetweenLayers": "5",
        "elk.nodeSize.minimum": "(0, 0)",
      },
    };
  }

  _moveMetaNodePorts(node: ElkNode): ElkNode {
    if (node.type !== NodeType.META) {
      return node;
    }

    const children = node.children || [];
    node.ports = node.ports || [];

    // For each port in the meta node
    for (const metaPort of node.ports) {
      // Find the internal port this meta port should connect to
      const internalPortId = metaPort.properties?.fromPortId;
      if (!internalPortId) continue;

      // Find the child node that has this internal port
      for (const child of children) {
        const childPorts = child.ports || [];
        const internalPort = childPorts.find(
          (port) => port.id === internalPortId
        );

        if (internalPort) {
          // Copy the position from the internal port, adjusting for the child's position
          const childX = child.x || 0;
          const childY = child.y || 0;
          const portX = (internalPort.x || 0) + childX;
          const portY = (internalPort.y || 0) + childY;

          // Determine which edge of the meta node this port should be on
          const metaWidth = node.width || 0;
          const metaHeight = node.height || 0;

          const distToLeft = portX;
          const distToRight = metaWidth - portX;
          const distToTop = portY;
          const distToBottom = metaHeight - portY;

          const minDist = Math.min(
            distToLeft,
            distToRight,
            distToTop,
            distToBottom
          );

          let side: "WEST" | "EAST" | "NORTH" | "SOUTH";
          let x: number;
          let y: number;

          if (minDist === distToLeft) {
            side = "WEST";
            x = 0;
            y = portY;
          } else if (minDist === distToRight) {
            side = "EAST";
            x = metaWidth;
            y = portY;
          } else if (minDist === distToTop) {
            side = "NORTH";
            x = portX;
            y = 0;
          } else {
            side = "SOUTH";
            x = portX;
            y = metaHeight;
          }

          // Update the meta port properties
          metaPort.x = x;
          metaPort.y = y;
          metaPort.properties = {
            ...metaPort.properties,
            "port.side": side,
            "port.index": `${
              side === "WEST" || side === "EAST"
                ? y / metaHeight
                : x / metaWidth
            }`,
            "port.alignment": "CENTER",
          };

          break; // Found the matching port, no need to check other children
        }
      }
    }

    node.properties = {
      ...node.properties,
      "elk.portConstraints": "FIXED_POS",
    };

    return node;
  }

  _collectComponentsFromModule(instance_ref: string): ElkNode[] {
    const instance = this.netlist.instances[instance_ref];
    if (!instance) {
      return [];
    }

    let components: ElkNode[] = [];

    // Process all children
    for (const child_ref of Object.values(instance.children)) {
      const child_instance = this.netlist.instances[child_ref];
      if (!child_instance) continue;

      if (child_instance.kind === InstanceKind.COMPONENT) {
        const node = this._nodeForInstance(child_ref);
        if (node) components.push(node);
      } else if (child_instance.kind === InstanceKind.MODULE) {
        // Respect the "collapsed" attribute: if set, treat this module as a
        // single node and do not explode its children further.
        const collapsedAttr = this._getAttributeValue(
          child_instance.attributes.collapsed
        );
        const isCollapsed =
          collapsedAttr !== null &&
          !["false", "0", "no"].includes(collapsedAttr.toLowerCase());

        if (isCollapsed) {
          const node = this._moduleOrComponentNode(child_ref);
          components.push(node);
        } else {
          // Recursively collect components from submodules
          components = components.concat(
            this._collectComponentsFromModule(child_ref)
          );
        }
      }
    }

    return components;
  }

  _getAllNodes(
    node: ElkNode | ElkGraph | { id: string; children?: ElkNode[] }
  ): ElkNode[] {
    const nodes: ElkNode[] = [];

    const children = node.children || [];

    for (const child of children) {
      nodes.push(child);

      // Recursively get nodes from children
      if (child.children) {
        nodes.push(...this._getAllNodes(child));
      }
    }

    return nodes;
  }

  _cleanupGraph(graph: ElkGraph): ElkGraph {
    // First, remove any self-edges (edges that connect a port to itself)
    graph.edges = graph.edges.filter((edge) => {
      return !(
        edge.sources.length === 1 &&
        edge.targets.length === 1 &&
        edge.sources[0] === edge.targets[0]
      );
    });

    // Next, for each module, remove ports that have no connections
    // Keep all ports on modules – do not remove those without connections.

    // Create a map to track net reference connections
    const netRefConnections = new Map<
      string,
      { node: ElkNode; edges: ElkEdge[] }
    >();

    // Collect information about net reference connections
    for (const edge of graph.edges) {
      const sourceNode = graph.children.find(
        (node) => node.id === edge.sourceComponentRef
      );
      const targetNode = graph.children.find(
        (node) => node.id === edge.targetComponentRef
      );

      if (!sourceNode || !targetNode) continue;

      // If either node is a net reference, track its connections
      if (sourceNode.type === NodeType.NET_REFERENCE) {
        if (!netRefConnections.has(sourceNode.id)) {
          netRefConnections.set(sourceNode.id, { node: sourceNode, edges: [] });
        }
        netRefConnections.get(sourceNode.id)!.edges.push(edge);
      }
      if (targetNode.type === NodeType.NET_REFERENCE) {
        if (!netRefConnections.has(targetNode.id)) {
          netRefConnections.set(targetNode.id, { node: targetNode, edges: [] });
        }
        netRefConnections.get(targetNode.id)!.edges.push(edge);
      }
    }

    // Process each net reference
    Array.from(netRefConnections.entries()).forEach(
      ([netRefId, { node: netRefNode, edges }]) => {
        // Skip if this is a ground or VDD net reference
        if (
          netRefNode.netReferenceType === NetReferenceType.GROUND ||
          netRefNode.netReferenceType === NetReferenceType.VDD
        ) {
          return;
        }

        // Check all connected ports
        let allWest = true;
        let hasConnections = false;

        for (const edge of edges) {
          const otherNodeId =
            edge.sourceComponentRef === netRefId
              ? edge.targetComponentRef
              : edge.sourceComponentRef;
          const otherNode = graph.children.find(
            (node) => node.id === otherNodeId
          );

          if (
            !otherNode ||
            ![NodeType.MODULE, NodeType.COMPONENT, NodeType.SYMBOL].includes(
              otherNode.type
            )
          ) {
            continue;
          }

          hasConnections = true;
          const portId =
            edge.sourceComponentRef === netRefId
              ? edge.targets[0]
              : edge.sources[0];
          const portSide = otherNode.ports?.find((p) => p.id === portId)
            ?.properties?.["port.side"];

          if (portSide !== "WEST") {
            allWest = false;
            break;
          }
        }

        // If all connected ports are on the WEST side, update the net reference port to be on the EAST side
        if (
          hasConnections &&
          allWest &&
          netRefNode.ports &&
          netRefNode.ports.length > 0
        ) {
          netRefNode.ports[0].properties = {
            ...netRefNode.ports[0].properties,
            "port.side": "EAST",
          };
        }
      }
    );

    return graph;
  }

  public _graphForInstance(instance_ref: string): ElkGraph {
    const instance = this.netlist.instances[instance_ref];
    if (!instance) {
      // Find all instances that are in this file
      const instances = Object.keys(this.netlist.instances).filter(
        (sub_instance_ref) => {
          const [filename, path] = sub_instance_ref.split(":");
          return filename === instance_ref.split(":")[0] && !path.includes(".");
        }
      );

      return {
        id: instance_ref,
        children: instances
          .map((instance_ref) => this._nodeForInstance(instance_ref))
          .filter((node) => node !== null) as ElkNode[],
        edges: [],
      };
    }

    // If global explodeModules is enabled, always explode this module.
    if (
      this.config.layout.explodeModules &&
      instance.kind === InstanceKind.MODULE
    ) {
      const nodes = this._collectComponentsFromModule(instance_ref);
      let graph: ElkGraph = {
        id: instance_ref,
        children: nodes,
        edges: [],
      };

      graph = this._addConnectivity(graph);
      graph = this._applySmartEdgeSplitting(graph);
      graph = this._cleanupGraph(graph);
      graph = this._createLayoutMetaNodes(graph);
      return graph;
    }

    // Create all nodes.
    const nodes: ElkNode[] = Object.values(instance.children)
      .map((child_ref) => this._nodeForInstance(child_ref))
      .filter((node) => node !== null) as ElkNode[];

    // Create edges.
    let graph: ElkGraph = {
      id: instance_ref,
      children: nodes,
      edges: [],
    };

    graph = this._addConnectivity(graph);
    graph = this._applySmartEdgeSplitting(graph);
    graph = this._cleanupGraph(graph);
    graph = this._createLayoutMetaNodes(graph);

    return graph;
  }

  _addConnectivity(graph: ElkGraph): ElkGraph {
    // For each net in the netlist:
    //  - We will build a set `S` of ports in this graph that are in this net.
    //  - For each node in the graph:
    //    - If there is a port on the node that is in `net`, add it to `S`.
    //    - If there is a connection within `net` to something INSIDE of `node`
    //      but that is not already a port on `node`, add a port to `node` for
    //      it, and add it to `S`. Do this ONLY if `node` is of type MODULE.
    //  - If `|S| >= 1` AND there is a port in `net` that is to something
    //    OUTSIDE of the current graph, add a NetReference node to graph and
    //    connect it to the net.
    //  - Build edges to connect everything in `S` together with edges.

    // For each net in the netlist, process its connectivity
    for (const [netName, net] of Array.from(this.nets.entries())) {
      // Build a version of this net with collapsed port IDs so that if an
      // interface has been aggregated its constituent sub-ports map to the
      // aggregate port ID.
      const collapsedNet = new Set<string>();
      for (const pr of Array.from(net)) {
        collapsedNet.add(this._collapsePortId(pr as string));
      }

      // Set of ports in this graph that are in this net
      const portsInNetToInstanceRef = new Map<string, string>();

      // Check for connections outside the graph
      const outsideConnections = new Set<string>();
      for (const portRef of Array.from(collapsedNet)) {
        const isInGraph = (portRef as string).startsWith(graph.id + ".");
        if (!isInGraph) {
          outsideConnections.add(portRef as string);
        }
      }

      const isGndNet = this._isGndNet(net);
      const isPowerNet = this._isPowerNet(net);

      // For each node in the graph
      for (const node of this._getAllNodes(graph)) {
        let foundConnectionInNode = false;

        // Check if any of the node's ports are in this net (using collapsed IDs)
        const nodePorts = node.ports || [];
        for (const port of nodePorts) {
          if (collapsedNet.has(port.id)) {
            foundConnectionInNode = true;
            portsInNetToInstanceRef.set(port.id, node.id);
            port.netId = netName;
          }
        }

        // If this is a MODULE, check for internal connections that need new ports
        // Determine if this net crosses the boundary of the current module.
        let netCrossesModuleBoundary = false;
        if (node.type === NodeType.MODULE && !foundConnectionInNode) {
          let hasInternal = false;
          let hasExternal = false;

          for (const portRef of Array.from(collapsedNet)) {
            if ((portRef as string).startsWith(node.id + ".")) {
              hasInternal = true;
            } else if ((portRef as string).startsWith(graph.id + ".")) {
              // Only treat as external if the reference is still within the current graph.
              hasExternal = true;
            }

            if (hasInternal && hasExternal) {
              break;
            }
          }

          netCrossesModuleBoundary = hasInternal && hasExternal;
        }

        if (netCrossesModuleBoundary) {
          let matchingInternalPorts: string[] = [];
          for (const portRef of Array.from(collapsedNet)) {
            // Check if this port reference belongs inside this node
            if ((portRef as string).startsWith(node.id + ".")) {
              matchingInternalPorts.push(portRef as string);
            }
          }

          matchingInternalPorts.sort((a, b) => {
            return a.split(".").length - b.split(".").length;
          });

          if (matchingInternalPorts.length > 0 && !isGndNet) {
            const portId = matchingInternalPorts[0];
            portsInNetToInstanceRef.set(portId, node.id);
            node.ports?.push({
              id: portId,
              labels: [{ text: portId.replace(node.id + ".", "") }],
            });
          }
        }
      }

      // Add a net reference if we need it.
      if (
        portsInNetToInstanceRef.size >= 1 &&
        outsideConnections.size >= 1 &&
        !isGndNet &&
        !isPowerNet
      ) {
        const netRefId = `${netName}_ref`;
        const netRefNode = this._netReferenceNode(netRefId, netName);
        graph.children.push(netRefNode);
        portsInNetToInstanceRef.set(netRefNode.ports![0].id, netRefId);
      }

      // Create edges to connect everything in portsInNetToInstanceRef
      const portsList = Array.from(portsInNetToInstanceRef.entries());
      portsList.sort((a, b) => a[0].localeCompare(b[0]));

      const portToComponentType = (port: string) => {
        const instanceRef = portsInNetToInstanceRef.get(port);
        const node = this._getAllNodes(graph).find(
          (node) => node.id === instanceRef
        );
        return node?.type;
      };

      const portsOfType = (types: NodeType[]) => {
        return portsList.filter(([port, _instanceRef]) => {
          let componentType = portToComponentType(port);
          if (!componentType) {
            return false;
          }

          return types.includes(componentType);
        });
      };

      const passivePorts = portsOfType([
        NodeType.RESISTOR,
        NodeType.CAPACITOR,
        NodeType.INDUCTOR,
      ]);

      const netReferencePorts = portsOfType([NodeType.NET_REFERENCE]);

      const modulePorts = portsOfType([
        NodeType.COMPONENT,
        NodeType.MODULE,
        NodeType.SYMBOL,
      ]);

      if (isGndNet) {
        // Group ports by their instance reference
        const instanceToPorts = new Map<string, string[]>();
        for (const [port, instanceRef] of portsList) {
          if (!instanceToPorts.has(instanceRef)) {
            instanceToPorts.set(instanceRef, []);
          }
          instanceToPorts.get(instanceRef)!.push(port);
        }

        // Create one GND reference per instance
        for (const [instanceRef, ports] of Array.from(
          instanceToPorts.entries()
        )) {
          const node = graph.children.find((n) => n.id === instanceRef);
          if (!node) continue;

          const netRefId = `${netName}_gnd_${instanceRef.replace(/\./g, "_")}`;
          const netRefNode = this._netReferenceNode(
            netRefId,
            netName,
            "NORTH",
            NetReferenceType.GROUND
          );
          netRefNode.netId = netName;

          // For modules, connect all the ports to the net reference node.
          graph.children.push(netRefNode);
          for (const port of ports) {
            graph.edges.push({
              id: `${port}-${netRefId}`,
              sources: [port],
              targets: [netRefNode.ports![0].id],
              sourceComponentRef: instanceRef,
              targetComponentRef: netRefId,
              netId: netName,
            });
          }
        }
      } else if (isPowerNet) {
        // Group ports by their instance reference
        const instanceToPorts = new Map<string, string[]>();
        for (const [port, instanceRef] of portsList) {
          if (!instanceToPorts.has(instanceRef)) {
            instanceToPorts.set(instanceRef, []);
          }
          instanceToPorts.get(instanceRef)!.push(port);
        }

        // Create one GND reference per instance
        for (const [instanceRef, ports] of Array.from(
          instanceToPorts.entries()
        )) {
          const node = graph.children.find((n) => n.id === instanceRef);
          if (!node) continue;

          const netRefId = `${netName}_gnd_${instanceRef.replace(/\./g, "_")}`;
          const netRefNode = this._netReferenceNode(
            netRefId,
            netName,
            "SOUTH",
            NetReferenceType.VDD
          );
          netRefNode.netId = netName;

          // For modules, connect all the ports to the net reference node.
          graph.children.push(netRefNode);
          for (const port of ports) {
            graph.edges.push({
              id: `${port}-${netRefId}`,
              sources: [port],
              targets: [netRefNode.ports![0].id],
              sourceComponentRef: instanceRef,
              targetComponentRef: netRefId,
              netId: netName,
            });
          }
        }
      } else {
        // First, daisy chain all of the passive ports together.
        for (let i = 0; i < passivePorts.length - 1; i++) {
          const sourcePort = passivePorts[i][0];
          const targetPort = passivePorts[i + 1][0];

          const sourcePortInstanceRef = passivePorts[i][1];
          const targetPortInstanceRef = passivePorts[i + 1][1];

          graph.edges.push({
            id: `${sourcePort}-${targetPort}`,
            sources: [sourcePort],
            targets: [targetPort],
            sourceComponentRef: sourcePortInstanceRef,
            targetComponentRef: targetPortInstanceRef,
            netId: netName,
            properties: {
              "elk.layered.priority.direction": "10",
              "elk.layered.priority.shortness": "10",
            },
          });
        }

        // Next, connect the first passive port (or if we don't have, to a module
        // port) to all of the net reference ports.
        const netReferenceConnectorPort =
          passivePorts.length > 0
            ? passivePorts[0]
            : modulePorts[modulePorts.length - 1];

        for (const netReferencePort of netReferencePorts) {
          const sourcePort = netReferenceConnectorPort[0];
          const targetPort = netReferencePort[0];

          const sourcePortInstanceRef = netReferenceConnectorPort[1];
          const targetPortInstanceRef = netReferencePort[1];

          graph.edges.push({
            id: `${sourcePort}-${targetPort}`,
            sources: [sourcePort],
            targets: [targetPort],
            sourceComponentRef: sourcePortInstanceRef,
            targetComponentRef: targetPortInstanceRef,
            netId: netName,
          });
        }

        // And finally, connect all of the module ports to the first passive port
        // (or else to the last module port).
        const moduleConnectorPort =
          passivePorts.length > 0
            ? passivePorts[0]
            : modulePorts[modulePorts.length - 1];

        for (const modulePort of modulePorts) {
          const sourcePort = moduleConnectorPort[0];
          const targetPort = modulePort[0];

          const sourcePortInstanceRef = moduleConnectorPort[1];
          const targetPortInstanceRef = modulePort[1];

          graph.edges.push({
            id: `${sourcePort}-${targetPort}`,
            sources: [sourcePort],
            targets: [targetPort],
            sourceComponentRef: sourcePortInstanceRef,
            targetComponentRef: targetPortInstanceRef,
            netId: netName,
            properties: {
              "elk.layered.priority.direction": "10",
              "elk.layered.priority.shortness": "10",
            },
          });
        }
      }
    }

    return graph;
  }

  _createLayoutMetaNodes(graph: ElkGraph): ElkGraph {
    let edgeIdsInMetaNodes: Set<string> = new Set();
    const processedNodes = new Set<string>();
    const newChildren: ElkNode[] = [];
    const newEdges: ElkEdge[] = [];

    // Keep track of which meta nodes contain which passive components
    const passiveToMetaNode = new Map<string, string>();

    // First pass: Process all passive nodes and their connected net references
    for (const node of graph.children) {
      // Skip if not a passive component or already processed
      if (
        ![NodeType.RESISTOR, NodeType.CAPACITOR, NodeType.INDUCTOR].includes(
          node.type
        ) ||
        processedNodes.has(node.id)
      ) {
        continue;
      }

      // Find all edges connected to this passive node
      const connectedEdges = graph.edges.filter(
        (e) =>
          e.sourceComponentRef === node.id || e.targetComponentRef === node.id
      );

      // Find all net references connected to this passive node
      const connectedRefs = new Set<string>();
      for (const edge of connectedEdges) {
        const otherNodeId =
          edge.sourceComponentRef === node.id
            ? edge.targetComponentRef
            : edge.sourceComponentRef;
        const otherNode = graph.children.find((n) => n.id === otherNodeId);

        if (
          otherNode?.type === NodeType.NET_REFERENCE &&
          [NetReferenceType.GROUND, NetReferenceType.VDD].includes(
            otherNode.netReferenceType!
          )
        ) {
          connectedRefs.add(otherNodeId);
        }
      }

      // If we found connected net references, create a meta node
      if (connectedRefs.size > 0) {
        const refNodes = Array.from(connectedRefs).map(
          (refId) => graph.children.find((n) => n.id === refId)!
        );

        // Find all ports that need to be exposed (those with external connections)
        const exposedPorts = new Set<string>();
        for (const edge of graph.edges) {
          // If edge connects to our passive node but other end is not in our meta node
          if (
            edge.sourceComponentRef === node.id &&
            !connectedRefs.has(edge.targetComponentRef)
          ) {
            edge.sources.forEach((port) => {
              if (port.startsWith(node.id)) {
                exposedPorts.add(port);
              }
            });
          }
          if (
            edge.targetComponentRef === node.id &&
            !connectedRefs.has(edge.sourceComponentRef)
          ) {
            edge.targets.forEach((port) => {
              if (port.startsWith(node.id)) {
                exposedPorts.add(port);
              }
            });
          }
        }

        // Create meta node containing the passive and its net references
        const metaNodeId = `${node.id}_with_refs`;
        const metaNodeEdges = connectedEdges.filter(
          (e) =>
            (e.sourceComponentRef === node.id ||
              connectedRefs.has(e.sourceComponentRef)) &&
            (e.targetComponentRef === node.id ||
              connectedRefs.has(e.targetComponentRef))
        );

        for (const edge of metaNodeEdges) {
          edgeIdsInMetaNodes.add(edge.id);
        }

        const metaNode = this._metaNode(
          [node, ...refNodes],
          metaNodeEdges,
          exposedPorts
        );

        // Keep track of which meta node contains this passive component
        passiveToMetaNode.set(node.id, metaNodeId);

        // Mark these nodes as processed
        processedNodes.add(node.id);
        connectedRefs.forEach((refId) => processedNodes.add(refId));

        // Add the meta node to our new children
        newChildren.push(metaNode);
      }
    }

    // Add all unprocessed nodes
    for (const node of graph.children) {
      if (!processedNodes.has(node.id)) {
        newChildren.push(node);
      }
    }

    // Process all edges
    for (const edge of graph.edges) {
      if (edgeIdsInMetaNodes.has(edge.id)) {
        continue;
      }

      // If neither endpoint is in a meta node, keep the edge as is
      if (
        !processedNodes.has(edge.sourceComponentRef) &&
        !processedNodes.has(edge.targetComponentRef)
      ) {
        newEdges.push(edge);
        continue;
      }

      // If one endpoint is in a meta node, we need to update the edge
      const sourceMetaId = passiveToMetaNode.get(edge.sourceComponentRef);
      const targetMetaId = passiveToMetaNode.get(edge.targetComponentRef);

      // Create a new edge with updated endpoints if needed
      const newEdge: ElkEdge = {
        ...edge,
        sourceComponentRef: sourceMetaId || edge.sourceComponentRef,
        targetComponentRef: targetMetaId || edge.targetComponentRef,
      };

      newEdges.push(newEdge);
    }

    return {
      ...graph,
      children: newChildren,
      edges: newEdges,
    };
  }

  roots(): string[] {
    return Object.keys(this.netlist.instances).filter(
      (instance_ref) =>
        this.netlist.instances[instance_ref].kind === InstanceKind.MODULE
    );
  }
  _flattenGraph(graph: ElkGraph): ElkGraph {
    const flattenedNodes: ElkNode[] = [];
    const flattenedEdges: ElkEdge[] = [];

    const portIdToNodeMap = new Map<string, string>();

    function flattenNode(node: ElkNode, parentX = 0, parentY = 0) {
      // If this is a meta node, we need to restore the original port IDs
      if (node.type === NodeType.META) {
        // Build a map of internal port IDs to their original IDs
        const portIdMap = new Map<string, string>();
        for (const metaPort of node.ports || []) {
          const internalPortId = metaPort.properties?.fromPortId;
          if (internalPortId) {
            portIdMap.set(internalPortId, metaPort.id);
          }

          const nodeId = metaPort.properties?.fromNodeId;
          if (nodeId) {
            portIdToNodeMap.set(metaPort.id, nodeId);
          }
        }

        // Process children with restored port IDs
        if (node.children) {
          for (const child of node.children) {
            // Create a copy of the child with adjusted coordinates
            const flatChild: ElkNode = {
              ...child,
              children: undefined,
              edges: undefined,
              x: (child.x || 0) + (node.x || 0) + parentX,
              y: (child.y || 0) + (node.y || 0) + parentY,
              // Restore original port IDs
              ports: child.ports?.map((port) => ({
                ...port,
                id: portIdMap.get(port.id) || port.id,
              })),
            };
            flattenedNodes.push(flatChild);
          }
        }

        // Process edges with restored port IDs
        if (node.edges) {
          for (const edge of node.edges) {
            const flatEdge: ElkEdge = {
              ...edge,
              // Restore original port IDs in sources and targets
              sources: edge.sources.map(
                (source) => portIdMap.get(source) || source
              ),
              targets: edge.targets.map(
                (target) => portIdMap.get(target) || target
              ),
              // Adjust coordinates
              sections: edge.sections?.map((section) => ({
                ...section,
                startPoint: {
                  x: section.startPoint.x + (node.x || 0) + parentX,
                  y: section.startPoint.y + (node.y || 0) + parentY,
                },
                endPoint: {
                  x: section.endPoint.x + (node.x || 0) + parentX,
                  y: section.endPoint.y + (node.y || 0) + parentY,
                },
                bendPoints: section.bendPoints?.map((point) => ({
                  x: point.x + (node.x || 0) + parentX,
                  y: point.y + (node.y || 0) + parentY,
                })),
              })),
              junctionPoints: edge.junctionPoints?.map((point) => ({
                x: point.x + (node.x || 0) + parentX,
                y: point.y + (node.y || 0) + parentY,
              })),
            };
            flattenedEdges.push(flatEdge);
          }
        }
      } else {
        // For non-meta nodes, just flatten normally
        if (!node.children || node.children.length === 0) {
          const flatNode: ElkNode = {
            ...node,
            children: undefined,
            edges: undefined,
            x: (node.x || 0) + parentX,
            y: (node.y || 0) + parentY,
          };
          flattenedNodes.push(flatNode);
        }

        // Process nested nodes
        if (node.children) {
          for (const child of node.children) {
            flattenNode(
              child,
              (node.x || 0) + parentX,
              (node.y || 0) + parentY
            );
          }
        }

        // Process nested edges
        if (node.edges) {
          for (const edge of node.edges) {
            const flatEdge: ElkEdge = {
              ...edge,
              sections: edge.sections?.map((section) => ({
                ...section,
                startPoint: {
                  x: section.startPoint.x + (node.x || 0) + parentX,
                  y: section.startPoint.y + (node.y || 0) + parentY,
                },
                endPoint: {
                  x: section.endPoint.x + (node.x || 0) + parentX,
                  y: section.endPoint.y + (node.y || 0) + parentY,
                },
                bendPoints: section.bendPoints?.map((point) => ({
                  x: point.x + (node.x || 0) + parentX,
                  y: point.y + (node.y || 0) + parentY,
                })),
              })),
              junctionPoints: edge.junctionPoints?.map((point) => ({
                x: point.x + (node.x || 0) + parentX,
                y: point.y + (node.y || 0) + parentY,
              })),
            };
            flattenedEdges.push(flatEdge);
          }
        }
      }
    }

    // Process top-level nodes
    for (const node of graph.children) {
      flattenNode(node);
    }

    // Process top-level edges
    for (const edge of graph.edges) {
      const flatEdge: ElkEdge = {
        ...edge,
        sourceComponentRef:
          portIdToNodeMap.get(edge.sources[0]) || edge.sourceComponentRef,
        targetComponentRef:
          portIdToNodeMap.get(edge.targets[0]) || edge.targetComponentRef,
        sections: edge.sections?.map((section) => ({
          ...section,
          startPoint: { ...section.startPoint },
          endPoint: { ...section.endPoint },
          bendPoints: section.bendPoints?.map((point) => ({ ...point })),
        })),
        junctionPoints: edge.junctionPoints?.map((point) => ({ ...point })),
      };
      flattenedEdges.push(flatEdge);
    }

    return {
      id: graph.id,
      children: flattenedNodes,
      edges: flattenedEdges,
    };
  }

  _computeNodeDimensionsAfterPortAssignment(node: ElkNode): ElkNode {
    if (
      node.type !== NodeType.MODULE &&
      node.type !== NodeType.COMPONENT &&
      node.type !== NodeType.SYMBOL
    ) {
      return node;
    }

    // Symbol nodes already have fixed dimensions and port positions
    if (node.type === NodeType.SYMBOL) {
      return node;
    }

    // Get the main label dimensions
    const mainLabelHeight = node.labels?.[0]?.height || 0;
    const mainLabelWidth = node.labels?.[0]?.width || 0;

    // Group ports by side
    const portsBySide = {
      WEST: [] as ElkPort[],
      EAST: [] as ElkPort[],
      NORTH: [] as ElkPort[],
      SOUTH: [] as ElkPort[],
    };

    // Collect ports by their assigned sides
    for (const port of node.ports || []) {
      const side = port.properties?.["port.side"];
      if (side && side in portsBySide) {
        portsBySide[side as keyof typeof portsBySide].push(port);
      }
    }

    // Calculate required width and height based on port labels
    let requiredWidth = mainLabelWidth;
    let requiredHeight = mainLabelHeight;

    // Helper to get max label width for a set of ports
    const getMaxPortLabelWidth = (ports: ElkPort[]): number => {
      return Math.max(0, ...ports.map((port) => port.labels?.[0]?.width || 0));
    };

    // Helper to get total height needed for a set of ports
    const getTotalPortHeight = (ports: ElkPort[]): number => {
      return ports.reduce(
        (sum, port) => sum + (port.labels?.[0]?.height || 0) + 10,
        0
      ); // 10px spacing between ports
    };

    // Calculate width needed for left and right ports
    const leftPortsWidth = getMaxPortLabelWidth(portsBySide.WEST);
    const rightPortsWidth = getMaxPortLabelWidth(portsBySide.EAST);
    requiredWidth = Math.max(
      requiredWidth,
      leftPortsWidth + rightPortsWidth + 80 // Add padding and space between sides
    );

    // Calculate height needed for top and bottom ports
    const topPortsHeight = getTotalPortHeight(portsBySide.NORTH);
    const bottomPortsHeight = getTotalPortHeight(portsBySide.SOUTH);

    // Calculate height needed for left and right side ports
    const leftPortsHeight = getTotalPortHeight(portsBySide.WEST);
    const rightPortsHeight = getTotalPortHeight(portsBySide.EAST);

    // Take the maximum height needed
    requiredHeight = Math.max(
      requiredHeight + 40, // Add padding for the main label
      topPortsHeight + bottomPortsHeight + 60, // Add padding between top and bottom
      Math.max(leftPortsHeight, rightPortsHeight) + 40 // Height for side ports
    );

    // Update node dimensions
    return {
      ...node,
      width: Math.max(requiredWidth, node.width || 0),
      height: Math.max(requiredHeight, node.height || 0),
      properties: {
        ...node.properties,
        "elk.nodeSize.minimum": `(${requiredWidth}, ${requiredHeight})`,
      },
    };
  }

  async render(instance_ref: string): Promise<ElkGraph> {
    const graph = this._graphForInstance(instance_ref);

    const layoutOptions = {
      "elk.algorithm": "layered",
      "elk.direction": this.config.layout.direction,
      "elk.spacing.nodeNode": `${this.config.layout.spacing}`,
      "elk.layered.spacing.nodeNodeBetweenLayers": `${this.config.layout.spacing}`,
      "elk.padding": `[top=${this.config.layout.padding}, left=${this.config.layout.padding}, bottom=${this.config.layout.padding}, right=${this.config.layout.padding}]`,
      "elk.nodeSize.constraints": "NODE_LABELS PORTS PORT_LABELS MINIMUM_SIZE",
      "elk.partitioning.activate": "true",
      "elk.layered.nodePlacement.strategy": "NETWORK_SIMPLEX",
      "elk.portLabels.placement": "INSIDE NEXT_TO_PORT_IF_POSSIBLE",
    };

    // First pass - run layout with free port constraints
    console.log("First pass layout - running with free port constraints");
    const firstLayoutOptions = {
      ...layoutOptions,
      "elk.portConstraints": "FREE",
    };
    console.log(
      JSON.stringify({ ...graph, layoutOptions: firstLayoutOptions }, null, 2)
    );
    const firstPassLayout = await this.elk.layout(graph, {
      layoutOptions: firstLayoutOptions,
    });
    console.log("Output of first pass layout:");
    console.log(JSON.stringify(firstPassLayout, null, 2));

    // Analyze port positions and fix their sides
    const allNodes = this._getAllNodes(firstPassLayout);
    for (const node of allNodes) {
      if (node.type === NodeType.MODULE || node.type === NodeType.COMPONENT) {
        if (!node.ports) continue;

        const nodeWidth = node.width || 0;
        const nodeHeight = node.height || 0;

        // First pass: determine initial closest sides
        const westPorts: ElkPort[] = [];
        const eastPorts: ElkPort[] = [];
        const northSouthPorts: ElkPort[] = [];

        for (const port of node.ports) {
          if (port.x === undefined || port.y === undefined) continue;

          // Calculate distances to each edge
          const distToLeft = port.x;
          const distToRight = nodeWidth - port.x;
          const distToTop = port.y;
          const distToBottom = nodeHeight - port.y;

          // Find the minimum distance and its corresponding side
          const distances = [
            { side: "WEST", dist: distToLeft },
            { side: "EAST", dist: distToRight },
            { side: "NORTH", dist: distToTop },
            { side: "SOUTH", dist: distToBottom },
          ];

          const closestEdge = distances.reduce((min, curr) =>
            curr.dist < min.dist ? curr : min
          );

          // Group ports based on their closest edge
          if (closestEdge.side === "WEST") {
            westPorts.push(port);
          } else if (closestEdge.side === "EAST") {
            eastPorts.push(port);
          } else {
            // For NORTH or SOUTH ports, we'll redistribute them
            northSouthPorts.push(port);
          }
        }

        // Redistribute NORTH/SOUTH ports to balance WEST/EAST sides
        for (const port of northSouthPorts) {
          if (port.x === undefined) continue;

          // Determine which side to assign based on current balance
          const assignToWest = westPorts.length <= eastPorts.length;

          if (assignToWest) {
            westPorts.push(port);
          } else {
            eastPorts.push(port);
          }
        }

        // Assign final sides to all ports
        for (const port of westPorts) {
          port.properties = {
            ...port.properties,
            "port.side": "WEST",
          };
        }

        for (const port of eastPorts) {
          port.properties = {
            ...port.properties,
            "port.side": "EAST",
          };
        }

        // After assigning port sides, compute final dimensions
        const updatedNode =
          this._computeNodeDimensionsAfterPortAssignment(node);
        Object.assign(node, updatedNode);
      }
    }

    // Apply smart net reference positioning after port sides are assigned
    if (this.config.layout.smartNetReferencePositioning) {
      this._applySmartNetReferencePositioning({
        ...firstPassLayout,
        children: firstPassLayout.children || [],
      });
    }

    for (const node of firstPassLayout.children || []) {
      this._moveMetaNodePorts(node);
    }

    // Clear junction points and sections; they will be re-computed in the second pass
    for (const edge of firstPassLayout.edges || []) {
      edge.junctionPoints = [];
      edge.sections = [];
    }

    // Second pass - run layout with fixed port sides
    console.log("Second pass layout - running with fixed port sides");
    const secondLayoutOptions = {
      ...layoutOptions,
      "elk.portConstraints": "FIXED_SIDE",
      "elk.interactive": "true",
    };
    console.log(
      JSON.stringify({ ...graph, layoutOptions: secondLayoutOptions }, null, 2)
    );
    const secondPassLayout = await this.elk.layout(
      {
        ...graph,
        children: firstPassLayout.children || [],
      },
      {
        layoutOptions: secondLayoutOptions,
      }
    );

    console.log("Output of second pass layout:");
    console.log(JSON.stringify(secondPassLayout, null, 2));

    let flattenedGraph = this._flattenGraph({
      ...secondPassLayout,
      children: secondPassLayout.children || [],
      edges: secondPassLayout.edges || [],
    });

    console.log("Output of flattened graph:");
    console.log(JSON.stringify(flattenedGraph, null, 2));

    // Flatten the graph before returning
    return flattenedGraph;
  }

  _applySmartNetReferencePositioning(graph: ElkGraph): void {
    // For each net reference, analyze and potentially reverse edge directions
    const netRefNodes = graph.children.filter(
      (node) => node.type === NodeType.NET_REFERENCE
    );

    for (const netRefNode of netRefNodes) {
      // // Skip ground and VDD nets
      // if (
      //   netRefNode.netReferenceType === NetReferenceType.GROUND ||
      //   netRefNode.netReferenceType === NetReferenceType.VDD
      // ) {
      //   continue;
      // }

      // Find all edges connected to this net reference
      const connectedEdges = graph.edges.filter(
        (edge) =>
          edge.sourceComponentRef === netRefNode.id ||
          edge.targetComponentRef === netRefNode.id
      );

      if (connectedEdges.length === 0) continue;

      // Count connections to each side
      let westConnections = 0;
      let eastConnections = 0;

      for (const edge of connectedEdges) {
        // Find the other component in the edge
        const otherComponentId =
          edge.sourceComponentRef === netRefNode.id
            ? edge.targetComponentRef
            : edge.sourceComponentRef;

        const otherNode = graph.children.find((n) => n.id === otherComponentId);
        if (!otherNode) continue;

        // Only consider module/component/symbol nodes
        if (
          ![NodeType.MODULE, NodeType.COMPONENT, NodeType.SYMBOL].includes(
            otherNode.type
          )
        ) {
          continue;
        }

        // Find which port on the other node is connected
        const connectedPortId =
          edge.sourceComponentRef === netRefNode.id
            ? edge.targets[0]
            : edge.sources[0];

        const connectedPort = otherNode.ports?.find(
          (p) => p.id === connectedPortId
        );
        if (!connectedPort) continue;

        // Check which side the port is on
        const portSide = connectedPort.properties?.["port.side"];
        if (portSide === "WEST") {
          westConnections++;
        } else if (portSide === "EAST") {
          eastConnections++;
        }
      }

      // Determine if we need to reverse edges
      // If most connections are on WEST side, net reference should be SOURCE
      // If most connections are on EAST side, net reference should be DESTINATION
      const shouldNetRefBeSource = eastConnections >= westConnections;

      // Determine the port side for the net reference
      // If most connections come from WEST ports, the net reference port should be on EAST
      // If most connections come from EAST ports, the net reference port should be on WEST
      // Skip this for GROUND and VDD net references as they have specific sides
      if (
        netRefNode.netReferenceType !== NetReferenceType.GROUND &&
        netRefNode.netReferenceType !== NetReferenceType.VDD
      ) {
        const netRefPortSide =
          westConnections > eastConnections ? "EAST" : "WEST";

        // Update the net reference port properties
        if (netRefNode.ports && netRefNode.ports.length > 0) {
          const port = netRefNode.ports[0];
          port.properties = {
            ...port.properties,
            "port.side": netRefPortSide,
          };

          // Update port position based on the new side
          const nodeWidth = netRefNode.width || 0;
          const nodeHeight = netRefNode.height || 0;

          switch (netRefPortSide) {
            case "EAST":
              port.x = nodeWidth;
              port.y = nodeHeight / 2;
              break;
            case "WEST":
              port.x = 0;
              port.y = nodeHeight / 2;
              break;
          }
        }
      }

      // Update all connected edges
      for (const edge of connectedEdges) {
        const currentlyIsSource = edge.sourceComponentRef === netRefNode.id;

        if (shouldNetRefBeSource && !currentlyIsSource) {
          // Reverse the edge - make net reference the source
          const temp = edge.sources;
          edge.sources = edge.targets;
          edge.targets = temp;

          const tempRef = edge.sourceComponentRef;
          edge.sourceComponentRef = edge.targetComponentRef;
          edge.targetComponentRef = tempRef;
        } else if (!shouldNetRefBeSource && currentlyIsSource) {
          // Reverse the edge - make net reference the destination
          const temp = edge.sources;
          edge.sources = edge.targets;
          edge.targets = temp;

          const tempRef = edge.sourceComponentRef;
          edge.sourceComponentRef = edge.targetComponentRef;
          edge.targetComponentRef = tempRef;
        }
      }
    }
  }

  _getPowerInterfaceName(instance_ref: string): string | null {
    const instance = this.netlist.instances[instance_ref];
    if (!instance || instance.kind !== InstanceKind.INTERFACE) return null;

    // Check if this is a Power interface
    if (instance.type_ref.module_name !== "Power") return null;

    // Get the parent instance (the component that owns this interface)
    const parentRef = instance_ref.split(".").slice(0, -1).join(".");
    const parentInstance = this.netlist.instances[parentRef];
    if (!parentInstance || parentInstance.type_ref.module_name === "Capacitor")
      return null;

    return instance_ref.split(":").pop()?.split(".").pop() || null;
  }

  _generatePowerNetName(net: Set<string>): string[] {
    const powerNamesToLength: Map<string, number> = new Map();

    // Find all power interfaces connected to this net
    for (const portRef of Array.from(net)) {
      // Get the interface reference (everything up to the last dot)
      const interfaceRef = portRef.split(".").slice(0, -1).join(".");
      const powerName = this._getPowerInterfaceName(interfaceRef);
      if (powerName) {
        powerNamesToLength.set(powerName, interfaceRef.split(".").length);
      }
    }

    // Sort by length (prefer shorter names) and then alphabetically for deterministic behavior
    return Array.from(powerNamesToLength.entries())
      .sort((a, b) => {
        if (a[1] !== b[1]) return a[1] - b[1];
        return a[0].localeCompare(b[0]);
      })
      .map(([name]) => name);
  }

  _generateUniqueNetNames(): Map<string, string> {
    const netNames = new Map<string, string>();

    // Group nets by their top-level module
    const netsByModule = new Map<string, Map<string, Set<string>>>();

    for (const [netId, net] of Array.from(this.nets.entries())) {
      if (!this._isPowerNet(net)) continue;

      // Find the top-level module for this net
      let topLevelModule = "";
      for (const portRef of Array.from(net)) {
        const parts = (portRef as string).split(".");
        if (parts.length > 0) {
          // The first part is the filename, second part is the top-level module
          if (parts.length > 1) {
            topLevelModule = parts[1];
            break;
          }
        }
      }

      if (!topLevelModule) continue;

      // Initialize map for this module if it doesn't exist
      if (!netsByModule.has(topLevelModule)) {
        netsByModule.set(topLevelModule, new Map());
      }
      netsByModule.get(topLevelModule)!.set(netId, net);
    }

    // Process each module's nets separately
    for (const moduleNets of netsByModule.values()) {
      const usedNames = new Set<string>();

      // Collect all nets and their possible names for this module
      const netsAndNames = Array.from(moduleNets.entries())
        .map(([netId, net]: [string, Set<string>]) => ({
          netId,
          possibleNames: this._generatePowerNetName(net),
        }))
        // Sort by number of name options (handle nets with fewer options first)
        .sort((a, b) => a.possibleNames.length - b.possibleNames.length);

      // Process each net within this module
      for (const { netId, possibleNames } of netsAndNames) {
        let assigned = false;

        // Try each possible name in order
        if (possibleNames.length > 0) {
          for (const name of possibleNames) {
            const fullName = `${name}`;
            if (!usedNames.has(fullName)) {
              usedNames.add(fullName);
              netNames.set(netId, fullName);
              assigned = true;
              break;
            }
          }
        }

        // If we couldn't assign any of the preferred names, use the shortest one with a prime
        if (!assigned) {
          const baseName = possibleNames.length > 0 ? possibleNames[0] : "VDD";
          let uniqueName = `${baseName}`;
          let primeCount = 0;

          while (usedNames.has(uniqueName)) {
            primeCount++;
            uniqueName = `${baseName}${"'".repeat(primeCount)}`;
          }

          usedNames.add(uniqueName);
          netNames.set(netId, uniqueName);
        }
      }
    }

    return netNames;
  }

  /** Return true if all sub-ports of the given interface connect externally
   *  only to ports that share the same parent interface (or none).
   */
  private _canAggregateInterface(
    ifaceRef: string,
    parentModuleRef: string
  ): boolean {
    const ifaceInstance = this.netlist.instances[ifaceRef];
    if (!ifaceInstance) return false;

    const subPortNames: string[] = Object.keys(ifaceInstance.children || {});
    if (subPortNames.length === 0) return false; // nothing to aggregate

    const subPortIds = subPortNames.map((n) => `${ifaceRef}.${n}`);

    // Track which sub-ports had at least one external connection and record
    // the set of peer interfaces seen per sub-port.
    const subPortExternallyConnected = new Set<string>();
    const peerInterfacesPerSubPort: Record<string, Set<string>> = {};
    const extNamesPerSubPort: Record<string, Set<string>> = {};
    let touchedByAnyNet = false;

    // Iterate through each net in the design
    for (const net of Array.from(this.nets.values())) {
      // Sub-ports from this interface that participate in this net
      const subPortsInNet = subPortIds.filter((p) => net.has(p));
      if (subPortsInNet.length === 0) continue;

      touchedByAnyNet = true;

      // Does the net cross the parent-module boundary?
      const externalPortRefs = Array.from(net).filter(
        (pr) => !(pr as string).startsWith(parentModuleRef + ".")
      ) as string[];

      if (externalPortRefs.length === 0) {
        // Purely internal – ignore for aggregation logic
        continue;
      }

      // We have at least one external connection overall
      // For every participating sub-port, record the peer interface paths.
      const peerIfaces = externalPortRefs.map((pr) =>
        pr.split(".").slice(0, -1).join(".")
      );

      subPortsInNet.forEach((p) => {
        const name = p.split(".").pop() as string;
        subPortExternallyConnected.add(name);

        if (!peerInterfacesPerSubPort[name]) {
          peerInterfacesPerSubPort[name] = new Set<string>();
        }
        peerIfaces.forEach((ifacePath) =>
          peerInterfacesPerSubPort[name].add(ifacePath)
        );
      });

      // Map external port names for each participating sub-port
      externalPortRefs.forEach((extRef) => {
        const extPortName = extRef.split(".").pop() as string;
        subPortsInNet.forEach((p) => {
          const name = p.split(".").pop() as string;
          if (!extNamesPerSubPort[name]) {
            extNamesPerSubPort[name] = new Set<string>();
          }
          extNamesPerSubPort[name].add(extPortName);
        });
      });

      // Pin-name matching handled later.
    }

    // If the interface is never referenced by any net at all, aggregate.
    if (!touchedByAnyNet) return true;

    // If no sub-port ever left the module (all connections internal), aggregate too
    if (subPortExternallyConnected.size === 0) return true;

    // Otherwise, ensure every declared sub-port saw at least one external connection
    if (subPortExternallyConnected.size !== subPortNames.length) return false;

    // Finally, verify that each sub-port connects to an identical set of peer interfaces.
    const referencePeers =
      peerInterfacesPerSubPort[subPortNames[0]] || new Set();
    for (const name of subPortNames) {
      const peers = peerInterfacesPerSubPort[name] || new Set();
      if (peers.size !== referencePeers.size) return false;
      for (const p of Array.from(peers)) {
        if (!referencePeers.has(p)) return false;
      }

      // Enforce pin-name matching: each sub-port must only connect to external ports of the same name
      const extNames = extNamesPerSubPort[name] || new Set<string>();
      if (extNames.size > 0 && (!extNames.has(name) || extNames.size > 1)) {
        return false;
      }
    }

    return true;
  }

  /**
   * Given a port ID, return the ID that should be used for connectivity
   * computation – i.e. the aggregated interface port if this port was
   * collapsed, otherwise the original ID.
   */
  private _collapsePortId(portId: string): string {
    return this.interfacePortCollapse.get(portId) || portId;
  }

  /**
   * Utility to determine the canonical names for the two electrical ports of a
   * passive two-terminal component (resistor, capacitor, inductor, etc.).  The
   * generic components in Atopile tend to expose their pins as `P1`/`P2`, but
   * earlier versions of the schematic renderer assumed lowercase `p1`/`p2`.
   * Because net connectivity is case-sensitive, using the wrong case will cause
   * the renderer to miss the link entirely.  This method looks at the child
   * instance names to find the best match.
   */
  private _getPassivePortNames(instance_ref: string): {
    p1Name: string;
    p2Name: string;
  } {
    const instance = this.netlist.instances[instance_ref];
    if (!instance) {
      throw new Error(`Instance ${instance_ref} not found`);
    }

    const childNames = Object.keys(instance.children || {});

    // Attempt to find explicit P1/P2 (case-insensitive) first
    const matchName = (wanted: string, fallbackIdx: number): string => {
      const exact = childNames.find(
        (n) => n.toLowerCase() === wanted.toLowerCase()
      );
      if (exact) return exact;
      // Fallback to deterministic index if the expected name is absent.
      if (childNames.length > fallbackIdx) return childNames[fallbackIdx];
      // Ultimately just return the wanted string so that caller can still
      // construct a reasonable port ID even when the child is missing.
      return wanted;
    };

    const p1Name = matchName("P1", 0);
    const p2Name = matchName("P2", 1);

    return { p1Name, p2Name };
  }

  _applySmartEdgeSplitting(graph: ElkGraph): ElkGraph {
    if (!this.config.layout.smartEdgeSplitting) {
      return graph;
    }

    const newNodes: ElkNode[] = [...graph.children];
    const newEdges: ElkEdge[] = [];
    const blockTypes = [NodeType.MODULE, NodeType.COMPONENT, NodeType.SYMBOL];

    // Build a map of which edges belong to which net
    const netToEdges = new Map<string, ElkEdge[]>();
    for (const edge of graph.edges) {
      if (!netToEdges.has(edge.netId)) {
        netToEdges.set(edge.netId, []);
      }
      netToEdges.get(edge.netId)!.push(edge);
    }

    // Process each edge
    for (const edge of graph.edges) {
      const sourceNode = graph.children.find(
        (n) => n.id === edge.sourceComponentRef
      );
      const targetNode = graph.children.find(
        (n) => n.id === edge.targetComponentRef
      );

      if (!sourceNode || !targetNode) {
        newEdges.push(edge);
        continue;
      }

      // Check if both nodes are blocks (not passives or net references)
      const sourceIsBlock = blockTypes.includes(sourceNode.type);
      const targetIsBlock = blockTypes.includes(targetNode.type);

      // Check if at least one node is a passive
      const passiveTypes = [
        NodeType.RESISTOR,
        NodeType.CAPACITOR,
        NodeType.INDUCTOR,
      ];
      const sourceIsPassive = passiveTypes.includes(sourceNode.type);
      const targetIsPassive = passiveTypes.includes(targetNode.type);

      // Apply smart splitting if:
      // 1. Both nodes are blocks, OR
      // 2. One node is a block and the other is a passive
      if (
        (sourceIsBlock && targetIsBlock) ||
        (sourceIsBlock && targetIsPassive) ||
        (sourceIsPassive && targetIsBlock)
      ) {
        // Check if this net has other connections beyond this single edge
        const netEdges = netToEdges.get(edge.netId) || [];

        // Also check if the net has connections outside this graph (external connections)
        const net = this.nets.get(edge.netId);
        const hasExternalConnections =
          net &&
          Array.from(net).some(
            (portRef) => !portRef.startsWith(graph.id + ".")
          );

        // Only split if there are other edges in this net or external connections
        if (netEdges.length > 1 || hasExternalConnections) {
          // Create net references for both sides
          const netId = edge.netId;
          const sourceNetRefId = `${netId}_split_${edge.sourceComponentRef.replace(
            /\./g,
            "_"
          )}`;
          const targetNetRefId = `${netId}_split_${edge.targetComponentRef.replace(
            /\./g,
            "_"
          )}`;

          // Create source-side net reference
          const sourceNetRef = this._netReferenceNode(
            sourceNetRefId,
            netId,
            "WEST", // Default side, will be adjusted by smart positioning
            NetReferenceType.NORMAL
          );
          sourceNetRef.netId = netId;

          // Create target-side net reference
          const targetNetRef = this._netReferenceNode(
            targetNetRefId,
            netId,
            "EAST", // Default side, will be adjusted by smart positioning
            NetReferenceType.NORMAL
          );
          targetNetRef.netId = netId;

          // Add the new net reference nodes
          newNodes.push(sourceNetRef);
          newNodes.push(targetNetRef);

          // Create edge from source block to source net reference
          newEdges.push({
            id: `${edge.id}_split_source`,
            sources: edge.sources,
            targets: [sourceNetRef.ports![0].id],
            sourceComponentRef: edge.sourceComponentRef,
            targetComponentRef: sourceNetRefId,
            netId: edge.netId,
            properties: edge.properties,
          });

          // Create edge from target net reference to target block
          newEdges.push({
            id: `${edge.id}_split_target`,
            sources: [targetNetRef.ports![0].id],
            targets: edge.targets,
            sourceComponentRef: targetNetRefId,
            targetComponentRef: edge.targetComponentRef,
            netId: edge.netId,
            properties: edge.properties,
          });
        } else {
          // Keep the edge as is - it's a direct connection with no other connections
          newEdges.push(edge);
        }
      } else {
        // Keep the edge as is
        newEdges.push(edge);
      }
    }

    return {
      ...graph,
      children: newNodes,
      edges: newEdges,
    };
  }
}
