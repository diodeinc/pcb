import React, { useCallback, useEffect, useRef, useState } from "react";
import type { CSSProperties } from "react";
import {
  ReactFlow,
  Controls,
  Position,
  Handle,
  type Node,
  ReactFlowProvider,
  Panel,
  Background,
  BackgroundVariant,
} from "@xyflow/react";
import type { Edge, EdgeProps, EdgeTypes } from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import "./ReactFlowSchematicViewer.css";
import { NodeType, DEFAULT_CONFIG } from "../LayoutEngine";
import type {
  ElkEdge,
  ElkNode,
  SchematicConfig,
  NodePositions,
} from "../LayoutEngine";
import { useSchematicViewerStore } from "./schematicViewerStore";
import type { Netlist } from "../types/NetlistTypes";
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

type SchematicNodeData = ElkNode & {
  componentType: NodeType;
} & Record<string, unknown>;

type SchematicEdgeData = ElkEdge & Record<string, unknown>;

export type SchematicNode = Node<SchematicNodeData, NodeType>;
export type SchematicEdge = Edge<SchematicEdgeData>;

export function createSchematicNode(
  elkNode: ElkNode,
  netlist?: Netlist
): SchematicNode {
  // Note: positions should already be snapped by the layout engine
  return {
    id: elkNode.id,
    data: {
      componentType: elkNode.type,
      ...elkNode,
      ...(elkNode.type === NodeType.SYMBOL && netlist ? { netlist } : {}),
      // Ensure rotation is included in data
      rotation: elkNode.rotation || 0,
      // Include isNetSymbol flag if present
      isNetSymbol: elkNode.properties?.isNetSymbol === "true",
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

export function createSchematicEdge(elkEdge: ElkEdge): SchematicEdge {
  return {
    id: elkEdge.id,
    data: { ...elkEdge },
    source: elkEdge.sourceComponentRef,
    target: elkEdge.targetComponentRef,
    sourceHandle: `${elkEdge.sources[0]}-source`,
    targetHandle: `${elkEdge.targets[0]}-target`,
    type: "electrical",
  };
}

// Common color for electrical components
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
    opacity: 1,
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
                opacity: 1,
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
        opacity: 1,
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
        opacity: 1,
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

        let symbolContent: string | undefined;

        // Check if this is a net symbol
        if ((data as any).isNetSymbol && data.netId) {
          // For net symbols, get the symbol content from the net
          const net = netlist.nets[data.netId];
          const symbolValueAttr = net?.properties?.__symbol_value;

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
            setRenderError("Symbol content not found in net properties");
            setIsRendering(false);
            return;
          }
        } else {
          // For component symbols, get the symbol content from the instance
          const instance = netlist.instances[data.id];
          const symbolValueAttr = instance?.attributes?.__symbol_value;

          // Extract the string value from AttributeValue
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
  }, [data, netlist.instances, netlist.nets, selected]);

  return (
    <div
      className="react-flow-symbol-node"
      style={{
        width: data.width,
        height: data.height,
        pointerEvents: "none",
        position: "relative",
        opacity: 1,
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

  // Get junction points from edge data
  const junctionPoints = data?.junctionPoints || [];

  // Get edge labels
  const edgeLabels = data?.labels || [];

  return (
    <>
      <path
        id={id}
        style={{
          strokeWidth: 1.5,
          stroke: edgeColor,
          pointerEvents: "none",
          ...style,
          opacity: 1,
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
          opacity={1}
          className="electrical-edge-junction"
        />
      ))}

      {/* Render edge labels */}
      {edgeLabels.map((label, index) => (
        <text
          key={`${id}-label-${index}`}
          x={(label.x || 0) + (label.width || 0) / 2}
          y={(label.y || 0) + (label.height || 0) / 2}
          textAnchor="middle"
          dominantBaseline="middle"
          fontSize="10px"
          fill={labelColor}
          fontFamily="Newstroke, 'Courier New', monospace"
          className="electrical-edge-label"
        >
          {label.text}
        </text>
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
  // Use Zustand store for nodes and edges
  const {
    nodes,
    edges,
    onNodesChange,
    onEdgesChange,
    rotateNodes,
    handleNodeClick,
    clearSelection,
    initializeViewer,
    deleteNetSymbolNodes,
    createNetSymbolNode,
  } = useSchematicViewerStore();

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
  const reactFlowInstance = useRef<any>(null);

  // Use the new initializeViewer function to set all context at once
  useEffect(() => {
    initializeViewer({
      selectedComponent,
      netlist,
      config: currentConfig,
      onPositionsChange,
      loadPositions,
    });
  }, [
    selectedComponent,
    netlist,
    currentConfig,
    onPositionsChange,
    loadPositions,
    initializeViewer,
  ]);

  useEffect(() => {
    if (!selectedComponent) return;

    const isNewComponent = prevComponent !== selectedComponent;
    if (!isNewComponent) return;

    console.log(`[ReactFlow] Component changed to: ${selectedComponent}`);
    setPrevComponent(selectedComponent);

    // Center the view after component change
    // The layout and position loading is handled by the store
    setTimeout(() => {
      reactFlowInstance.current?.fitView({
        padding: 0.2,
        duration: 200,
      });
    }, 100);
  }, [selectedComponent, prevComponent]);

  // Add keyboard event handler for rotation
  useEffect(() => {
    const handleKeyPress = (event: KeyboardEvent) => {
      // Check if 'R' key is pressed (case insensitive)
      if (event.key.toLowerCase() === "r") {
        console.log("[Rotation] R key pressed");

        // Get the currently selected nodes from the store
        const selectedNodeIds = Array.from(
          useSchematicViewerStore.getState().getSelectedNodeIds()
        );

        console.log("[Rotation] Selected nodes:", selectedNodeIds);

        if (selectedNodeIds.length > 0) {
          console.log("[Rotation] Rotating nodes:", selectedNodeIds);
          rotateNodes(selectedNodeIds);
        } else {
          console.log("[Rotation] No nodes selected");
        }
      }

      // Check if Delete or Backspace key is pressed
      if (event.key === "Delete" || event.key === "Backspace") {
        console.log("[Delete] Delete/Backspace key pressed");

        // Prevent default behavior if we're not in an input field
        const target = event.target as HTMLElement;
        if (target.tagName !== "INPUT" && target.tagName !== "TEXTAREA") {
          event.preventDefault();

          // Get the currently selected nodes from the store
          const selectedNodeIds = Array.from(
            useSchematicViewerStore.getState().getSelectedNodeIds()
          );

          if (selectedNodeIds.length > 0) {
            console.log(
              "[Delete] Attempting to delete nodes:",
              selectedNodeIds
            );
            deleteNetSymbolNodes(selectedNodeIds);
          }
        }
      }
    };

    // Add event listener
    window.addEventListener("keydown", handleKeyPress);

    // Cleanup
    return () => {
      window.removeEventListener("keydown", handleKeyPress);
    };
  }, [rotateNodes, deleteNetSymbolNodes]);

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
          onNodesChange={onNodesChange}
          onEdgesChange={onEdgesChange}
          nodeTypes={nodeTypes}
          edgeTypes={edgeTypes}
          fitView
          onInit={(instance) => {
            reactFlowInstance.current = instance;
          }}
          onNodeClick={(event, node) => {
            handleNodeClick(
              node.id,
              event.shiftKey || event.metaKey || event.ctrlKey
            );
          }}
          onNodeDoubleClick={(event, node) => {
            if ((node.data as any).isNetSymbol) {
              createNetSymbolNode(node.id);
            }
          }}
          onPaneClick={() => {
            // Clear selection when clicking on background
            clearSelection();
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
