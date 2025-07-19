import React from "react";
import SchematicContainer from "../../src/components/SchematicContainer";
import type { Netlist } from "../../src/types/NetlistTypes";
import type { NodePositions } from "../../src/LayoutEngine";

interface SchematicWithPositionsProps {
  netlist: Netlist;
  positions: NodePositions;
}

/**
 * Test story component that wraps SchematicContainer and provides saved node positions.
 *
 * This component is necessary because Playwright Component Testing doesn't properly
 * handle function props that return values when passed directly from test files.
 * By creating the loadPositions function inside a React component, we ensure it
 * works correctly within the browser context.
 *
 * The positions are expected to be in "cleaned" format (without file paths or <root>)
 * as they are stored in .zen files, and this component handles transforming them
 * to the full node ID format expected by the layout engine.
 */
export const SchematicWithPositions: React.FC<SchematicWithPositionsProps> = ({
  netlist,
  positions,
}) => {
  // Create the loadPositions function inside the component
  const loadPositions = React.useCallback(
    async (componentId: string): Promise<NodePositions | null> => {
      // Check if we have any positions to return
      if (!positions || Object.keys(positions).length === 0) {
        return null;
      }

      // The positions in the file are already cleaned (no file paths or <root>)
      // We need to "unclean" them by adding back the component prefix
      const transformedPositions: NodePositions = {};

      for (const [cleanId, pos] of Object.entries(positions)) {
        // The componentId already ends with :<root>, so just add a dot and the clean ID
        const fullId = `${componentId}.${cleanId}`;
        transformedPositions[fullId] = pos;
      }

      return transformedPositions;
    },
    [positions]
  );

  return (
    <SchematicContainer
      netlistData={netlist}
      currentFile={netlist.root_ref.split(":")[0]}
      selectedModule={netlist.root_ref}
      loadPositions={loadPositions}
    />
  );
};

export default SchematicWithPositions;
