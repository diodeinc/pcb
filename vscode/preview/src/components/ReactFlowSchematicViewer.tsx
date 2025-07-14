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
  Background,
  BackgroundVariant,
} from "@xyflow/react";
import type { Edge, EdgeProps, EdgeTypes } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import "./ReactFlowSchematicViewer.css";
import {
  NodeType,
  SchematicLayoutEngine,
  DEFAULT_CONFIG,
} from "../LayoutEngine";
import type {
  ElkEdge,
  ElkNode,
  SchematicConfig,
  NodePositions,
} from "../LayoutEngine";

// import { PDFSchematicRenderer } from "../PDFSchematicRenderer";
import type { Netlist } from "../types/NetlistTypes";
import { debounce } from "lodash";
import { Settings } from "react-feather";
import {
  renderKicadSymbol,
  getKicadSymbolInfo,
  DEFAULT_THEME,
  SELECTED_THEME,
} from "../renderer/kicad_sym";
import {
  renderGlobalLabel,
  type LabelDirection,
} from "../renderer/kicad_global_label";
// import { Color } from "../third_party/kicanvas/base/color";

// Utility function for grid snapping
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
  // Note: positions should already be snapped by the layout engine
  return {
    id: elkNode.id,
    data: {
      componentType: elkNode.type,
      selectionState,
      ...elkNode,
      ...(elkNode.type === NodeType.SYMBOL && netlist ? { netlist } : {}),
      // Ensure rotation is included in data
      rotation: elkNode.rotation || 0,
    },
    position: { x: elkNode.x || 0, y: elkNode.y || 0 },
    type: elkNode.type,
    draggable: true,
    // Make all nodes selectable so they can be rotated
    selectable: true,
    connectable: false,
    // Add custom styles based on node type
    style: {
      // Prevent hover effects on component nodes
      ...(elkNode.type === NodeType.COMPONENT
        ? {
            cursor: "move",
            // Add some !important styles but NOT transform
            backgroundColor: "#f5f5f5 !important",
            border: "1px solid #ddd !important",
            boxShadow: "none !important",
          }
        : {}),
    },
    // Add class for additional styling with CSS
    className:
      elkNode.type === NodeType.MODULE ? "module-node" : "component-node",
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
// const electricalComponentColor = "var(--vscode-editor-foreground, #666)";
// const edgeColor = "var(--vscode-editorLineNumber-dimmedForeground, #666)";
// const accentColor = "var(--vscode-activityBarBadge-background, #666)";
const electricalComponentColor = DEFAULT_THEME.component_outline.to_css();
const edgeColor = DEFAULT_THEME.wire.to_css();
const backgroundColor = DEFAULT_THEME.background.to_css();
const labelColor = DEFAULT_THEME.reference.to_css();

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

  // Get rotation from data
  const rotation = data.rotation || 0;

  // Different styles for modules vs components
  const nodeStyle: CSSProperties = {
    width: data.width,
    height: data.height,
    backgroundColor: isModule
      ? backgroundColor
      : `color-mix(in srgb, ${electricalComponentColor} 5%, ${backgroundColor})`,
    border: `1px solid ${electricalComponentColor}`,
    opacity: moduleOpacity,
    cursor: "move",
    pointerEvents: "auto",
    borderRadius: "0px",
    // Apply rotation transform
    transform: rotation !== 0 ? `rotate(${rotation}deg)` : undefined,
    transformOrigin: "center",
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
            color: labelColor,
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
                  {port.labels && port.labels.length > 0 && (
                    <div className="port-label" style={labelStyle}>
                      {port.labels[0].text}
                    </div>
                  )}
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
  // const isGround = data.netReferenceType === NetReferenceType.GROUND;
  // const isVdd = data.netReferenceType === NetReferenceType.VDD;
  const isGround = false;
  const isVdd = false;

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

// Define a node specifically for net junctions - a small dot at wire intersections
const NetJunctionNode = ({ data }: { data: SchematicNodeData }) => {
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
      className="react-flow-net-junction-node"
      style={{
        width: 10,
        height: 10,
        backgroundColor: "transparent",
        border: "none",
        cursor: "default",
        pointerEvents: "none",
        position: "relative",
        opacity: opacity,
      }}
    >
      {/* Junction dot */}
      <div
        style={{
          position: "absolute",
          width: "6px",
          height: "6px",
          borderRadius: "50%",
          backgroundColor: edgeColor,
          top: "2px",
          left: "2px",
        }}
      />

      <div className="junction-ports" data-port-id={data.ports?.[0]?.id}>
        {/* Single handle for connections */}
        <Handle
          type="source"
          id={`${data.ports?.[0]?.id}-source`}
          position={Position.Left}
          style={{ opacity: 0, left: 5, top: 5 }}
        />
        <Handle
          type="target"
          id={`${data.ports?.[0]?.id}-target`}
          position={Position.Left}
          style={{ opacity: 0, left: 5, top: 5 }}
        />
      </div>
    </div>
  );
};

// Utility to get a CSS variable and convert to Color
// function getVSCodeColor(varName: string, fallback: string): Color {
//   const cssValue = getComputedStyle(document.documentElement)
//     .getPropertyValue(varName)
//     .trim();
//   try {
//     return Color.from_css(cssValue || fallback);
//   } catch {
//     // fallback if parsing fails
//     return Color.from_css(fallback);
//   }
// }

// Utility to build a SchematicTheme from VSCode theme variables
// function getVSCodeSchematicTheme(): Partial<
//   import("../third_party/kicanvas/kicad/theme").SchematicTheme
// > {
//   return {
//     background: getVSCodeColor("--vscode-editor-background", "#ffffff"),
//     component_outline: getVSCodeColor("--vscode-editor-foreground", "#666666"),
//     component_body: getVSCodeColor("--vscode-editor-background", "#ffffff"),
//     pin: getVSCodeColor("--vscode-editor-foreground", "#666666"),
//     pin_name: getVSCodeColor("--vscode-editor-foreground", "#666666"),
//     pin_number: getVSCodeColor("--vscode-editor-foreground", "#666666"),
//     reference: getVSCodeColor(
//       "--vscode-editorLineNumber-foreground",
//       "#666666"
//     ),
//     value: getVSCodeColor("--vscode-editorLineNumber-foreground", "#666666"),
//     fields: getVSCodeColor("--vscode-editorLineNumber-foreground", "#666666"),
//     wire: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "#666666"
//     ),
//     bus: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "#666666"
//     ),
//     junction: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "#666666"
//     ),
//     label_local: getVSCodeColor("--vscode-foreground", "#000000"),
//     label_global: getVSCodeColor(
//       "--vscode-activityBarBadge-background",
//       "#666666"
//     ),
//     label_hier: getVSCodeColor(
//       "--vscode-activityBarBadge-background",
//       "#666666"
//     ),
//     no_connect: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "#666666"
//     ),
//     note: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "#666666"
//     ),
//     sheet_background: getVSCodeColor("--vscode-editor-background", "#ffffff"),
//     sheet: getVSCodeColor("--vscode-editor-foreground", "#666666"),
//     sheet_label: getVSCodeColor(
//       "--vscode-activityBarBadge-background",
//       "#666666"
//     ),
//     sheet_fields: getVSCodeColor(
//       "--vscode-activityBarBadge-background",
//       "#666666"
//     ),
//     sheet_filename: getVSCodeColor(
//       "--vscode-activityBarBadge-background",
//       "#666666"
//     ),
//     sheet_name: getVSCodeColor(
//       "--vscode-activityBarBadge-background",
//       "#666666"
//     ),
//     erc_warning: getVSCodeColor("--vscode-editorWarning-foreground", "#FFA500"),
//     erc_error: getVSCodeColor("--vscode-editorError-foreground", "#FF0000"),
//     grid: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "#cccccc"
//     ),
//     grid_axes: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "#cccccc"
//     ),
//     hidden: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "#cccccc"
//     ),
//     brightened: getVSCodeColor(
//       "--vscode-activityBarBadge-background",
//       "#ff00ff"
//     ),
//     worksheet: getVSCodeColor("--vscode-editor-background", "#ffffff"),
//     cursor: getVSCodeColor("--vscode-editorCursor-foreground", "#000000"),
//     aux_items: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "#666666"
//     ),
//     anchor: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "#0000ff"
//     ),
//     shadow: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "rgba(128,128,128,0.5)"
//     ),
//     bus_junction: getVSCodeColor(
//       "--vscode-editorLineNumber-dimmedForeground",
//       "#008000"
//     ),
//   };
// }

// Component to render net reference labels using KiCanvas
const NetReferenceLabel = React.memo(function NetReferenceLabel({
  port,
  side,
  portX,
  portY,
  canvasRef,
  selected,
}: {
  port: any;
  side: string;
  portX: number;
  portY: number;
  canvasRef: (canvas: HTMLCanvasElement | null) => void;
  selected?: boolean;
}) {
  const internalCanvasRef = useRef<HTMLCanvasElement>(null);
  const [dimensions, setDimensions] = useState({ width: 100, height: 60 });

  // Get the label text
  const labelText =
    port.labels.find(
      (label: any) => label.properties?.labelType === "netReference"
    )?.text || "";

  // Convert side to direction - the arrow should point toward the symbol
  const getDirection = useCallback((): LabelDirection => {
    switch (side) {
      case "WEST":
        return "left"; // Arrow points left (toward symbol on the right)
      case "EAST":
        return "right"; // Arrow points right (toward symbol on the left)
      case "NORTH":
        return "up"; // Arrow points up (toward symbol below)
      case "SOUTH":
        return "down"; // Arrow points down (toward symbol above)
      default:
        return "left";
    }
  }, [side]);

  useEffect(() => {
    const renderLabel = async () => {
      if (!internalCanvasRef.current || !labelText) return;

      try {
        const PADDING_MM = 0.15;

        const info = await renderGlobalLabel(
          internalCanvasRef.current,
          labelText,
          {
            direction: getDirection(),
            shape: "input",
            scale: 10, // Scale up for visibility in the schematic
            padding: PADDING_MM,
            fontSize: 1.27, // Default KiCad font size
            theme: selected ? SELECTED_THEME : DEFAULT_THEME,
          }
        );

        // Update dimensions if they changed
        if (
          info.width !== dimensions.width ||
          info.height !== dimensions.height
        ) {
          setDimensions({ width: info.width, height: info.height });
        }
      } catch (error) {
        console.error("Error rendering net reference label:", error);
      }
    };

    renderLabel();
  }, [labelText, side, dimensions, getDirection, selected]);

  // Calculate position offset based on side
  const getPositionStyle = () => {
    const baseStyle = {
      position: "absolute" as const,
      pointerEvents: "none" as const,
      zIndex: 100,
    };

    switch (side) {
      case "WEST":
        return {
          ...baseStyle,
          left: portX - dimensions.width,
          top: portY - dimensions.height / 2,
        };
      case "EAST":
        return {
          ...baseStyle,
          left: portX,
          top: portY - dimensions.height / 2,
        };
      case "NORTH":
        return {
          ...baseStyle,
          left: portX - dimensions.width / 2,
          top: portY - dimensions.height,
        };
      case "SOUTH":
        return {
          ...baseStyle,
          left: portX - dimensions.width / 2,
          top: portY,
        };
      default:
        return baseStyle;
    }
  };

  return (
    <div className="port-net-reference" style={getPositionStyle()}>
      <canvas
        ref={(canvas) => {
          if (internalCanvasRef.current !== canvas) {
            (internalCanvasRef as any).current = canvas;
          }
          canvasRef(canvas);
        }}
        width={dimensions.width}
        height={dimensions.height}
        style={{
          width: `${dimensions.width}px`,
          height: `${dimensions.height}px`,
          backgroundColor: "rgba(0, 0, 0, 0)",
        }}
      />
    </div>
  );
});

// Define a node for KiCad symbols
const SymbolNode = React.memo(function SymbolNode({
  data,
  selected,
}: {
  data: SchematicNodeData;
  selected?: boolean;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const labelCanvasRefs = useRef<Map<string, HTMLCanvasElement>>(new Map());
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

  // Get rotation from data
  const rotation = data.rotation || 0;

  // Get netlist from node data
  const netlist = (data as any).netlist as Netlist;

  useEffect(() => {
    const renderSymbol = async () => {
      if (!canvasRef.current) {
        setRenderError("Canvas not available");
        setIsRendering(false);
        return;
      }

      try {
        setIsRendering(true);
        const canvas = canvasRef.current;

        // Get the symbol content from the __symbol_value attribute
        const instance = netlist.instances[data.id];
        const symbolValueAttr = instance?.attributes?.__symbol_value;

        // Extract the string value from AttributeValue
        let symbolContent: string | undefined;
        if (typeof symbolValueAttr === "string") {
          symbolContent = symbolValueAttr;
        } else if (
          symbolValueAttr &&
          typeof symbolValueAttr === "object" &&
          "String" in symbolValueAttr
        ) {
          symbolContent = symbolValueAttr.String;
        }

        if (!symbolContent) {
          setRenderError(
            "Symbol content not found in __symbol_value attribute"
          );
          setIsRendering(false);
          return;
        }

        // First, get the symbol info to know its natural size
        // We don't need a symbol name anymore since the content is self-contained
        const symbolInfo = getKicadSymbolInfo(symbolContent, undefined, {
          unit: 1,
          bodyStyle: 1,
          tightBounds: false,
        });

        // The node dimensions are already calculated by renderer.ts
        const nodeWidth = data.width || 100;
        const nodeHeight = data.height || 100;

        // Set canvas size to match node size exactly
        // The KiCad renderer handles device pixel ratio internally
        canvas.width = nodeWidth;
        canvas.height = nodeHeight;

        // The renderer.ts uses a scale factor of 10 to convert from symbol units to schematic units
        // So if the node is sized as symbolInfo.bbox.w * 10, we need to render at a scale
        // that makes the symbol fill the canvas

        // Calculate the scale needed to fit the symbol in the canvas
        // Use zero padding for exact fit
        const symbolPadding = 0; // Zero padding

        // The symbol's natural size (no padding needed since we want exact fit)
        const symbolWidthWithPadding = symbolInfo.bbox.w;
        const symbolHeightWithPadding = symbolInfo.bbox.h;

        // Calculate scale to fit the symbol in the logical node size
        const scaleX = nodeWidth / symbolWidthWithPadding;
        const scaleY = nodeHeight / symbolHeightWithPadding;
        const scale = Math.min(scaleX, scaleY);

        // Use selected theme if the node is selected
        const theme = selected ? SELECTED_THEME : DEFAULT_THEME;

        // Render the symbol at the calculated scale
        await renderKicadSymbol(canvas, symbolContent, undefined, {
          scale: scale,
          padding: symbolPadding, // Zero padding
          showPinNames: false,
          showPinNumbers: false,
          tightBounds: false, // Include pins to match renderer.ts
          theme: theme,
        });

        // No context state to restore since we're not manually scaling

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
  }, [data.width, data.height, data.id, netlist.instances, selected]);

  return (
    <div
      className="react-flow-symbol-node"
      style={{
        width: data.width,
        height: data.height,
        pointerEvents: "none",
        position: "relative",
        opacity: opacity,
        // Apply rotation transform
        transform: rotation !== 0 ? `rotate(${rotation}deg)` : undefined,
        transformOrigin: "center",
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
            <React.Fragment key={port.id}>
              <div
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
              </div>

              {/* Port label - rendered outside the port div */}
              {port.labels && port.labels[0] && (
                <>
                  {/* Check if this is a net reference label */}
                  {port.labels.some(
                    (label) => label.properties?.labelType === "netReference"
                  ) ? (
                    // Render net reference with global label style using canvas
                    <NetReferenceLabel
                      port={port}
                      side={side || "WEST"}
                      portX={portX}
                      portY={portY}
                      selected={selected}
                      canvasRef={(canvas) => {
                        if (canvas) {
                          labelCanvasRefs.current.set(port.id, canvas);
                        }
                      }}
                    />
                  ) : (
                    // Regular port label (non-net reference)
                    <div
                      className="port-label"
                      style={{
                        position: "absolute",
                        left: portX,
                        top: portY,
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
                </>
              )}
            </React.Fragment>
          );
        })}
      </div>
    </div>
  );
});

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

  // Get junction points from edge data
  const junctionPoints = data?.junctionPoints || [];

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

      {/* Render junction points as small dots */}
      {junctionPoints.map((point, index) => (
        <circle
          key={`${id}-junction-${index}`}
          cx={point.x}
          cy={point.y}
          r={3}
          fill={edgeColor}
          opacity={opacity}
          className="electrical-edge-junction"
        />
      ))}
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
  showSettings?: boolean;
  showDownloadButton?: boolean;
  // Persistence callbacks
  onPositionsChange?: (componentId: string, positions: NodePositions) => void;
  loadPositions?: (componentId: string) => Promise<NodePositions | null>;
}

const Visualizer = ({
  netlist,
  onComponentSelect = () => {},
  selectedComponent = null,
  config = DEFAULT_CONFIG,
  showSettings = false,
  showDownloadButton = false,
  onPositionsChange,
  loadPositions,
}: {
  netlist: Netlist;
  onComponentSelect?: (componentId: string | null) => void;
  selectedComponent?: string | null;
  config?: Partial<SchematicConfig>;
  showSettings?: boolean;
  showDownloadButton?: boolean;
  onPositionsChange?: (componentId: string, positions: NodePositions) => void;
  loadPositions?: (componentId: string) => Promise<NodePositions | null>;
}) => {
  const [nodes, setNodes, onNodesChange] = useNodesState<SchematicNode>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<SchematicEdge>([]);
  const [layoutError, setLayoutError] = useState<string | null>(null);
  const [nodePositions, setNodePositions] = useState<NodePositions>({});
  const [selectionState, setSelectionState] = useState<SelectionState>({
    selectedNetId: null,
    hoveredNetId: null,
  });
  const [prevComponent, setPrevComponent] = useState<string | null>(null);
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

  // Debounced callback for position changes
  const debouncedOnPositionsChange = useMemo(
    () =>
      debounce((componentId: string, positions: NodePositions) => {
        console.log(
          `[ReactFlow] Debounced position change fired for component: ${componentId}`
        );
        console.log(
          `[ReactFlow] Sending ${
            Object.keys(positions).length
          } positions to parent`
        );
        if (onPositionsChange) {
          onPositionsChange(componentId, positions);
        }
      }, 500), // 500ms debounce for persistence
    [onPositionsChange]
  );

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

  // Function to update both nodes and edges after drag
  // This is separate from the main layout effect to avoid triggering fitView
  const updateLayoutAfterDrag = useCallback(
    async (updatedPositions: NodePositions) => {
      if (!selectedComponent) return;

      console.log("Updating layout after drag: ", updatedPositions);

      try {
        const renderer = new SchematicLayoutEngine(netlist, currentConfig);

        // Use the unified layout method with updated positions
        const layoutResult = await renderer.layout(
          selectedComponent,
          updatedPositions
        );

        // Get current nodes to preserve selection state
        const currentNodes = reactFlowInstance.current?.getNodes() || [];
        const selectedNodeIds = new Set(
          currentNodes.filter((n: any) => n.selected).map((n: any) => n.id)
        );

        // Update both nodes and edges from the layout result
        const newNodes = (layoutResult.children || []).map((elkNode) => {
          const node = createSchematicNode(elkNode, selectionState, netlist);
          // Preserve selection state
          node.selected = selectedNodeIds.has(node.id);
          return node;
        });

        setNodes(newNodes);

        // Update edges as well
        const newEdges = layoutResult.edges.map((elkEdge) =>
          createSchematicEdge(elkEdge, selectionState)
        );

        setEdges(newEdges);

        // Update the stored node positions
        setNodePositions(layoutResult.nodePositions);

        // Notify about position changes
        if (selectedComponent && onPositionsChange) {
          debouncedOnPositionsChange(
            selectedComponent,
            layoutResult.nodePositions
          );
        }
      } catch (error) {
        console.error("Error updating nodes and edges:", error);
      }
    },
    [
      netlist,
      selectedComponent,
      currentConfig,
      selectionState,
      setNodes,
      setEdges,
      debouncedOnPositionsChange,
      onPositionsChange,
    ]
  );

  // Cleanup debounced functions on unmount
  useEffect(() => {
    return () => {
      debouncedSetSelectedNet.cancel();
      debouncedSetHoveredNet.cancel();
      debouncedOnPositionsChange.cancel();
    };
  }, [
    debouncedSetSelectedNet,
    debouncedSetHoveredNet,
    debouncedOnPositionsChange,
  ]);

  // Initialize ELK engine
  useEffect(() => {
    elkInstance.current = new ELK();
  }, []);

  // Main layout effect - runs when component changes or is first loaded
  // This effect handles fitView to center the schematic
  useEffect(() => {
    async function render() {
      console.log("Running main render effect: ", selectedComponent);
      if (selectedComponent) {
        try {
          // Determine if we should animate based on whether the component changed
          const isNewComponent = prevComponent !== selectedComponent;

          // If switching to a new component, load saved positions or clear
          let currentNodePositions = nodePositions;
          if (isNewComponent && prevComponent !== null) {
            // Try to load saved positions for this component
            if (loadPositions) {
              console.log(
                `[ReactFlow] Loading saved positions for component: ${selectedComponent}`
              );
              const savedPos = await loadPositions(selectedComponent);
              if (savedPos) {
                console.log(
                  `[ReactFlow] Found ${
                    Object.keys(savedPos).length
                  } saved positions`
                );
                currentNodePositions = savedPos;
                setNodePositions(currentNodePositions);
              } else {
                console.log(
                  `[ReactFlow] No saved positions found, clearing positions`
                );
                // Clear positions when switching components without saved positions
                setNodePositions({});
                currentNodePositions = {};
              }
            } else {
              console.log(
                `[ReactFlow] No loadPositions function provided, clearing positions`
              );
              // Clear positions when no load function provided
              setNodePositions({});
              currentNodePositions = {};
            }
          } else if (isNewComponent && loadPositions) {
            // First time loading, check for saved positions
            console.log(
              `[ReactFlow] First time loading, checking for saved positions`
            );
            const savedPos = await loadPositions(selectedComponent);
            if (savedPos) {
              console.log(
                `[ReactFlow] Found ${
                  Object.keys(savedPos).length
                } saved positions for initial load`
              );
              currentNodePositions = savedPos;
              setNodePositions(currentNodePositions);
            }
          }

          const renderer = new SchematicLayoutEngine(netlist, currentConfig);

          const layoutResult = await renderer.layout(
            selectedComponent,
            currentNodePositions
          );

          setPrevComponent(selectedComponent);

          // Update node positions with the layout result
          setNodePositions(layoutResult.nodePositions);

          const nodes = (layoutResult.children || []).map((elkNode) =>
            createSchematicNode(elkNode, selectionState, netlist)
          );
          const edges = layoutResult.edges.map((elkEdge) =>
            createSchematicEdge(elkEdge, selectionState)
          );

          setNodes(nodes);
          setEdges(edges);

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
    // TODO: fix
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    netlist,
    selectedComponent,
    prevComponent,
    currentConfig,
    selectionState,
    setEdges,
    setNodes,
    loadPositions,
  ]);

  // Handle node click to select a component - only if the component is clickable (modules)
  const handleNodeClick = useCallback(
    (event: React.MouseEvent, node: Node) => {
      // Don't prevent default - let React Flow handle selection

      // Check if the node is a module (which should be clickable for navigation)
      const nodeData = node.data as SchematicNodeData;
      if (nodeData.componentType === NodeType.MODULE) {
        // Only navigate if it's a double-click or some other interaction
        // Single click should just select the node
        if (event.detail === 2) {
          // Double click
          onComponentSelect(node.id);
        }
      }

      // For all nodes, ensure selection is handled properly
      if (reactFlowInstance.current) {
        const currentNodes = reactFlowInstance.current.getNodes();
        const isMultiSelect = event.shiftKey || event.metaKey || event.ctrlKey;

        const updatedNodes = currentNodes.map((n: any) => ({
          ...n,
          selected: isMultiSelect
            ? n.id === node.id
              ? !n.selected
              : n.selected
            : n.id === node.id,
        }));

        reactFlowInstance.current.setNodes(updatedNodes);
      }
    },
    [onComponentSelect]
  );

  // Track dragging state
  const [isDragging, setIsDragging] = useState(false);
  const [pendingPositions, setPendingPositions] = useState<NodePositions>({});

  // Create a debounced version of updateLayoutAfterDrag for real-time updates
  const debouncedUpdateLayout = useMemo(
    () =>
      debounce((positions: NodePositions) => {
        updateLayoutAfterDrag(positions);
      }, 50), // 50ms debounce for smooth real-time updates
    [updateLayoutAfterDrag]
  );

  // Cleanup debounced layout update on unmount
  useEffect(() => {
    return () => {
      debouncedUpdateLayout.cancel();
    };
  }, [debouncedUpdateLayout]);

  // Add keyboard event handler for rotation
  useEffect(() => {
    const handleKeyPress = (event: KeyboardEvent) => {
      // Check if 'R' key is pressed (case insensitive)
      if (event.key.toLowerCase() === "r") {
        console.log("[Rotation] R key pressed");

        // Get the currently selected nodes from React Flow
        const selectedNodes = reactFlowInstance.current
          ?.getNodes()
          .filter((node: any) => node.selected);

        console.log("[Rotation] Selected nodes:", selectedNodes);

        if (selectedNodes && selectedNodes.length > 0) {
          // Update rotation for each selected node
          const updatedPositions = { ...nodePositions };
          let hasChanges = false;

          selectedNodes.forEach((node: any) => {
            const currentPosition = nodePositions[node.id];
            console.log(
              `[Rotation] Current position for ${node.id}:`,
              currentPosition
            );

            if (currentPosition) {
              // Rotate by 90 degrees clockwise
              const currentRotation = currentPosition.rotation || 0;
              const newRotation = (currentRotation + 90) % 360;

              console.log(
                `[Rotation] Rotating ${node.id} from ${currentRotation} to ${newRotation} degrees`
              );

              updatedPositions[node.id] = {
                ...currentPosition,
                rotation: newRotation,
              };
              hasChanges = true;
            } else {
              console.log(
                `[Rotation] No position found for ${node.id}, creating new position`
              );
              // If no position exists yet, create one with just rotation
              updatedPositions[node.id] = {
                x: node.position.x,
                y: node.position.y,
                rotation: 90,
              };
              hasChanges = true;
            }
          });

          if (hasChanges) {
            console.log("[Rotation] Updated positions:", updatedPositions);
            // Update positions and trigger layout update
            setNodePositions(updatedPositions);
            updateLayoutAfterDrag(updatedPositions);

            // Preserve selection after rotation by re-selecting the nodes
            setTimeout(() => {
              if (reactFlowInstance.current) {
                // Get the current nodes
                const currentNodes = reactFlowInstance.current.getNodes();

                // Update the selected nodes to maintain selection
                const updatedNodes = currentNodes.map((node: any) => {
                  const wasSelected = selectedNodes.some(
                    (selected: any) => selected.id === node.id
                  );
                  return {
                    ...node,
                    selected: wasSelected,
                  };
                });

                reactFlowInstance.current.setNodes(updatedNodes);
              }
            }, 100); // Small delay to ensure layout update has completed
          }
        } else {
          console.log("[Rotation] No nodes selected");
        }
      }
    };

    // Add event listener
    window.addEventListener("keydown", handleKeyPress);

    // Cleanup
    return () => {
      window.removeEventListener("keydown", handleKeyPress);
    };
  }, [
    nodePositions,
    updateLayoutAfterDrag,
    selectedComponent,
    onPositionsChange,
    debouncedOnPositionsChange,
  ]);

  // Custom handler for node changes to capture position updates
  const handleNodesChange = useCallback(
    (changes: any[]) => {
      // Apply snapping to position changes before they're applied
      if (currentConfig.layout.gridSnap.enabled) {
        const gridSize = currentConfig.layout.gridSnap.size;

        changes = changes.map((change) => {
          if (change.type === "position" && change.position) {
            // Snap the position immediately
            const snapped = snapPosition(
              change.position.x,
              change.position.y,
              gridSize
            );
            return {
              ...change,
              position: snapped,
            };
          }
          return change;
        });
      }

      // First, apply the changes using the default handler
      onNodesChange(changes);

      // Check if any position changes occurred
      const positionChanges = changes.filter(
        (change) => change.type === "position" && change.position
      );

      if (positionChanges.length > 0) {
        // Update node positions based on the changes
        const updatedPositions = { ...nodePositions };
        let hasActualChanges = false;

        positionChanges.forEach((change) => {
          if (change.position) {
            const currentPos = nodePositions[change.id];
            // Only update if position actually changed
            if (
              !currentPos ||
              Math.abs(currentPos.x - change.position.x) > 0.01 ||
              Math.abs(currentPos.y - change.position.y) > 0.01
            ) {
              hasActualChanges = true;
              updatedPositions[change.id] = {
                x: change.position.x,
                y: change.position.y,
                // Preserve existing width/height/rotation if they exist
                ...(currentPos && {
                  width: currentPos.width,
                  height: currentPos.height,
                  rotation: currentPos.rotation,
                }),
              };
            }
          }
        });

        // Only update if there were actual changes
        if (hasActualChanges) {
          // Update positions immediately
          setNodePositions(updatedPositions);
          setPendingPositions(updatedPositions);

          // Notify about position changes
          if (selectedComponent && onPositionsChange) {
            console.log(
              `[ReactFlow] Notifying position changes for component: ${selectedComponent}`
            );
            console.log(`[ReactFlow] Updated positions:`, updatedPositions);
            debouncedOnPositionsChange(selectedComponent, updatedPositions);
          }

          // If we're dragging, update layout with debouncing
          if (isDragging) {
            debouncedUpdateLayout(updatedPositions);
          } else {
            // Not dragging, update positions immediately (e.g., programmatic moves)
            updateLayoutAfterDrag(updatedPositions);
          }
        }
      }
    },
    [
      onNodesChange,
      nodePositions,
      updateLayoutAfterDrag,
      isDragging,
      debouncedUpdateLayout,
      currentConfig,
      selectedComponent,
      onPositionsChange,
      debouncedOnPositionsChange,
    ]
  );

  // Handle node drag start
  const handleNodeDragStart = useCallback(
    (event: React.MouseEvent, node: Node) => {
      setIsDragging(true);

      // Select the node when dragging starts
      if (reactFlowInstance.current) {
        const currentNodes = reactFlowInstance.current.getNodes();

        // Update nodes to select only the dragged node (unless shift/cmd is held)
        const isMultiSelect = event.shiftKey || event.metaKey || event.ctrlKey;

        const updatedNodes = currentNodes.map((n: any) => ({
          ...n,
          selected: isMultiSelect
            ? n.id === node.id
              ? true
              : n.selected
            : n.id === node.id,
        }));

        reactFlowInstance.current.setNodes(updatedNodes);
      }
    },
    []
  );

  // Handle node drag stop - finalize the drag operation
  const handleNodeDragStop = useCallback(() => {
    setIsDragging(false);

    // Cancel any pending debounced updates
    debouncedUpdateLayout.cancel();

    // If we have pending positions from the drag, do a final update
    if (Object.keys(pendingPositions).length > 0) {
      // Do a final update to ensure we have the exact final positions
      updateLayoutAfterDrag(pendingPositions);
      setPendingPositions({});
    }
  }, [pendingPositions, updateLayoutAfterDrag, debouncedUpdateLayout]);

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
      {layoutError && (
        <div
          className="error-message"
          style={{
            color: DEFAULT_THEME.erc_error.to_css(),
            backgroundColor: backgroundColor,
            border: `1px solid ${DEFAULT_THEME.erc_error.to_css()}`,
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
          backgroundColor: backgroundColor,
          color: labelColor,
          height: "100%",
          width: "100%",
          outline: "none",
        }}
      >
        <ReactFlow
          proOptions={{ hideAttribution: true }}
          nodes={nodes}
          edges={edges}
          onNodesChange={handleNodesChange}
          onEdgesChange={onEdgesChange}
          nodeTypes={nodeTypes}
          edgeTypes={edgeTypes}
          fitView
          onInit={(instance) => {
            reactFlowInstance.current = instance;
          }}
          onNodeClick={handleNodeClick}
          onNodeDragStart={handleNodeDragStart}
          onNodeDragStop={handleNodeDragStop}
          onPaneClick={() => {
            // Clear selection when clicking on background
            if (reactFlowInstance.current) {
              const currentNodes = reactFlowInstance.current.getNodes();
              const updatedNodes = currentNodes.map((n: any) => ({
                ...n,
                selected: false,
              }));
              reactFlowInstance.current.setNodes(updatedNodes);
            }
          }}
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
            backgroundColor: backgroundColor,
          }}
          nodesDraggable={true}
          nodesConnectable={false}
          elementsSelectable={true}
          selectNodesOnDrag={true}
          zoomOnScroll={true}
          panOnScroll={true}
          panOnDrag={true}
          preventScrolling={false}
          minZoom={0.1}
          maxZoom={1.5}
        >
          <Background
            variant={BackgroundVariant.Dots}
            gap={currentConfig.layout.gridSnap.size}
            size={1}
            color={electricalComponentColor}
            style={{ opacity: 0.25 }}
          />
          <Controls showInteractive={false} />
          {(showSettings || showDownloadButton) && (
            <Panel position="top-right">
              <div
                style={{ display: "flex", gap: "8px", alignItems: "center" }}
              >
                {showSettings && (
                  <button
                    className="debug-toggle-button"
                    onClick={() => setShowDebugPane(!showDebugPane)}
                    title="Toggle debug options"
                  >
                    <Settings size={16} />
                  </button>
                )}
                {/* {showDownloadButton && (
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
                )} */}
              </div>
            </Panel>
          )}

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
                      color: labelColor,
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
                </div>

                <div className="debug-pane-section">
                  <h4>Grid Snapping</h4>

                  <div className="debug-pane-control">
                    <label htmlFor="gridSnapEnabled">Enable Grid Snap:</label>
                    <input
                      id="gridSnapEnabled"
                      type="checkbox"
                      checked={currentConfig.layout.gridSnap.enabled}
                      onChange={(e) =>
                        updateConfig({
                          layout: {
                            ...currentConfig.layout,
                            gridSnap: {
                              ...currentConfig.layout.gridSnap,
                              enabled: e.target.checked,
                            },
                          },
                        })
                      }
                    />
                  </div>

                  <div className="debug-pane-control">
                    <label htmlFor="gridSize">Grid Size:</label>
                    <input
                      id="gridSize"
                      type="range"
                      min="5"
                      max="50"
                      step="0.1"
                      value={currentConfig.layout.gridSnap.size}
                      onChange={(e) =>
                        updateConfig({
                          layout: {
                            ...currentConfig.layout,
                            gridSnap: {
                              ...currentConfig.layout.gridSnap,
                              size: Number(e.target.value),
                            },
                          },
                        })
                      }
                    />
                    <span className="value-display">
                      {currentConfig.layout.gridSnap.size.toFixed(1)}
                    </span>
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
  showSettings = false,
  showDownloadButton = false,
  onPositionsChange,
  loadPositions,
}: ReactFlowSchematicViewerProps) => {
  return (
    <ReactFlowProvider>
      <Visualizer
        netlist={netlist}
        onComponentSelect={onComponentSelect}
        selectedComponent={selectedComponent}
        config={config}
        showSettings={showSettings}
        showDownloadButton={showDownloadButton}
        onPositionsChange={onPositionsChange}
        loadPositions={loadPositions}
      />
    </ReactFlowProvider>
  );
};

export type { SchematicConfig };
export { DEFAULT_CONFIG };
export default ReactFlowSchematicViewer;
