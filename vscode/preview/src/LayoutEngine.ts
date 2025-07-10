import ELK from "elkjs/lib/elk.bundled.js";
import type { ELK as ELKType } from "elkjs/lib/elk-api";
import { InstanceKind } from "./types/NetlistTypes";
import type { Netlist, AttributeValue } from "./types/NetlistTypes";
import { createCanvas } from "canvas";
import { getKicadSymbolInfo } from "./renderer/kicad_sym";
import * as LZString from "lz-string";

// Re-export all the public types and enums from the old implementation
export enum NodeType {
  META = "meta",
  MODULE = "module",
  COMPONENT = "component",
  NET_JUNCTION = "net_junction",
  NET_REFERENCE = "net_reference",
  SYMBOL = "symbol",
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
  children?: ElkNode[];
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
  netJunction: {
    width: number;
    height: number;
  };
  netReference: {
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

  // Layout configuration
  layout: {
    // Direction of the layout - will be passed to ELK
    direction: "LEFT" | "RIGHT" | "UP" | "DOWN";
    // Spacing between nodes
    spacing: number;
    // Padding around the entire layout
    padding: number;
    // Create hierarchical nodes for symbols with vertical net references
    hierarchicalSymbols?: boolean;
  };

  // Visual configuration
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
    netJunction: {
      width: 10,
      height: 10,
    },
    netReference: {
      width: 10,
      height: 10,
    },
    symbol: {
      width: 100,
      height: 100,
    },
  },
  layout: {
    direction: "LEFT",
    spacing: 0,
    padding: 0,
    hierarchicalSymbols: false,
  },
  visual: {
    showPortLabels: true,
    showComponentValues: true,
    showFootprints: true,
  },
};

// Utility function - keeping it outside the class as in the original
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

export class SchematicLayoutEngine {
  netlist: Netlist;
  elk: ELKType;
  nets: Map<string, Set<string>>;
  config: SchematicConfig;

  constructor(netlist: Netlist, config: Partial<SchematicConfig> = {}) {
    this.netlist = netlist;
    // Use the default ELK configuration which works in the browser
    this.elk = new ELK();
    this.nets = this._generateNets();
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

  /**
   * Get the nets map
   */
  getNets(): Map<string, Set<string>> {
    return this.nets;
  }

  /**
   * Get root module instances
   */
  roots(): string[] {
    return Object.keys(this.netlist.instances).filter(
      (instance_ref) =>
        this.netlist.instances[instance_ref].kind === InstanceKind.MODULE
    );
  }

  /**
   * Create a node for the given instance
   */
  public _nodeForInstance(instance_ref: string): ElkNode | null {
    const instance = this.netlist.instances[instance_ref];
    if (!instance) {
      throw new Error(`Instance ${instance_ref} not found`);
    }

    if ([InstanceKind.MODULE, InstanceKind.COMPONENT].includes(instance.kind)) {
      // Check if this component has a __symbol_value attribute
      const symbolValueAttr = instance.attributes.__symbol_value;
      const hasSymbolValue =
        symbolValueAttr &&
        (typeof symbolValueAttr === "string" ||
          (typeof symbolValueAttr === "object" && "String" in symbolValueAttr));

      if (hasSymbolValue) {
        return this._symbolNode(instance_ref);
      } else {
        return this._moduleNode(instance_ref);
      }
    }

    return null;
  }

  /**
   * Create a graph for the given instance
   */
  public _graphForInstance(instance_ref: string): ElkGraph {
    const instance = this.netlist.instances[instance_ref];

    if (!instance) {
      // If instance not found, try to find all top-level instances in the file
      const instances = Object.keys(this.netlist.instances).filter(
        (sub_instance_ref) => {
          const [filename, path] = sub_instance_ref.split(":");
          return filename === instance_ref.split(":")[0] && !path.includes(".");
        }
      );

      return {
        id: instance_ref,
        children: instances
          .map((ref) => this._nodeForInstance(ref))
          .filter((node) => node !== null) as ElkNode[],
        edges: [],
      };
    }

    // Collect all nodes, applying auto-explode logic
    const nodes: ElkNode[] = [];

    // Process all children
    for (const child_ref of Object.values(instance.children)) {
      const child_instance = this.netlist.instances[child_ref];
      if (!child_instance) continue;

      // Only process module and component children
      if (
        child_instance.kind === InstanceKind.MODULE ||
        child_instance.kind === InstanceKind.COMPONENT
      ) {
        // Use auto-explode logic to collect nodes
        nodes.push(...this._collectNodesWithAutoExplode(child_ref));
      }
    }

    // Create the graph
    const graph: ElkGraph = {
      id: instance_ref,
      children: nodes,
      edges: [],
    };

    // Add connectivity (edges and net references)
    return this._addConnectivity(graph);
  }

  /**
   * Layout the schematic for the given instance
   */
  async layout(instance_ref: string): Promise<ElkGraph> {
    const graph = this._graphForInstance(instance_ref);

    // Basic layout options
    const layoutOptions = {
      "elk.algorithm": "layered",
      "elk.direction": this.config.layout.direction,
      "elk.spacing.nodeNode": `${this.config.layout.spacing}`,
      "elk.layered.spacing.nodeNodeBetweenLayers": `${this.config.layout.spacing}`,
      "elk.padding": `[top=${this.config.layout.padding}, left=${this.config.layout.padding}, bottom=${this.config.layout.padding}, right=${this.config.layout.padding}]`,
      "elk.nodeSize.constraints": "NODE_LABELS PORTS PORT_LABELS MINIMUM_SIZE",
      "elk.portConstraints": "FIXED_ORDER",
      "elk.portLabels.placement": "INSIDE NEXT_TO_PORT_IF_POSSIBLE",
    };

    // Create pre-layout graph with noLayout option for debugging
    const preLayoutGraph = {
      ...graph,
      layoutOptions: {
        noLayout: true,
      },
    };

    // Generate debugging link for pre-layout graph
    const preLayoutJson = JSON.stringify(preLayoutGraph, null, 2);
    const preLayoutCompressed =
      LZString.compressToEncodedURIComponent(preLayoutJson);
    console.log("Pre-layout ELK Live link:");
    console.log(
      `https://rtsys.informatik.uni-kiel.de/elklive/json.html?compressedContent=${preLayoutCompressed}`
    );

    // Create the graph with layout options for actual layout
    const graphWithOptions = {
      ...graph,
      layoutOptions: layoutOptions,
    };

    // Run the layout
    const layoutedGraph = await this.elk.layout(graphWithOptions);

    // Generate debugging link for post-layout graph
    const postLayoutJson = JSON.stringify(layoutedGraph, null, 2);
    const postLayoutCompressed =
      LZString.compressToEncodedURIComponent(postLayoutJson);
    console.log("\nPost-layout ELK Live link:");
    console.log(
      `https://rtsys.informatik.uni-kiel.de/elklive/json.html?compressedContent=${postLayoutCompressed}`
    );

    const flattenedGraph = this._flattenGraph(layoutedGraph);

    // Ensure the graph has the required properties
    return flattenedGraph;
  }

  // Private helper methods
  private _generateNets(): Map<string, Set<string>> {
    const nets = new Map<string, Set<string>>();

    if (!this.netlist.nets) {
      return nets;
    }

    for (const [netId, net] of Object.entries(this.netlist.nets)) {
      nets.set(netId, new Set(net.ports));
    }

    return nets;
  }

  // Helper methods from old implementation
  private _getAttributeValue(
    attr: AttributeValue | string | undefined
  ): string | null {
    if (!attr) return null;
    if (typeof attr === "string") return attr;
    if (attr.String) return attr.String;
    if (attr.Boolean !== undefined) return String(attr.Boolean);
    if (attr.Number !== undefined) return String(attr.Number);
    return null;
  }

  private _renderValue(
    value: string | AttributeValue | undefined
  ): string | undefined {
    if (typeof value === "string") return value;
    if (value?.String) return value.String;
    if (value?.Number !== undefined) return String(value.Number);
    if (value?.Boolean !== undefined) return String(value.Boolean);
    if (value?.Physical !== undefined) return String(value.Physical);

    return undefined;
  }

  private _symbolNode(instance_ref: string): ElkNode | null {
    const instance = this.netlist.instances[instance_ref];
    if (!instance) return null;

    // Check if we have __symbol_value attribute
    const symbolValueAttr = instance.attributes.__symbol_value;
    let symbolContent: string | undefined;

    if (typeof symbolValueAttr === "string") {
      symbolContent = symbolValueAttr;
    } else if (
      symbolValueAttr &&
      typeof symbolValueAttr === "object" &&
      "String" in symbolValueAttr
    ) {
      symbolContent = (symbolValueAttr as any).String;
    }

    // If we don't have symbol content, fall back to module node
    if (!symbolContent) {
      return this._moduleNode(instance_ref);
    }

    try {
      // Get symbol info including bounding box and pin endpoints
      const symbolInfo = getKicadSymbolInfo(symbolContent, undefined, {
        unit: 1,
        bodyStyle: 1,
        tightBounds: false, // Include pins in the bounding box
      });

      // Calculate node size based on symbol bounding box
      const scale = 10;
      const nodeWidth = symbolInfo.bbox.w * scale;
      const nodeHeight = symbolInfo.bbox.h * scale;

      console.log("Node", instance_ref, "size:", nodeWidth, nodeHeight);

      // Get reference designator and value
      const refDes = instance.reference_designator;
      const value = this._renderValue(instance.attributes.value);
      const footprint = this._getAttributeValue(instance.attributes.package);

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
        },
      };

      // Create ports based on pin endpoints
      for (const pinEndpoint of symbolInfo.pinEndpoints) {
        // Find the corresponding port in the instance children
        let portName = pinEndpoint.name;
        let portRef = `${instance_ref}.${portName}`;

        // If the pin name is ~ (unnamed), try to match by pin number
        if (portName === "~" && pinEndpoint.number) {
          const childNames = Object.keys(instance.children || {});
          const pinNumberMatch = childNames.find((name) => {
            return name.toLowerCase() === `p${pinEndpoint.number}`;
          });

          if (pinNumberMatch) {
            portName = pinNumberMatch;
            portRef = `${instance_ref}.${pinNumberMatch}`;
          }
        } else {
          // Check if this port exists in the instance children
          const childNames = Object.keys(instance.children || {});
          const matchingChild = childNames.find((name) => {
            // Try exact match first
            if (name === portName) return true;
            // Try case-insensitive match
            if (name.toLowerCase() === portName.toLowerCase()) return true;
            // Try matching by pin number
            const childInstance =
              this.netlist.instances[instance.children[name]];
            if (childInstance && childInstance.kind === InstanceKind.PORT) {
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
        }

        // Calculate port position relative to node
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
                  text:
                    pinEndpoint.name === "~"
                      ? pinEndpoint.number || "~"
                      : portName,
                  width: calculateTextDimensions(
                    pinEndpoint.name === "~"
                      ? pinEndpoint.number || "~"
                      : portName,
                    10
                  ).width,
                  height: calculateTextDimensions(
                    pinEndpoint.name === "~"
                      ? pinEndpoint.number || "~"
                      : portName,
                    10
                  ).height,
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
      // Fall back to module node
      return this._moduleNode(instance_ref);
    }
  }

  private _moduleNode(instance_ref: string): ElkNode {
    const instance = this.netlist.instances[instance_ref];
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
    let minWidth = Math.max(sizes.width, mainLabelDimensions.width + 20);
    let minHeight = Math.max(sizes.height, mainLabelDimensions.height + 20);

    const node: ElkNode = {
      id: instance_ref,
      type: NodeType.MODULE,
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

    // Add ports for all children (no interface aggregation)
    for (const [child_name, child_ref] of Object.entries(instance.children)) {
      const child_instance = this.netlist.instances[child_ref];
      if (!child_instance) {
        throw new Error(`Child ${child_ref} not found`);
      }

      if (child_instance.kind === InstanceKind.PORT) {
        const port_ref = `${instance_ref}.${child_name}`;
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

        // Update minimum dimensions
        minWidth = Math.max(minWidth, portLabelDimensions.width * 2 + 60);
        minHeight = Math.max(
          minHeight,
          mainLabelDimensions.height + portLabelDimensions.height * 2 + 40
        );
      } else if (child_instance.kind === InstanceKind.INTERFACE) {
        // Show all interface ports individually (no aggregation)
        for (const port_name of Object.keys(child_instance.children)) {
          const full_port_ref = `${instance_ref}.${child_name}.${port_name}`;
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

          // Update minimum dimensions
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

      // Natural sort for ports
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
        const aName = a.id.split(".").pop() || "";
        const bName = b.id.split(".").pop() || "";
        return naturalCompare(aName, bName);
      });

      // Assign ports to sides
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

  /**
   * Create a simple net reference node for a net
   */
  private _netReferenceNode(
    ref_id: string,
    netName: string,
    side: "NORTH" | "WEST" | "SOUTH" | "EAST" = "WEST"
  ): ElkNode {
    // Calculate label dimensions
    const labelDimensions = calculateTextDimensions(netName, 12);

    // Use configured size for net reference, but expand for label
    const baseWidth = this.config.nodeSizes.netReference.width;
    const baseHeight = this.config.nodeSizes.netReference.height;
    const nodeWidth = Math.max(baseWidth, labelDimensions.width + 20);
    const nodeHeight = Math.max(baseHeight, 20);

    // Calculate port position based on side
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
      netId: netName,
      labels: [
        {
          text: netName,
          x:
            side === "EAST"
              ? -labelDimensions.width - 5
              : side === "WEST"
              ? nodeWidth + 5
              : 10,
          y:
            side === "NORTH" || side === "SOUTH"
              ? -labelDimensions.height - 5
              : (nodeHeight - labelDimensions.height) / 2,
          width: labelDimensions.width,
          height: labelDimensions.height,
          textAlign: "center" as const,
        },
      ],
      ports: [
        {
          id: `${ref_id}.port`,
          x: portX,
          y: portY,
          width: 0,
          height: 0,
          properties: {
            "port.side": side,
            "port.alignment": "CENTER",
          },
        },
      ],
      properties: {
        "elk.portConstraints": "FIXED_POS",
        "elk.nodeSize.constraints": "MINIMUM_SIZE",
        "elk.nodeSize.minimum": `(${nodeWidth}, ${nodeHeight})`,
      },
    };
  }

  /**
   * Recursively collect nodes from a module, auto-exploding single-child modules
   */
  private _collectNodesWithAutoExplode(instance_ref: string): ElkNode[] {
    const instance = this.netlist.instances[instance_ref];
    if (!instance) {
      return [];
    }

    // If this is a component, just return it as a node
    if (instance.kind === InstanceKind.COMPONENT) {
      const node = this._nodeForInstance(instance_ref);
      return node ? [node] : [];
    }

    // If this is a module, always auto-explode
    if (instance.kind === InstanceKind.MODULE) {
      // Find all module/component children
      const childNodes: ElkNode[] = [];

      for (const child_ref of Object.values(instance.children)) {
        const child_instance = this.netlist.instances[child_ref];
        if (!child_instance) continue;

        if (
          child_instance.kind === InstanceKind.MODULE ||
          child_instance.kind === InstanceKind.COMPONENT
        ) {
          // Recursively collect from this child
          childNodes.push(...this._collectNodesWithAutoExplode(child_ref));
        }
      }

      // If we found children, return them; otherwise show this module as a node
      if (childNodes.length > 0) {
        return childNodes;
      }
    }

    // Otherwise, this module should be shown as a node
    const node = this._nodeForInstance(instance_ref);
    return node ? [node] : [];
  }

  /**
   * Add connectivity to the graph by creating net references for each net
   */
  private _addConnectivity(graph: ElkGraph): ElkGraph {
    // First pass: collect all net references that would be created
    const nodeNetReferences: Map<
      string,
      Array<{
        netRefNode: ElkNode;
        edge: ElkEdge;
        portSide: "NORTH" | "SOUTH" | "EAST" | "WEST";
      }>
    > = new Map();

    // For each net in the netlist
    for (const [netId, net] of this.nets.entries()) {
      // Find all ports in this graph that are connected to this net
      const connectedPorts: { portId: string; nodeId: string }[] = [];

      // Check all nodes in the graph
      for (const node of graph.children) {
        if (!node.ports) continue;

        // Check each port on the node
        for (const port of node.ports) {
          if (net.has(port.id)) {
            connectedPorts.push({ portId: port.id, nodeId: node.id });
            // Mark the port as connected to this net
            port.netId = netId;
          }
        }
      }

      // Create a separate net reference for each connected port
      if (connectedPorts.length >= 1) {
        // Get the net name from the netlist
        const net = this.netlist.nets[netId];
        const netName = net?.name || netId;

        // Create a net reference for each connected port
        for (let i = 0; i < connectedPorts.length; i++) {
          const { portId, nodeId } = connectedPorts[i];

          // Find the port to get its side
          let portSide: "NORTH" | "SOUTH" | "EAST" | "WEST" = "WEST";
          const node = graph.children.find((n) => n.id === nodeId);
          if (node && node.ports) {
            const port = node.ports.find((p) => p.id === portId);
            if (port && port.properties && port.properties["port.side"]) {
              portSide = port.properties["port.side"] as
                | "NORTH"
                | "SOUTH"
                | "EAST"
                | "WEST";
            }
          }

          // Determine opposite side for net reference
          let netRefSide: "NORTH" | "SOUTH" | "EAST" | "WEST";
          switch (portSide) {
            case "NORTH":
              netRefSide = "SOUTH";
              break;
            case "SOUTH":
              netRefSide = "NORTH";
              break;
            case "EAST":
              netRefSide = "WEST";
              break;
            case "WEST":
              netRefSide = "EAST";
              break;
          }

          // Create a unique net reference for this port
          const netRefId = `${netId}_ref_${i}`;
          const netRefNode = this._netReferenceNode(
            netRefId,
            netName,
            netRefSide
          );

          // Create edge from the port to its dedicated net reference
          const edge: ElkEdge = {
            id: `${portId}_to_${netRefId}`,
            netId: netId,
            sources: [portId],
            targets: [netRefNode.ports![0].id],
            sourceComponentRef: nodeId,
            targetComponentRef: netRefId,
          };

          // Store the net reference info grouped by node
          if (!nodeNetReferences.has(nodeId)) {
            nodeNetReferences.set(nodeId, []);
          }
          nodeNetReferences.get(nodeId)!.push({
            netRefNode,
            edge,
            portSide,
          });
        }
      }
    }

    // Second pass: process nodes and create hierarchical nodes where needed
    const processedNodes = new Set<string>();
    const newChildren: ElkNode[] = [];
    const newEdges: ElkEdge[] = [];

    for (const node of graph.children) {
      if (processedNodes.has(node.id)) continue;

      const netRefs = nodeNetReferences.get(node.id) || [];
      const verticalNetRefs = netRefs.filter(
        (ref) => ref.portSide === "NORTH" || ref.portSide === "SOUTH"
      );
      const horizontalNetRefs = netRefs.filter(
        (ref) => ref.portSide === "EAST" || ref.portSide === "WEST"
      );

      // If the node has vertical net references, create a hierarchical node
      if (
        verticalNetRefs.length > 0 &&
        node.type === NodeType.SYMBOL &&
        this.config.layout.hierarchicalSymbols
      ) {
        const hierarchicalNode = this._createHierarchicalNode(
          node,
          verticalNetRefs,
          horizontalNetRefs
        );
        newChildren.push(hierarchicalNode);

        // Add edges for horizontal net references (they stay outside)
        for (const ref of horizontalNetRefs) {
          newChildren.push(ref.netRefNode);
          newEdges.push(ref.edge);
        }

        processedNodes.add(node.id);
      } else {
        // No vertical net references, add node and all its net references normally
        newChildren.push(node);
        for (const ref of netRefs) {
          newChildren.push(ref.netRefNode);
          newEdges.push(ref.edge);
        }
        processedNodes.add(node.id);
      }
    }

    // Update the graph with new children and edges
    graph.children = newChildren;
    graph.edges = [...graph.edges, ...newEdges];

    return graph;
  }

  /**
   * Create a hierarchical node containing a symbol and its vertical net references
   */
  private _createHierarchicalNode(
    symbolNode: ElkNode,
    verticalNetRefs: Array<{
      netRefNode: ElkNode;
      edge: ElkEdge;
      portSide: "NORTH" | "SOUTH" | "EAST" | "WEST";
    }>,
    horizontalNetRefs: Array<{
      netRefNode: ElkNode;
      edge: ElkEdge;
      portSide: "NORTH" | "SOUTH" | "EAST" | "WEST";
    }>
  ): ElkNode {
    const hierarchicalId = `${symbolNode.id}_hierarchical`;

    // Create internal edges for vertical net references
    const internalEdges: ElkEdge[] = verticalNetRefs.map((ref) => ({
      ...ref.edge,
      // Update the edge to reference the internal nodes
      sourceComponentRef: ref.edge.sourceComponentRef.replace(
        symbolNode.id,
        symbolNode.id
      ),
      targetComponentRef: ref.netRefNode.id,
    }));

    // Create the hierarchical node
    const hierarchicalNode: ElkNode = {
      id: hierarchicalId,
      type: NodeType.MODULE, // Use MODULE type for hierarchical nodes
      children: [symbolNode, ...verticalNetRefs.map((ref) => ref.netRefNode)],
      edges: internalEdges,
      ports: [],
      labels: [],
      properties: {
        "elk.algorithm": "layered",
        "elk.direction": "DOWN", // Vertical layout for this subgraph
        "elk.spacing.nodeNode": "10",
        "elk.layered.spacing.nodeNodeBetweenLayers": "20",
        "elk.padding": "[top=10, left=10, bottom=10, right=10]",
        "elk.nodeSize.constraints":
          "NODE_LABELS PORTS PORT_LABELS MINIMUM_SIZE",
        "elk.portConstraints": "FIXED_ORDER",
      },
    };

    // Create ports on the hierarchical node for horizontal connections
    // These will be used to connect horizontal net references from outside
    for (const port of symbolNode.ports || []) {
      // Only expose ports that have horizontal connections
      const hasHorizontalConnection = horizontalNetRefs.some((ref) =>
        ref.edge.sources.includes(port.id)
      );

      if (hasHorizontalConnection) {
        const hierarchicalPort: ElkPort = {
          id: port.id.replace(symbolNode.id, hierarchicalId),
          x: port.x,
          y: port.y,
          width: port.width,
          height: port.height,
          labels: port.labels,
          properties: port.properties,
          netId: port.netId,
        };
        hierarchicalNode.ports!.push(hierarchicalPort);
      }
    }

    // Update horizontal net reference edges to connect to the hierarchical node's ports
    for (const ref of horizontalNetRefs) {
      // Update the edge source to use the hierarchical node's port
      ref.edge.sources = ref.edge.sources.map((source) =>
        source.replace(symbolNode.id, hierarchicalId)
      );
      ref.edge.sourceComponentRef = hierarchicalId;
    }

    return hierarchicalNode;
  }

  /**
   * Flatten a hierarchical graph by extracting all nested nodes and edges
   * This is useful after layout when we want to render everything at the same level
   */
  private _flattenGraph(graph: ElkGraph): ElkGraph {
    const flattenedNodes: ElkNode[] = [];
    const flattenedEdges: ElkEdge[] = [];

    // Helper function to recursively process nodes
    const processNode = (
      node: ElkNode,
      parentX: number = 0,
      parentY: number = 0
    ) => {
      // If this is a hierarchical container node (has children)
      if (node.children && node.children.length > 0) {
        // Process all child nodes, adjusting their positions relative to parent
        for (const child of node.children) {
          processNode(child, parentX + (node.x || 0), parentY + (node.y || 0));
        }

        // Process all edges within this hierarchical node
        if (node.edges) {
          for (const edge of node.edges) {
            // Adjust edge positions if they have layout information
            if (edge.sections) {
              for (const section of edge.sections) {
                if (section.startPoint) {
                  section.startPoint.x += parentX + (node.x || 0);
                  section.startPoint.y += parentY + (node.y || 0);
                }
                if (section.endPoint) {
                  section.endPoint.x += parentX + (node.x || 0);
                  section.endPoint.y += parentY + (node.y || 0);
                }
                if (section.bendPoints) {
                  for (const bendPoint of section.bendPoints) {
                    bendPoint.x += parentX + (node.x || 0);
                    bendPoint.y += parentY + (node.y || 0);
                  }
                }
              }
            }
            flattenedEdges.push(edge);
          }
        }
      } else {
        // This is a leaf node, add it to the flattened list
        // Adjust its position based on parent offset
        const flatNode = {
          ...node,
          x: (node.x || 0) + parentX,
          y: (node.y || 0) + parentY,
        };
        flattenedNodes.push(flatNode);
      }
    };

    // Process all top-level nodes
    for (const node of graph.children) {
      processNode(node);
    }

    // Add all top-level edges
    flattenedEdges.push(...graph.edges);

    return {
      id: graph.id,
      children: flattenedNodes,
      edges: flattenedEdges,
    };
  }
}
