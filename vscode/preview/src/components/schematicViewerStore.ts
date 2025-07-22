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
import { SchematicLayoutEngine, snapPosition } from "../LayoutEngine";
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

  // KiCad schematic
  kicadSchematic: string | null;
  kicadSchematicFull: string | null;

  // Component context
  selectedComponent: string | null;
  netlist: Netlist | null;
  config: SchematicConfig | null;
  readonly: boolean; // Add readonly state
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
  setReadonly: (readonly: boolean) => void; // Add setter for readonly
  setOnPositionsChange: (
    callback?: (componentId: string, positions: NodePositions) => void
  ) => void;
  setLoadPositions: (
    callback?: (componentId: string) => Promise<NodePositions | null>
  ) => void;

  // New unified initialization
  initializeViewer: (params: {
    selectedComponent: string | null;
    netlist: Netlist;
    config: SchematicConfig;
    readonly?: boolean; // Add readonly to initialization params
    onPositionsChange?: (componentId: string, positions: NodePositions) => void;
    loadPositions?: (componentId: string) => Promise<NodePositions | null>;
  }) => void;

  // Semantic actions
  storeLayoutResult: (
    layoutResult: {
      children?: any[];
      edges: any[];
      nodePositions: NodePositions;
      kicadSchematic?: string;
      kicadSchematicFull?: string;
    },
    netlist: any
  ) => void;

  rotateNodes: (nodeIds: string[]) => void;

  // Selection actions
  handleNodeClick: (nodeId: string, isMultiSelect: boolean) => void;
  clearSelection: () => void;
  getSelectedNodeIds: () => Set<string>;

  // Net symbol actions
  createNetSymbolNode: (originalNodeId: string) => void;
  deleteNetSymbolNodes: (nodeIds: string[]) => void;

  loadSavedPositions: (positions: NodePositions) => void;

  clearComponentData: () => void;

  // Change handlers
  onNodesChange: (changes: NodeChange[]) => {
    hasPositionChanges: boolean;
    updatedPositions: NodePositions;
  };
  onEdgesChange: (changes: EdgeChange[]) => void;
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
        kicadSchematic: null,
        kicadSchematicFull: null,
        selectedComponent: null,
        netlist: null,
        config: null,
        readonly: false, // Initialize readonly
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
            return;
          }

          set({ netlist });

          // Trigger position loading and layout if netlist changed and we have all required data
          if (netlist && state.selectedComponent) {
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
            return;
          }

          set({ config });

          // Trigger position loading and layout if config changed and we have all required data
          if (config && state.selectedComponent && state.netlist) {
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
        setReadonly: (readonly) => set({ readonly }), // Add setReadonly
        setOnPositionsChange: (callback) =>
          set({ onPositionsChange: callback }),
        setLoadPositions: (callback) => set({ loadPositions: callback }),

        // New unified initialization
        initializeViewer: (params) => {
          const state = get();

          // Check if anything actually changed
          const hasChanges =
            params.selectedComponent !== state.selectedComponent ||
            !isEqual(params.netlist, state.netlist) ||
            !isEqual(params.config, state.config) ||
            params.loadPositions !== state.loadPositions ||
            params.onPositionsChange !== state.onPositionsChange ||
            params.readonly !== state.readonly; // Add readonly to check

          if (!hasChanges) {
            return;
          }

          // Set all state at once
          set({
            selectedComponent: params.selectedComponent,
            netlist: params.netlist,
            config: params.config,
            readonly: params.readonly ?? false, // Set readonly from params
            onPositionsChange: params.onPositionsChange,
            loadPositions: params.loadPositions,
          });

          // Now trigger layout with all the correct state
          if (params.selectedComponent && params.netlist && params.config) {
            loadPositionsAndLayout(
              params.selectedComponent,
              params.netlist,
              params.config,
              params.loadPositions,
              get().storeLayoutResult,
              set
            );
          }
        },

        // Semantic actions
        storeLayoutResult: (layoutResult, netlist) => {
          const {
            children = [],
            edges,
            nodePositions,
            kicadSchematic,
            kicadSchematicFull,
          } = layoutResult;
          const state = get();

          // Preserve selection state from current nodes
          const selectedNodeIds = new Set(
            state.nodes.filter((node) => node.selected).map((node) => node.id)
          );

          // Create new nodes with preserved selection state and readonly flag
          const nodes = children.map((elkNode: any) => {
            const node = createSchematicNode(elkNode, netlist, state.readonly);
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
            kicadSchematic: kicadSchematic || null,
            kicadSchematicFull: kicadSchematicFull || null,
          });
        },

        rotateNodes: (nodeIds) => {
          const state = get();
          const updatedPositions = { ...state.nodePositions };

          nodeIds.forEach((nodeId) => {
            const currentPosition = state.nodePositions[nodeId];
            const node = state.nodes.find((n) => n.id === nodeId);

            if (currentPosition) {
              const currentRotation = currentPosition.rotation || 0;
              const newRotation = (currentRotation + 90) % 360;

              updatedPositions[nodeId] = {
                ...currentPosition,
                rotation: newRotation,
              };
            } else if (node && node.position) {
              // Create new position with rotation from the node's current position
              updatedPositions[nodeId] = {
                x: node.position.x,
                y: node.position.y,
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

        createNetSymbolNode: (originalNodeId) => {
          const state = get();
          if (!state.netlist || !state.selectedComponent || !state.config)
            return;

          // Create a LayoutEngine instance to use helper methods
          const layoutEngine = new SchematicLayoutEngine(
            state.netlist,
            state.config
          );

          // Extract net name from the original node ID
          const netName =
            layoutEngine.getNetNameFromSymbolNodeId(originalNodeId);
          if (!netName) {
            console.warn(`Node ${originalNodeId} is not a net symbol node`);
            return;
          }

          // Find all existing net symbol nodes for this net based on positions
          const netSymbolPattern = new RegExp(
            `^${state.netlist.root_ref}\\.${netName}\\.(\\d+)$`
          );
          const existingNumbers = new Set<number>();

          for (const nodeId of Object.keys(state.nodePositions)) {
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

          const newNodeId = `${state.netlist.root_ref}.${netName}.${nextNumber}`;

          // Get the original node position
          const originalPosition = state.nodePositions[originalNodeId];
          if (!originalPosition) {
            console.warn(`No position found for node ${originalNodeId}`);
            return;
          }

          // Find the original node to get its width
          const originalNode = state.nodes.find((n) => n.id === originalNodeId);
          const nodeWidth = originalNode?.width || 100;

          // Create new position offset by the node width + some spacing
          const updatedPositions = {
            ...state.nodePositions,
            [newNodeId]: {
              x: originalPosition.x + nodeWidth + 50, // Offset by width + 50px spacing
              y: originalPosition.y,
              width: originalPosition.width,
              height: originalPosition.height,
              rotation: originalPosition.rotation || 0,
            },
          };

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
        },

        deleteNetSymbolNodes: (nodeIds) => {
          const state = get();
          if (!state.netlist || !state.selectedComponent || !state.config)
            return;

          // Create a LayoutEngine instance to use helper methods
          const layoutEngine = new SchematicLayoutEngine(
            state.netlist,
            state.config
          );

          const updatedPositions = { ...state.nodePositions };
          let hasChanges = false;

          for (const nodeId of nodeIds) {
            // Check if this is a net symbol node
            const netName = layoutEngine.getNetNameFromSymbolNodeId(nodeId);
            if (!netName) {
              console.warn(`Node ${nodeId} is not a net symbol node`);
              continue;
            }

            // Find all net symbol nodes for this net based on positions
            const netSymbolPattern = new RegExp(
              `^${state.netlist.root_ref}\\.${netName}\\.(\\d+)$`
            );
            const allSymbolNodes: string[] = [];

            for (const posNodeId of Object.keys(state.nodePositions)) {
              if (netSymbolPattern.test(posNodeId)) {
                allSymbolNodes.push(posNodeId);
              }
            }

            // Only delete if there's more than one symbol for this net
            if (allSymbolNodes.length > 1) {
              delete updatedPositions[nodeId];
              hasChanges = true;
              console.log(`Deleted net symbol node ${nodeId}`);
            } else {
              console.warn(
                `Cannot delete the last net symbol node for net ${netName}`
              );
            }
          }

          if (hasChanges) {
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
          }
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
            kicadSchematic: null,
            kicadSchematicFull: null,
          });
        },

        onNodesChange: (changes) => {
          const state = get();

          // Get grid snap settings from config
          const gridSnapEnabled = state.config?.layout.gridSnap.enabled ?? true;
          const gridSize = state.config?.layout.gridSnap.size ?? 12.7;

          // Apply grid snapping to position changes if enabled
          let processedChanges = changes;
          if (gridSnapEnabled) {
            processedChanges = changes.map((change) => {
              if (change.type === "position" && change.position && change.id) {
                // Find the node to get its dimensions
                const node = state.nodes.find((n) => n.id === change.id);
                if (node) {
                  const snappedPos = snapPosition(
                    change.position.x,
                    change.position.y,
                    gridSize,
                    node.width,
                    node.height
                  );
                  return {
                    ...change,
                    position: snappedPos,
                  };
                }
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
