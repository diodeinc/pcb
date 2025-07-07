import { createCanvas } from "canvas";
import type { Canvas, CanvasRenderingContext2D } from "canvas";
import type { ElkGraph, ElkNode, ElkEdge, NodeType } from "../LayoutEngine";

/**
 * Simple image renderer for graphs that creates visual snapshots.
 * Produces basic visual representations of the layout graph.
 */
export class GraphImageRenderer {
  private canvas: Canvas;
  private ctx: CanvasRenderingContext2D;
  private padding = 20;

  constructor(width: number = 800, height: number = 600) {
    this.canvas = createCanvas(width, height);
    this.ctx = this.canvas.getContext("2d");
  }

  makeImage(graph: ElkGraph): Buffer {
    // Clear canvas with white background
    this.ctx.fillStyle = "white";
    this.ctx.fillRect(0, 0, this.canvas.width, this.canvas.height);

    // Calculate bounds and scale
    const bounds = this.calculateBounds(graph);
    const scale = this.calculateScale(bounds);

    // Apply padding and scaling
    this.ctx.save();
    this.ctx.translate(this.padding, this.padding);
    this.ctx.scale(scale, scale);
    this.ctx.translate(-bounds.minX, -bounds.minY);

    // Render edges first (so they appear behind nodes)
    if (graph.edges) {
      graph.edges.forEach((edge) => this.renderEdge(edge, graph));
    }

    // Render nodes
    if (graph.children) {
      graph.children.forEach((node) => this.renderNode(node));
    }

    this.ctx.restore();

    // Add title
    this.ctx.fillStyle = "black";
    this.ctx.font = "14px Arial";
    this.ctx.fillText(`Graph: ${graph.id}`, 10, 15);

    return this.canvas.toBuffer("image/png");
  }

  private calculateBounds(graph: ElkGraph): {
    minX: number;
    minY: number;
    maxX: number;
    maxY: number;
    width: number;
    height: number;
  } {
    let minX = Infinity,
      minY = Infinity,
      maxX = -Infinity,
      maxY = -Infinity;

    const processNode = (node: ElkNode) => {
      if (
        node.x !== undefined &&
        node.y !== undefined &&
        node.width !== undefined &&
        node.height !== undefined
      ) {
        minX = Math.min(minX, node.x);
        minY = Math.min(minY, node.y);
        maxX = Math.max(maxX, node.x + node.width);
        maxY = Math.max(maxY, node.y + node.height);
      }
      if (node.children) {
        node.children.forEach(processNode);
      }
    };

    if (graph.children) {
      graph.children.forEach(processNode);
    }

    // Fallback to reasonable defaults if no nodes have positions
    if (minX === Infinity) {
      minX = 0;
      minY = 0;
      maxX = 400;
      maxY = 300;
    }

    return {
      minX,
      minY,
      maxX,
      maxY,
      width: maxX - minX,
      height: maxY - minY,
    };
  }

  private calculateScale(bounds: { width: number; height: number }): number {
    const availableWidth = this.canvas.width - 2 * this.padding;
    const availableHeight = this.canvas.height - 2 * this.padding - 20; // Extra space for title

    const scaleX = availableWidth / bounds.width;
    const scaleY = availableHeight / bounds.height;

    return Math.min(scaleX, scaleY, 1); // Don't scale up, only down if needed
  }

  private renderNode(node: ElkNode) {
    if (
      node.x === undefined ||
      node.y === undefined ||
      node.width === undefined ||
      node.height === undefined
    ) {
      return;
    }

    // Draw node rectangle
    this.ctx.strokeStyle = this.getNodeColor(node.type);
    this.ctx.lineWidth = 2;
    this.ctx.strokeRect(node.x, node.y, node.width, node.height);

    // Fill with light color
    this.ctx.fillStyle = this.getNodeFillColor(node.type);
    this.ctx.fillRect(node.x, node.y, node.width, node.height);

    // Draw node ID
    this.ctx.fillStyle = "black";
    this.ctx.font = "10px Arial";
    this.ctx.textAlign = "center";
    this.ctx.textBaseline = "middle";

    const nodeId = node.id.split(".").pop() || node.id;
    this.ctx.fillText(
      nodeId,
      node.x + node.width / 2,
      node.y + node.height / 2
    );

    // Draw node type
    this.ctx.font = "8px Arial";
    this.ctx.fillStyle = "gray";
    this.ctx.fillText(
      `[${node.type}]`,
      node.x + node.width / 2,
      node.y + node.height / 2 + 12
    );

    // Draw ports
    if (node.ports) {
      node.ports.forEach((port) => {
        this.renderPort(port, node);
      });
    }

    // Render child nodes
    if (node.children) {
      node.children.forEach((child) => this.renderNode(child));
    }
  }

  private renderPort(port: any, parentNode: ElkNode) {
    if (
      parentNode.x === undefined ||
      parentNode.y === undefined ||
      parentNode.width === undefined ||
      parentNode.height === undefined
    ) {
      return;
    }

    const portSize = 6;
    let portX = parentNode.x;
    let portY = parentNode.y;

    // Position port based on side property
    const side = port.properties?.["port.side"] || "WEST";
    switch (side) {
      case "NORTH":
        portX = parentNode.x + (port.x || parentNode.width / 2);
        portY = parentNode.y;
        break;
      case "SOUTH":
        portX = parentNode.x + (port.x || parentNode.width / 2);
        portY = parentNode.y + parentNode.height;
        break;
      case "EAST":
        portX = parentNode.x + parentNode.width;
        portY = parentNode.y + (port.y || parentNode.height / 2);
        break;
      case "WEST":
      default:
        portX = parentNode.x;
        portY = parentNode.y + (port.y || parentNode.height / 2);
        break;
    }

    // Draw port
    this.ctx.fillStyle = "darkblue";
    this.ctx.fillRect(
      portX - portSize / 2,
      portY - portSize / 2,
      portSize,
      portSize
    );
  }

  private renderEdge(edge: ElkEdge, graph: ElkGraph) {
    // Find source and target nodes/ports
    const sourcePort = this.findPort(edge.sources?.[0], graph);
    const targetPort = this.findPort(edge.targets?.[0], graph);

    if (!sourcePort || !targetPort) {
      return;
    }

    // Draw line between ports
    this.ctx.strokeStyle = "blue";
    this.ctx.lineWidth = 1;
    this.ctx.beginPath();
    this.ctx.moveTo(sourcePort.x, sourcePort.y);

    // Simple straight line for now
    this.ctx.lineTo(targetPort.x, targetPort.y);

    this.ctx.stroke();

    // Draw edge label if present
    if (edge.labels && edge.labels.length > 0) {
      const midX = (sourcePort.x + targetPort.x) / 2;
      const midY = (sourcePort.y + targetPort.y) / 2;

      this.ctx.fillStyle = "blue";
      this.ctx.font = "8px Arial";
      this.ctx.textAlign = "center";
      this.ctx.fillText(edge.labels[0].text, midX, midY - 5);
    }
  }

  private findPort(
    portId: string | undefined,
    graph: ElkGraph
  ): { x: number; y: number } | null {
    if (!portId) return null;

    const findInNode = (node: ElkNode): { x: number; y: number } | null => {
      if (
        node.x === undefined ||
        node.y === undefined ||
        node.width === undefined ||
        node.height === undefined
      ) {
        return null;
      }

      // Check if this node has the port
      if (node.ports) {
        const port = node.ports.find((p) => p.id === portId);
        if (port) {
          const side = port.properties?.["port.side"] || "WEST";
          switch (side) {
            case "NORTH":
              return { x: node.x + (port.x || node.width / 2), y: node.y };
            case "SOUTH":
              return {
                x: node.x + (port.x || node.width / 2),
                y: node.y + node.height,
              };
            case "EAST":
              return {
                x: node.x + node.width,
                y: node.y + (port.y || node.height / 2),
              };
            case "WEST":
            default:
              return { x: node.x, y: node.y + (port.y || node.height / 2) };
          }
        }
      }

      // Check child nodes
      if (node.children) {
        for (const child of node.children) {
          const result = findInNode(child);
          if (result) return result;
        }
      }

      return null;
    };

    if (graph.children) {
      for (const node of graph.children) {
        const result = findInNode(node);
        if (result) return result;
      }
    }

    return null;
  }

  private getNodeColor(type: NodeType | string): string {
    switch (type) {
      case "module":
        return "purple";
      case "component":
        return "green";
      case "resistor":
        return "brown";
      case "capacitor":
        return "orange";
      case "inductor":
        return "teal";
      case "net_reference":
        return "red";
      case "net_junction":
        return "black";
      case "symbol":
        return "darkgreen";
      default:
        return "gray";
    }
  }

  private getNodeFillColor(type: NodeType | string): string {
    switch (type) {
      case "module":
        return "lavender";
      case "component":
        return "lightgreen";
      case "resistor":
        return "wheat";
      case "capacitor":
        return "peachpuff";
      case "inductor":
        return "lightcyan";
      case "net_reference":
        return "pink";
      case "net_junction":
        return "white";
      case "symbol":
        return "palegreen";
      default:
        return "lightgray";
    }
  }
}
