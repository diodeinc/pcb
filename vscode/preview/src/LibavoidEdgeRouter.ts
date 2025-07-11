import { AvoidLib } from "libavoid-js";
import type { Avoid } from "libavoid-js";

// Input types
export interface Obstacle {
  id: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Port {
  id: string;
  x: number;
  y: number;
  visibilityDirection?: "NORTH" | "SOUTH" | "EAST" | "WEST" | "ALL";
}

export interface Hyperedge {
  id: string;
  ports: Port[];
}

// Output types
export interface Junction {
  id: string;
  x: number;
  y: number;
  hyperedgeId: string;
}

export interface PointToPointEdge {
  id: string;
  sourceType: "port" | "junction";
  sourceId: string;
  sourceX: number;
  sourceY: number;
  targetType: "port" | "junction";
  targetId: string;
  targetX: number;
  targetY: number;
  points: { x: number; y: number }[];
}

export class LibavoidEdgeRouter {
  private avoidLib: Avoid | null = null;
  private router: any = null;
  private shapes: Map<string, any> = new Map();
  private connectors: Map<string, any> = new Map();
  private junctions: Map<string, any> = new Map();
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
    this.router.setRoutingParameter(this.avoidLib.segmentPenalty, 1);
    this.router.setRoutingParameter(this.avoidLib.fixedSharedPathPenalty, 10);
    this.router.setRoutingParameter(this.avoidLib.anglePenalty, 100);
    this.router.setRoutingParameter(this.avoidLib.crossingPenalty, 0);
    this.router.setRoutingParameter(this.avoidLib.shapeBufferDistance, 15);
    this.router.setRoutingParameter(this.avoidLib.idealNudgingDistance, 1);

    // Enable hyperedge routing options
    this.router.setRoutingOption(
      this.avoidLib.improveHyperedgeRoutesMovingJunctions,
      true
    );
    // this.router.setRoutingOption(
    //   this.avoidLib.improveHyperedgeRoutesMovingAddingAndDeletingJunctions,
    //   true
    // );
    this.router.setRoutingOption(
      this.avoidLib.nudgeSharedPathsWithCommonEndPoint,
      true
    );
    this.router.setRoutingOption(
      this.avoidLib.penaliseOrthogonalSharedPathsAtConnEnds,
      true
    );
    this.router.setRoutingOption(
      this.avoidLib.nudgeOrthogonalSegmentsConnectedToShapes,
      true
    );
    this.router.setRoutingOption(
      this.avoidLib.nudgeOrthogonalTouchingColinearSegments,
      true
    );

    this.isInitialized = true;
  }

  /**
   * Route hyperedges using libavoid
   * @param obstacles List of rectangular obstacles to avoid
   * @param hyperedges List of hyperedges (each connecting multiple ports)
   * @returns Junctions and point-to-point edges
   */
  async route(
    obstacles: Obstacle[],
    hyperedges: Hyperedge[]
  ): Promise<{
    junctions: Junction[];
    edges: PointToPointEdge[];
  }> {
    if (!this.isInitialized) {
      await this.initialize();
    }

    console.log(
      "Starting libavoid routing:",
      JSON.stringify(obstacles),
      JSON.stringify(hyperedges)
    );

    if (!this.avoidLib || !this.router) {
      throw new Error("LibavoidEdgeRouter not initialized");
    }

    // Clear previous routing
    this.clearPreviousRouting();

    // Add obstacles
    for (const obstacle of obstacles) {
      this.addObstacle(obstacle);
    }

    // Process hyperedges
    const junctionResults: Junction[] = [];
    const edgeResults: PointToPointEdge[] = [];

    for (const hyperedge of hyperedges) {
      if (hyperedge.ports.length < 2) {
        continue; // Skip edges with less than 2 ports
      }

      if (hyperedge.ports.length === 2) {
        // Simple point-to-point edge
        const connectorId = `edge_${hyperedge.id}`;
        this.createSimpleConnector(
          connectorId,
          hyperedge.ports[0],
          hyperedge.ports[1]
        );
      } else {
        // Hyperedge with junction
        const junction = this.createJunction(hyperedge);
        if (junction) {
          // Don't add to results yet - we'll update position after routing

          // Create connectors from each port to the junction
          for (let i = 0; i < hyperedge.ports.length; i++) {
            const port = hyperedge.ports[i];
            const connectorId = `${hyperedge.id}_port_${i}`;
            this.createPortToJunctionConnector(connectorId, port, junction.id);
          }
        }
      }
    }

    // Register all junctions with the hyperedge rerouter after all connections are created
    for (const [hyperedgeId, junctionRef] of this.junctions) {
      this.router
        .hyperedgeRerouter()
        .registerHyperedgeForRerouting(junctionRef);
      console.log("Registered junction:", hyperedgeId);
    }

    // Process routing
    this.router.processTransaction();

    // Now get the actual junction positions after routing
    for (const [hyperedgeId, junctionRef] of this.junctions) {
      const pos = junctionRef.recommendedPosition();
      junctionResults.push({
        id: `junction_${hyperedgeId}`,
        x: pos.x,
        y: pos.y,
        hyperedgeId: hyperedgeId,
      });
    }

    // Extract routes
    for (const [connectorId, connector] of this.connectors) {
      const polyline = connector.displayRoute();
      const points: { x: number; y: number }[] = [];

      const size = polyline.size();
      for (let i = 0; i < size; i++) {
        const point = polyline.get_ps(i);
        points.push({ x: point.x, y: point.y });
      }

      // Determine source and target info from connector ID
      if (connectorId.startsWith("edge_")) {
        // Simple edge
        const hyperedgeId = connectorId.substring(5);
        const hyperedge = hyperedges.find((h) => h.id === hyperedgeId);
        if (hyperedge && hyperedge.ports.length === 2) {
          edgeResults.push({
            id: connectorId,
            sourceType: "port",
            sourceId: hyperedge.ports[0].id,
            sourceX: hyperedge.ports[0].x,
            sourceY: hyperedge.ports[0].y,
            targetType: "port",
            targetId: hyperedge.ports[1].id,
            targetX: hyperedge.ports[1].x,
            targetY: hyperedge.ports[1].y,
            points,
          });
        }
      } else if (connectorId.includes("_port_")) {
        // Port to junction edge
        // Extract hyperedge ID and port index more carefully
        // The connector ID format is: {hyperedgeId}_port_{portIndex}
        const portSeparator = "_port_";
        const portSeparatorIndex = connectorId.lastIndexOf(portSeparator);

        if (portSeparatorIndex !== -1) {
          const hyperedgeId = connectorId.substring(0, portSeparatorIndex);
          const portIndexStr = connectorId.substring(
            portSeparatorIndex + portSeparator.length
          );
          const portIndex = parseInt(portIndexStr);

          const hyperedge = hyperedges.find((h) => h.id === hyperedgeId);
          const junction = junctionResults.find(
            (j) => j.hyperedgeId === hyperedgeId
          );

          if (
            hyperedge &&
            junction &&
            !isNaN(portIndex) &&
            portIndex < hyperedge.ports.length
          ) {
            const port = hyperedge.ports[portIndex];
            edgeResults.push({
              id: connectorId,
              sourceType: "port",
              sourceId: port.id,
              sourceX: port.x,
              sourceY: port.y,
              targetType: "junction",
              targetId: junction.id,
              targetX: junction.x,
              targetY: junction.y,
              points,
            });
          }
        }
      }
    }

    console.log(
      "Libavoid routing results:",
      JSON.stringify(junctionResults),
      JSON.stringify(edgeResults)
    );

    return {
      junctions: junctionResults,
      edges: edgeResults,
    };
  }

  /**
   * Add an obstacle to the routing graph
   */
  private addObstacle(obstacle: Obstacle): void {
    if (!this.avoidLib) {
      return;
    }

    const padding = 0;
    const topLeft = new this.avoidLib.Point(
      obstacle.x - padding,
      obstacle.y - padding
    );
    const bottomRight = new this.avoidLib.Point(
      obstacle.x + obstacle.width + padding,
      obstacle.y + obstacle.height + padding
    );

    const rect = new this.avoidLib.Rectangle(topLeft, bottomRight);
    const shape = new this.avoidLib.ShapeRef(this.router, rect);

    this.shapes.set(obstacle.id, shape);
  }

  /**
   * Create a simple connector between two ports
   */
  private createSimpleConnector(
    connectorId: string,
    sourcePort: Port,
    targetPort: Port
  ): void {
    if (!this.avoidLib) {
      return;
    }

    const srcPoint = new this.avoidLib.Point(sourcePort.x, sourcePort.y);
    const dstPoint = new this.avoidLib.Point(targetPort.x, targetPort.y);

    // Convert visibility directions to ConnDirFlags
    const srcVisDirs = this.getConnDirFlags(sourcePort.visibilityDirection);
    const dstVisDirs = this.getConnDirFlags(targetPort.visibilityDirection);

    const srcEnd = new this.avoidLib.ConnEnd(srcPoint, srcVisDirs);
    const dstEnd = new this.avoidLib.ConnEnd(dstPoint, dstVisDirs);

    const connector = new this.avoidLib.ConnRef(this.router, srcEnd, dstEnd);
    connector.setRoutingType(this.avoidLib.OrthogonalRouting);

    this.connectors.set(connectorId, connector);
  }

  /**
   * Create a junction for a hyperedge
   */
  private createJunction(hyperedge: Hyperedge): Junction | null {
    if (!this.avoidLib || hyperedge.ports.length < 2) {
      return null;
    }

    // Calculate centroid of all ports
    const centerX =
      hyperedge.ports.reduce((sum, p) => sum + p.x, 0) / hyperedge.ports.length;
    const centerY =
      hyperedge.ports.reduce((sum, p) => sum + p.y, 0) / hyperedge.ports.length;

    // Create junction
    const junctionPoint = new this.avoidLib.Point(centerX, centerY);
    const junction = new this.avoidLib.JunctionRef(this.router, junctionPoint);

    // Let libavoid optimize the junction position
    junction.setPositionFixed(false);

    // Store junction
    const junctionId = `junction_${hyperedge.id}`;
    this.junctions.set(junctionId, junction);

    // Get the actual position after creation
    const pos = junction.position();

    return {
      id: junctionId,
      x: pos.x,
      y: pos.y,
      hyperedgeId: hyperedge.id,
    };
  }

  /**
   * Create a connector from a port to a junction
   */
  private createPortToJunctionConnector(
    connectorId: string,
    port: Port,
    junctionId: string
  ): void {
    if (!this.avoidLib) {
      return;
    }

    // Extract hyperedge ID from junction ID
    const junction = this.junctions.get(junctionId);

    const portPoint = new this.avoidLib.Point(port.x, port.y);
    const portVisDirs = this.getConnDirFlags(port.visibilityDirection);
    const portEnd = new this.avoidLib.ConnEnd(portPoint, portVisDirs);

    // Try creating ConnEnd directly with the junction reference
    // The second parameter is the classId (connection pin ID), using 0 as default
    const junctionEnd = new this.avoidLib.ConnEnd(junction);

    const connector = new this.avoidLib.ConnRef(
      this.router,
      junctionEnd,
      portEnd
    );
    console.log("Created connector:", port.id, junctionId);
    connector.setRoutingType(this.avoidLib.OrthogonalRouting);

    this.connectors.set(connectorId, connector);
  }

  /**
   * Convert visibility direction to libavoid ConnDirFlags
   */
  private getConnDirFlags(
    direction?: "NORTH" | "SOUTH" | "EAST" | "WEST" | "ALL"
  ): number {
    if (!this.avoidLib || !direction) {
      return this.avoidLib?.ConnDirAll || 15; // Default to all directions
    }

    switch (direction) {
      case "NORTH":
        return this.avoidLib.ConnDirUp;
      case "SOUTH":
        return this.avoidLib.ConnDirDown;
      case "EAST":
        return this.avoidLib.ConnDirRight;
      case "WEST":
        return this.avoidLib.ConnDirLeft;
      case "ALL":
        return this.avoidLib.ConnDirAll;
      default:
        return this.avoidLib.ConnDirAll;
    }
  }

  /**
   * Clear previous routing data
   */
  private clearPreviousRouting(): void {
    if (!this.avoidLib) return;

    // Delete all shapes
    for (const [_, shape] of this.shapes) {
      this.router.deleteShape(shape);
    }
    this.shapes.clear();

    // Delete all connectors
    for (const [_, connector] of this.connectors) {
      this.router.deleteConnector(connector);
    }
    this.connectors.clear();

    // Junctions are automatically deleted when their connectors are deleted
    this.junctions.clear();
  }

  /**
   * Clean up resources
   */
  destroy(): void {
    this.clearPreviousRouting();

    if (this.router && this.avoidLib) {
      this.router = null;
    }

    this.avoidLib = null;
    this.isInitialized = false;
  }
}
