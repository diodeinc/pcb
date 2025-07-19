import ELK from "elkjs/lib/elk.bundled.js";
import type { ELK as ELKType } from "elkjs/lib/elk-api";
import { InstanceKind } from "./types/NetlistTypes";
import type { Netlist, AttributeValue, Net } from "./types/NetlistTypes";
import { createCanvas } from "canvas";
import { getKicadSymbolInfo } from "./renderer/kicad_sym";
import * as LZString from "lz-string";
import {
  LibavoidEdgeRouter,
  type Obstacle,
  type Port,
  type Hyperedge,
} from "./LibavoidEdgeRouter";

// Re-export all the public types and enums from the old implementation
export enum NodeType {
  META = "meta",
  MODULE = "module",
  COMPONENT = "component",
  NET_JUNCTION = "net_junction",
  NET_REFERENCE = "net_reference",
  SYMBOL = "symbol",
}

// New interfaces for node positioning and rotation
export interface NodePosition {
  x: number;
  y: number;
  width?: number; // Optional, in case user resizes
  height?: number; // Optional, in case user resizes
  rotation?: number; // Rotation in degrees (0-360)
}

export interface NodePositions {
  [nodeId: string]: NodePosition;
}

export interface LayoutResult extends ElkGraph {
  // Additional metadata about the layout
  nodePositions: NodePositions;
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
  layoutOptions?: Record<string, string>;
  type: NodeType;
  netId?: string; // Only used for net reference nodes
  children?: ElkNode[];
  edges?: ElkEdge[];
  rotation?: number; // Rotation in degrees
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
  sections?: {
    id: string;
    startPoint: { x: number; y: number };
    endPoint: { x: number; y: number };
    bendPoints?: { x: number; y: number }[];
  }[];
  properties?: Record<string, string>;
  junctionPoints?: { x: number; y: number }[];
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
    // Distance threshold for creating edges between pins on the same net
    netConnectionThreshold: number;
    // Whether to hide net labels on ports that have edges
    hideLabelsOnConnectedPorts: boolean;
    // Grid snapping configuration
    gridSnap: {
      enabled: boolean;
      size: number; // Grid size in pixels (50mil converted to pixels)
    };
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
    spacing: 20,
    padding: 20,
    netConnectionThreshold: 300, // Default to 300 units (about 1-2 node widths)
    hideLabelsOnConnectedPorts: true,
    gridSnap: {
      enabled: true,
      size: 12.7, // 50mil = 1.27mm, at 10x scale = 12.7 pixels
    },
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
  fontWeight: string = "normal",
  paddingWidth: number = 15,
  paddingHeight: number = 5
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

  return {
    width: width + paddingWidth * 2,
    height: height + paddingHeight * 2,
  };
}

// Utility functions for grid snapping
function snapToGrid(value: number, gridSize: number): number {
  return Math.round(value / gridSize) * gridSize;
}

function snapPosition(
  x: number,
  y: number,
  gridSize: number
): { x: number; y: number } {
  return {
    x: snapToGrid(x, gridSize),
    y: snapToGrid(y, gridSize),
  };
}

// Junction detection types
interface Segment {
  start: { x: number; y: number };
  end: { x: number; y: number };
  edgeId: string;
  isHorizontal: boolean;
}

export class SchematicLayoutEngine {
  netlist: Netlist;
  elk: ELKType;
  nets: Map<string, Set<string>>;
  config: SchematicConfig;
  private _nodePositions: NodePositions;

  constructor(netlist: Netlist, config: Partial<SchematicConfig> = {}) {
    this.netlist = netlist;
    // Use the default ELK configuration which works in the browser
    this.elk = new ELK();
    this.nets = this._generateNets();
    this._nodePositions = {};
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
        edges: [], // Start with no edges
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

    // Add net symbol nodes
    const netSymbolNodes = this._createNetSymbolNodes();
    nodes.push(...netSymbolNodes);

    // Create the graph without edges initially
    const graph: ElkGraph = {
      id: instance_ref,
      children: nodes,
      edges: [], // Start with no edges
    };

    // Don't add connectivity here - it will be added later
    return graph;
  }

  /**
   * Layout the schematic for the given instance
   */
  async layout(
    instance_ref: string,
    nodePositions: NodePositions = {}
  ): Promise<LayoutResult> {
    console.log("*** STARTING LAYOUT PASS ***");
    console.log("Node positions with rotations:", nodePositions);

    // Store the provided node positions
    this._nodePositions = nodePositions;

    // Create the graph structure without edges
    const graph = this._graphForInstance(instance_ref);

    // Check if all nodes have positions
    const allNodesHavePositions = this._checkAllNodesHavePositions(
      graph,
      nodePositions
    );

    let layoutedGraph: ElkGraph;
    let connectivityInfo: ReturnType<typeof this._buildConnectivity>;

    if (!allNodesHavePositions) {
      // Build connectivity information early to use for ELK placement
      // Use ignoreClusters=true for ELK layout to connect all ports on same net
      connectivityInfo = this._buildConnectivity(graph, true);

      // Add edges temporarily for ELK layout
      graph.edges = connectivityInfo.elkEdges;

      // Use ELK only for node placement
      const layoutOptions = {
        "elk.algorithm": "layered",
        "elk.direction": this.config.layout.direction,
        "elk.spacing.nodeNode": `${this.config.layout.spacing}`,
        "elk.layered.spacing.nodeNodeBetweenLayers": `${this.config.layout.spacing}`,
        "elk.padding": `[top=${this.config.layout.padding}, left=${this.config.layout.padding}, bottom=${this.config.layout.padding}, right=${this.config.layout.padding}]`,
        "elk.nodeSize.constraints":
          "NODE_LABELS PORTS PORT_LABELS MINIMUM_SIZE",
        "elk.portConstraints": "FIXED_ORDER",
        // "elk.portLabels.placement": "INSIDE NEXT_TO_PORT_IF_POSSIBLE",
        "elk.layered.considerModelOrder": "NODES_AND_EDGES",
      };

      // Create pre-layout graph for debugging
      const preLayoutGraph = {
        ...graph,
        layoutOptions: {
          ...layoutOptions,
          noLayout: true,
        },
      };

      // Generate debugging link for pre-layout graph
      const preLayoutJson = JSON.stringify(preLayoutGraph, null, 2);
      const preLayoutCompressed =
        LZString.compressToEncodedURIComponent(preLayoutJson);
      console.log("Pre-layout ELK Live link (with edges for placement):");
      console.log(
        `https://rtsys.informatik.uni-kiel.de/elklive/json.html?compressedContent=${preLayoutCompressed}`
      );

      // Create the graph with layout options for actual layout
      const graphWithOptions = {
        ...graph,
        layoutOptions: layoutOptions,
      };

      // Run ELK layout for node placement
      layoutedGraph = await this.elk.layout(graphWithOptions);

      // Discard the edges after placement - we'll route them with libavoid
      layoutedGraph.edges = [];
    } else {
      // Skip ELK layout - use existing positions
      console.log("All nodes have positions, skipping ELK layout");
      layoutedGraph = graph;
    }

    // Apply existing positions to nodes (including net symbol nodes)
    this._applyExistingPositions(layoutedGraph, nodePositions);

    if (this.config.layout.gridSnap.enabled) {
      const gridSize = this.config.layout.gridSnap.size;

      if (layoutedGraph.children) {
        for (const node of layoutedGraph.children) {
          if (node.x !== undefined && node.y !== undefined) {
            const snapped = snapPosition(node.x, node.y, gridSize);
            node.x = snapped.x;
            node.y = snapped.y;
          }
        }
      }
    }

    // Build connectivity for routing - use clustering (ignoreClusters=false) for efficient routing
    connectivityInfo = this._buildConnectivity(layoutedGraph, false);

    // Remove net labels from ports that have edges
    this._removePortNetLabels(layoutedGraph, connectivityInfo.portsWithEdges);

    // Store the ELK edges for later use
    layoutedGraph.edges = connectivityInfo.elkEdges;

    // Initialize extracted node positions early since we'll add junction nodes
    const extractedNodePositions: NodePositions = {};

    // Convert nodes to obstacles for libavoid
    const obstacles: Obstacle[] = [];
    if (layoutedGraph.children) {
      for (const node of layoutedGraph.children) {
        if (
          node.x !== undefined &&
          node.y !== undefined &&
          node.width &&
          node.height
        ) {
          // Add the node itself as an obstacle
          obstacles.push({
            id: node.id,
            x: node.x,
            y: node.y,
            width: node.width,
            height: node.height,
          });

          // Add port labels as obstacles
          if (node.ports) {
            for (const port of node.ports) {
              if (port.labels && port.x !== undefined && port.y !== undefined) {
                for (let i = 0; i < port.labels.length; i++) {
                  const label = port.labels[i];
                  if (
                    label.x !== undefined &&
                    label.y !== undefined &&
                    label.width !== undefined &&
                    label.height !== undefined
                  ) {
                    // Calculate absolute position of the label
                    // Port positions are relative to node, label positions are relative to port
                    const labelX = node.x + port.x + label.x;
                    const labelY = node.y + port.y + label.y;

                    obstacles.push({
                      id: `${port.id}_label_${i}`,
                      x: labelX,
                      y: labelY,
                      width: label.width,
                      height: label.height,
                    });
                  }
                }
              }
            }
          }
        }
      }
    }

    // Decompose hyperedges into MST edges before routing
    console.log("Decomposing hyperedges into MST edges...");
    const decomposedHyperedges: Hyperedge[] = [];
    for (const hyperedge of connectivityInfo.hyperedges) {
      const mstEdges = this._decomposeHyperedgeToMST(hyperedge);
      decomposedHyperedges.push(...mstEdges);
    }
    console.log(
      `Decomposed ${connectivityInfo.hyperedges.length} hyperedges into ${decomposedHyperedges.length} simple edges`
    );

    // Route edges using libavoid
    const edgeRouter = new LibavoidEdgeRouter();
    try {
      const routingResult = await edgeRouter.route(
        obstacles,
        decomposedHyperedges
      );

      // Simple 1:1 mapping - each libavoid edge becomes one ELK edge
      const newEdges: ElkEdge[] = [];

      // Group edges by their original hyperedge (MST)
      const edgesByMST = new Map<
        string,
        Array<{
          routedEdge: any;
          elkEdge: ElkEdge;
        }>
      >();

      for (const routedEdge of routingResult.edges) {
        // Use context directly instead of trying to parse edge IDs
        if (!routedEdge.context) {
          console.warn(`No context found for routed edge ${routedEdge.id}`);
          continue;
        }

        // Determine source and target IDs and component refs
        let sourceId: string;
        let targetId: string;
        let sourceComponentRef: string;
        let targetComponentRef: string;

        // Handle source
        if (routedEdge.sourceType === "port") {
          sourceId = routedEdge.sourceId;
          // Find which node owns this port
          sourceComponentRef = this._findNodeOwningPort(
            sourceId,
            layoutedGraph.children || []
          );
        } else if (routedEdge.sourceType === "junction") {
          // Junction ports are created with a .port suffix
          sourceId = `${routedEdge.sourceId}.port`;
          sourceComponentRef = routedEdge.sourceId; // Junction node ID
        } else {
          console.warn(`Unknown source type: ${routedEdge.sourceType}`);
          continue;
        }

        // Handle target
        if (routedEdge.targetType === "port") {
          targetId = routedEdge.targetId;
          // Find which node owns this port
          targetComponentRef = this._findNodeOwningPort(
            targetId,
            layoutedGraph.children || []
          );
        } else if (routedEdge.targetType === "junction") {
          // Junction ports are created with a .port suffix
          targetId = `${routedEdge.targetId}.port`;
          targetComponentRef = routedEdge.targetId; // Junction node ID
        } else {
          console.warn(`Unknown target type: ${routedEdge.targetType}`);
          continue;
        }

        // Create the ELK edge using context information
        const elkEdge: ElkEdge = {
          id: routedEdge.id,
          netId: routedEdge.context.netId,
          sources: [sourceId],
          targets: [targetId],
          sourceComponentRef: sourceComponentRef,
          targetComponentRef: targetComponentRef,
          labels: [], // Start with empty labels, will add once per MST
          sections: [
            {
              id: `${routedEdge.id}_section`,
              startPoint: routedEdge.points[0],
              endPoint: routedEdge.points[routedEdge.points.length - 1],
              bendPoints: routedEdge.points.slice(1, -1),
            },
          ],
          properties: {
            netName: routedEdge.context.netName,
          },
        };

        // Group edges by their original hyperedge/cluster
        // Use the context to determine which cluster this edge belongs to
        const mstId =
          routedEdge.context.originalHyperedgeId || routedEdge.context.netId;

        // Group edges by MST
        if (!edgesByMST.has(mstId)) {
          edgesByMST.set(mstId, []);
        }
        edgesByMST.get(mstId)!.push({ routedEdge, elkEdge });
      }

      // Now process each MST to add a single label on the longest segment
      for (const [, mstEdges] of edgesByMST) {
        // Find the longest segment across all edges in this MST
        let longestSegmentInfo: {
          edge: ElkEdge;
          position: {
            x: number;
            y: number;
            isHorizontal: boolean;
            segmentLength: number;
          };
        } | null = null;

        for (const { routedEdge, elkEdge } of mstEdges) {
          const labelPosition = this._findLongestSegmentMidpoint(
            routedEdge.points
          );
          if (labelPosition) {
            if (
              !longestSegmentInfo ||
              labelPosition.segmentLength >
                longestSegmentInfo.position.segmentLength
            ) {
              longestSegmentInfo = {
                edge: elkEdge,
                position: labelPosition,
              };
            }
          }
        }

        // Add label to the edge with the longest segment
        if (
          longestSegmentInfo &&
          longestSegmentInfo.position.segmentLength > 50
        ) {
          const netName = longestSegmentInfo.edge.properties?.netName || "";
          const labelDimensions = calculateTextDimensions(netName, 10);

          // For horizontal segments, label goes above; for vertical, label goes to the side
          const labelOffset = 10;
          let labelX = longestSegmentInfo.position.x;
          let labelY = longestSegmentInfo.position.y;

          if (longestSegmentInfo.position.isHorizontal) {
            // Center label horizontally, place above the line
            labelX -= labelDimensions.width / 2;
            labelY -= labelDimensions.height / 2 + labelOffset;
          } else {
            // Center label vertically, place to the right of the line
            labelX += labelOffset;
            labelY -= labelDimensions.height / 2;
          }

          longestSegmentInfo.edge.labels = [
            {
              text: netName,
              x: labelX,
              y: labelY,
              width: labelDimensions.width,
              height: labelDimensions.height,
              properties: {
                labelType: "netName",
              },
            },
          ];
        }

        // Add all edges from this MST to the results
        for (const { elkEdge } of mstEdges) {
          newEdges.push(elkEdge);
        }
      }

      // Replace the edges with the new routed edges
      layoutedGraph.edges = newEdges;

      // Find junction points where edges on the same net intersect
      this._findJunctionPoints(newEdges);

      // Restore net labels for any ports that don't have edges
      // (e.g., if libavoid filtered out non-orthogonal edges)
      this._restoreNetLabelsForUnconnectedPorts(layoutedGraph, newEdges);
    } finally {
      // Clean up the edge router
      edgeRouter.destroy();
    }

    // Add noLayout option for debugging in post-layout graph
    const postLayoutGraph = {
      ...layoutedGraph,
      layoutOptions: {
        noLayout: true,
      },
    };

    // Generate debugging link for post-layout graph
    const postLayoutJson = JSON.stringify(postLayoutGraph, null, 2);
    const postLayoutCompressed =
      LZString.compressToEncodedURIComponent(postLayoutJson);
    console.log("\nPost-layout ELK Live link (with routed edges):");
    console.log(
      `https://rtsys.informatik.uni-kiel.de/elklive/json.html?compressedContent=${postLayoutCompressed}`
    );

    // Extract positions from the layout result

    const extractPositions = (nodes: ElkNode[], parentX = 0, parentY = 0) => {
      for (const node of nodes) {
        // Skip junction nodes - they are dynamically created
        if (node.type === NodeType.NET_JUNCTION) {
          continue;
        }

        if (node.x !== undefined && node.y !== undefined) {
          const absoluteX = node.x + parentX;
          const absoluteY = node.y + parentY;

          extractedNodePositions[node.id] = {
            x: absoluteX,
            y: absoluteY,
            width: node.width,
            height: node.height,
            rotation: nodePositions[node.id]?.rotation || 0,
          };
        }

        // Recursively extract from children if any
        if (node.children) {
          extractPositions(
            node.children,
            (node.x || 0) + parentX,
            (node.y || 0) + parentY
          );
        }
      }
    };

    extractPositions(layoutedGraph.children || []);

    console.log("*** ENDING LAYOUT PASS ***");

    // Ensure the graph has the required properties
    return {
      ...layoutedGraph,
      nodePositions: extractedNodePositions,
    } as LayoutResult;
  }

  /**
   * Check if all nodes in the graph have positions
   */
  private _checkAllNodesHavePositions(
    graph: ElkGraph,
    nodePositions: NodePositions
  ): boolean {
    if (!graph.children) return true;

    const checkNodes = (nodes: ElkNode[]): boolean => {
      for (const node of nodes) {
        // Check if this node has a position
        if (!nodePositions[node.id]) {
          return false;
        }

        // Check children recursively
        if (node.children) {
          if (!checkNodes(node.children)) {
            return false;
          }
        }
      }
      return true;
    };

    // Check regular nodes
    const regularNodesHavePositions = checkNodes(graph.children);

    // Also check if net symbol nodes have positions
    for (const [netId, net] of Object.entries(this.netlist.nets)) {
      if (net.properties?.__symbol_value) {
        const netName = net.name || netId;

        // Look for all net symbol nodes in nodePositions for this net
        const netSymbolPattern = new RegExp(
          `^${this.netlist.root_ref}\\.${netName}\\.(\\d+)$`
        );
        let hasAtLeastOneSymbol = false;

        for (const nodeId of Object.keys(nodePositions)) {
          if (netSymbolPattern.test(nodeId)) {
            hasAtLeastOneSymbol = true;
            break;
          }
        }

        // If this net should have symbols but none are positioned, return false
        if (!hasAtLeastOneSymbol) {
          return false;
        }
      }
    }

    return regularNodesHavePositions;
  }

  /**
   * Apply existing positions to nodes in the graph
   */
  private _applyExistingPositions(
    graph: ElkGraph,
    nodePositions: NodePositions
  ): void {
    if (!graph.children) return;

    const gridSize = this.config.layout.gridSnap.enabled
      ? this.config.layout.gridSnap.size
      : 0;

    const applyToNodes = (nodes: ElkNode[]) => {
      for (const node of nodes) {
        const position = nodePositions[node.id];
        if (position) {
          if (gridSize > 0) {
            // Snap the position when applying
            const snapped = snapPosition(position.x, position.y, gridSize);
            node.x = snapped.x;
            node.y = snapped.y;
          } else {
            node.x = position.x;
            node.y = position.y;
          }

          if (position.width !== undefined) node.width = position.width;
          if (position.height !== undefined) node.height = position.height;
          if (position.rotation !== undefined)
            node.rotation = position.rotation;
        }

        // Apply to children recursively
        if (node.children) {
          applyToNodes(node.children);
        }
      }
    };

    applyToNodes(graph.children);
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

  /**
   * Find which net a port belongs to
   */
  private _findNetForPort(portId: string): string | null {
    for (const [netId, portSet] of this.nets.entries()) {
      if (portSet.has(portId)) {
        return netId;
      }
    }
    return null;
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
        // Apply position if provided
        ...(this._nodePositions[instance_ref] && {
          x: this._nodePositions[instance_ref].x,
          y: this._nodePositions[instance_ref].y,
          rotation: this._nodePositions[instance_ref].rotation || 0,
        }),
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
          // Mark as fixed if position is provided
          ...(this._nodePositions[instance_ref] && {
            "elk.position": `(${this._nodePositions[instance_ref].x},${this._nodePositions[instance_ref].y})`,
            "elk.fixed": "true",
          }),
        },
      };

      if (this._nodePositions[instance_ref]?.rotation) {
        console.log(
          `[LayoutEngine] Applied rotation ${this._nodePositions[instance_ref].rotation} to symbol ${instance_ref}`
        );
      }

      // Create ports based on pin endpoints
      for (const pinEndpoint of symbolInfo.pinEndpoints) {
        // Find the corresponding port in the instance children
        let portName = pinEndpoint.name;
        let portRef = `${instance_ref}.${portName}`;

        // If the pin name is ~ (unnamed), try to match by pin number
        if (portName === "~" && pinEndpoint.number) {
          const childNames = Object.keys(instance.children || {});
          const pinNumberMatch = childNames.find((name) => {
            return (
              name.toLowerCase() === `${pinEndpoint.number}` ||
              name.toLowerCase() === `p${pinEndpoint.number}`
            );
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
        let snappedX: number;
        let snappedY: number;

        if (minDist === distToLeft) {
          side = "WEST";
          snappedX = 0;
          snappedY = portY;
        } else if (minDist === distToRight) {
          side = "EAST";
          snappedX = nodeWidth;
          snappedY = portY;
        } else if (minDist === distToTop) {
          side = "NORTH";
          snappedX = portX;
          snappedY = 0;
        } else {
          side = "SOUTH";
          snappedX = portX;
          snappedY = nodeHeight;
        }

        const isVertical = side === "NORTH" || side === "SOUTH";

        // Prepare port labels
        const portLabels: ElkLabel[] = [];

        // Check if this port is connected to a net and add net label
        const netId = this._findNetForPort(portRef);
        if (netId) {
          const netInfo = this.netlist.nets[netId];
          const netName = netInfo?.name || netId;

          const maxLabelLength = 10;
          let truncatedLabelText = netName;

          if (netName.length > maxLabelLength) {
            truncatedLabelText = "..." + netName.slice(-(maxLabelLength - 3));
          }
          // Calculate net label dimensions and position
          const netLabelDimensions = calculateTextDimensions(
            truncatedLabelText,
            12
          );
          const netLabelWidth = isVertical
            ? netLabelDimensions.height
            : netLabelDimensions.width;
          const netLabelHeight = isVertical
            ? netLabelDimensions.width
            : netLabelDimensions.height;

          // Position net label further out than pin label
          const netLabelOffset = 15; // Further from port than pin labels
          let netLabelX: number, netLabelY: number;

          switch (side) {
            case "WEST":
              netLabelX = -netLabelWidth - netLabelOffset;
              netLabelY = -netLabelHeight / 2;
              break;
            case "EAST":
              netLabelX = netLabelOffset;
              netLabelY = -netLabelHeight / 2;
              break;
            case "NORTH":
              netLabelX = -netLabelWidth / 2;
              netLabelY = -netLabelHeight - netLabelOffset;
              break;
            case "SOUTH":
              netLabelX = -netLabelWidth / 2;
              netLabelY = netLabelOffset;
              break;
          }

          portLabels.push({
            text: truncatedLabelText,
            x: netLabelX,
            y: netLabelY,
            width: netLabelWidth,
            height: netLabelHeight,
            properties: {
              labelType: "netReference",
            },
          });
        }

        // Add the port
        node.ports?.push({
          id: portRef,
          x: snappedX,
          y: snappedY,
          width: 0,
          height: 0,
          labels: portLabels,
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
      // Apply position and rotation if provided
      ...(this._nodePositions[instance_ref] && {
        x: this._nodePositions[instance_ref].x,
        y: this._nodePositions[instance_ref].y,
        rotation: this._nodePositions[instance_ref].rotation || 0,
      }),
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
      properties: {
        // Mark as fixed if position is provided
        ...(this._nodePositions[instance_ref] && {
          "elk.position": `(${this._nodePositions[instance_ref].x},${this._nodePositions[instance_ref].y})`,
          "elk.fixed": "true",
        }),
      },
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
   * Decompose a hyperedge into a minimum spanning tree of regular edges
   * Uses Kruskal's algorithm to find the MST
   */
  private _decomposeHyperedgeToMST(hyperedge: Hyperedge): Hyperedge[] {
    const ports = hyperedge.ports;

    // If already a simple edge, return as-is
    if (ports.length <= 2) {
      return [hyperedge];
    }

    // Calculate all pairwise distances
    interface Edge {
      from: number;
      to: number;
      distance: number;
    }

    const edges: Edge[] = [];
    for (let i = 0; i < ports.length; i++) {
      for (let j = i + 1; j < ports.length; j++) {
        const dx = ports[i].x - ports[j].x;
        const dy = ports[i].y - ports[j].y;
        const distance = Math.sqrt(dx * dx + dy * dy);
        edges.push({ from: i, to: j, distance });
      }
    }

    // Sort edges by distance
    edges.sort((a, b) => a.distance - b.distance);

    // Kruskal's algorithm using Union-Find
    const parent: number[] = Array.from({ length: ports.length }, (_, i) => i);

    function find(x: number): number {
      if (parent[x] !== x) {
        parent[x] = find(parent[x]); // Path compression
      }
      return parent[x];
    }

    function union(x: number, y: number): boolean {
      const rootX = find(x);
      const rootY = find(y);
      if (rootX === rootY) return false;
      parent[rootX] = rootY;
      return true;
    }

    // Build MST
    const mstEdges: Edge[] = [];
    for (const edge of edges) {
      if (union(edge.from, edge.to)) {
        mstEdges.push(edge);
        if (mstEdges.length === ports.length - 1) {
          break; // MST complete
        }
      }
    }

    // Convert MST edges to hyperedges
    const result: Hyperedge[] = [];
    for (let i = 0; i < mstEdges.length; i++) {
      const edge = mstEdges[i];
      result.push({
        id: `${hyperedge.id}_mst_${i}`,
        ports: [ports[edge.from], ports[edge.to]],
        context: {
          ...hyperedge.context, // Preserve all context including originalHyperedgeId
        },
      });
    }

    return result;
  }

  /**
   * Build connectivity information and return hyperedges for routing
   */
  private _buildConnectivity(
    graph: ElkGraph,
    ignoreClusters: boolean = false
  ): {
    hyperedges: Hyperedge[];
    elkEdges: ElkEdge[];
    portsWithEdges: Set<string>;
  } {
    // Track which ports have edges so we can optionally hide their labels
    const portsWithEdges = new Set<string>();
    const hyperedges: Hyperedge[] = [];
    const elkEdges: ElkEdge[] = [];

    // First pass: Add net information to ports
    for (const [netId, net] of this.nets.entries()) {
      // Get the net name from the netlist

      // Find all ports in this graph that are connected to this net
      for (const node of graph.children || []) {
        if (!node.ports) continue;

        // Check each port on the node
        for (const port of node.ports) {
          if (net.has(port.id)) {
            // Mark the port as connected to this net
            port.netId = netId;
          }
        }
      }
    }

    // Second pass: Create hyperedges for routing and corresponding binary ELK edges
    let edgeCounter = 0;
    for (const [netId] of this.nets.entries()) {
      const netInfo = this.netlist.nets[netId];
      const netName = netInfo?.name || netId;

      // Check if this net has a symbol
      const netHasSymbol = netInfo?.properties?.__symbol_value;

      if (netHasSymbol) {
        // For nets with symbols, create edges from each port to the closest net symbol
        const netName = netInfo?.name || netId;

        // Find all net symbol nodes for this net
        const netSymbolPattern = new RegExp(
          `^${this.netlist.root_ref}\\.${netName}\\.(\\d+)$`
        );
        const symbolNodes: ElkNode[] = [];

        for (const node of graph.children || []) {
          if (netSymbolPattern.test(node.id)) {
            symbolNodes.push(node);
          }
        }

        if (symbolNodes.length === 0) {
          console.warn(`No net symbol nodes found for net ${netId}`);
          continue;
        }

        // Find all ports connected to this net
        const connectedPorts: Array<{ node: ElkNode; port: ElkPort }> = [];
        for (const node of graph.children || []) {
          if (symbolNodes.some((sn) => sn.id === node.id)) continue; // Skip the symbol nodes themselves
          if (!node.ports) continue;

          for (const port of node.ports) {
            if (port.netId === netId) {
              connectedPorts.push({ node, port });
            }
          }
        }

        // For each connected port, find the closest net symbol and create an edge
        for (const { node, port } of connectedPorts) {
          const portPosition = this._getPortPosition(node, port);
          if (!portPosition) continue;

          // Find the closest symbol node
          let closestSymbol: ElkNode | null = null;
          let closestDistance = Infinity;
          let closestSymbolPort: ElkPort | null = null;

          for (const symbolNode of symbolNodes) {
            // For each port on the symbol node, calculate distance
            for (const symbolPort of symbolNode.ports || []) {
              const symbolPortPosition = this._getPortPosition(
                symbolNode,
                symbolPort
              );
              if (symbolPortPosition) {
                const distance = this._calculateDistance(
                  portPosition,
                  symbolPortPosition
                );
                if (distance < closestDistance) {
                  closestDistance = distance;
                  closestSymbol = symbolNode;
                  closestSymbolPort = symbolPort;
                }
              }
            }
          }

          if (!closestSymbol || !closestSymbolPort) {
            console.warn(`Could not find closest symbol for port ${port.id}`);
            continue;
          }

          const edgeId = `net_${netId}_edge_${edgeCounter++}`;
          const symbolPortPosition = this._getPortPosition(
            closestSymbol,
            closestSymbolPort
          );

          if (symbolPortPosition) {
            // Create hyperedge for libavoid
            const hyperedge: Hyperedge = {
              id: edgeId,
              ports: [
                {
                  id: port.id,
                  x: portPosition.x,
                  y: portPosition.y,
                  visibilityDirection: this._getPortVisibilityDirection(
                    port,
                    node.rotation
                  ),
                },
                {
                  id: closestSymbolPort.id,
                  x: symbolPortPosition.x,
                  y: symbolPortPosition.y,
                  visibilityDirection: this._getPortVisibilityDirection(
                    closestSymbolPort,
                    closestSymbol.rotation
                  ),
                },
              ],
              context: {
                netId: netId,
                netName: netName,
              },
            };
            hyperedges.push(hyperedge);

            // Create corresponding ELK edge
            const elkEdge: ElkEdge = {
              id: edgeId,
              netId: netId,
              sources: [port.id],
              targets: [closestSymbolPort.id],
              sourceComponentRef: node.id,
              targetComponentRef: closestSymbol.id,
              labels: [],
              properties: {
                netName: netName,
              },
            };
            elkEdges.push(elkEdge);

            // Track that these ports have edges
            portsWithEdges.add(port.id);
            portsWithEdges.add(closestSymbolPort.id);
          }
        }
      } else {
        // For nets without symbols, use the existing clustering logic
        // Find clusters of nearby ports
        const clusters = this._findNetClusters(
          netId,
          graph.children || [],
          ignoreClusters
        );

        // Create hyperedges for each cluster
        for (const cluster of clusters) {
          const edgeId = `net_${netId}_cluster_${edgeCounter++}`;

          // Collect all ports in the cluster with their positions and visibility directions
          const ports: Port[] = cluster.map(({ node, port, position }) => ({
            id: port.id,
            x: position.x,
            y: position.y,
            visibilityDirection: this._getPortVisibilityDirection(
              port,
              node.rotation
            ),
          }));

          // Create hyperedge for libavoid with context
          const hyperedge: Hyperedge = {
            id: edgeId,
            ports: ports,
            context: {
              netId: netId,
              netName: netName,
              originalHyperedgeId: edgeId, // Add this to track the original cluster
            },
          };
          hyperedges.push(hyperedge);

          // Decompose hyperedge into MST edges and create binary ELK edges
          const mstEdges = this._decomposeHyperedgeToMST(hyperedge);
          for (const mstEdge of mstEdges) {
            // Each MST edge should have exactly 2 ports
            if (mstEdge.ports.length !== 2) {
              console.warn(
                `MST edge ${mstEdge.id} has ${mstEdge.ports.length} ports, expected 2`
              );
              continue;
            }

            const sourcePort = mstEdge.ports[0];
            const targetPort = mstEdge.ports[1];

            // Find the nodes that own these ports
            const sourceNode = cluster.find(
              (c) => c.port.id === sourcePort.id
            )?.node;
            const targetNode = cluster.find(
              (c) => c.port.id === targetPort.id
            )?.node;

            if (!sourceNode || !targetNode) {
              console.warn(
                `Could not find nodes for ports ${sourcePort.id} and ${targetPort.id}`
              );
              continue;
            }

            // Create binary ELK edge
            const elkEdge: ElkEdge = {
              id: mstEdge.id,
              netId: netId,
              sources: [sourcePort.id],
              targets: [targetPort.id],
              sourceComponentRef: sourceNode.id,
              targetComponentRef: targetNode.id,
              labels: [],
              properties: {
                netName: netName,
                originalHyperedge: edgeId,
              },
            };

            elkEdges.push(elkEdge);
          }

          // Track that these ports have edges
          cluster.forEach(({ port }) => portsWithEdges.add(port.id));
        }
      }
    }

    return { hyperedges, elkEdges, portsWithEdges };
  }

  /**
   * Get the visibility direction for a port based on its side
   */
  private _getPortVisibilityDirection(
    port: ElkPort,
    nodeRotation?: number
  ): "NORTH" | "SOUTH" | "EAST" | "WEST" | "ALL" | undefined {
    const portSide = port.properties?.["port.side"];

    // If no side is specified, allow connections from all directions
    if (!portSide) {
      return "ALL";
    }

    // If node is rotated, transform the port side accordingly
    if (nodeRotation && nodeRotation !== 0) {
      const rotationSteps = Math.round(nodeRotation / 90) % 4;
      const sides = ["NORTH", "EAST", "SOUTH", "WEST"];
      const currentIndex = sides.indexOf(portSide);

      if (currentIndex !== -1) {
        const newIndex = (currentIndex + rotationSteps + 4) % 4;
        return sides[newIndex] as "NORTH" | "SOUTH" | "EAST" | "WEST";
      }
    }

    switch (portSide) {
      case "NORTH":
        return "NORTH";
      case "SOUTH":
        return "SOUTH";
      case "EAST":
        return "EAST";
      case "WEST":
        return "WEST";
      default:
        // If no side is specified, allow connections from all directions
        return "ALL";
    }
  }

  /**
   * Remove net labels from ports that have edges
   */
  private _removePortNetLabels(
    graph: ElkGraph,
    portsWithEdges: Set<string>
  ): void {
    for (const node of graph.children || []) {
      if (!node.ports) continue;

      for (const port of node.ports) {
        // If this port has edges, remove any net reference labels
        if (portsWithEdges.has(port.id)) {
          if (port.labels) {
            port.labels = port.labels.filter(
              (label) => label.properties?.labelType !== "netReference"
            );
          }
        }
      }
    }
  }

  /**
   * Restore net labels for ports that don't have any edges connected
   */
  private _restoreNetLabelsForUnconnectedPorts(
    graph: ElkGraph,
    edges: ElkEdge[]
  ): void {
    // Build a set of all ports that have edges
    const connectedPorts = new Set<string>();
    for (const edge of edges) {
      edge.sources.forEach((source) => connectedPorts.add(source));
      edge.targets.forEach((target) => connectedPorts.add(target));
    }

    // Check all ports and add net labels to those without edges
    for (const node of graph.children || []) {
      if (!node.ports) continue;

      // Skip junction nodes - they don't need net labels
      if (node.type === NodeType.NET_JUNCTION) continue;

      for (const port of node.ports) {
        // If this port doesn't have any edges, add a net label if it has a netId
        if (!connectedPorts.has(port.id) && port.netId) {
          const netInfo = this.netlist.nets[port.netId];
          const netName = netInfo?.name || port.netId;

          // Check if we already have a net reference label
          const hasNetLabel = port.labels?.some(
            (label) => label.properties?.labelType === "netReference"
          );

          if (!hasNetLabel) {
            const maxLabelLength = 10;
            let truncatedLabelText = netName;

            if (netName.length > maxLabelLength) {
              truncatedLabelText = "..." + netName.slice(-(maxLabelLength - 3));
            }

            // Calculate label dimensions based on port side
            const portSide = port.properties?.["port.side"] || "WEST";
            const isVertical = portSide === "NORTH" || portSide === "SOUTH";

            const netLabelDimensions = calculateTextDimensions(
              truncatedLabelText,
              10
            );
            const netLabelWidth = isVertical
              ? netLabelDimensions.height
              : netLabelDimensions.width;
            const netLabelHeight = isVertical
              ? netLabelDimensions.width
              : netLabelDimensions.height;

            // Position net label based on port side
            const netLabelOffset = 15;
            let netLabelX: number, netLabelY: number;

            switch (portSide) {
              case "WEST":
                netLabelX = -netLabelWidth - netLabelOffset;
                netLabelY = -netLabelHeight / 2;
                break;
              case "EAST":
                netLabelX = netLabelOffset;
                netLabelY = -netLabelHeight / 2;
                break;
              case "NORTH":
                netLabelX = -netLabelWidth / 2;
                netLabelY = -netLabelHeight - netLabelOffset;
                break;
              case "SOUTH":
                netLabelX = -netLabelWidth / 2;
                netLabelY = netLabelOffset;
                break;
              default:
                netLabelX = netLabelOffset;
                netLabelY = -netLabelHeight / 2;
            }

            // Initialize labels array if it doesn't exist
            if (!port.labels) {
              port.labels = [];
            }

            port.labels.push({
              text: truncatedLabelText,
              x: netLabelX,
              y: netLabelY,
              width: netLabelWidth,
              height: netLabelHeight,
              properties: {
                labelType: "netReference",
              },
            });
          }
        }
      }
    }
  }

  /**
   * Calculate the absolute position of a port
   */
  private _getPortPosition(
    node: ElkNode,
    port: ElkPort
  ): { x: number; y: number } | null {
    // If node doesn't have a position yet, return null
    if (node.x === undefined || node.y === undefined) {
      return null;
    }

    const nodeX = node.x;
    const nodeY = node.y;
    const portX = port.x || 0;
    const portY = port.y || 0;

    // If the node has rotation, transform the port position
    if (node.rotation && node.rotation !== 0) {
      const nodeWidth = node.width || 0;
      const nodeHeight = node.height || 0;

      // Transform port position based on rotation
      const rotatedPos = this._rotatePoint(
        portX,
        portY,
        nodeWidth / 2,
        nodeHeight / 2,
        node.rotation
      );

      return {
        x: nodeX + rotatedPos.x,
        y: nodeY + rotatedPos.y,
      };
    }

    return {
      x: nodeX + portX,
      y: nodeY + portY,
    };
  }

  /**
   * Rotate a point around a center by a given angle in degrees
   */
  private _rotatePoint(
    x: number,
    y: number,
    centerX: number,
    centerY: number,
    angleDegrees: number
  ): { x: number; y: number } {
    const angleRadians = (angleDegrees * Math.PI) / 180;
    const cos = Math.cos(angleRadians);
    const sin = Math.sin(angleRadians);

    // Translate point to origin
    const translatedX = x - centerX;
    const translatedY = y - centerY;

    // Rotate
    const rotatedX = translatedX * cos - translatedY * sin;
    const rotatedY = translatedX * sin + translatedY * cos;

    // Translate back
    return {
      x: rotatedX + centerX,
      y: rotatedY + centerY,
    };
  }

  /**
   * Find which node owns a given port
   */
  private _findNodeOwningPort(portId: string, nodes: ElkNode[]): string {
    for (const node of nodes) {
      if (node.ports) {
        for (const port of node.ports) {
          if (port.id === portId) {
            return node.id;
          }
        }
      }

      // Check children recursively
      if (node.children) {
        const childResult = this._findNodeOwningPort(portId, node.children);
        if (childResult) {
          return childResult;
        }
      }
    }

    // Check if the port belongs to a net symbol node
    // Net symbol ports have IDs like "root_ref.NET_NAME.1.portName"
    // Extract the node ID by removing the last part (port name)
    const lastDotIndex = portId.lastIndexOf(".");
    if (lastDotIndex > 0) {
      const possibleNodeId = portId.substring(0, lastDotIndex);

      // Check if this matches a net symbol node pattern (ends with .number)
      const nodeIdParts = possibleNodeId.split(".");
      if (nodeIdParts.length >= 3) {
        const lastPart = nodeIdParts[nodeIdParts.length - 1];

        // Check if last part is a number (net symbol index)
        if (/^\d+$/.test(lastPart)) {
          // This could be a net symbol node, verify by checking if the net exists
          const netNameIndex = nodeIdParts.length - 2;
          const netName = nodeIdParts[netNameIndex];

          for (const [netId, net] of Object.entries(this.netlist.nets)) {
            if (
              (net.name === netName || netId === netName) &&
              net.properties?.__symbol_value
            ) {
              return possibleNodeId;
            }
          }
        }
      }
    }

    // If not found, return the port ID itself as a fallback
    console.warn(`Could not find node owning port ${portId}`);
    return portId;
  }

  /**
   * Calculate Euclidean distance between two positions
   */
  private _calculateDistance(
    pos1: { x: number; y: number },
    pos2: { x: number; y: number }
  ): number {
    const dx = pos2.x - pos1.x;
    const dy = pos2.y - pos1.y;
    return Math.sqrt(dx * dx + dy * dy);
  }

  /**
   * Find clusters of nearby ports for a given net
   */
  private _findNetClusters(
    netId: string,
    nodes: ElkNode[],
    ignoreClusters: boolean = false
  ): Array<
    Array<{ node: ElkNode; port: ElkPort; position: { x: number; y: number } }>
  > {
    // Collect all ports connected to this net with their positions
    const connectedPorts: Array<{
      node: ElkNode;
      port: ElkPort;
      position: { x: number; y: number };
    }> = [];

    for (const node of nodes) {
      if (!node.ports) continue;
      for (const port of node.ports) {
        if (port.netId === netId) {
          const position = this._getPortPosition(node, port);
          // If ignoring clusters, we don't need positions - just collect all ports
          if (ignoreClusters) {
            connectedPorts.push({
              node,
              port,
              position: { x: 0, y: 0 }, // Dummy position since we're ignoring clusters
            });
          } else {
            // Only include ports that have valid positions for clustering
            if (position) {
              connectedPorts.push({ node, port, position });
            }
          }
        }
      }
    }

    // If less than 2 ports, no edges needed
    if (connectedPorts.length < 2) {
      return [];
    }

    // If ignoring clusters, just return one cluster with all ports
    if (ignoreClusters) {
      return [connectedPorts];
    }

    // Build adjacency graph based on distance threshold
    const threshold = this.config.layout.netConnectionThreshold;
    const adjacency: Map<number, Set<number>> = new Map();

    for (let i = 0; i < connectedPorts.length; i++) {
      adjacency.set(i, new Set());
    }

    for (let i = 0; i < connectedPorts.length; i++) {
      for (let j = i + 1; j < connectedPorts.length; j++) {
        const distance = this._calculateDistance(
          connectedPorts[i].position,
          connectedPorts[j].position
        );
        if (distance <= threshold) {
          adjacency.get(i)!.add(j);
          adjacency.get(j)!.add(i);
        }
      }
    }

    // Find connected components using DFS
    const visited = new Set<number>();
    const clusters: Array<
      Array<{
        node: ElkNode;
        port: ElkPort;
        position: { x: number; y: number };
      }>
    > = [];

    const dfs = (index: number, cluster: number[]) => {
      visited.add(index);
      cluster.push(index);
      for (const neighbor of adjacency.get(index)!) {
        if (!visited.has(neighbor)) {
          dfs(neighbor, cluster);
        }
      }
    };

    for (let i = 0; i < connectedPorts.length; i++) {
      if (!visited.has(i)) {
        const clusterIndices: number[] = [];
        dfs(i, clusterIndices);

        // Only create clusters with 2 or more ports
        if (clusterIndices.length >= 2) {
          const cluster = clusterIndices.map((idx) => connectedPorts[idx]);
          clusters.push(cluster);
        }
      }
    }

    return clusters;
  }

  /**
   * Find orthogonal intersection between two segments
   * Returns the intersection point if segments cross, null otherwise
   */
  private _findOrthogonalIntersection(
    seg1: Segment,
    seg2: Segment
  ): { x: number; y: number } | null {
    // One must be horizontal, one must be vertical
    if (seg1.isHorizontal === seg2.isHorizontal) {
      return null;
    }

    const horizontal = seg1.isHorizontal ? seg1 : seg2;
    const vertical = seg1.isHorizontal ? seg2 : seg1;

    // Check if they actually intersect
    const hMinX = Math.min(horizontal.start.x, horizontal.end.x);
    const hMaxX = Math.max(horizontal.start.x, horizontal.end.x);
    const vMinY = Math.min(vertical.start.y, vertical.end.y);
    const vMaxY = Math.max(vertical.start.y, vertical.end.y);

    const intersectX = vertical.start.x;
    const intersectY = horizontal.start.y;

    // Check if intersection point is within both segments
    if (
      intersectX >= hMinX &&
      intersectX <= hMaxX &&
      intersectY >= vMinY &&
      intersectY <= vMaxY
    ) {
      return { x: intersectX, y: intersectY };
    }

    return null;
  }

  /**
   * Check if a point lies on a segment (excluding endpoints)
   */
  private _isPointOnSegment(
    point: { x: number; y: number },
    segment: Segment
  ): boolean {
    // Check if point is at either endpoint (we exclude these)
    if (
      (point.x === segment.start.x && point.y === segment.start.y) ||
      (point.x === segment.end.x && point.y === segment.end.y)
    ) {
      return false;
    }

    if (segment.isHorizontal) {
      // For horizontal segment, y must match and x must be between start and end
      const minX = Math.min(segment.start.x, segment.end.x);
      const maxX = Math.max(segment.start.x, segment.end.x);
      return point.y === segment.start.y && point.x > minX && point.x < maxX;
    } else {
      // For vertical segment, x must match and y must be between start and end
      const minY = Math.min(segment.start.y, segment.end.y);
      const maxY = Math.max(segment.start.y, segment.end.y);
      return point.x === segment.start.x && point.y > minY && point.y < maxY;
    }
  }

  /**
   * Find junction points where edges on the same net intersect
   */
  private _findJunctionPoints(edges: ElkEdge[]): void {
    // Group edges by netId for efficiency
    const edgesByNet = new Map<string, ElkEdge[]>();

    for (const edge of edges) {
      if (!edgesByNet.has(edge.netId)) {
        edgesByNet.set(edge.netId, []);
      }
      edgesByNet.get(edge.netId)!.push(edge);
    }

    // Process each net separately
    for (const [, netEdges] of edgesByNet) {
      // Skip nets with only one edge
      if (netEdges.length < 2) continue;

      // Extract all segments from all edges in this net
      const segments: Segment[] = [];

      for (const edge of netEdges) {
        const section = edge.sections?.[0];
        if (!section) continue;

        // Build path points
        const points = [
          section.startPoint,
          ...(section.bendPoints || []),
          section.endPoint,
        ];

        // Convert to segments
        for (let i = 0; i < points.length - 1; i++) {
          segments.push({
            start: points[i],
            end: points[i + 1],
            edgeId: edge.id,
            isHorizontal: points[i].y === points[i + 1].y,
          });
        }
      }

      // Find intersections between segments
      const junctionPoints = new Map<string, { x: number; y: number }>();

      // Find cross intersections
      for (let i = 0; i < segments.length; i++) {
        for (let j = i + 1; j < segments.length; j++) {
          const seg1 = segments[i];
          const seg2 = segments[j];

          // Skip if both segments are from the same edge
          if (seg1.edgeId === seg2.edgeId) continue;

          // Check for intersection
          const intersection = this._findOrthogonalIntersection(seg1, seg2);
          if (intersection) {
            // Use a key to avoid duplicate junction points
            const key = `${intersection.x},${intersection.y}`;
            junctionPoints.set(key, intersection);
          }
        }
      }

      // Check for T-intersections (endpoints on segments)
      for (const segment of segments) {
        // Check if any other edge's endpoint lies on this segment
        for (const otherEdge of netEdges) {
          if (otherEdge.id === segment.edgeId) continue;

          const section = otherEdge.sections?.[0];
          if (!section) continue;

          // Check start and end points
          for (const point of [section.startPoint, section.endPoint]) {
            if (this._isPointOnSegment(point, segment)) {
              const key = `${point.x},${point.y}`;
              junctionPoints.set(key, point);
            }
          }
        }
      }

      // Assign junction points to edges
      for (const edge of netEdges) {
        edge.junctionPoints = [];

        // Check which junction points lie on this edge's path
        const section = edge.sections?.[0];
        if (!section) continue;

        const points = [
          section.startPoint,
          ...(section.bendPoints || []),
          section.endPoint,
        ];

        for (const [, junctionPoint] of junctionPoints) {
          // Check if junction point lies on any segment of this edge
          for (let i = 0; i < points.length - 1; i++) {
            const segment: Segment = {
              start: points[i],
              end: points[i + 1],
              edgeId: edge.id,
              isHorizontal: points[i].y === points[i + 1].y,
            };

            if (
              this._isPointOnSegment(junctionPoint, segment) ||
              (junctionPoint.x === segment.start.x &&
                junctionPoint.y === segment.start.y) ||
              (junctionPoint.x === segment.end.x &&
                junctionPoint.y === segment.end.y)
            ) {
              // Check if this is a bend point for the current edge
              const isBendPoint = section.bendPoints?.some(
                (p) => p.x === junctionPoint.x && p.y === junctionPoint.y
              );

              // A point should be shown as a junction if:
              // 1. It's not a bend point, OR
              // 2. It's a bend point but multiple edges meet here (making it a true junction)

              // Count how many edges pass through this junction point
              let edgeCount = 0;
              for (const checkEdge of netEdges) {
                const checkSection = checkEdge.sections?.[0];
                if (!checkSection) continue;

                const checkPoints = [
                  checkSection.startPoint,
                  ...(checkSection.bendPoints || []),
                  checkSection.endPoint,
                ];

                // Check if this junction point is on any segment of the edge
                for (let k = 0; k < checkPoints.length - 1; k++) {
                  const checkSegment: Segment = {
                    start: checkPoints[k],
                    end: checkPoints[k + 1],
                    edgeId: checkEdge.id,
                    isHorizontal: checkPoints[k].y === checkPoints[k + 1].y,
                  };

                  if (
                    this._isPointOnSegment(junctionPoint, checkSegment) ||
                    (junctionPoint.x === checkSegment.start.x &&
                      junctionPoint.y === checkSegment.start.y) ||
                    (junctionPoint.x === checkSegment.end.x &&
                      junctionPoint.y === checkSegment.end.y)
                  ) {
                    edgeCount++;
                    break;
                  }
                }
              }

              // Only skip if it's a bend point AND only 1 edge passes through it
              const shouldSkip = isBendPoint && edgeCount <= 1;

              if (!shouldSkip) {
                // Check if we already have this junction point
                const alreadyExists = edge.junctionPoints!.some(
                  (jp) => jp.x === junctionPoint.x && jp.y === junctionPoint.y
                );
                if (!alreadyExists) {
                  edge.junctionPoints!.push(junctionPoint);
                }
              }
              break; // Found on this edge, no need to check other segments
            }
          }
        }
      }
    }
  }

  /**
   * Find the longest segment in an edge path and return its midpoint for label placement
   */
  private _findLongestSegmentMidpoint(points: { x: number; y: number }[]): {
    x: number;
    y: number;
    isHorizontal: boolean;
    segmentLength: number;
  } | null {
    if (points.length < 2) {
      return null;
    }

    let longestSegment = {
      startIndex: 0,
      length: 0,
      isHorizontal: false,
    };

    // Find the longest segment
    for (let i = 0; i < points.length - 1; i++) {
      const start = points[i];
      const end = points[i + 1];
      const dx = end.x - start.x;
      const dy = end.y - start.y;
      const length = Math.sqrt(dx * dx + dy * dy);
      const isHorizontal = Math.abs(dy) < 0.001; // Nearly horizontal

      if (length > longestSegment.length) {
        longestSegment = {
          startIndex: i,
          length: length,
          isHorizontal: isHorizontal,
        };
      }
    }

    // Calculate midpoint of the longest segment
    const start = points[longestSegment.startIndex];
    const end = points[longestSegment.startIndex + 1];

    return {
      x: (start.x + end.x) / 2,
      y: (start.y + end.y) / 2,
      isHorizontal: longestSegment.isHorizontal,
      segmentLength: longestSegment.length,
    };
  }

  /**
   * Create symbol nodes for nets that have symbols attached
   */
  private _createNetSymbolNodes(): ElkNode[] {
    const netSymbolNodes: ElkNode[] = [];

    // Iterate through all nets
    for (const [netId, net] of Object.entries(this.netlist.nets)) {
      // Check if net has __symbol_value in properties
      const symbolValueAttr = net.properties?.__symbol_value;
      if (!symbolValueAttr) continue;

      // Extract symbol content
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

      if (!symbolContent) continue;

      const netName = net.name || netId;

      // Look for all net symbol nodes in nodePositions for this net
      const netSymbolPattern = new RegExp(
        `^${this.netlist.root_ref}\\.${netName}\\.(\\d+)$`
      );
      const existingSymbolNumbers = new Set<number>();

      for (const nodeId of Object.keys(this._nodePositions)) {
        const match = nodeId.match(netSymbolPattern);
        if (match) {
          existingSymbolNumbers.add(parseInt(match[1], 10));
        }
      }

      // If no existing symbols found, create the first one
      if (existingSymbolNumbers.size === 0) {
        existingSymbolNumbers.add(1);
      }

      // Create a symbol node for each existing number
      for (const symbolNumber of existingSymbolNumbers) {
        const nodeId = `${this.netlist.root_ref}.${netName}.${symbolNumber}`;

        console.log(`Creating net symbol node with ID: ${nodeId}`);

        const symbolNode = this._createNetSymbolNode(
          nodeId,
          netId,
          net,
          symbolContent
        );

        if (symbolNode) {
          netSymbolNodes.push(symbolNode);
        }
      }
    }

    return netSymbolNodes;
  }

  /**
   * Create a symbol node for a net
   */
  private _createNetSymbolNode(
    nodeId: string,
    netId: string,
    net: Net,
    symbolContent: string
  ): ElkNode | null {
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

      // Create the node
      const node: ElkNode = {
        id: nodeId,
        type: NodeType.SYMBOL,
        netId: netId, // Store the net ID for later reference
        width: nodeWidth,
        height: nodeHeight,
        // Apply position if provided
        ...(this._nodePositions[nodeId] && {
          x: this._nodePositions[nodeId].x,
          y: this._nodePositions[nodeId].y,
          rotation: this._nodePositions[nodeId].rotation || 0,
        }),
        ports: [],
        labels: [
          // Net name label
          {
            text: net.name || netId,
            x: nodeWidth / 2 - 25,
            y: -20,
            width: 50,
            height: 15,
            textAlign: "center" as const,
          },
        ],
        properties: {
          "elk.portConstraints": "FIXED_POS",
          "elk.nodeSize.constraints": "MINIMUM_SIZE",
          "elk.nodeSize.minimum": `(${nodeWidth}, ${nodeHeight})`,
          // Mark as fixed if position is provided
          ...(this._nodePositions[nodeId] && {
            "elk.position": `(${this._nodePositions[nodeId].x},${this._nodePositions[nodeId].y})`,
            "elk.fixed": "true",
          }),
          // Mark this as a net symbol
          isNetSymbol: "true",
        },
      };

      if (this._nodePositions[nodeId]?.rotation) {
        console.log(
          `[LayoutEngine] Applied rotation ${this._nodePositions[nodeId].rotation} to net symbol ${nodeId}`
        );
      }

      // Create ports based on pin endpoints
      for (const pinEndpoint of symbolInfo.pinEndpoints) {
        // For net symbols, we create generic ports
        const portName =
          pinEndpoint.name === "~"
            ? `pin${pinEndpoint.number}`
            : pinEndpoint.name;
        const portRef = `${nodeId}.${portName}`;

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
        let snappedX: number;
        let snappedY: number;

        if (minDist === distToLeft) {
          side = "WEST";
          snappedX = 0;
          snappedY = portY;
        } else if (minDist === distToRight) {
          side = "EAST";
          snappedX = nodeWidth;
          snappedY = portY;
        } else if (minDist === distToTop) {
          side = "NORTH";
          snappedX = portX;
          snappedY = 0;
        } else {
          side = "SOUTH";
          snappedX = portX;
          snappedY = nodeHeight;
        }

        // Add the port
        node.ports?.push({
          id: portRef,
          x: snappedX,
          y: snappedY,
          width: 0,
          height: 0,
          labels: [], // No labels on net symbol ports
          properties: {
            "port.side": side,
            "port.alignment": "CENTER",
            pinNumber: pinEndpoint.number,
            pinType: pinEndpoint.type,
          },
          netId: netId, // Store net ID on the port
        });
      }

      return node;
    } catch (error) {
      console.error(
        `Failed to create net symbol node for net ${netId}:`,
        error
      );
      return null;
    }
  }

  /**
   * Get the next available symbol number for a given net
   */
  public getNextNetSymbolNumber(netName: string): number {
    const netSymbolPattern = new RegExp(
      `^${this.netlist.root_ref}\\.${netName}\\.(\\d+)$`
    );
    const existingNumbers = new Set<number>();

    for (const nodeId of Object.keys(this._nodePositions)) {
      const match = nodeId.match(netSymbolPattern);
      if (match) {
        existingNumbers.add(parseInt(match[1], 10));
      }
    }

    // Find the next available number
    let nextNumber = 1;
    while (existingNumbers.has(nextNumber)) {
      nextNumber++;
    }

    return nextNumber;
  }

  /**
   * Check if a net has a symbol definition
   */
  public netHasSymbol(netName: string): boolean {
    for (const [netId, net] of Object.entries(this.netlist.nets)) {
      if (
        (net.name === netName || netId === netName) &&
        net.properties?.__symbol_value
      ) {
        return true;
      }
    }
    return false;
  }

  /**
   * Extract net name from a net symbol node ID
   * Returns null if the node ID is not a net symbol node
   */
  public getNetNameFromSymbolNodeId(nodeId: string): string | null {
    // Pattern: root_ref.NET_NAME.number
    const parts = nodeId.split(".");
    if (parts.length < 3) return null;

    // Check if last part is a number
    const lastPart = parts[parts.length - 1];
    if (!/^\d+$/.test(lastPart)) return null;

    // Check if this matches the root ref
    const rootRefParts = this.netlist.root_ref.split(".");
    if (
      parts.slice(0, rootRefParts.length).join(".") !== this.netlist.root_ref
    ) {
      return null;
    }

    // Extract net name (everything between root_ref and the number)
    const netNameParts = parts.slice(rootRefParts.length, -1);
    const netName = netNameParts.join(".");

    // Verify this net exists and has a symbol
    for (const [netId, net] of Object.entries(this.netlist.nets)) {
      if (
        (net.name === netName || netId === netName) &&
        net.properties?.__symbol_value
      ) {
        return netName;
      }
    }

    return null;
  }
}
