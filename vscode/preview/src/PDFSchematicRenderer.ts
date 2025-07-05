import { jsPDF } from "jspdf";
import { Netlist } from "./types/NetlistTypes";
import {
  SchematicRenderer,
  SchematicConfig,
  DEFAULT_CONFIG,
  ElkNode,
  ElkEdge,
  NodeType,
  ElkGraph,
  NetReferenceType,
} from "./renderer";

export interface PDFRenderOptions {
  pageSize: {
    width: number; // Width in points (72 points = 1 inch)
    height: number; // Height in points
    margin: number; // Margin in points
  };
  colors: {
    background: string;
    components: string;
    nets: string;
    labels: string;
  };
  fonts: {
    labels: string;
    values: string;
    ports: string;
  };
  components: {
    scale: number; // Global scale factor
    spacing: number; // Space between components
  };
}

interface TitleBlockField {
  label: string;
  value: string;
  fontSize: number;
  x: number;
  labelWidth: number;
  bold?: boolean;
  italic?: boolean;
}

export const DEFAULT_PDF_OPTIONS: PDFRenderOptions = {
  pageSize: {
    width: 841.89, // A4 height in points (swapped for landscape)
    height: 595.28, // A4 width in points (swapped for landscape)
    margin: 20,
  },
  colors: {
    background: "#FFFFFF",
    components: "#8B0000", // Dark red for components
    nets: "#2E8B57", // Sea green for nets/wires
    labels: "#000000",
  },
  fonts: {
    labels: "courier",
    values: "courier",
    ports: "courier",
  },
  components: {
    scale: 1.0,
    spacing: 50,
  },
};

export class PDFSchematicRenderer {
  private layoutRenderer: SchematicRenderer;
  private options: PDFRenderOptions;
  private transform: {
    scale: number;
    offsetX: number;
    offsetY: number;
  };
  private modulePageMap: Map<string, number>;

  constructor(
    netlist: Netlist,
    config: Partial<SchematicConfig> = {},
    options: Partial<PDFRenderOptions> = {}
  ) {
    this.layoutRenderer = new SchematicRenderer(netlist, config);
    this.options = { ...DEFAULT_PDF_OPTIONS, ...options };
    this.transform = {
      scale: 1,
      offsetX: this.options.pageSize.margin,
      offsetY: this.options.pageSize.margin,
    };
    this.modulePageMap = new Map();
  }

  private toPageCoords(x: number, y: number): [number, number] {
    return [
      x * this.transform.scale + this.transform.offsetX,
      y * this.transform.scale + this.transform.offsetY,
    ];
  }

  private drawResistor(doc: jsPDF, node: ElkNode) {
    const [x, y] = this.toPageCoords(node.x || 0, node.y || 0);
    const width = (node.width || 0) * this.transform.scale;
    const height = (node.height || 0) * this.transform.scale;

    // Draw resistor symbol - narrow rectangle
    const resistorWidth = 12 * this.transform.scale;
    const resistorHeight = 28 * this.transform.scale;
    const centerX = x + width / 2;
    const centerY = y + height / 2;

    // Draw vertical lines to connect to ports
    doc.setDrawColor(this.options.colors.components);
    doc.setLineWidth(1.5 * this.transform.scale);

    // Top line
    doc.line(centerX, y, centerX, centerY - resistorHeight / 2);
    // Bottom line
    doc.line(centerX, centerY + resistorHeight / 2, centerX, y + height);

    // Draw resistor body (rectangle)
    doc.rect(
      centerX - resistorWidth / 2,
      centerY - resistorHeight / 2,
      resistorWidth,
      resistorHeight
    );

    // Add labels if present
    if (node.labels?.length) {
      doc.setFont(this.options.fonts.values);
      const fontSize = 10 * this.transform.scale;
      doc.setFontSize(fontSize);

      node.labels.forEach((label) => {
        let [labelX, labelY] = this.toPageCoords(
          (node.x || 0) + (label.x || 0),
          (node.y || 0) + (label.y || 0)
        );

        if (label.textAlign === "right") {
          labelX += (label.width || 0) * this.transform.scale;
          labelX -= doc.getTextWidth(label.text);
        }

        doc.text(label.text, labelX, labelY, {
          baseline: "middle",
        });
      });
    }
  }

  private drawCapacitor(doc: jsPDF, node: ElkNode) {
    const [x, y] = this.toPageCoords(node.x || 0, node.y || 0);
    const width = (node.width || 0) * this.transform.scale;
    const height = (node.height || 0) * this.transform.scale;
    const centerX = x + width / 2;
    const centerY = y + height / 2;

    // Make capacitor plates narrower
    const symbolWidth = 12 * this.transform.scale; // Reduced from 20
    const plateGap = 4 * this.transform.scale;

    doc.setDrawColor(this.options.colors.components);
    doc.setLineWidth(1.5 * this.transform.scale);

    // Draw vertical lines to connect to ports
    doc.line(centerX, y, centerX, centerY - plateGap / 2);
    doc.line(centerX, centerY + plateGap / 2, centerX, y + height);

    // Draw capacitor plates
    doc.line(
      centerX - symbolWidth / 2,
      centerY - plateGap / 2,
      centerX + symbolWidth / 2,
      centerY - plateGap / 2
    );
    doc.line(
      centerX - symbolWidth / 2,
      centerY + plateGap / 2,
      centerX + symbolWidth / 2,
      centerY + plateGap / 2
    );

    // Add labels if present
    if (node.labels?.length) {
      doc.setFont(this.options.fonts.values);
      const fontSize = 10 * this.transform.scale;
      doc.setFontSize(fontSize);

      node.labels.forEach((label) => {
        let [labelX, labelY] = this.toPageCoords(
          (node.x || 0) + (label.x || 0),
          (node.y || 0) + (label.y || 0)
        );

        if (label.textAlign === "right") {
          labelX += (label.width || 0) * this.transform.scale;
          labelX -= doc.getTextWidth(label.text);
        }

        doc.text(label.text, labelX, labelY, {
          baseline: "middle",
        });
      });
    }
  }

  private drawModule(doc: jsPDF, node: ElkNode) {
    const [x, y] = this.toPageCoords(node.x || 0, node.y || 0);
    const width = (node.width || 0) * this.transform.scale;
    const height = (node.height || 0) * this.transform.scale;

    // Draw the node background and border
    if (node.type === NodeType.MODULE) {
      doc.setFillColor(255, 255, 255); // White background for modules

      // If this is a module and we have its page number, make it clickable
      const instanceRef = node.id;
      const targetPage = this.modulePageMap.get(instanceRef);
      if (targetPage !== undefined) {
        // Create link with explicit options for better compatibility
        doc.link(x, y, width, height, {
          pageNumber: targetPage + 1,
        });
      }
    } else {
      doc.setFillColor(255, 250, 230); // Slightly more noticeable warm/yellowish tint for components
    }
    doc.rect(x, y, width, height, "FD");

    // Draw module/component labels
    if (node.labels?.length) {
      doc.setFont(this.options.fonts.labels);
      const fontSize = 12 * this.transform.scale;
      doc.setFontSize(fontSize);

      for (let i = 0; i < node.labels.length; i++) {
        const label = node.labels[i];
        let [labelX, labelY] = this.toPageCoords(
          (node.x || 0) + (label.x || 0),
          (node.y || 0) + (label.y || 0) + 8 // aesthetic correction
        );

        if (label.textAlign === "right") {
          labelX += (label.width || 0) * this.transform.scale;
          labelX -= doc.getTextWidth(label.text);
        }

        doc.text(label.text, labelX, labelY, {
          baseline: "middle",
        });
      }
    }

    // Draw ports
    for (const port of node.ports || []) {
      const [portX, portY] = this.toPageCoords(
        (port.x || 0) + (node.x || 0),
        (port.y || 0) + (node.y || 0)
      );

      doc.setFillColor(this.options.colors.components);
      doc.circle(portX, portY, 2 * this.transform.scale, "F");

      // Draw port label
      if (port.labels?.[0]) {
        doc.setFont(this.options.fonts.ports);
        const fontSize = 8 * this.transform.scale;
        doc.setFontSize(fontSize);

        // Determine if port is on the right side
        const portSide = port.properties?.["port.side"] || "WEST";
        const isRightSide = portSide === "EAST";
        const offset = 4 * this.transform.scale;

        if (isRightSide) {
          // For right-side ports, place label inside and right-aligned
          doc.text(port.labels[0].text, portX - offset, portY, {
            baseline: "middle",
            align: "right",
          });
        } else {
          // For left-side ports, place label outside and left-aligned
          doc.text(port.labels[0].text, portX + offset, portY, {
            baseline: "middle",
            align: "left",
          });
        }
      }
    }
  }

  private drawConnections(doc: jsPDF, edges: ElkEdge[]) {
    doc.setDrawColor(this.options.colors.nets);

    for (const edge of edges) {
      if (!edge.sections?.[0]) continue;

      const section = edge.sections[0];
      const points = [
        section.startPoint,
        ...(section.bendPoints || []),
        section.endPoint,
      ];

      // Draw the connection line
      for (let i = 0; i < points.length - 1; i++) {
        const [x1, y1] = this.toPageCoords(points[i].x, points[i].y);
        const [x2, y2] = this.toPageCoords(points[i + 1].x, points[i + 1].y);
        doc.line(x1, y1, x2, y2);
      }

      // Draw junction points
      if (edge.junctionPoints) {
        doc.setFillColor(this.options.colors.nets);
        for (const point of edge.junctionPoints) {
          const [x, y] = this.toPageCoords(point.x, point.y);
          doc.circle(x, y, 2 * this.transform.scale, "F");
        }
      }
    }
  }

  private drawNetReference(doc: jsPDF, node: ElkNode) {
    const [x, y] = this.toPageCoords(node.x || 0, node.y || 0);
    const width = (node.width || 0) * this.transform.scale;
    const height = (node.height || 0) * this.transform.scale;
    const centerX = x + width / 2;
    const centerY = y + height / 2;

    doc.setDrawColor(this.options.colors.components);
    doc.setLineWidth(1.5 * this.transform.scale);

    // Determine port side
    const portSide = node.ports?.[0]?.properties?.["port.side"] || "WEST";
    const isEastSide = portSide === "EAST";

    if (node.netReferenceType === NetReferenceType.GROUND) {
      // Ground symbol dimensions
      const symbolWidth = 20 * this.transform.scale;
      const lineSpacing = 4 * this.transform.scale;

      // Calculate the position of the first horizontal line
      const groundY = centerY - 3 * lineSpacing;

      // Draw vertical line from port to first horizontal line
      doc.line(centerX, y, centerX, groundY);

      const groundLineWidths = [
        symbolWidth,
        symbolWidth * 0.75,
        symbolWidth * 0.5,
      ];

      // Draw horizontal ground lines
      for (let i = 0; i < 3; i++) {
        const lineWidth = groundLineWidths[i];
        doc.line(
          centerX - lineWidth / 2,
          groundY + i * lineSpacing,
          centerX + lineWidth / 2,
          groundY + i * lineSpacing
        );
      }
    } else if (node.netReferenceType === NetReferenceType.VDD) {
      // VDD symbol dimensions
      const symbolWidth = 20 * this.transform.scale;
      const verticalLineLength = 15 * this.transform.scale;

      // Draw vertical line
      doc.line(centerX, y + height, centerX, centerY - verticalLineLength / 2);

      // Draw horizontal line at top
      doc.line(
        centerX - symbolWidth / 2,
        centerY - verticalLineLength / 2,
        centerX + symbolWidth / 2,
        centerY - verticalLineLength / 2
      );

      // Add VDD label above the horizontal line
      if (node.labels?.[0]) {
        doc.setFont(this.options.fonts.labels);
        const fontSize = 10 * this.transform.scale;
        doc.setFontSize(fontSize);
        doc.text(
          node.labels[0].text,
          centerX,
          centerY - verticalLineLength / 2 - 5 * this.transform.scale,
          {
            align: "center",
            baseline: "bottom",
          }
        );
      }
    } else {
      // Regular net reference - small circle with dot
      const circleRadius = 3 * this.transform.scale;

      // Position circle at the port side
      const circleX = isEastSide
        ? x + width - circleRadius * 2 // Circle at right edge when port is on east
        : x + circleRadius * 2; // Circle at left edge when port is on west

      // Draw white background circle
      doc.setFillColor(255, 255, 255);
      doc.circle(circleX, centerY, circleRadius + this.transform.scale, "F");

      // Draw circle outline
      doc.setDrawColor(this.options.colors.components);
      doc.circle(circleX, centerY, circleRadius, "S");

      // Add small dot in center
      doc.setFillColor(this.options.colors.components);
      doc.circle(circleX, centerY, this.transform.scale, "F");

      // Add net name label, positioned based on port side
      if (node.labels?.[0]) {
        doc.setFont(this.options.fonts.labels);
        const fontSize = 10 * this.transform.scale;
        doc.setFontSize(fontSize);

        // Position label on opposite side of the port
        const labelX = isEastSide
          ? circleX - circleRadius * 4 // Label on left when port is on east
          : circleX + circleRadius * 4; // Label on right when port is on west

        doc.text(node.labels[0].text, labelX, centerY, {
          align: isEastSide ? "right" : "left",
          baseline: "middle",
        });
      }
    }
  }

  private drawInductor(doc: jsPDF, node: ElkNode) {
    const [x, y] = this.toPageCoords(node.x || 0, node.y || 0);
    const width = (node.width || 0) * this.transform.scale;
    const height = (node.height || 0) * this.transform.scale;
    const centerX = x + width / 2;
    const centerY = y + height / 2;

    doc.setDrawColor(this.options.colors.components);
    doc.setLineWidth(1.5 * this.transform.scale);

    // Inductor dimensions
    const inductorHeight = 40 * this.transform.scale;
    const numArcs = 4;
    const arcRadius = inductorHeight / (2 * numArcs);
    const coilWidth = 12 * this.transform.scale;

    // Draw vertical lines to ports
    doc.line(centerX, y, centerX, centerY - inductorHeight / 2);
    doc.line(centerX, centerY + inductorHeight / 2, centerX, y + height);

    // Draw inductor coils using proper half-circles
    const startY = centerY - inductorHeight / 2;
    const segments = 32; // Increased segments for smoother curves

    for (let i = 0; i < numArcs; i++) {
      const arcY = startY + i * 2 * arcRadius;

      // Generate points for a half-circle
      for (let j = 0; j <= segments; j++) {
        const angle1 = (j / segments) * Math.PI;
        const angle2 = ((j + 1) / segments) * Math.PI;

        if (j < segments) {
          // Don't draw past the last point
          const x1 = centerX + coilWidth * Math.sin(angle1);
          const y1 = arcY + arcRadius * (1 - Math.cos(angle1));

          const x2 = centerX + coilWidth * Math.sin(angle2);
          const y2 = arcY + arcRadius * (1 - Math.cos(angle2));

          doc.line(x1, y1, x2, y2);
        }
      }
    }

    // Add labels if present
    if (node.labels?.length) {
      doc.setFont(this.options.fonts.values);
      const fontSize = 10 * this.transform.scale;
      doc.setFontSize(fontSize);

      node.labels.forEach((label) => {
        const [labelX, labelY] = this.toPageCoords(
          (node.x || 0) + (label.x || 0),
          (node.y || 0) + (label.y || 0)
        );
        doc.text(label.text, labelX, labelY, {
          baseline: "middle",
          align: label.textAlign || "left",
        });
      });
    }
  }

  private drawNode(doc: jsPDF, node: ElkNode) {
    switch (node.type) {
      case NodeType.RESISTOR:
        this.drawResistor(doc, node);
        break;
      case NodeType.CAPACITOR:
        this.drawCapacitor(doc, node);
        break;
      case NodeType.INDUCTOR:
        this.drawInductor(doc, node);
        break;
      case NodeType.MODULE:
      case NodeType.COMPONENT:
        this.drawModule(doc, node);
        break;
      case NodeType.NET_REFERENCE:
        this.drawNetReference(doc, node);
        break;
    }
  }

  private calculateScale(layout: ElkGraph) {
    const availableWidth =
      this.options.pageSize.width - 2 * this.options.pageSize.margin;
    const availableHeight =
      this.options.pageSize.height - 2 * this.options.pageSize.margin;

    // Find the bounding box of all nodes
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;

    for (const node of layout.children) {
      const x = node.x || 0;
      const y = node.y || 0;
      const width = node.width || 0;
      const height = node.height || 0;

      // Base dimensions
      minX = Math.min(minX, x);
      minY = Math.min(minY, y);
      maxX = Math.max(maxX, x + width);
      maxY = Math.max(maxY, y + height);

      // Account for net reference labels
      if (node.type === NodeType.NET_REFERENCE && node.labels?.[0]) {
        // Estimate text width based on label length (rough approximation)
        const labelWidth = node.labels[0].text.length * 6; // Assume ~6 units per character
        const circleRadius = 3; // From drawNetReference

        // Add space for label to the right of the net reference circle
        maxX = Math.max(maxX, x + width + circleRadius + 5 + labelWidth);
      }
    }

    const layoutWidth = maxX - minX;
    const layoutHeight = maxY - minY;

    if (layoutWidth === 0 || layoutHeight === 0) {
      return 1;
    }

    const scaleX = availableWidth / layoutWidth;
    const scaleY = availableHeight / layoutHeight;

    return Math.min(scaleX, scaleY, 1) * this.options.components.scale;
  }

  private drawBorder(
    doc: jsPDF,
    instance_ref: string,
    pageNumber: number,
    totalPages: number
  ) {
    // Save current state
    const currentLineWidth = doc.getLineWidth();
    const currentFontSize = doc.getFontSize();
    const currentDrawColor = doc.getDrawColor();
    const currentTextColor = doc.getTextColor();

    // Set up border dimensions (in points, 72 points = 1 inch)
    const margin = 20; // 20pt margin
    const pageWidth = this.options.pageSize.width;
    const pageHeight = this.options.pageSize.height;
    const titleBlockHeight = 50; // Reduced height for title block
    const titleBlockWidth = 200; // Reduced width for title block
    const innerMargin = 5; // Space between outer and inner border

    // Use dark red color for border elements
    const borderColor = "#8B0000"; // Dark red, matching KiCAD style
    doc.setDrawColor(borderColor);
    doc.setTextColor(borderColor);
    doc.setFont("courier"); // Use fixed-width font consistently

    // Draw main border
    doc.setLineWidth(0.5);
    doc.rect(margin, margin, pageWidth - 2 * margin, pageHeight - 2 * margin);

    // Draw inner border (thinner)
    doc.setLineWidth(0.25);
    doc.rect(
      margin + innerMargin,
      margin + innerMargin,
      pageWidth - 2 * (margin + innerMargin),
      pageHeight - 2 * (margin + innerMargin)
    );

    // Section markers
    const numHorizontalSections = 6; // A-F
    const numVerticalSections = 6; // 1-6
    const sectionWidth = (pageWidth - 2 * margin) / numVerticalSections;
    const sectionHeight = (pageHeight - 2 * margin) / numHorizontalSections;
    const tickLength = innerMargin; // Use innerMargin as tick length for consistency

    // Draw vertical section markers and numbers (1-6)
    for (let i = 0; i <= numVerticalSections; i++) {
      const x = margin + i * sectionWidth;

      // Top ticks
      doc.line(x, margin, x, margin + tickLength);
      // Bottom ticks
      doc.line(x, pageHeight - margin - tickLength, x, pageHeight - margin);

      // Add numbers (skip last number on bottom due to title block)
      if (i < numVerticalSections) {
        // Top numbers - in the gap between borders
        doc.setFontSize(6);
        doc.text(
          String(i + 1),
          x + sectionWidth / 2,
          margin + innerMargin / 2,
          { align: "center", baseline: "middle" }
        );
        // Bottom numbers - always show them
        doc.text(
          String(i + 1),
          x + sectionWidth / 2,
          pageHeight - margin - innerMargin / 2,
          { align: "center", baseline: "middle" }
        );
      }
    }

    // Draw horizontal section markers and letters (A-F)
    for (let i = 0; i <= numHorizontalSections; i++) {
      const y = margin + i * sectionHeight;

      // Left ticks
      doc.line(margin, y, margin + tickLength, y);
      // Right ticks
      doc.line(pageWidth - margin - tickLength, y, pageWidth - margin, y);

      // Add letters (skip last letter on right due to title block)
      if (i < numHorizontalSections) {
        // Left letters - in the gap between borders
        doc.setFontSize(6);
        doc.text(
          String.fromCharCode(65 + i),
          margin + innerMargin / 2,
          y + sectionHeight / 2,
          { align: "center", baseline: "middle" }
        );
        // Right letters - always show them
        doc.text(
          String.fromCharCode(65 + i),
          pageWidth - margin - innerMargin / 2,
          y + sectionHeight / 2,
          { align: "center", baseline: "middle" }
        );
      }
    }

    // Draw title block in bottom right
    const titleBlockY = pageHeight - margin - titleBlockHeight - innerMargin;
    doc.setLineWidth(0.25); // Match inner border thickness
    doc.rect(
      pageWidth - margin - titleBlockWidth - innerMargin,
      titleBlockY,
      titleBlockWidth,
      titleBlockHeight
    );

    // Title block sections
    const sections: Array<{ height: number; fields: TitleBlockField[] }> = [
      {
        height: 15,
        fields: [
          {
            label: "",
            value: instance_ref.split(":").pop() || instance_ref,
            fontSize: 6,
            x: 5,
            labelWidth: 0,
          },
        ],
      },
      {
        height: 20,
        fields: [
          {
            label: "Title:",
            value: instance_ref.split(":").pop()?.split(".")[0] || "",
            fontSize: 8,
            x: 5,
            labelWidth: 35,
            bold: true,
            italic: true,
          },
        ],
      },
      {
        height: 15,
        fields: [
          {
            label: "Generated on:",
            value: new Date().toLocaleString(),
            fontSize: 6,
            x: 5,
            labelWidth: 55,
          },
        ],
      },
    ];

    let currentY = titleBlockY;
    sections.forEach((section, idx) => {
      // Draw fields for this section
      const sectionCenterY = currentY + section.height / 2;
      section.fields.forEach((field) => {
        const x = pageWidth - margin - titleBlockWidth - innerMargin + field.x;

        // Set font style if specified
        if (field.bold || field.italic) {
          const fontStyle = [];
          if (field.bold) fontStyle.push("bold");
          if (field.italic) fontStyle.push("italic");
          doc.setFont("courier", fontStyle.join(""));
        }

        doc.setFontSize(field.fontSize);

        // Draw label if it exists
        if (field.label) {
          doc.text(field.label, x, sectionCenterY, {
            align: "left",
            baseline: "middle",
          });
        }

        // Draw value
        doc.text(field.value, x + field.labelWidth, sectionCenterY, {
          align: "left",
          baseline: "middle",
        });

        // Reset font style
        doc.setFont("courier", "normal");
      });

      // Draw horizontal line after each section (except last)
      if (idx < sections.length - 1) {
        currentY += section.height;
        doc.line(
          pageWidth - margin - titleBlockWidth - innerMargin,
          currentY,
          pageWidth - margin - innerMargin,
          currentY
        );
      }

      // Move to next section's starting Y position
      if (idx < sections.length - 1) {
        currentY =
          titleBlockY +
          sections.slice(0, idx + 1).reduce((sum, s) => sum + s.height, 0);
      }
    });

    // Restore original state
    doc.setLineWidth(currentLineWidth);
    doc.setFontSize(currentFontSize);
    doc.setDrawColor(currentDrawColor);
    doc.setTextColor(currentTextColor);
  }

  private async renderModule(
    doc: jsPDF,
    instance_ref: string,
    isFirstPage: boolean = false,
    pageNumber: number,
    totalPages: number
  ) {
    // Store the page number for this module
    this.modulePageMap.set(instance_ref, pageNumber);

    // Add a new page if this isn't the first module
    if (!isFirstPage) {
      doc.addPage();
    }

    // Draw the border first (behind the schematic)
    this.drawBorder(doc, instance_ref, pageNumber, totalPages);

    // Get the layout for this module
    const graph = await this.layoutRenderer.render(instance_ref);

    // Set up dimensions accounting for title block
    const margin = this.options.pageSize.margin;
    const titleBlockHeight = 80; // Match the height from drawBorder

    // Calculate available space for schematic
    const availableWidth = this.options.pageSize.width - 2 * margin;
    const availableHeight =
      this.options.pageSize.height - 2 * margin - titleBlockHeight;

    // Find the bounds of the graph
    let minX = Infinity,
      minY = Infinity,
      maxX = -Infinity,
      maxY = -Infinity;
    for (const node of graph.children) {
      const x = node.x || 0;
      const y = node.y || 0;
      const width = node.width || 0;
      const height = node.height || 0;
      minX = Math.min(minX, x);
      minY = Math.min(minY, y);
      maxX = Math.max(maxX, x + width);
      maxY = Math.max(maxY, y + height);
    }

    // Calculate scale to fit within available space
    const graphWidth = maxX - minX;
    const graphHeight = maxY - minY;
    const scaleX = availableWidth / graphWidth;
    const scaleY = availableHeight / graphHeight;
    this.transform.scale = Math.min(scaleX, scaleY, 1) * 0.8; // Use 0.8 to leave some padding

    // Center the graph horizontally and vertically within the available space
    const scaledWidth = graphWidth * this.transform.scale;
    const scaledHeight = graphHeight * this.transform.scale;

    this.transform.offsetX =
      margin + (availableWidth - scaledWidth) / 2 - minX * this.transform.scale;

    // Position vertically in the center of the available space (excluding title block)
    this.transform.offsetY =
      margin +
      (availableHeight - scaledHeight) / 2 -
      minY * this.transform.scale;

    // Set line width for all drawings
    doc.setLineWidth(1.5 * this.transform.scale);

    // Draw all nodes
    for (const node of graph.children) {
      this.drawNode(doc, node);
    }

    // Draw all connections
    this.drawConnections(doc, graph.edges);
  }

  private getSubmodules(instance_ref: string): string[] {
    const instance = this.layoutRenderer.netlist.instances[instance_ref];
    if (!instance) return [];

    const submodules: string[] = [];

    // Add the current module if it's a module
    if (instance.kind === "Module") {
      submodules.push(instance_ref);

      // Recursively check all children
      for (const [_, child_ref] of Object.entries(instance.children)) {
        const child = this.layoutRenderer.netlist.instances[child_ref];
        if (child?.kind === "Module") {
          // Recursively get submodules of this child
          submodules.push(...this.getSubmodules(child_ref));
        }
      }
    }

    return submodules;
  }

  async render(rootModule: string): Promise<jsPDF> {
    // Create a new PDF document
    const doc = new jsPDF({
      orientation: "landscape",
      unit: "pt",
      format: [this.options.pageSize.height, this.options.pageSize.width],
    });

    // Get all modules in the subtree of the root module
    const modules = this.getSubmodules(rootModule);

    // Clear and build the module page map before rendering
    this.modulePageMap.clear();
    // Map each module to its page number (0-based index)
    modules.forEach((moduleId, index) => {
      this.modulePageMap.set(moduleId, index);
    });

    // Now render each module on its own page
    for (let i = 0; i < modules.length; i++) {
      await this.renderModule(doc, modules[i], i === 0, i, modules.length);
    }

    return doc;
  }
}
