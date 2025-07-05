import React, { useState, useEffect } from "react";
import ReactFlowSchematicViewer from "./ReactFlowSchematicViewer";
import "./ReactFlowSchematicViewer.css";
import { Netlist } from "../types/NetlistTypes";
import "@vscode-elements/elements/dist/bundled.js";

// Adjust styles for VSCode-like appearance
const containerStyles = `
.schematic-layout {
  display: flex;
  width: 100%;
  height: 100vh;
  overflow: hidden;
  font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Oxygen, Ubuntu, Cantarell, 'Open Sans', 'Helvetica Neue', sans-serif;
  font-size: 13px;
  background-color: var(--vscode-editor-background, #1e1e1e);
  color: var(--vscode-foreground, #cccccc);
}

.schematic-sidebar {
  width: 260px;
  height: 100%;
  background-color: var(--vscode-sideBar-background, #252526);
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  border-right: 1px solid var(--vscode-sideBar-border, #1e1e1e);
  box-sizing: border-box;
}

.up-button {
  padding: 8px 12px;
  margin: 8px;
  background-color: var(--vscode-button-background);
  color: var(--vscode-button-foreground);
  border: none;
  border-radius: 2px;
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
}

.up-button:hover {
  background-color: var(--vscode-button-hoverBackground);
}

.up-button svg {
  width: 16px;
  height: 16px;
}

.file-tree-container {
  flex: 1;
  overflow-y: auto;
  padding: 0;
  width: 100%;
  text-align: left;
}

.schematic-viewer-container {
  flex: 1;
  height: 100vh;
  position: relative;
  background-color: var(--vscode-editor-background, #1e1e1e);
  overflow: hidden;
}

.error-message {
  position: absolute;
  top: 16px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 1000;
  background-color: var(--vscode-inputValidation-errorBackground, #5a1d1d);
  color: var(--vscode-inputValidation-errorForeground, #ffffff);
  padding: 8px 12px;
  border-radius: 2px;
  border: 1px solid var(--vscode-inputValidation-errorBorder, #be1100);
  font-size: 13px;
  box-shadow: 0 2px 8px rgba(0, 0, 0, 0.2);
  max-width: 80%;
}

.breadcrumbs {
  position: absolute;
  top: 8px;
  left: 8px;
  z-index: 10;
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 4px 8px;
  background-color: var(--vscode-breadcrumb-background, rgba(37, 37, 38, 0.8));
  border-radius: 4px;
  backdrop-filter: blur(8px);
}

.breadcrumb-item {
  color: var(--vscode-breadcrumb-foreground, #cccccc);
  font-size: 12px;
  cursor: pointer;
  display: flex;
  align-items: center;
  gap: 4px;
  text-decoration: none;
  padding: 2px 4px;
  border-radius: 2px;
}

.breadcrumb-item:hover {
  background-color: var(--vscode-breadcrumb-focusForeground, rgba(255, 255, 255, 0.1));
}

.breadcrumb-separator {
  color: var(--vscode-breadcrumb-foreground, #cccccc);
  opacity: 0.6;
  font-size: 12px;
  user-select: none;
}
`;

// Create a style element to inject the styles
const StyleInjector = () => {
  useEffect(() => {
    const styleEl = document.createElement("style");
    styleEl.innerHTML = containerStyles;
    document.head.appendChild(styleEl);

    return () => {
      document.head.removeChild(styleEl);
    };
  }, []);

  return null;
};

interface SchematicContainerProps {
  netlistData: Netlist;
  currentFile: string;
  selectedModule?: string;
}

const Breadcrumbs = ({
  moduleId,
  onNavigate,
}: {
  moduleId: string;
  onNavigate: (id: string) => void;
}) => {
  if (!moduleId) return null;

  const [file_path, path] = moduleId.split(":");
  const filename = file_path.split("/").pop() || file_path;

  // Create breadcrumbs array starting with the filename but preserve full path for navigation
  const breadcrumbs = [{ label: filename, id: file_path }];

  // Add the rest of the path components if they exist
  if (path) {
    const parts = path.split(".");
    parts.forEach((part, index) => {
      const id = `${file_path}:${parts.slice(0, index + 1).join(".")}`;
      breadcrumbs.push({ label: part, id });
    });
  }

  return (
    <div className="breadcrumbs">
      {breadcrumbs.map((crumb, index) => (
        <React.Fragment key={crumb.id}>
          {index > 0 && <span className="breadcrumb-separator">/</span>}
          <span
            className="breadcrumb-item"
            onClick={() => onNavigate(crumb.id)}
            title={crumb.id}
          >
            {crumb.label}
          </span>
        </React.Fragment>
      ))}
    </div>
  );
};

/**
 * Container component that manages the schematic hierarchy and navigation
 */
const SchematicContainer: React.FC<SchematicContainerProps> = ({
  netlistData,
  currentFile,
  selectedModule: initialSelectedModule,
}) => {
  console.log("schematic container with currentFile", currentFile);
  const [error, setError] = useState<string | null>(null);
  const [selectedModule, setSelectedModule] = useState<string>(
    initialSelectedModule || currentFile
  );

  // Update selected module when prop changes
  useEffect(() => {
    if (initialSelectedModule) {
      setSelectedModule(initialSelectedModule);
    }
  }, [initialSelectedModule]);

  // Set initial view to the top-level file view if no module selected
  useEffect(() => {
    if (!selectedModule && currentFile) {
      setSelectedModule(currentFile);
    }
  }, [currentFile, selectedModule]);

  // Handle component selection
  const handleComponentSelect = (componentId: string | null) => {
    if (componentId) {
      setSelectedModule(componentId);
    }
  };

  // Handle errors
  const handleError = (message: string) => {
    setError(message);
  };

  return (
    <div className="schematic-layout">
      <StyleInjector />

      {/* <Sidebar
        netlist={netlistData}
        selectModule={handleComponentSelect}
        selectedModule={selectedModule}
        currentFile={currentFile}
      /> */}

      {/* Main Schematic Viewer */}
      <div className="schematic-viewer-container">
        {selectedModule && (
          <Breadcrumbs
            moduleId={selectedModule}
            onNavigate={handleComponentSelect}
          />
        )}
        {error && (
          <div className="error-message">
            <p>{error}</p>
            <button onClick={() => setError(null)}>Dismiss</button>
          </div>
        )}

        {/* Schematic Viewer */}
        <ReactFlowSchematicViewer
          netlist={netlistData}
          onError={handleError}
          onComponentSelect={handleComponentSelect}
          selectedComponent={selectedModule}
        />
      </div>
    </div>
  );
};

export default SchematicContainer;
