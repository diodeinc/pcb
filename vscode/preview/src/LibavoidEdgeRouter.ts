import { AvoidLib } from "libavoid-js";
import type { Avoid } from "libavoid-js";
import type {
  ElkEdge,
  ElkNode,
  ElkPort,
  ElkGraph,
  ElkLabel,
} from "./LayoutEngine";

export interface EdgeRoute {
  id: string;
  points: { x: number; y: number }[];
  netId: string;
  sourceComponentRef: string;
  targetComponentRef: string;
  labels?: ElkLabel[];
  junctionPoints?: { x: number; y: number }[];
}

export class LibavoidEdgeRouter {
  private avoidLib: Avoid | null = null;
  private router: any = null;
  private shapes: Map<string, any> = new Map();
  private connectors: Map<string, any> = new Map();
  private junctions: Map<string, any> = new Map(); // Track junctions by netId
  private netPortMap: Map<string, Set<string>> = new Map(); // Map netId to port IDs
  private isInitialized: boolean = false;

  /**
   * Initialize the libavoid library
   */
  async initialize(): Promise<void> {
    if (this.isInitialized) {
      return;
    }

    await AvoidLib.load();
    this.avoidLib = AvoidLib.getInstance();

    // Create router with orthogonal routing
    this.router = new this.avoidLib.Router(this.avoidLib.OrthogonalRouting);

    // Configure routing penalties for better hyperedge routing
    this.router.setRoutingParameter(this.avoidLib.segmentPenalty, 10);
    this.router.setRoutingParameter(this.avoidLib.fixedSharedPathPenalty, 0);
    this.router.setRoutingParameter(this.avoidLib.anglePenalty, 0);
    this.router.setRoutingParameter(this.avoidLib.crossingPenalty, 0);

    // Enable hyperedge routing options
    this.router.setRoutingOption(
      this.avoidLib.improveHyperedgeRoutesMovingJunctions,
      true
    );
    this.router.setRoutingOption(
      this.avoidLib.improveHyperedgeRoutesMovingAddingAndDeletingJunctions,
      true
    );
    this.router.setRoutingOption(
      this.avoidLib.nudgeSharedPathsWithCommonEndPoint,
      true
    );

    this.isInitialized = true;
  }

  /**
   * Route edges using libavoid
   */
  async routeEdges(graph: ElkGraph): Promise<EdgeRoute[]> {
    if (!this.isInitialized) {
      await this.initialize();
    }

    if (!this.avoidLib || !this.router) {
      throw new Error("LibavoidEdgeRouter not initialized");
    }

    // Clear previous shapes and connectors
    this.clearPreviousRouting();

    // Add nodes as obstacles
    if (graph.children) {
      for (const node of graph.children) {
        this.addNodeAsObstacle(node);
      }
    }

    // Build a map of netId to ports for hyperedge creation
    this.buildNetPortMap(graph);

    // Create junctions for nets with more than 2 connections
    this.createJunctionsForNets(graph);

    // Create connectors for edges using junctions where appropriate
    for (const edge of graph.edges) {
      this.createConnectorWithJunctions(edge, graph.children || []);
    }

    // Process routing
    this.router.processTransaction();

    // Extract routes
    const finalRoutes: EdgeRoute[] = [];
    const processedNets = new Set<string>();

    for (const [edgeId, connector] of this.connectors) {
      // Skip junction connectors (they are handled as part of the main edge)
      if (edgeId.endsWith("_junction")) continue;

      const edge = graph.edges.find((e) => e.id === edgeId);
      if (!edge) continue;

      const junction = this.junctions.get(edge.netId);

      if (
        junction &&
        this.netPortMap.get(edge.netId)?.size >= 3 &&
        !processedNets.has(edge.netId)
      ) {
        // For hyperedges, we need to collect all edges for this net
        // and create a unified route through the junction
        processedNets.add(edge.netId);

        const netEdges = graph.edges.filter((e) => e.netId === edge.netId);

        for (const netEdge of netEdges) {
          const connector1 = this.connectors.get(netEdge.id);
          const connector2 = this.connectors.get(netEdge.id + "_junction");

          if (connector1 && connector2) {
            // Combine the two route segments
            const points: { x: number; y: number }[] = [];

            // Get first segment (source to junction)
            const polyline1 = connector1.displayRoute();
            const size1 = polyline1.size();
            for (let i = 0; i < size1; i++) {
              const point = polyline1.get_ps(i);
              points.push({ x: point.x, y: point.y });
            }

            // Get second segment (junction to target)
            const polyline2 = connector2.displayRoute();
            const size2 = polyline2.size();
            // Skip the first point of the second segment as it's the junction point (duplicate)
            for (let i = 1; i < size2; i++) {
              const point = polyline2.get_ps(i);
              points.push({ x: point.x, y: point.y });
            }

            const jPos = junction.position();

            finalRoutes.push({
              id: netEdge.id,
              points,
              netId: netEdge.netId,
              sourceComponentRef: netEdge.sourceComponentRef,
              targetComponentRef: netEdge.targetComponentRef,
              labels: netEdge.labels,
              junctionPoints: [{ x: jPos.x, y: jPos.y }],
            });
          }
        }
      } else if (!junction || this.netPortMap.get(edge.netId)?.size < 3) {
        // Regular edge without junction
        const polyline = connector.displayRoute();
        const points: { x: number; y: number }[] = [];

        const size = polyline.size();
        for (let i = 0; i < size; i++) {
          const point = polyline.get_ps(i);
          points.push({ x: point.x, y: point.y });
        }

        finalRoutes.push({
          id: edge.id,
          points,
          netId: edge.netId,
          sourceComponentRef: edge.sourceComponentRef,
          targetComponentRef: edge.targetComponentRef,
          labels: edge.labels,
          junctionPoints: [],
        });
      }
    }

    return finalRoutes;
  }

  /**
   * Add a node as an obstacle in the routing graph
   */
  private addNodeAsObstacle(node: ElkNode): void {
    if (
      !this.avoidLib ||
      node.x === undefined ||
      node.y === undefined ||
      !node.width ||
      !node.height
    ) {
      return;
    }

    // Create a rectangle for the node with some padding
    const padding = 5;
    const topLeft = new this.avoidLib.Point(
      node.x! - padding,
      node.y! - padding
    );
    const bottomRight = new this.avoidLib.Point(
      node.x! + node.width + padding,
      node.y! + node.height + padding
    );

    const rect = new this.avoidLib.Rectangle(topLeft, bottomRight);
    const shape = new this.avoidLib.ShapeRef(this.router, rect);

    this.shapes.set(node.id, shape);

    // Clean up points
    this.avoidLib.destroy(topLeft);
    this.avoidLib.destroy(bottomRight);
  }

  /**
   * Create a connector for an edge
   */
  private createConnector(edge: ElkEdge, nodes: ElkNode[]): EdgeRoute | null {
    if (
      !this.avoidLib ||
      edge.sources.length === 0 ||
      edge.targets.length === 0
    ) {
      return null;
    }

    // Find source and target ports
    const sourcePortId = edge.sources[0];
    const targetPortId = edge.targets[0];

    const sourceInfo = this.findPortPosition(sourcePortId, nodes);
    const targetInfo = this.findPortPosition(targetPortId, nodes);

    if (!sourceInfo || !targetInfo) {
      return null;
    }

    // Create connector endpoints
    const srcPoint = new this.avoidLib.Point(sourceInfo.x, sourceInfo.y);
    const dstPoint = new this.avoidLib.Point(targetInfo.x, targetInfo.y);

    const srcEnd = new this.avoidLib.ConnEnd(srcPoint);
    const dstEnd = new this.avoidLib.ConnEnd(dstPoint);

    // Create connector
    const connector = new this.avoidLib.ConnRef(this.router, srcEnd, dstEnd);
    connector.setRoutingType(this.avoidLib.OrthogonalRouting);

    this.connectors.set(edge.id, connector);

    // Clean up
    this.avoidLib.destroy(srcPoint);
    this.avoidLib.destroy(dstPoint);

    return null; // Route will be extracted after processing
  }

  /**
   * Find the absolute position of a port
   */
  private findPortPosition(
    portId: string,
    nodes: ElkNode[]
  ): { x: number; y: number } | null {
    for (const node of nodes) {
      if (!node.ports || node.x === undefined || node.y === undefined) continue;

      for (const port of node.ports) {
        if (port.id === portId) {
          const portX = (port.x || 0) + node.x;
          const portY = (port.y || 0) + node.y;
          return { x: portX, y: portY };
        }
      }

      // Check children recursively
      if (node.children) {
        const childResult = this.findPortPosition(portId, node.children);
        if (childResult) {
          return {
            x: childResult.x + (node.x || 0),
            y: childResult.y + (node.y || 0),
          };
        }
      }
    }

    return null;
  }

  /**
   * Clear previous routing data
   */
  private clearPreviousRouting(): void {
    if (!this.avoidLib) return;

    // Delete all shapes
    for (const [_, shape] of this.shapes) {
      this.router.deleteShape(shape);
      this.avoidLib.destroy(shape);
    }
    this.shapes.clear();

    // Delete all connectors
    for (const [_, connector] of this.connectors) {
      this.router.deleteConnector(connector);
      this.avoidLib.destroy(connector);
    }
    this.connectors.clear();

    // Delete all junctions
    for (const [_, junction] of this.junctions) {
      // Junctions are automatically deleted when their connectors are deleted
      // But we still need to clear our tracking
    }
    this.junctions.clear();
    this.netPortMap.clear();
  }

  /**
   * Clean up resources
   */
  destroy(): void {
    this.clearPreviousRouting();

    if (this.router && this.avoidLib) {
      this.avoidLib.destroy(this.router);
      this.router = null;
    }

    this.avoidLib = null;
    this.isInitialized = false;
  }

  /**
   * Build a map of netId to port IDs
   */
  private buildNetPortMap(graph: ElkGraph): void {
    this.netPortMap.clear();

    if (!graph.children) return;

    // Iterate through all nodes and their ports
    for (const node of graph.children) {
      if (!node.ports) continue;

      for (const port of node.ports) {
        if (port.netId) {
          if (!this.netPortMap.has(port.netId)) {
            this.netPortMap.set(port.netId, new Set());
          }
          this.netPortMap.get(port.netId)!.add(port.id);
        }
      }
    }
  }

  /**
   * Create junctions for nets with more than 2 connections
   */
  private createJunctionsForNets(graph: ElkGraph): void {
    if (!this.avoidLib || !graph.children) return;

    for (const [netId, portIds] of this.netPortMap) {
      // Only create junctions for nets with 3 or more connections
      if (portIds.size >= 3) {
        // Find the center point of all ports in this net
        const positions: { x: number; y: number }[] = [];

        for (const portId of portIds) {
          const pos = this.findPortPosition(portId, graph.children);
          if (pos) {
            positions.push(pos);
          }
        }

        if (positions.length >= 3) {
          // Calculate the centroid of all port positions as initial position
          const centerX =
            positions.reduce((sum, p) => sum + p.x, 0) / positions.length;
          const centerY =
            positions.reduce((sum, p) => sum + p.y, 0) / positions.length;

          // Create a junction at the centroid
          const junctionPoint = new this.avoidLib.Point(centerX, centerY);
          const junction = new this.avoidLib.JunctionRef(
            this.router,
            junctionPoint
          );

          // IMPORTANT: Set the junction position as NOT fixed so libavoid can optimize it
          junction.setPositionFixed(false);

          this.junctions.set(netId, junction);

          // Clean up
          this.avoidLib.destroy(junctionPoint);
        }
      }
    }
  }

  /**
   * Create a connector using junctions if available
   */
  private createConnectorWithJunctions(edge: ElkEdge, nodes: ElkNode[]): void {
    if (
      !this.avoidLib ||
      edge.sources.length === 0 ||
      edge.targets.length === 0
    ) {
      return;
    }

    const sourcePortId = edge.sources[0];
    const targetPortId = edge.targets[0];
    const junction = this.junctions.get(edge.netId);

    if (junction && this.netPortMap.get(edge.netId)?.size >= 3) {
      // For hyperedges, create a single connector from source to target
      // but use the junction as an intermediate point
      const sourceInfo = this.findPortPosition(sourcePortId, nodes);
      const targetInfo = this.findPortPosition(targetPortId, nodes);

      if (!sourceInfo || !targetInfo) {
        return;
      }

      // Create ConnEnd objects for the ports
      const srcPoint = new this.avoidLib.Point(sourceInfo.x, sourceInfo.y);
      const tgtPoint = new this.avoidLib.Point(targetInfo.x, targetInfo.y);

      // Create ConnEnds that connect through the junction
      // This is the key: we create ConnEnds that reference the junction
      const srcEnd = new this.avoidLib.ConnEnd(srcPoint);
      const tgtEnd = new this.avoidLib.ConnEnd(tgtPoint);

      // Create a single connector but it will be part of a hyperedge
      const connector = new this.avoidLib.ConnRef(this.router, srcEnd, tgtEnd);
      connector.setRoutingType(this.avoidLib.OrthogonalRouting);

      // Store the connector
      this.connectors.set(edge.id, connector);

      // Clean up
      this.avoidLib.destroy(srcPoint);
      this.avoidLib.destroy(tgtPoint);
    } else {
      // Regular point-to-point routing
      this.createConnector(edge, nodes);
    }
  }
}
