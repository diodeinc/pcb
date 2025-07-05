import React, {
  useCallback,
  useEffect,
  useRef,
  useState,
  useMemo,
} from "react";
import type { CSSProperties } from "react";
import ELK from "elkjs/lib/elk.bundled.js";
import type { ELK as ELKType } from "elkjs/lib/elk-api";
import {
  ReactFlow,
  Controls,
  Position,
  useNodesState,
  useEdgesState,
  Handle,
  type Node,
  useOnSelectionChange,
  ReactFlowProvider,
  Panel,
} from "@xyflow/react";
import type { Edge, EdgeProps, EdgeTypes } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import {
  NodeType,
  SchematicRenderer,
  DEFAULT_CONFIG,
  NetReferenceType,
} from "../renderer";
import type { ElkEdge, ElkGraph, ElkNode, SchematicConfig } from "../renderer";
import { PDFSchematicRenderer } from "../PDFSchematicRenderer";
import type { Netlist } from "../types/NetlistTypes";
import { debounce } from "lodash";
import { Download, Loader, Settings } from "react-feather";
import { renderKicadSymbol, getKicadSymbolInfo } from "../renderer/kicad_sym";
import { Color } from "kicanvas/base/color";

type SelectionState = {
  selectedNetId: string | null;
  hoveredNetId: string | null;
};

type SchematicNodeData = ElkNode & {
  componentType: NodeType;
  selectionState: SelectionState;
} & Record<string, unknown>;

type SchematicEdgeData = ElkEdge & {
  selectionState: SelectionState;
} & Record<string, unknown>;

type SchematicNode = Node<SchematicNodeData, NodeType>;
type SchematicEdge = Edge<SchematicEdgeData>;

function createSchematicNode(
  elkNode: ElkNode,
  selectionState: SelectionState,
  netlist?: Netlist
): SchematicNode {
  return {
    id: elkNode.id,
    data: {
      componentType: elkNode.type,
      selectionState,
      ...elkNode,
      ...(elkNode.type === NodeType.SYMBOL && netlist ? { netlist } : {}),
    },
    position: { x: elkNode.x || 0, y: elkNode.y || 0 },
    type: elkNode.type,
    draggable: false,
    // Only make modules selectable
    selectable: elkNode.type === NodeType.MODULE,
    connectable: false,
    // Add custom styles based on node type
    style: {
      // Prevent hover effects on component nodes
      ...(elkNode.type === NodeType.COMPONENT
        ? {
            cursor: "default",
            // Add some !important styles but NOT transform
            backgroundColor: "#f5f5f5 !important",
            border: "1px solid #ddd !important",
            boxShadow: "none !important",
          }
        : {}),
    },
    // Add class for additional styling with CSS
    className:
      elkNode.type === NodeType.MODULE
        ? "module-node"
        : "component-node non-interactive",
  };
}

function createSchematicEdge(
  elkEdge: ElkEdge,
  selectionState: SelectionState
): SchematicEdge {
  return {
    id: elkEdge.id,
    data: { ...elkEdge, selectionState },
    source: elkEdge.sourceComponentRef,
    target: elkEdge.targetComponentRef,
    sourceHandle: `${elkEdge.sources[0]}-source`,
    targetHandle: `${elkEdge.targets[0]}-target`,
    type: "electrical",
  };
}

// Common color for electrical components
const electricalComponentColor = "var(--vscode-editor-foreground, #666)";
const edgeColor = "var(--vscode-editorLineNumber-dimmedForeground, #666)";
const accentColor = "var(--vscode-activityBarBadge-background, #666)";

// Common CSS to override ReactFlow default hover effects
const customStyles = `
  /* Use VSCode theme colors for nodes and edges with fallbacks */
  .react-flow__node {
    color: var(--vscode-foreground, #000);
    border-color: var(--vscode-editor-selectionBackground, #666);
  }

  /* Add transition for smooth layout changes */
  .react-flow__node.animate-layout {
    transition: transform 300ms ease-in-out;
  }

  .react-flow__edge.animate-layout .react-flow__edge-path {
    transition: d 300ms ease-in-out;
  }

  .react-flow__edge {
    stroke: ${edgeColor};
  }

  .react-flow__edge-path {
    stroke: ${edgeColor} !important;
  }

  /* Style the graph background */
  .react-flow {
    background-color: var(--vscode-editor-background, #fff);
  }

  /* Disable hover effects for component nodes */
  .react-flow__node-componentNode {
    pointer-events: none !important;
  }
  
  .react-flow__node-componentNode .component-port {
    pointer-events: auto !important;
  }
  
  /* Prevent hover color change for component nodes */
  .react-flow__node-componentNode:hover {
    background-color: var(--vscode-editor-background, #f5f5f5) !important;
    border-color: ${edgeColor} !important;
    box-shadow: none !important;
    cursor: default !important;
  }
  
  /* Keep module nodes interactive */
  .react-flow__node-moduleNode {
    cursor: pointer;
  }

  /* Module node hover state */
  .react-flow__node-moduleNode:hover {
    border-color: var(--vscode-focusBorder, #0066cc) !important;
    box-shadow: 0 0 0 2px var(--vscode-focusBorder, #0066cc) !important;
  }
  
  /* Make sure the port connection points remain interactive */
  .react-flow__handle {
    pointer-events: all !important;
  }

  /* Style the minimap */
  .react-flow__minimap {
    background-color: var(--vscode-editor-background, #fff);
  }

  /* Style the controls */
  .react-flow__controls {
    background-color: var(--vscode-editor-background, #fff);
    border-color: var(--vscode-editor-selectionBackground, #666);
  }

  .react-flow__controls button {
    background-color: var(--vscode-button-background, #0066cc);
    color: var(--vscode-button-foreground, #fff);
    border-color: var(--vscode-button-border, transparent);
  }

  .react-flow__controls button:hover {
    background-color: var(--vscode-button-hoverBackground, #0052a3);
  }

  /* Style port labels */
  .port-label {
    color: ${accentColor};
    font-weight: 1000;
    font-size: 12px;
  }

  /* Style component/module names */
  .module-header, .component-header {
    color: ${electricalComponentColor} !important;
    font-weight: 600;
  }

  /* Style the download button */
  .download-button {
    display: flex;
    align-items: center;
    gap: 8px;
    background-color: var(--vscode-button-background, #0066cc);
    color: var(--vscode-button-foreground, #fff);
    border: 1px solid var(--vscode-button-border, transparent);
    padding: 8px 12px;
    border-radius: 4px;
    cursor: pointer;
    font-size: 12px;
    transition: background-color 0.2s;
  }

  .download-button:disabled {
    opacity: 0.7;
    cursor: not-allowed;
  }

  .download-button:not(:disabled):hover {
    background-color: var(--vscode-button-hoverBackground, #0052a3);
  }

  .download-button:active {
    background-color: var(--vscode-button-activeBackground, #004080);
  }

  .download-button svg {
    width: 16px;
    height: 16px;
  }

  .download-button .loading-icon {
    animation: spin 1s linear infinite;
  }

  @keyframes spin {
    from {
      transform: rotate(0deg);
    }
    to {
      transform: rotate(360deg);
    }
  }
`;

// Common style for all handles - subtle dots on component borders
const portHandleStyle = {
  background: edgeColor,
  border: `1px solid ${edgeColor}`,
  borderRadius: "50%",
  width: "4px",
  height: "4px",
  opacity: 0.5,
  zIndex: 20,
};

// Define custom node component for modules and components
const ModuleNode = ({ data }: { data: SchematicNodeData }) => {
  // Find the original component to determine its type
  const isModule = data.componentType === NodeType.MODULE;

  // Determine if this node should be dimmed based on selection state
  const selectionState = data.selectionState;
  const shouldDim =
    selectionState?.selectedNetId || selectionState?.hoveredNetId;
  const isConnectedToHighlightedNet =
    shouldDim &&
    data.ports?.some((port) => {
      const netId = port.netId;
      return (
        netId === selectionState.selectedNetId ||
        netId === selectionState.hoveredNetId
      );
    });
  const moduleOpacity = shouldDim && !isConnectedToHighlightedNet ? 0.2 : 1;

  // Function to determine port label opacity
  const getPortLabelOpacity = (port: any) => {
    if (!shouldDim) return 1;
    const isPortHighlighted =
      port.netId === selectionState.selectedNetId ||
      port.netId === selectionState.hoveredNetId;
    return isPortHighlighted ? 1 : 0.2;
  };

  // Different styles for modules vs components
  const nodeStyle: CSSProperties = {
    width: data.width,
    height: data.height,
    backgroundColor: isModule
      ? "var(--vscode-editor-background, #fff)"
      : `color-mix(in srgb, var(--vscode-editorLineNumber-foreground, #666) 5%, var(--vscode-editor-background, #fff))`,
    border: `1px solid ${electricalComponentColor}`,
    opacity: moduleOpacity,
    cursor: isModule ? "pointer" : "default",
    pointerEvents: isModule ? "auto" : "none",
    borderRadius: "0px",
  };

  return (
    <div
      className={`react-flow-${isModule ? "module" : "component"}-node`}
      style={nodeStyle}
    >
      {/* Component/Module label - top left corner */}
      {data.labels?.map((label, index) => (
        <div
          key={`label-${index}`}
          className={`${isModule ? "module" : "component"}-header`}
          style={{
            position: "absolute",
            top: label.y,
            left: label.x,
            padding: "4px",
            fontSize: "12px",
            fontWeight: "bold",
            color: "var(--vscode-foreground, #000)",
            textAlign: label.textAlign || "left",
            width: label.width || "auto",
          }}
        >
          {label.text}
        </div>
      ))}

      {/* Port connections */}
      <div className={`${isModule ? "module" : "component"}-content`}>
        {data.ports && data.ports.length > 0 && (
          <div className={`${isModule ? "module" : "component"}-ports`}>
            {data.ports.map((port) => {
              // Calculate port position relative to node
              let position = "left";
              if (port.properties && port.properties["port.side"]) {
                // Use ELK-provided port side if available
                const side = port.properties["port.side"];
                position =
                  side === "WEST"
                    ? "left"
                    : side === "EAST"
                    ? "right"
                    : side === "NORTH"
                    ? "top"
                    : "bottom";
              } else {
                // Otherwise determine based on position within node
                const tolerance = 20; // Pixels from edge to consider as boundary
                if (port.x && port.x <= tolerance) position = "left";
                else if (port.x && port.x >= (data.width || 0) - tolerance)
                  position = "right";
                else if (port.y && port.y <= tolerance) position = "top";
                else if (port.y && port.y >= (data.height || 0) - tolerance)
                  position = "bottom";
              }

              // Set label position relative to port based on which side it's on
              const labelStyle = {
                position: "absolute" as const,
                fontSize: "10px",
                whiteSpace: "nowrap" as const,
                pointerEvents: "none" as const,
                transform: "",
                textAlign: "left" as React.CSSProperties["textAlign"],
                width: position === "right" ? "auto" : "70px", // Auto width for right labels
                maxWidth: position === "right" ? "150px" : "70px", // Add maxWidth to prevent extreme stretching
                right: position === "right" ? "0px" : "auto", // Position from right edge for right-side labels
                left: position === "right" ? "auto" : undefined, // Don't set left for right-side labels
                opacity: getPortLabelOpacity(port), // Add opacity based on net connection
              };

              // Position label based on port side
              switch (position) {
                case "left":
                  labelStyle.transform = "translate(10px, -5px)";
                  labelStyle.textAlign = "left";
                  break;
                case "right":
                  labelStyle.transform = "translate(-10px, -5px)"; // More symmetrical offset
                  labelStyle.textAlign = "right";
                  break;
                case "top":
                  labelStyle.transform = "translate(-30px, 10px)";
                  break;
                case "bottom":
                  labelStyle.transform = "translate(-30px, -15px)";
                  break;
              }

              return (
                <div
                  key={port.id}
                  className={`${isModule ? "module" : "component"}-port`}
                  style={{
                    position: "absolute",
                    left: port.x,
                    top: port.y,
                    width: 0,
                    height: 0,
                    borderRadius: "50%",
                    backgroundColor: "#000",
                    opacity: 0.7,
                    zIndex: 10,
                    pointerEvents: "auto", // Enable pointer events for ports only
                  }}
                  data-port-id={port.id}
                >
                  {/* Hidden connection handles that React Flow needs for connections */}
                  <Handle
                    type="source"
                    position={
                      position === "left"
                        ? Position.Left
                        : position === "right"
                        ? Position.Right
                        : position === "top"
                        ? Position.Top
                        : Position.Bottom
                    }
                    id={`${port.id}-source`}
                    style={{ ...portHandleStyle, opacity: 0 }}
                  />
                  <Handle
                    type="target"
                    position={
                      position === "left"
                        ? Position.Left
                        : position === "right"
                        ? Position.Right
                        : position === "top"
                        ? Position.Top
                        : Position.Bottom
                    }
                    id={`${port.id}-target`}
                    style={{ ...portHandleStyle, opacity: 0 }}
                  />

                  {/* Port label */}
                  <div className="port-label" style={labelStyle}>
                    {port.labels?.[0]?.text}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
};

// Define a node specifically for capacitors with authentic schematic symbol
const CapacitorNode = ({ data }: { data: any }) => {
  // Calculate center point for drawing the symbol
  const centerX = data.width / 2;

  // Size of the capacitor symbol
  const symbolSize = 20;

  // Gap between capacitor plates
  const plateGap = 6;

  // Line length (distance from port to capacitor plate)
  const lineLength = 8; // Shorter lines than before

  // Determine if this node should be dimmed based on selection state
  const selectionState = data.selectionState;
  const shouldDim =
    selectionState?.selectedNetId || selectionState?.hoveredNetId;
  const isConnectedToHighlightedNet =
    shouldDim &&
    data.ports?.some((port: any) => {
      const netId = port.netId;
      return (
        netId === selectionState.selectedNetId ||
        netId === selectionState.hoveredNetId
      );
    });
  const opacity = shouldDim && !isConnectedToHighlightedNet ? 0.2 : 1;

  return (
    <div
      className="react-flow-capacitor-node"
      style={{
        width: data.width,
        height: data.height,
        backgroundColor: "transparent",
        border: "none",
        cursor: "default",
        pointerEvents: "none",
        position: "relative",
        transform: "translate(-0.7px, 0.7px)",
        opacity: opacity,
      }}
    >
      {/* Capacitor Symbol */}
      <div
        className="capacitor-symbol"
        style={{
          position: "absolute",
          width: data.width,
          height: data.height,
        }}
      >
        {/* Top vertical line connecting port to top plate */}
        <div
          style={{
            position: "absolute",
            top: 0,
            left: centerX,
            width: "1.5px",
            height: lineLength,
            backgroundColor: electricalComponentColor,
          }}
        />

        {/* Top capacitor plate */}
        <div
          style={{
            position: "absolute",
            top: lineLength,
            left: centerX - symbolSize / 2,
            width: symbolSize,
            height: "2px",
            backgroundColor: electricalComponentColor,
          }}
        />

        {/* Bottom capacitor plate */}
        <div
          style={{
            position: "absolute",
            top: lineLength + plateGap, // Gap between plates
            left: centerX - symbolSize / 2,
            width: symbolSize,
            height: "2px",
            backgroundColor: electricalComponentColor,
          }}
        />

        {/* Bottom vertical line connecting bottom plate to port */}
        <div
          style={{
            position: "absolute",
            top: lineLength + plateGap + 2, // Position after bottom plate
            left: centerX,
            width: "1.5px",
            height: lineLength,
            backgroundColor: electricalComponentColor,
          }}
        />

        {/* Component Labels */}
        {data.labels?.map((label: any, index: number) => (
          <div
            key={index}
            style={{
              position: "absolute",
              left: label.x,
              top: label.y,
              fontSize: "12px",
              color: electricalComponentColor,
              whiteSpace: "pre-line",
              width: label.width,
              height: label.height,
              textAlign: label.textAlign || "left",
              alignItems: "center",
              fontWeight: "600",
            }}
          >
            {label.text}
          </div>
        ))}
      </div>

      {/* Hidden port connections with no visible dots */}
      <div className="component-ports">
        {/* Port 1 - Top */}
        <div
          key={data.ports[0].id}
          className="component-port"
          style={{
            position: "absolute",
            left: centerX,
            top: 0,
            width: 1,
            height: 1,
            opacity: 0,
            zIndex: 10,
            pointerEvents: "auto",
          }}
          data-port-id={data.ports[0].id}
        >
          <Handle
            type="source"
            position={Position.Top}
            id={`${data.ports[0].id}-source`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
          <Handle
            type="target"
            position={Position.Top}
            id={`${data.ports[0].id}-target`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
        </div>

        {/* Port 2 - Bottom */}
        <div
          key={data.ports[1].id}
          className="component-port"
          style={{
            position: "absolute",
            left: centerX,
            top: data.height,
            width: 1,
            height: 1,
            opacity: 0,
            zIndex: 10,
            pointerEvents: "auto",
          }}
          data-port-id={data.ports[1].id}
        >
          <Handle
            type="source"
            position={Position.Bottom}
            id={`${data.ports[1].id}-source`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
          <Handle
            type="target"
            position={Position.Bottom}
            id={`${data.ports[1].id}-target`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
        </div>
      </div>
    </div>
  );
};

// Define a node specifically for resistors with authentic schematic symbol
const ResistorNode = ({ data }: { data: any }) => {
  // Calculate center point for drawing the symbol
  const centerX = data.width / 2;

  // Resistor dimensions
  const resistorHeight = 28;
  const resistorWidth = 12;

  // Determine if this node should be dimmed based on selection state
  const selectionState = data.selectionState;
  const shouldDim =
    selectionState?.selectedNetId || selectionState?.hoveredNetId;
  const isConnectedToHighlightedNet =
    shouldDim &&
    data.ports?.some((port: any) => {
      const netId = port.netId;
      return (
        netId === selectionState.selectedNetId ||
        netId === selectionState.hoveredNetId
      );
    });
  const opacity = shouldDim && !isConnectedToHighlightedNet ? 0.2 : 1;

  return (
    <div
      className="react-flow-resistor-node"
      style={{
        width: data.width,
        height: data.height,
        backgroundColor: "transparent",
        border: "none",
        cursor: "default",
        pointerEvents: "none",
        position: "relative",
        opacity: opacity,
        transform: "translate(-1.5px, -1.3px)",
      }}
    >
      {/* Resistor Symbol */}
      <div
        className="resistor-symbol"
        style={{
          position: "absolute",
          width: data.width,
          height: data.height,
        }}
      >
        {/* Resistor body (rectangle) */}
        <div
          style={{
            position: "absolute",
            top: data.height / 2 - resistorHeight / 2,
            left: centerX - resistorWidth / 2,
            width: resistorWidth,
            height: resistorHeight,
            backgroundColor: "transparent",
            border: `1.5px solid ${electricalComponentColor}`,
          }}
        />

        {/* Component Labels */}
        {data.labels?.map((label: any, index: number) => (
          <div
            key={index}
            style={{
              position: "absolute",
              left: label.x,
              top: label.y,
              fontSize: "12px",
              color: electricalComponentColor,
              whiteSpace: "pre-line",
              width: label.width,
              height: label.height,
              textAlign: label.textAlign || "left",
              fontWeight: "600",
            }}
          >
            {label.text}
          </div>
        ))}
      </div>

      {/* Hidden port connections with no visible dots */}
      <div className="component-ports">
        {/* Port 1 - Top */}
        <div
          key={data.ports[0].id}
          className="component-port"
          style={{
            position: "absolute",
            left: centerX,
            top: 0,
            width: 1,
            height: 1,
            opacity: 0,
            zIndex: 10,
            pointerEvents: "auto",
          }}
          data-port-id={data.ports[0].id}
        >
          <Handle
            type="source"
            position={Position.Top}
            id={`${data.ports[0].id}-source`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
          <Handle
            type="target"
            position={Position.Top}
            id={`${data.ports[0].id}-target`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
        </div>

        {/* Port 2 - Bottom */}
        <div
          key={data.ports[1].id}
          className="component-port"
          style={{
            position: "absolute",
            left: centerX,
            top: data.height,
            width: 1,
            height: 1,
            opacity: 0,
            zIndex: 10,
            pointerEvents: "auto",
          }}
          data-port-id={data.ports[1].id}
        >
          <Handle
            type="source"
            position={Position.Bottom}
            id={`${data.ports[1].id}-source`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
          <Handle
            type="target"
            position={Position.Bottom}
            id={`${data.ports[1].id}-target`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
        </div>
      </div>
    </div>
  );
};

// Define a node specifically for inductors with authentic schematic symbol
const InductorNode = ({ data }: { data: SchematicNodeData }) => {
  // Calculate center point for drawing the symbol
  const centerX = (data.width || 0) / 2;
  const height = data.height || 100; // Default height if undefined

  // Size of the inductor symbol
  const inductorHeight = 40;
  const numArcs = 4;
  const arcRadius = inductorHeight / (2 * numArcs);

  // Determine if this node should be dimmed based on selection state
  const selectionState = data.selectionState;
  const shouldDim =
    selectionState?.selectedNetId || selectionState?.hoveredNetId;
  const isConnectedToHighlightedNet =
    shouldDim &&
    data.ports?.some((port) => {
      const netId = port.netId;
      return (
        netId === selectionState.selectedNetId ||
        netId === selectionState.hoveredNetId
      );
    });
  const opacity = shouldDim && !isConnectedToHighlightedNet ? 0.2 : 1;

  return (
    <div
      className="react-flow-inductor-node"
      style={{
        width: data.width,
        height: height,
        backgroundColor: "transparent",
        border: "none",
        cursor: "default",
        pointerEvents: "none",
        position: "relative",
        opacity: opacity,
        transform: "translate(-0.2px, 0)",
      }}
    >
      {/* Inductor Symbol */}
      <div
        className="inductor-symbol"
        style={{
          position: "absolute",
          width: data.width,
          height: height,
        }}
      >
        {/* Inductor arcs */}
        <svg
          style={{
            position: "absolute",
            top: height / 2 - inductorHeight / 2,
            left: 0,
            width: data.width,
            height: inductorHeight,
          }}
        >
          <path
            d={`M ${centerX} 0 ${Array.from(
              { length: numArcs },
              (_, i) =>
                `A ${arcRadius} ${arcRadius} 0 0 0 ${centerX} ${
                  (i + 1) * 2 * arcRadius
                }`
            ).join(" ")}`}
            fill="none"
            stroke={electricalComponentColor}
            strokeWidth="1.5"
          />
        </svg>

        {/* Component Labels */}
        {data.labels?.map((label: any, index: number) => (
          <div
            key={index}
            style={{
              position: "absolute",
              left: label.x,
              top: label.y,
              fontSize: "12px",
              color: electricalComponentColor,
              whiteSpace: "pre-line",
              width: label.width,
              height: label.height,
              textAlign: label.textAlign || "left",
              alignItems: "center",
              fontWeight: "600",
            }}
          >
            {label.text}
          </div>
        ))}
      </div>

      {/* Hidden port connections with no visible dots */}
      <div className="component-ports">
        {/* Port 1 - Top */}
        <div
          key={data.ports?.[0]?.id}
          className="component-port"
          style={{
            position: "absolute",
            left: centerX,
            top: 0,
            width: 1,
            height: 1,
            opacity: 0,
            zIndex: 10,
            pointerEvents: "auto",
          }}
          data-port-id={data.ports?.[0]?.id}
        >
          <Handle
            type="source"
            position={Position.Top}
            id={`${data.ports?.[0]?.id}-source`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
          <Handle
            type="target"
            position={Position.Top}
            id={`${data.ports?.[0]?.id}-target`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
        </div>

        {/* Port 2 - Bottom */}
        <div
          key={data.ports?.[1]?.id}
          className="component-port"
          style={{
            position: "absolute",
            left: centerX,
            top: height,
            width: 1,
            height: 1,
            opacity: 0,
            zIndex: 10,
            pointerEvents: "auto",
          }}
          data-port-id={data.ports?.[1]?.id}
        >
          <Handle
            type="source"
            position={Position.Bottom}
            id={`${data.ports?.[1]?.id}-source`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
          <Handle
            type="target"
            position={Position.Bottom}
            id={`${data.ports?.[1]?.id}-target`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
        </div>
      </div>
    </div>
  );
};

// Define a node specifically for net references with an open circle symbol or ground/VDD symbol
const NetReferenceNode = ({ data }: { data: SchematicNodeData }) => {
  const isGround = data.netReferenceType === NetReferenceType.GROUND;
  const isVdd = data.netReferenceType === NetReferenceType.VDD;

  // Use fixed size for circle, ground, and VDD symbols
  const circleRadius = 3;
  const symbolSize = isGround || isVdd ? 20 : circleRadius * 2;

  // Determine label position based on port side
  const portSide = data.ports?.[0]?.properties?.["port.side"] || "WEST";
  const isEastSide = portSide === "EAST";

  // Calculate circle position - it should be at the port side
  const circleX = isEastSide
    ? (data.width || 0) - circleRadius * 2
    : circleRadius * 2;
  const circleY = (data.height || 0) / 2;

  // Determine if this node should be dimmed based on selection state
  const selectionState = data.selectionState;
  const isSelected = data.netId === selectionState?.selectedNetId;
  const isHovered = data.netId === selectionState?.hoveredNetId;
  const shouldDim =
    (selectionState?.selectedNetId || selectionState?.hoveredNetId) &&
    !isSelected &&
    !isHovered;
  const opacity = shouldDim ? 0.2 : 1;

  // Ground symbol dimensions
  const groundSymbolWidth = symbolSize;
  const groundLineSpacing = 6;
  const numGroundLines = 3;
  const groundLineWidth = [
    groundSymbolWidth,
    groundSymbolWidth * 0.6,
    groundSymbolWidth * 0.2,
  ];
  const verticalLineLength = 15;

  // VDD symbol dimensions
  const vddSymbolWidth = symbolSize;
  const vddVerticalLineLength = 15;

  return (
    <div
      className="react-flow-net-reference-node"
      style={{
        width: data.width || 0,
        height: data.height || 0,
        backgroundColor: "transparent",
        border: "none",
        cursor: "default",
        pointerEvents: "none",
        position: "relative",
        opacity: opacity,
      }}
    >
      {/* Net Reference Symbol - Either Ground Symbol, VDD Symbol, or Open Circle */}
      <div
        className="net-reference-symbol"
        style={{
          position: "absolute",
          width: data.width || 0,
          height: data.height || 0,
        }}
      >
        <svg
          style={{
            position: "absolute",
            top: 0,
            left: 0,
            width: "100%",
            height: "100%",
          }}
        >
          {isGround ? (
            // Ground Symbol
            <g
              transform={`translate(${(data.width || 0) / 2}, ${circleY - 10})`}
            >
              {/* Vertical line */}
              <line
                x1="0"
                y1={-verticalLineLength}
                x2="0"
                y2="0"
                stroke={electricalComponentColor}
                strokeWidth="1.5"
              />
              {/* Horizontal ground lines */}
              {Array.from({ length: numGroundLines }).map((_, index) => (
                <line
                  key={`ground-line-${index}`}
                  x1={-groundLineWidth[index] / 2}
                  y1={index * groundLineSpacing}
                  x2={groundLineWidth[index] / 2}
                  y2={index * groundLineSpacing}
                  stroke={electricalComponentColor}
                  strokeWidth="2"
                />
              ))}
            </g>
          ) : isVdd ? (
            // VDD Symbol
            <g transform={`translate(${(data.width || 0) / 2}, ${circleY})`}>
              {/* Vertical line */}
              <line
                x1="0"
                y1={vddVerticalLineLength}
                x2="0"
                y2="0"
                stroke={electricalComponentColor}
                strokeWidth="1.5"
              />
              {/* Horizontal line at top */}
              <line
                x1={-vddSymbolWidth / 2}
                y1="0"
                x2={vddSymbolWidth / 2}
                y2="0"
                stroke={electricalComponentColor}
                strokeWidth="2"
              />
            </g>
          ) : (
            // Regular Net Reference Circle - position at the port side
            <>
              {/* White background circle */}
              <circle
                cx={circleX}
                cy={circleY}
                r={circleRadius + 1}
                fill="white"
              />
              {/* Net reference circle */}
              <circle
                cx={circleX}
                cy={circleY}
                r={circleRadius}
                stroke={electricalComponentColor}
                strokeWidth="1.5"
                fill="transparent"
              />
            </>
          )}
        </svg>
      </div>

      {/* Single port for net reference */}
      <div className="component-ports">
        <div
          key={data.ports?.[0]?.id}
          className="component-port"
          style={{
            position: "absolute",
            left: isEastSide ? data.width || 0 : 0,
            top: circleY,
            width: 1,
            height: 1,
            opacity: 0,
            zIndex: 10,
            pointerEvents: "auto",
          }}
          data-port-id={data.ports?.[0]?.id}
        >
          {/* Single handle that will be used for all connections */}
          <Handle
            type="source"
            position={Position.Left}
            id={`${data.ports?.[0]?.id}-source`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
          <Handle
            type="target"
            position={Position.Left}
            id={`${data.ports?.[0]?.id}-target`}
            style={{ ...portHandleStyle, opacity: 0 }}
          />
        </div>
      </div>

      {/* Net reference name/label - only show for regular nets and VDD nets */}
      {!isGround && data.labels && data.labels[0] && (
        <div
          className="net-reference-label"
          style={{
            position: "absolute",
            top: isVdd ? circleY - 15 : circleY,
            left: isVdd ? "50%" : isEastSide ? "auto" : circleRadius * 4,
            right: isVdd ? "auto" : isEastSide ? circleRadius * 4 : "auto",
            transform: isVdd ? "translateX(-50%)" : "translateY(-50%)",
            textAlign: isVdd ? "center" : isEastSide ? "left" : "right",
            fontSize: "10px",
            fontWeight: "bold",
            color: electricalComponentColor,
          }}
        >
          {data.labels[0].text}
        </div>
      )}
    </div>
  );
};

// Define a node specifically for net junctions - invisible in the final rendering
const NetJunctionNode = ({ data }: { data: SchematicNodeData }) => {
  return (
    <div
      className="react-flow-net-junction-node"
      style={{
        width: 10,
        height: 10,
        backgroundColor: "transparent",
        border: "none",
        cursor: "default",
        pointerEvents: "none",
        position: "relative",
        opacity: 0, // Make it completely invisible
      }}
    >
      <div className="module-ports" data-port-id={data.ports?.[0]?.id}>
        {/* Single handle for connections */}
        <Handle
          type="source"
          id={`${data.ports?.[0]?.id}-source`}
          position={Position.Left}
          style={{ opacity: 0 }}
        />
        <Handle
          type="target"
          id={`${data.ports?.[0]?.id}-target`}
          position={Position.Left}
          style={{ opacity: 0 }}
        />
        <Handle
          type="target"
          id={`${data.ports?.[0]?.id}`}
          position={Position.Left}
          style={{ opacity: 0 }}
        />
      </div>
    </div>
  );
};

// Utility to get a CSS variable and convert to Color
function getVSCodeColor(varName: string, fallback: string): Color {
  const cssValue = getComputedStyle(document.documentElement)
    .getPropertyValue(varName)
    .trim();
  try {
    return Color.from_css(cssValue || fallback);
  } catch {
    // fallback if parsing fails
    return Color.from_css(fallback);
  }
}

// Utility to build a SchematicTheme from VSCode theme variables
function getVSCodeSchematicTheme(): Partial<
  import("kicanvas/kicad/theme").SchematicTheme
> {
  return {
    background: getVSCodeColor("--vscode-editor-background", "#ffffff"),
    component_outline: getVSCodeColor("--vscode-editor-foreground", "#666666"),
    component_body: getVSCodeColor("--vscode-editor-background", "#ffffff"),
    pin: getVSCodeColor("--vscode-editor-foreground", "#666666"),
    pin_name: getVSCodeColor("--vscode-editor-foreground", "#666666"),
    pin_number: getVSCodeColor("--vscode-editor-foreground", "#666666"),
    reference: getVSCodeColor(
      "--vscode-editorLineNumber-foreground",
      "#666666"
    ),
    value: getVSCodeColor("--vscode-editorLineNumber-foreground", "#666666"),
    fields: getVSCodeColor("--vscode-editorLineNumber-foreground", "#666666"),
    wire: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "#666666"
    ),
    bus: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "#666666"
    ),
    junction: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "#666666"
    ),
    label_local: getVSCodeColor("--vscode-foreground", "#000000"),
    label_global: getVSCodeColor(
      "--vscode-activityBarBadge-background",
      "#666666"
    ),
    label_hier: getVSCodeColor(
      "--vscode-activityBarBadge-background",
      "#666666"
    ),
    no_connect: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "#666666"
    ),
    note: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "#666666"
    ),
    sheet_background: getVSCodeColor("--vscode-editor-background", "#ffffff"),
    sheet: getVSCodeColor("--vscode-editor-foreground", "#666666"),
    sheet_label: getVSCodeColor(
      "--vscode-activityBarBadge-background",
      "#666666"
    ),
    sheet_fields: getVSCodeColor(
      "--vscode-activityBarBadge-background",
      "#666666"
    ),
    sheet_filename: getVSCodeColor(
      "--vscode-activityBarBadge-background",
      "#666666"
    ),
    sheet_name: getVSCodeColor(
      "--vscode-activityBarBadge-background",
      "#666666"
    ),
    erc_warning: getVSCodeColor("--vscode-editorWarning-foreground", "#FFA500"),
    erc_error: getVSCodeColor("--vscode-editorError-foreground", "#FF0000"),
    grid: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "#cccccc"
    ),
    grid_axes: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "#cccccc"
    ),
    hidden: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "#cccccc"
    ),
    brightened: getVSCodeColor(
      "--vscode-activityBarBadge-background",
      "#ff00ff"
    ),
    worksheet: getVSCodeColor("--vscode-editor-background", "#ffffff"),
    cursor: getVSCodeColor("--vscode-editorCursor-foreground", "#000000"),
    aux_items: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "#666666"
    ),
    anchor: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "#0000ff"
    ),
    shadow: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "rgba(128,128,128,0.5)"
    ),
    bus_junction: getVSCodeColor(
      "--vscode-editorLineNumber-dimmedForeground",
      "#008000"
    ),
  };
}

// Define a node for KiCad symbols
const SymbolNode = React.memo(
  ({ data }: { data: SchematicNodeData }) => {
    const canvasRef = useRef<HTMLCanvasElement>(null);
    const [isRendering, setIsRendering] = useState(true);
    const [renderError, setRenderError] = useState<string | null>(null);

    // Determine if this node should be dimmed based on selection state
    const selectionState = data.selectionState;
    const shouldDim =
      selectionState?.selectedNetId || selectionState?.hoveredNetId;
    const isConnectedToHighlightedNet =
      shouldDim &&
      data.ports?.some((port) => {
        const netId = port.netId;
        return (
          netId === selectionState.selectedNetId ||
          netId === selectionState.hoveredNetId
        );
      });
    const opacity = shouldDim && !isConnectedToHighlightedNet ? 0.2 : 1;

    // Get symbol properties from node data
    const symbolPath = data.properties?.symbolPath as string;
    const symbolName = data.properties?.symbolName as string;
    const netlist = (data as any).netlist as Netlist;

    useEffect(() => {
      const renderSymbol = async () => {
        if (
          !canvasRef.current ||
          !symbolPath ||
          !netlist?.symbols?.[symbolPath]
        ) {
          setRenderError("Symbol data not available");
          setIsRendering(false);
          return;
        }

        try {
          setIsRendering(true);
          const canvas = canvasRef.current;
          const symbolContent = netlist.symbols[symbolPath];

          // First, get the symbol info to know its natural size
          const symbolInfo = getKicadSymbolInfo(symbolContent, symbolName, {
            unit: 1,
            bodyStyle: 1,
            tightBounds: false,
          });

          // The node dimensions are already calculated by renderer.ts
          const nodeWidth = data.width || 100;
          const nodeHeight = data.height || 100;

          // Get device pixel ratio for crisp rendering
          const dpr = window.devicePixelRatio || 1;

          // Set canvas size to match node size exactly, accounting for device pixel ratio
          canvas.width = nodeWidth * dpr;
          canvas.height = nodeHeight * dpr;

          // Scale the canvas context to account for device pixel ratio
          const ctx = canvas.getContext("2d");
          if (ctx) {
            ctx.save(); // Save the context state
            ctx.scale(dpr, dpr);
          }

          // The renderer.ts uses a scale factor of 10 to convert from symbol units to schematic units
          // So if the node is sized as symbolInfo.bbox.w * 10, we need to render at a scale
          // that makes the symbol fill the canvas

          // Calculate the scale needed to fit the symbol in the canvas
          // Use zero padding for exact fit
          const symbolPadding = 0; // Zero padding
          // When rendering, we use the logical size (not multiplied by dpr)
          const availableWidth = nodeWidth;
          const availableHeight = nodeHeight;

          // The symbol's natural size (no padding needed since we want exact fit)
          const symbolWidthWithPadding = symbolInfo.bbox.w;
          const symbolHeightWithPadding = symbolInfo.bbox.h;

          // Calculate scale to fit
          const scaleX = availableWidth / symbolWidthWithPadding;
          const scaleY = availableHeight / symbolHeightWithPadding;
          const scale = Math.min(scaleX, scaleY);

          console.log("Symbol rendering debug:", {
            nodeWidth,
            nodeHeight,
            symbolBBox: symbolInfo.bbox,
            scaleX,
            scaleY,
            finalScale: scale,
          });

          // Get theme from VSCode CSS variables
          const vscodeTheme = getVSCodeSchematicTheme();

          // Render the symbol at the calculated scale
          await renderKicadSymbol(canvas, symbolContent, symbolName, {
            scale: scale,
            padding: symbolPadding, // Zero padding
            showPinNames: false,
            showPinNumbers: false,
            tightBounds: false, // Include pins to match renderer.ts
            theme: vscodeTheme,
          });

          // Restore canvas context state
          if (ctx) {
            ctx.restore();
          }

          setIsRendering(false);
        } catch (error) {
          console.error("Error rendering symbol:", error);
          setRenderError(
            error instanceof Error ? error.message : "Unknown error"
          );
          setIsRendering(false);
        }
      };

      renderSymbol();
    }, [
      symbolPath,
      symbolName,
      netlist.symbols.symbolPath,
      data.width,
      data.height,
      netlist.symbols,
    ]);

    return (
      <div
        className="react-flow-symbol-node"
        style={{
          width: data.width,
          height: data.height,
          backgroundColor: "transparent",
          border: "none",
          cursor: "default",
          pointerEvents: "none",
          position: "relative",
          opacity: opacity,
        }}
      >
        {/* Canvas for KiCad symbol rendering */}
        <canvas
          ref={canvasRef}
          style={{
            position: "absolute",
            top: 0,
            left: 0,
            width: `${data.width}px`,
            height: `${data.height}px`,
            imageRendering: "crisp-edges",
            backgroundColor: "transparent",
          }}
        />

        {/* Loading indicator */}
        {isRendering && (
          <div
            style={{
              position: "absolute",
              top: "50%",
              left: "50%",
              transform: "translate(-50%, -50%)",
              color: electricalComponentColor,
              fontSize: "12px",
            }}
          >
            Loading...
          </div>
        )}

        {/* Error message */}
        {renderError && (
          <div
            style={{
              position: "absolute",
              top: "50%",
              left: "50%",
              transform: "translate(-50%, -50%)",
              color: "red",
              fontSize: "10px",
              textAlign: "center",
              width: "90%",
            }}
          >
            {renderError}
          </div>
        )}

        {/* Port connections */}
        <div className="component-ports">
          {data.ports?.map((port) => {
            // Port position is already calculated by the renderer
            const portX = port.x || 0;
            const portY = port.y || 0;

            // Determine handle position based on port side
            let handlePosition = Position.Left;
            const side = port.properties?.["port.side"];
            if (side === "EAST") handlePosition = Position.Right;
            else if (side === "NORTH") handlePosition = Position.Top;
            else if (side === "SOUTH") handlePosition = Position.Bottom;

            return (
              <div
                key={port.id}
                className="component-port"
                style={{
                  position: "absolute",
                  left: portX,
                  top: portY,
                  width: 1,
                  height: 1,
                  opacity: 0,
                  zIndex: 20,
                  pointerEvents: "auto",
                }}
                data-port-id={port.id}
              >
                <Handle
                  type="source"
                  position={handlePosition}
                  id={`${port.id}-source`}
                  style={{ ...portHandleStyle, opacity: 0 }}
                />
                <Handle
                  type="target"
                  position={handlePosition}
                  id={`${port.id}-target`}
                  style={{ ...portHandleStyle, opacity: 0 }}
                />

                {/* Port label - only show if configured */}
                {port.labels && port.labels[0] && (
                  <div
                    className="port-label"
                    style={{
                      position: "absolute",
                      fontSize: "10px",
                      whiteSpace: "nowrap",
                      pointerEvents: "none",
                      color: electricalComponentColor,
                      opacity: 0.7,
                      transform:
                        side === "WEST"
                          ? "translate(5px, -5px)"
                          : side === "EAST"
                          ? "translate(-100%, -5px) translateX(-5px)"
                          : side === "NORTH"
                          ? "translate(-50%, 5px)"
                          : "translate(-50%, -15px)",
                      textAlign:
                        side === "WEST"
                          ? "left"
                          : side === "EAST"
                          ? "right"
                          : "center",
                    }}
                  >
                    {port.labels[0].text}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    );
  },
  (prevProps, nextProps) => {
    // Custom comparison function for React.memo
    // Only re-render if the symbol data or dimensions change
    return (
      prevProps.data.properties?.symbolPath ===
        nextProps.data.properties?.symbolPath &&
      prevProps.data.properties?.symbolName ===
        nextProps.data.properties?.symbolName &&
      prevProps.data.width === nextProps.data.width &&
      prevProps.data.height === nextProps.data.height &&
      // Also check if the opacity should change due to selection state
      prevProps.data.selectionState?.selectedNetId ===
        nextProps.data.selectionState?.selectedNetId &&
      prevProps.data.selectionState?.hoveredNetId ===
        nextProps.data.selectionState?.hoveredNetId &&
      // Check if connected ports' netIds are the same
      JSON.stringify(prevProps.data.ports?.map((p) => p.netId)) ===
        JSON.stringify(nextProps.data.ports?.map((p) => p.netId))
    );
  }
);

// Define custom edge for electrical connections
const ElectricalEdge = ({
  id,
  data,
  interactionWidth,
  style = {},
}: EdgeProps<SchematicEdge>) => {
  // Get section data from the ElkEdge
  const section = data?.sections?.[0];

  // Build points array from section data
  let points = [
    // Start with the section's startPoint
    { x: section?.startPoint?.x || 0, y: section?.startPoint?.y || 0 },
    // Add any bend points from the section
    ...(section?.bendPoints || []),
    // End with the section's endPoint
    { x: section?.endPoint?.x || 0, y: section?.endPoint?.y || 0 },
  ];

  // Build path data string with straight lines (L commands)
  let pathData = `M${points[0].x},${points[0].y}`;

  for (let i = 1; i < points.length; i++) {
    pathData += ` L${points[i].x},${points[i].y}`;
  }

  // Determine if this edge should be dimmed based on selection state
  const selectionState = data?.selectionState;
  const isSelected = data?.netId === selectionState?.selectedNetId;
  const isHovered = data?.netId === selectionState?.hoveredNetId;
  const shouldDim =
    (selectionState?.selectedNetId || selectionState?.hoveredNetId) &&
    !isSelected &&
    !isHovered;
  const opacity = shouldDim ? 0.2 : 1;

  return (
    <>
      <path
        id={id}
        style={{
          strokeWidth: 1.5,
          stroke: edgeColor,
          pointerEvents: "none",
          ...style,
          opacity: opacity,
        }}
        className="react-flow__edge-path electrical-edge straight-line"
        d={pathData}
      />

      <path
        d={pathData}
        fill="none"
        strokeOpacity={0}
        strokeWidth={interactionWidth}
        className="react-flow__edge-interaction"
      />

      {/* Render junction points if they exist */}
      {data?.junctionPoints &&
        data.junctionPoints.map(
          (point: { x: number; y: number }, index: number) => (
            <circle
              key={`junction-${id}-${index}`}
              cx={point.x}
              cy={point.y}
              r={3.5}
              fill={style.stroke || edgeColor}
              style={{
                ...style,
                opacity: opacity,
              }}
              className="electrical-junction-point"
            />
          )
        )}
    </>
  );
};

// Define edge types
const edgeTypes: EdgeTypes = {
  electrical: ElectricalEdge,
};

// Define node types
const nodeTypes = {
  module: ModuleNode,
  component: ModuleNode,
  capacitor: CapacitorNode,
  resistor: ResistorNode,
  inductor: InductorNode,
  net_reference: NetReferenceNode,
  net_junction: NetJunctionNode,
  symbol: SymbolNode,
};

interface ReactFlowSchematicViewerProps {
  netlist: Netlist;
  onError?: (message: string) => void;
  onComponentSelect?: (componentId: string | null) => void;
  selectedComponent?: string | null;
  config?: Partial<SchematicConfig>;
}

const Visualizer = ({
  netlist,
  onComponentSelect = () => {},
  selectedComponent = null,
  config = DEFAULT_CONFIG,
}: {
  netlist: Netlist;
  onComponentSelect?: (componentId: string | null) => void;
  selectedComponent?: string | null;
  config?: Partial<SchematicConfig>;
}) => {
  const [nodes, setNodes, onNodesChange] = useNodesState<SchematicNode>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<SchematicEdge>([]);
  const [elkLayout, setElkLayout] = useState<ElkGraph | null>(null);
  const [layoutError, setLayoutError] = useState<string | null>(null);
  const [selectionState, setSelectionState] = useState<SelectionState>({
    selectedNetId: null,
    hoveredNetId: null,
  });
  const [prevComponent, setPrevComponent] = useState<string | null>(null);
  const [shouldAnimate, setShouldAnimate] = useState(false);
  const [isGeneratingPDF, setIsGeneratingPDF] = useState(false);
  const [showDebugPane, setShowDebugPane] = useState(false);
  const [currentConfig, setCurrentConfig] = useState<SchematicConfig>({
    ...DEFAULT_CONFIG,
    ...config,
    nodeSizes: {
      ...DEFAULT_CONFIG.nodeSizes,
      ...config?.nodeSizes,
    },
    layout: {
      ...DEFAULT_CONFIG.layout,
      ...config?.layout,
    },
    visual: {
      ...DEFAULT_CONFIG.visual,
      ...config?.visual,
    },
  });
  const elkInstance = useRef<ELKType | null>(null);
  const reactFlowInstance = useRef<any>(null);

  // Create separate debounced functions for each state field
  const debouncedSetSelectedNet = useMemo(
    () =>
      debounce((selectedNetId: string | null) => {
        setSelectionState((prev) => ({
          ...prev,
          selectedNetId,
        }));
      }, 200), // Slightly longer debounce for selection
    []
  );

  const debouncedSetHoveredNet = useMemo(
    () =>
      debounce((hoveredNetId: string | null) => {
        setSelectionState((prev) => ({
          ...prev,
          hoveredNetId,
        }));
      }, 100), // Shorter debounce for hover to feel more responsive
    []
  );

  // Cleanup debounced functions on unmount
  useEffect(() => {
    return () => {
      debouncedSetSelectedNet.cancel();
      debouncedSetHoveredNet.cancel();
    };
  }, [debouncedSetSelectedNet, debouncedSetHoveredNet]);

  // Initialize ELK engine
  useEffect(() => {
    elkInstance.current = new ELK();
  }, []);

  useEffect(() => {
    async function render() {
      const renderer = new SchematicRenderer(netlist, currentConfig);
      if (selectedComponent) {
        try {
          let layout = await renderer.render(selectedComponent);

          // Determine if we should animate based on whether the component changed
          const isNewComponent = prevComponent !== selectedComponent;
          setShouldAnimate(!isNewComponent);
          setPrevComponent(selectedComponent);

          setElkLayout(layout as ElkGraph);
          // Center the view after new component is rendered
          setTimeout(() => {
            reactFlowInstance.current?.fitView({
              padding: 0.2,
              duration: 200,
            });
          }, 10);
        } catch (error) {
          console.error("Error rendering component: ", error);
          setLayoutError(
            error instanceof Error ? error.message : "Unknown error"
          );
        }
      }
    }

    render();
  }, [netlist, selectedComponent, prevComponent, currentConfig]);

  // Update nodes and edges when layout changes
  useEffect(() => {
    if (elkLayout) {
      const nodes = elkLayout.children.map((elkNode) =>
        createSchematicNode(elkNode, selectionState, netlist)
      );

      // Add animation class if we're updating the same component
      const nodesWithAnimation = nodes.map((node) => ({
        ...node,
        className: `${node.className || ""} ${
          shouldAnimate ? "animate-layout" : ""
        }`.trim(),
      }));

      setNodes(nodesWithAnimation);

      const edges = elkLayout.edges.map((elkEdge) =>
        createSchematicEdge(elkEdge, selectionState)
      );

      // Add animation class to edges as well
      const edgesWithAnimation = edges.map((edge) => ({
        ...edge,
        className: shouldAnimate ? "animate-layout" : "",
      }));

      setEdges(edgesWithAnimation);

      // Reset animation flag after applying it
      if (shouldAnimate) {
        setTimeout(() => {
          setShouldAnimate(false);
        }, 50);
      }
    }
  }, [elkLayout, setNodes, setEdges, selectionState, shouldAnimate, netlist]);

  // Handle node click to select a component - only if the component is clickable (modules)
  const handleNodeClick = useCallback(
    (event: React.MouseEvent, node: Node) => {
      event.preventDefault();

      // Check if the node is a module (which should be clickable)
      const nodeData = node.data as SchematicNodeData;
      if (nodeData.componentType === NodeType.MODULE) {
        onComponentSelect(node.id);
      }
    },
    [onComponentSelect]
  );

  useOnSelectionChange({
    onChange: useCallback(
      ({ nodes, edges }) => {
        let selectedNetId =
          edges.length > 0 ? (edges[0].data?.netId as string) : null;

        if (selectedNetId !== selectionState.selectedNetId) {
          debouncedSetSelectedNet(selectedNetId);
        }
      },
      [selectionState.selectedNetId, debouncedSetSelectedNet]
    ),
  });

  const handleDownloadPDF = async () => {
    if (!selectedComponent) return;

    setIsGeneratingPDF(true);
    try {
      // Create PDF renderer with current config - use the exact same config as the React viewer
      const pdfRenderer = new PDFSchematicRenderer(netlist, currentConfig);

      // Render the PDF
      const doc = await pdfRenderer.render(selectedComponent);

      // Save the PDF with a clean filename
      const filename = `${selectedComponent.split(".").pop()}_schematic.pdf`;
      doc.save(filename);
    } catch (error) {
      console.error("Error generating PDF:", error);
    } finally {
      setIsGeneratingPDF(false);
    }
  };

  const updateConfig = useCallback((updates: Partial<SchematicConfig>) => {
    setCurrentConfig((prev) => ({
      ...prev,
      ...updates,
      nodeSizes: {
        ...prev.nodeSizes,
        ...updates.nodeSizes,
      },
      layout: {
        ...prev.layout,
        ...updates.layout,
      },
      visual: {
        ...prev.visual,
        ...updates.visual,
      },
    }));
  }, []);

  return (
    <div className="schematic-viewer">
      <style>
        {customStyles}
        {`
        /* Debug pane styles */
        .debug-pane {
          background-color: var(--vscode-sideBar-background, #252526);
          border: 1px solid var(--vscode-sideBar-border, #3c3c3c);
          border-radius: 4px;
          padding: 12px;
          max-width: 280px;
          max-height: 400px;
          overflow-y: auto;
          box-shadow: 0 2px 8px rgba(0, 0, 0, 0.15);
        }

        .debug-pane h3 {
          margin: 0 0 12px 0;
          font-size: 14px;
          font-weight: 600;
          color: var(--vscode-foreground, #cccccc);
          display: flex;
          align-items: center;
          justify-content: space-between;
        }

        .debug-pane-section {
          margin-bottom: 16px;
        }

        .debug-pane-section h4 {
          margin: 0 0 8px 0;
          font-size: 12px;
          font-weight: 600;
          color: var(--vscode-foreground, #cccccc);
          opacity: 0.8;
        }

        .debug-pane-control {
          margin-bottom: 8px;
          display: flex;
          align-items: center;
          justify-content: space-between;
        }

        .debug-pane-control label {
          font-size: 12px;
          color: var(--vscode-foreground, #cccccc);
          flex: 1;
        }

        .debug-pane-control input[type="checkbox"] {
          margin-left: 8px;
        }

        .debug-pane-control input[type="range"] {
          flex: 1;
          margin: 0 8px;
        }

        .debug-pane-control select {
          background-color: var(--vscode-input-background, #3c3c3c);
          color: var(--vscode-input-foreground, #cccccc);
          border: 1px solid var(--vscode-input-border, #616161);
          border-radius: 2px;
          padding: 2px 4px;
          font-size: 12px;
          margin-left: 8px;
        }

        .debug-pane-control .value-display {
          font-size: 11px;
          color: var(--vscode-foreground, #cccccc);
          opacity: 0.7;
          min-width: 30px;
          text-align: right;
        }

        .debug-toggle-button {
          display: flex;
          align-items: center;
          justify-content: center;
          background-color: var(--vscode-button-background, #0066cc);
          color: var(--vscode-button-foreground, #fff);
          border: 1px solid var(--vscode-button-border, transparent);
          padding: 8px;
          border-radius: 4px;
          cursor: pointer;
          transition: background-color 0.2s;
        }

        .debug-toggle-button:hover {
          background-color: var(--vscode-button-hoverBackground, #0052a3);
        }

        .debug-toggle-button svg {
          width: 16px;
          height: 16px;
        }
      `}
      </style>

      {layoutError && (
        <div
          className="error-message"
          style={{
            color: "var(--vscode-errorForeground, #f44336)",
            backgroundColor:
              "var(--vscode-inputValidation-errorBackground, #fde7e9)",
            border:
              "1px solid var(--vscode-inputValidation-errorBorder, #f44336)",
            padding: "10px",
            margin: "10px",
            borderRadius: "4px",
          }}
        >
          <h3>Layout Error</h3>
          <p>{layoutError}</p>
        </div>
      )}

      <div
        className="react-flow-schematic-viewer"
        style={{
          backgroundColor: "var(--vscode-editor-background, #fff)",
          color: "var(--vscode-foreground, #000)",
          height: "100%",
          width: "100%",
          outline: "none",
        }}
      >
        <ReactFlow
          proOptions={{ hideAttribution: true }}
          nodes={nodes}
          edges={edges}
          onNodesChange={onNodesChange}
          onEdgesChange={onEdgesChange}
          nodeTypes={nodeTypes}
          edgeTypes={edgeTypes}
          fitView
          onInit={(instance) => {
            reactFlowInstance.current = instance;
          }}
          onNodeClick={handleNodeClick}
          onEdgeMouseEnter={(_event, edge) => {
            if (
              edge.data?.netId &&
              edge.data?.netId !== selectionState.selectedNetId &&
              edge.data?.netId !== selectionState.hoveredNetId
            ) {
              debouncedSetHoveredNet(edge.data?.netId);
            }
          }}
          onEdgeMouseLeave={() => {
            debouncedSetHoveredNet(null);
          }}
          defaultEdgeOptions={{
            type: "electrical",
            style: {
              stroke: edgeColor,
              strokeWidth: 1.5,
            },
            interactionWidth: 10,
          }}
          style={{
            backgroundColor: "var(--vscode-editor-background, #fff)",
          }}
          nodesDraggable={false}
          nodesConnectable={false}
          elementsSelectable={true}
          selectNodesOnDrag={false}
          zoomOnScroll={true}
          panOnScroll={true}
          panOnDrag={true}
          preventScrolling={false}
        >
          <Controls showInteractive={false} />
          <Panel position="top-right">
            <div style={{ display: "flex", gap: "8px", alignItems: "center" }}>
              <button
                className="debug-toggle-button"
                onClick={() => setShowDebugPane(!showDebugPane)}
                title="Toggle debug options"
              >
                <Settings size={16} />
              </button>
              <button
                className="download-button"
                onClick={handleDownloadPDF}
                disabled={!selectedComponent || isGeneratingPDF}
                title={
                  !selectedComponent
                    ? "Select a component to download"
                    : isGeneratingPDF
                    ? "Generating PDF..."
                    : "Download schematic as PDF"
                }
              >
                {isGeneratingPDF ? (
                  <Loader size={16} className="loading-icon" />
                ) : (
                  <Download size={16} />
                )}
                {isGeneratingPDF ? "Generating..." : "Download PDF"}
              </button>
            </div>
          </Panel>

          {showDebugPane && (
            <Panel position="top-left">
              <div className="debug-pane">
                <h3>
                  Debug Options
                  <button
                    onClick={() => setShowDebugPane(false)}
                    style={{
                      background: "none",
                      border: "none",
                      color: "var(--vscode-foreground, #cccccc)",
                      cursor: "pointer",
                      padding: "4px",
                      opacity: 0.7,
                    }}
                  >
                    ×
                  </button>
                </h3>

                <div className="debug-pane-section">
                  <h4>Layout</h4>

                  <div className="debug-pane-control">
                    <label htmlFor="direction">Direction:</label>
                    <select
                      id="direction"
                      value={currentConfig.layout.direction}
                      onChange={(e) =>
                        updateConfig({
                          layout: {
                            ...currentConfig.layout,
                            direction: e.target.value as any,
                          },
                        })
                      }
                    >
                      <option value="LEFT">Left</option>
                      <option value="RIGHT">Right</option>
                      <option value="UP">Up</option>
                      <option value="DOWN">Down</option>
                    </select>
                  </div>

                  <div className="debug-pane-control">
                    <label htmlFor="spacing">Spacing:</label>
                    <input
                      id="spacing"
                      type="range"
                      min="5"
                      max="50"
                      value={currentConfig.layout.spacing}
                      onChange={(e) =>
                        updateConfig({
                          layout: {
                            ...currentConfig.layout,
                            spacing: Number(e.target.value),
                          },
                        })
                      }
                    />
                    <span className="value-display">
                      {currentConfig.layout.spacing}
                    </span>
                  </div>

                  <div className="debug-pane-control">
                    <label htmlFor="padding">Padding:</label>
                    <input
                      id="padding"
                      type="range"
                      min="0"
                      max="100"
                      value={currentConfig.layout.padding}
                      onChange={(e) =>
                        updateConfig({
                          layout: {
                            ...currentConfig.layout,
                            padding: Number(e.target.value),
                          },
                        })
                      }
                    />
                    <span className="value-display">
                      {currentConfig.layout.padding}
                    </span>
                  </div>

                  <div className="debug-pane-control">
                    <label htmlFor="explodeModules">Explode Modules:</label>
                    <input
                      id="explodeModules"
                      type="checkbox"
                      checked={currentConfig.layout.explodeModules}
                      onChange={(e) =>
                        updateConfig({
                          layout: {
                            ...currentConfig.layout,
                            explodeModules: e.target.checked,
                          },
                        })
                      }
                    />
                  </div>

                  <div className="debug-pane-control">
                    <label htmlFor="smartNetReferencePositioning">
                      Smart Net Reference Positioning:
                    </label>
                    <input
                      id="smartNetReferencePositioning"
                      type="checkbox"
                      checked={
                        currentConfig.layout.smartNetReferencePositioning ??
                        true
                      }
                      onChange={(e) =>
                        updateConfig({
                          layout: {
                            ...currentConfig.layout,
                            smartNetReferencePositioning: e.target.checked,
                          },
                        })
                      }
                    />
                  </div>

                  <div className="debug-pane-control">
                    <label htmlFor="smartEdgeSplitting">
                      Smart Edge Splitting:
                    </label>
                    <input
                      id="smartEdgeSplitting"
                      type="checkbox"
                      checked={currentConfig.layout.smartEdgeSplitting ?? true}
                      onChange={(e) =>
                        updateConfig({
                          layout: {
                            ...currentConfig.layout,
                            smartEdgeSplitting: e.target.checked,
                          },
                        })
                      }
                    />
                  </div>
                </div>

                <div className="debug-pane-section">
                  <h4>Visual</h4>

                  <div className="debug-pane-control">
                    <label htmlFor="showPortLabels">Show Port Labels:</label>
                    <input
                      id="showPortLabels"
                      type="checkbox"
                      checked={currentConfig.visual.showPortLabels}
                      onChange={(e) =>
                        updateConfig({
                          visual: {
                            ...currentConfig.visual,
                            showPortLabels: e.target.checked,
                          },
                        })
                      }
                    />
                  </div>

                  <div className="debug-pane-control">
                    <label htmlFor="showComponentValues">
                      Show Component Values:
                    </label>
                    <input
                      id="showComponentValues"
                      type="checkbox"
                      checked={currentConfig.visual.showComponentValues}
                      onChange={(e) =>
                        updateConfig({
                          visual: {
                            ...currentConfig.visual,
                            showComponentValues: e.target.checked,
                          },
                        })
                      }
                    />
                  </div>

                  <div className="debug-pane-control">
                    <label htmlFor="showFootprints">Show Footprints:</label>
                    <input
                      id="showFootprints"
                      type="checkbox"
                      checked={currentConfig.visual.showFootprints}
                      onChange={(e) =>
                        updateConfig({
                          visual: {
                            ...currentConfig.visual,
                            showFootprints: e.target.checked,
                          },
                        })
                      }
                    />
                  </div>
                </div>
              </div>
            </Panel>
          )}
        </ReactFlow>
      </div>
    </div>
  );
};

const ReactFlowSchematicViewer = ({
  netlist,
  onComponentSelect = () => {},
  selectedComponent = null,
  config = DEFAULT_CONFIG,
}: ReactFlowSchematicViewerProps) => {
  return (
    <ReactFlowProvider>
      <Visualizer
        netlist={netlist}
        onComponentSelect={onComponentSelect}
        selectedComponent={selectedComponent}
        config={config}
      />
    </ReactFlowProvider>
  );
};

export type { SchematicConfig };
export { DEFAULT_CONFIG };
export default ReactFlowSchematicViewer;
