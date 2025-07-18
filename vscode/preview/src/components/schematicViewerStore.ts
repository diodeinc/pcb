import { create } from "zustand";
import { devtools } from "zustand/middleware";
import type { NodeChange, EdgeChange } from "@xyflow/react";
import { applyNodeChanges, applyEdgeChanges } from "@xyflow/react";
import type { SchematicNode, SchematicEdge } from "./ReactFlowSchematicViewer";
import {
  createSchematicNode,
  createSchematicEdge,
} from "./ReactFlowSchematicViewer";
import type { NodePositions, SchematicConfig } from "../LayoutEngine";
import { SchematicLayoutEngine } from "../LayoutEngine";
import { debounce, isEqual } from "lodash";
import type { Netlist } from "../types/NetlistTypes";

// Custom logger middleware
const logger = (config: any) => (set: any, get: any, api: any) =>
  config(
    (...args: any[]) => {
      const prevState = get();
      set(...args);
      const nextState = get();

      // Log the action
      console.group(
        `[SchematicViewerStore] State Update @ ${new Date().toLocaleTimeString()}`
      );
      console.log("Previous State:", prevState);
      console.log("Next State:", nextState);

      // Log what changed
      const changes: Record<string, any> = {};
      Object.keys(nextState).forEach((key) => {
        if (prevState[key] !== nextState[key]) {
          changes[key] = {
            from: prevState[key],
            to: nextState[key],
          };
        }
      });

      if (Object.keys(changes).length > 0) {
        console.log("Changes:", changes);
      }

      console.groupEnd();
    },
    get,
    api
  );

interface SchematicViewerState {
  // Node and Edge state
  nodes: SchematicNode[];
  edges: SchematicEdge[];

  // Node positions state
  nodePositions: NodePositions;
  positionsLoaded: boolean;

  // Component context
  selectedComponent: string | null;
  netlist: Netlist | null;
  config: SchematicConfig | null;
  onPositionsChange?: (componentId: string, positions: NodePositions) => void;
  loadPositions?: (componentId: string) => Promise<NodePositions | null>;

  // Legacy setters (to be removed gradually)
  setNodes: (nodes: SchematicNode[]) => void;
  setEdges: (edges: SchematicEdge[]) => void;
  setNodePositions: (positions: NodePositions) => void;

  // Context setters
  setSelectedComponent: (component: string | null) => void;
  setNetlist: (netlist: Netlist) => void;
  setConfig: (config: SchematicConfig) => void;
  setOnPositionsChange: (
    callback?: (componentId: string, positions: NodePositions) => void
  ) => void;
  setLoadPositions: (
    callback?: (componentId: string) => Promise<NodePositions | null>
  ) => void;

  // Semantic actions
  storeLayoutResult: (
    layoutResult: {
      children?: any[];
      edges: any[];
      nodePositions: NodePositions;
    },
    netlist: any
  ) => void;

  rotateNodes: (nodeIds: string[]) => void;

  // Selection actions
  handleNodeClick: (nodeId: string, isMultiSelect: boolean) => void;
  clearSelection: () => void;
  getSelectedNodeIds: () => Set<string>;

  loadSavedPositions: (positions: NodePositions) => void;

  clearComponentData: () => void;

  // Change handlers
  onNodesChange: (
    changes: NodeChange[],
    gridSnapEnabled?: boolean,
    gridSize?: number
  ) => { hasPositionChanges: boolean; updatedPositions: NodePositions };
  onEdgesChange: (changes: EdgeChange[]) => void;
}

// Utility function for grid snapping
function snapToGrid(value: number, gridSize: number): number {
  return Math.round(value / gridSize) * gridSize;
}

// Helper function to load positions and trigger layout
async function loadPositionsAndLayout(
  selectedComponent: string | null,
  netlist: Netlist | null,
  config: SchematicConfig | null,
  loadPositions:
    | ((componentId: string) => Promise<NodePositions | null>)
    | undefined,
  storeLayoutResult: (layoutResult: any, netlist: Netlist) => void,
  set: (state: Partial<SchematicViewerState>) => void
) {
  if (!selectedComponent || !netlist || !config) return;

  // Reset positionsLoaded flag
  set({ positionsLoaded: false });

  try {
    let positions: NodePositions = {};

    // Try to load saved positions
    if (loadPositions) {
      console.log(
        `[Store] Loading positions for component: ${selectedComponent}`
      );
      const savedPositions = await loadPositions(selectedComponent);
      if (savedPositions) {
        console.log(
          `[Store] Found ${Object.keys(savedPositions).length} saved positions`
        );
        positions = savedPositions;
        set({ nodePositions: savedPositions });
      } else {
        console.log(`[Store] No saved positions found`);
        set({ nodePositions: {} });
      }
    } else {
      set({ nodePositions: {} });
    }

    // Mark positions as loaded
    set({ positionsLoaded: true });

    // Trigger layout with loaded positions
    const renderer = new SchematicLayoutEngine(netlist, config);
    const layoutResult = await renderer.layout(selectedComponent, positions);
    storeLayoutResult(layoutResult, netlist);
  } catch (error) {
    console.error("Error loading positions and layout:", error);
    set({ positionsLoaded: true }); // Mark as loaded even on error
  }
}

// Create a debounced layout update function
const debouncedLayoutUpdate = debounce(
  async (
    selectedComponent: string | null,
    netlist: Netlist,
    config: SchematicConfig,
    updatedPositions: NodePositions,
    storeLayoutResult: (layoutResult: any, netlist: Netlist) => void,
    onPositionsChange?: (componentId: string, positions: NodePositions) => void
  ) => {
    if (!selectedComponent) return;

    try {
      const renderer = new SchematicLayoutEngine(netlist, config);
      console.log(
        "Running debounced layout update with positions: ",
        updatedPositions
      );

      const layoutResult = await renderer.layout(
        selectedComponent,
        updatedPositions
      );

      // Store the layout result (which preserves selection)
      storeLayoutResult(layoutResult, netlist);

      // Notify about position changes if callback provided
      if (onPositionsChange) {
        onPositionsChange(selectedComponent, layoutResult.nodePositions);
      }
    } catch (error) {
      console.error("Error in debounced layout update:", error);
    }
  },
  50,
  {
    maxWait: 50,
    trailing: true,
  }
);

export const useSchematicViewerStore = create(
  logger(
    devtools<SchematicViewerState>(
      (set, get) => ({
        // Initial state
        nodes: [],
        edges: [],
        nodePositions: {},
        positionsLoaded: false,
        selectedComponent: null,
        netlist: null,
        config: null,
        onPositionsChange: undefined,
        loadPositions: undefined,

        // Legacy setters
        setNodes: (nodes) => set({ nodes }),
        setEdges: (edges) => set({ edges }),
        setNodePositions: (positions) => set({ nodePositions: positions }),

        // Context setters
        setSelectedComponent: (component) => {
          const state = get();
          const prevComponent = state.selectedComponent;

          set({ selectedComponent: component });

          // Trigger position loading and layout when component changes
          if (component && component !== prevComponent) {
            loadPositionsAndLayout(
              component,
              state.netlist,
              state.config,
              state.loadPositions,
              get().storeLayoutResult,
              set
            );
          }
        },
        setNetlist: (netlist) => {
          const state = get();
          const prevNetlist = state.netlist;

          // Perform deep comparison to check if netlist actually changed
          if (isEqual(netlist, prevNetlist)) {
            console.log("[Store] Netlist unchanged, skipping layout update");
            return;
          }

          set({ netlist });

          // Trigger position loading and layout if netlist changed and we have all required data
          if (netlist && state.selectedComponent) {
            console.log("[Store] Netlist changed, triggering layout update");
            loadPositionsAndLayout(
              state.selectedComponent,
              netlist,
              state.config,
              state.loadPositions,
              get().storeLayoutResult,
              set
            );
          }
        },

        setConfig: (config) => {
          const state = get();
          const prevConfig = state.config;

          // Perform deep comparison to check if config actually changed
          if (isEqual(config, prevConfig)) {
            console.log("[Store] Config unchanged, skipping layout update");
            return;
          }

          set({ config });

          // Trigger position loading and layout if config changed and we have all required data
          if (config && state.selectedComponent && state.netlist) {
            console.log("[Store] Config changed, triggering layout update");
            loadPositionsAndLayout(
              state.selectedComponent,
              state.netlist,
              config,
              state.loadPositions,
              get().storeLayoutResult,
              set
            );
          }
        },
        setOnPositionsChange: (callback) =>
          set({ onPositionsChange: callback }),
        setLoadPositions: (callback) => set({ loadPositions: callback }),

        // Semantic actions
        storeLayoutResult: (layoutResult, netlist) => {
          const { children = [], edges, nodePositions } = layoutResult;
          const state = get();

          // Preserve selection state from current nodes
          const selectedNodeIds = new Set(
            state.nodes.filter((node) => node.selected).map((node) => node.id)
          );

          // Create new nodes with preserved selection state
          const nodes = children.map((elkNode: any) => {
            const node = createSchematicNode(elkNode, netlist);
            node.selected = selectedNodeIds.has(node.id);
            return node;
          });

          const schematicEdges = edges.map((elkEdge: any) =>
            createSchematicEdge(elkEdge)
          );

          set({
            nodes,
            edges: schematicEdges,
            nodePositions,
          });
        },

        rotateNodes: (nodeIds) => {
          const state = get();
          const updatedPositions = { ...state.nodePositions };

          nodeIds.forEach((nodeId) => {
            const currentPosition = state.nodePositions[nodeId];

            if (currentPosition) {
              const currentRotation = currentPosition.rotation || 0;
              const newRotation = (currentRotation + 90) % 360;

              updatedPositions[nodeId] = {
                ...currentPosition,
                rotation: newRotation,
              };
            } else {
              // Create new position with rotation
              updatedPositions[nodeId] = {
                x: currentPosition.x,
                y: currentPosition.y,
                rotation: 90,
              };
            }
          });

          set({ nodePositions: updatedPositions });

          // Trigger layout update with the new positions
          if (state.selectedComponent && state.netlist && state.config) {
            debouncedLayoutUpdate(
              state.selectedComponent,
              state.netlist,
              state.config,
              updatedPositions,
              get().storeLayoutResult,
              state.onPositionsChange
            );
          }

          return updatedPositions;
        },

        handleNodeClick: (nodeId, isMultiSelect) => {
          const state = get();

          const updatedNodes = state.nodes.map((node) => ({
            ...node,
            selected: isMultiSelect
              ? node.id === nodeId
                ? !node.selected // Toggle selection for clicked node
                : node.selected // Keep existing selection for others
              : node.id === nodeId, // Single select: only this node
          }));

          set({ nodes: updatedNodes });
        },

        clearSelection: () => {
          const state = get();
          const updatedNodes = state.nodes.map((node) => ({
            ...node,
            selected: false,
          }));
          set({ nodes: updatedNodes });
        },

        getSelectedNodeIds: () => {
          const state = get();
          return new Set(
            state.nodes.filter((node) => node.selected).map((node) => node.id)
          );
        },

        loadSavedPositions: (positions) => {
          set({ nodePositions: positions });
        },

        clearComponentData: () => {
          set({
            nodes: [],
            edges: [],
            nodePositions: {},
            positionsLoaded: false,
          });
        },

        onNodesChange: (changes, gridSnapEnabled = true, gridSize = 12.7) => {
          const state = get();

          // Apply grid snapping to position changes if enabled
          let processedChanges = changes;
          if (gridSnapEnabled) {
            processedChanges = changes.map((change) => {
              if (change.type === "position" && change.position) {
                return {
                  ...change,
                  position: {
                    x: snapToGrid(change.position.x, gridSize),
                    y: snapToGrid(change.position.y, gridSize),
                  },
                };
              }
              return change;
            });
          }

          // Apply the changes to nodes
          set({
            nodes: applyNodeChanges(
              processedChanges,
              state.nodes
            ) as SchematicNode[],
          });

          // Check if any position changes occurred
          const positionChanges = processedChanges.filter(
            (change) => change.type === "position" && change.position
          );

          let hasPositionChanges = false;
          const updatedPositions = { ...state.nodePositions };

          if (positionChanges.length > 0) {
            positionChanges.forEach((change: any) => {
              if (change.type === "position" && change.position && change.id) {
                const currentPos = state.nodePositions[change.id];
                // Only update if position actually changed
                if (
                  !currentPos ||
                  Math.abs(currentPos.x - change.position.x) > 0.01 ||
                  Math.abs(currentPos.y - change.position.y) > 0.01
                ) {
                  hasPositionChanges = true;
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

            // Update node positions if there were changes
            if (hasPositionChanges) {
              set({ nodePositions: updatedPositions });

              // Trigger layout update if we have all the required parameters
              if (state.selectedComponent && state.netlist && state.config) {
                debouncedLayoutUpdate(
                  state.selectedComponent,
                  state.netlist,
                  state.config,
                  updatedPositions,
                  get().storeLayoutResult,
                  state.onPositionsChange
                );
              }
            }
          }

          return { hasPositionChanges, updatedPositions };
        },

        onEdgesChange: (changes) => {
          set({
            edges: applyEdgeChanges(changes, get().edges) as SchematicEdge[],
          });
        },
      }),
      {
        name: "schematic-viewer-store", // Name for the devtools
      }
    )
  )
);
