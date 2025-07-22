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
  context?: any; // Additional context information to pass through routing
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
  context?: any; // Context information passed through from hyperedge
}

export class LibavoidEdgeRouter {
  private avoidLib: Avoid | null = null;
  private router: any = null;
  private shapes: Map<string, any> = new Map();
  private connectors: Map<string, any> = new Map();
  private junctions: Map<string, any> = new Map();
  private isInitialized: boolean = false;

  // Store metadata about connectors to avoid string parsing
  private connectorMetadata: Map<
    string,
    {
      type: "simple" | "port-to-junction";
      hyperedgeId: string;
      sourcePortIndex?: number;
      targetPortIndex?: number;
    }
  > = new Map();

  /**
   * Check if a series of points forms an orthogonal path
   * (all segments are either horizontal or vertical)
   */
  private isOrthogonalPath(points: { x: number; y: number }[]): boolean {
    if (points.length < 2) return true;

    for (let i = 1; i < points.length; i++) {
      const prev = points[i - 1];
      const curr = points[i];

      // Check if segment is neither horizontal nor vertical
      if (prev.x !== curr.x && prev.y !== curr.y) {
        return false;
      }
    }

    return true;
  }

  /**
   * Initialize the libavoid library
   */
  async initialize(): Promise<void> {
    if (this.isInitialized) {
      return;
    }

    await AvoidLib.load("/wasm/libavoid.wasm");
    this.avoidLib = AvoidLib.getInstance();

    // Create router with orthogonal routing
    this.router = new this.avoidLib.Router(this.avoidLib.OrthogonalRouting);

    // Configure routing penalties for better hyperedge routing
    this.router.setRoutingParameter(this.avoidLib.segmentPenalty, 1);
    this.router.setRoutingParameter(this.avoidLib.fixedSharedPathPenalty, 10);
    this.router.setRoutingParameter(this.avoidLib.anglePenalty, 100);
    this.router.setRoutingParameter(this.avoidLib.crossingPenalty, 0);
    this.router.setRoutingParameter(this.avoidLib.shapeBufferDistance, 5);
    this.router.setRoutingParameter(this.avoidLib.idealNudgingDistance, 12.7);

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
      false
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

    console.log("LibavoidEdgeRouter initialized");
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
        const connectorId = `connector_${hyperedge.id}_simple`;
        this.createSimpleConnector(
          connectorId,
          hyperedge.ports[0],
          hyperedge.ports[1],
          hyperedge.id
        );
      } else {
        // Hyperedge with junction
        const junction = this.createJunction(hyperedge);
        if (junction) {
          // Don't add to results yet - we'll update position after routing

          // Create connectors from each port to the junction
          for (let i = 0; i < hyperedge.ports.length; i++) {
            const port = hyperedge.ports[i];
            const connectorId = `connector_${hyperedge.id}_p${i}`;
            this.createPortToJunctionConnector(
              connectorId,
              port,
              junction.id,
              hyperedge.id,
              i
            );
          }
        }
      }
    }

    // Register all junctions with the hyperedge rerouter after all connections are created
    for (const [, junctionRef] of this.junctions) {
      this.router
        .hyperedgeRerouter()
        .registerHyperedgeForRerouting(junctionRef);
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

      // Get metadata for this connector
      const metadata = this.connectorMetadata.get(connectorId);
      if (!metadata) {
        console.warn(`No metadata found for connector ${connectorId}`);
        continue;
      }

      const hyperedge = hyperedges.find((h) => h.id === metadata.hyperedgeId);
      if (!hyperedge) {
        console.warn(`Hyperedge ${metadata.hyperedgeId} not found`);
        continue;
      }

      if (metadata.type === "simple") {
        // Simple edge between two ports
        if (hyperedge.ports.length === 2) {
          const sourcePort = hyperedge.ports[0];
          const targetPort = hyperedge.ports[1];
          const correctedPoints = this.ensureEndpoints(
            points,
            sourcePort.x,
            sourcePort.y,
            targetPort.x,
            targetPort.y
          );

          // Check if the path is orthogonal
          if (!this.isOrthogonalPath(correctedPoints)) {
            console.warn(
              `Skipping non-orthogonal edge ${connectorId} with points:`,
              correctedPoints
            );
            continue;
          }

          edgeResults.push({
            id: connectorId,
            sourceType: "port",
            sourceId: sourcePort.id,
            sourceX: sourcePort.x,
            sourceY: sourcePort.y,
            targetType: "port",
            targetId: targetPort.id,
            targetX: targetPort.x,
            targetY: targetPort.y,
            points: correctedPoints,
            context: hyperedge.context,
          });
        }
      } else if (metadata.type === "port-to-junction") {
        // Port to junction edge
        const junction = junctionResults.find(
          (j) => j.hyperedgeId === metadata.hyperedgeId
        );

        if (
          junction &&
          metadata.sourcePortIndex !== undefined &&
          metadata.sourcePortIndex < hyperedge.ports.length
        ) {
          const port = hyperedge.ports[metadata.sourcePortIndex];
          const correctedPoints = this.ensureEndpoints(
            points,
            port.x,
            port.y,
            junction.x,
            junction.y
          );

          // Check if the path is orthogonal
          if (!this.isOrthogonalPath(correctedPoints)) {
            console.warn(
              `Skipping non-orthogonal edge ${connectorId} with points:`,
              correctedPoints
            );
            continue;
          }

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
            points: correctedPoints,
            context: hyperedge.context,
          });
        }
      }
    }

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
    targetPort: Port,
    hyperedgeId: string
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
    this.connectorMetadata.set(connectorId, {
      type: "simple",
      hyperedgeId: hyperedgeId,
    });
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
    junctionId: string,
    hyperedgeId: string,
    portIndex: number
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
    connector.setRoutingType(this.avoidLib.OrthogonalRouting);

    this.connectors.set(connectorId, connector);
    this.connectorMetadata.set(connectorId, {
      type: "port-to-junction",
      hyperedgeId: hyperedgeId,
      sourcePortIndex: portIndex,
    });
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
   * Ensure points array starts at source and ends at target coordinates
   */
  private ensureEndpoints(
    points: { x: number; y: number }[],
    sourceX: number,
    sourceY: number,
    targetX: number,
    targetY: number
  ): { x: number; y: number }[] {
    if (points.length === 0) {
      // If no points, create a direct line
      return [
        { x: sourceX, y: sourceY },
        { x: targetX, y: targetY },
      ];
    }

    const result = [...points];

    // Check if first point matches source
    const firstPoint = result[0];
    if (firstPoint.x !== sourceX || firstPoint.y !== sourceY) {
      // Add source point at the beginning
      result.unshift({ x: sourceX, y: sourceY });
    }

    // Check if last point matches target
    const lastPoint = result[result.length - 1];
    if (lastPoint.x !== targetX || lastPoint.y !== targetY) {
      // Add target point at the end
      result.push({ x: targetX, y: targetY });
    }

    return result;
  }

  /**
   * Clear previous routing data
   */
  private clearPreviousRouting(): void {
    if (!this.avoidLib) return;

    // Delete all shapes
    for (const [, shape] of this.shapes) {
      this.router.deleteShape(shape);
    }
    this.shapes.clear();

    // Delete all connectors
    for (const [, connector] of this.connectors) {
      this.router.deleteConnector(connector);
    }
    this.connectors.clear();

    // Clear connector metadata
    this.connectorMetadata.clear();

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
