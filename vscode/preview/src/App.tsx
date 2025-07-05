import React, { useState, useEffect } from "react";
import "./App.css";
import SchematicContainer from "./components/SchematicContainer";
import demoData from "./data/demo.json";
import type { Netlist } from "./types/NetlistTypes";
import { renderKicadSymbol, getKicadSymbolInfo } from "./renderer/kicad_sym";

// Get VSCode API
declare const acquireVsCodeApi: () => {
  postMessage: (message: any) => void;
  getState: () => any;
  setState: (state: any) => void;
};

// Helper to detect if we're in VSCode
const isVSCodeEnvironment = () => {
  try {
    return !!acquireVsCodeApi;
  } catch {
    return false;
  }
};

// Initialize VSCode API only in production
const vscode = isVSCodeEnvironment() ? acquireVsCodeApi() : null;

const LED_SYMBOL = `
(kicad_symbol_lib
	(version 20241209)
	(generator "kicad_symbol_editor")
	(generator_version "9.0")
	(symbol "LED"
		(pin_numbers
			(hide yes)
		)
		(pin_names
			(offset 1.016)
			(hide yes)
		)
		(exclude_from_sim no)
		(in_bom yes)
		(on_board yes)
		(property "Reference" "D"
			(at 0 2.54 0)
			(effects
				(font
					(size 1.27 1.27)
				)
			)
		)
		(property "Value" "LED"
			(at 0 -2.54 0)
			(effects
				(font
					(size 1.27 1.27)
				)
			)
		)
		(property "Footprint" ""
			(at 0 0 0)
			(effects
				(font
					(size 1.27 1.27)
				)
				(hide yes)
			)
		)
		(property "Datasheet" "~"
			(at 0 0 0)
			(effects
				(font
					(size 1.27 1.27)
				)
				(hide yes)
			)
		)
		(property "Description" "Light emitting diode"
			(at 0 0 0)
			(effects
				(font
					(size 1.27 1.27)
				)
				(hide yes)
			)
		)
		(property "Sim.Pins" "1=K 2=A"
			(at 0 0 0)
			(effects
				(font
					(size 1.27 1.27)
				)
				(hide yes)
			)
		)
		(property "ki_keywords" "LED diode"
			(at 0 0 0)
			(effects
				(font
					(size 1.27 1.27)
				)
				(hide yes)
			)
		)
		(property "ki_fp_filters" "LED* LED_SMD:* LED_THT:*"
			(at 0 0 0)
			(effects
				(font
					(size 1.27 1.27)
				)
				(hide yes)
			)
		)
		(symbol "LED_0_1"
			(polyline
				(pts
					(xy -3.048 -0.762) (xy -4.572 -2.286) (xy -3.81 -2.286) (xy -4.572 -2.286) (xy -4.572 -1.524)
				)
				(stroke
					(width 0)
					(type default)
				)
				(fill
					(type none)
				)
			)
			(polyline
				(pts
					(xy -1.778 -0.762) (xy -3.302 -2.286) (xy -2.54 -2.286) (xy -3.302 -2.286) (xy -3.302 -1.524)
				)
				(stroke
					(width 0)
					(type default)
				)
				(fill
					(type none)
				)
			)
			(polyline
				(pts
					(xy -1.27 0) (xy 1.27 0)
				)
				(stroke
					(width 0)
					(type default)
				)
				(fill
					(type none)
				)
			)
			(polyline
				(pts
					(xy -1.27 -1.27) (xy -1.27 1.27)
				)
				(stroke
					(width 0.254)
					(type default)
				)
				(fill
					(type none)
				)
			)
			(polyline
				(pts
					(xy 1.27 -1.27) (xy 1.27 1.27) (xy -1.27 0) (xy 1.27 -1.27)
				)
				(stroke
					(width 0.254)
					(type default)
				)
				(fill
					(type none)
				)
			)
		)
		(symbol "LED_1_1"
			(pin passive line
				(at -3.81 0 0)
				(length 2.54)
				(name "K"
					(effects
						(font
							(size 1.27 1.27)
						)
					)
				)
				(number "1"
					(effects
						(font
							(size 1.27 1.27)
						)
					)
				)
			)
			(pin passive line
				(at 3.81 0 180)
				(length 2.54)
				(name "A"
					(effects
						(font
							(size 1.27 1.27)
						)
					)
				)
				(number "2"
					(effects
						(font
							(size 1.27 1.27)
						)
					)
				)
			)
		)
		(embedded_fonts no)
	)
)
`;

function App() {
  const [netlistData, setNetlistData] = useState<Netlist | null>(null);
  const [currentFile, setCurrentFile] = useState<string | undefined>();
  const [selectedModule, setSelectedModule] = useState<string | undefined>();
  const [isLoading, setIsLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [showSymbolTest, setShowSymbolTest] = useState(false);

  // Helper to validate netlist
  const isValidNetlist = (netlist: any) => {
    return netlist && Object.keys(netlist.instances || {}).length > 0;
  };

  useEffect(() => {
    if (vscode) {
      // VSCode environment
      vscode.postMessage({ command: "ready" });

      const messageHandler = (event: MessageEvent) => {
        const message = event.data;

        switch (message.command) {
          case "update":
            setIsLoading(false);
            setLoadError(null);
            // Only update netlist if it's valid, otherwise keep the old one
            if (isValidNetlist(message.netlist)) {
              setNetlistData(message.netlist);

              // Determine a sensible default module to display. Prefer the
              // explicitly-provided `selectedModule`, otherwise fall back to
              // the <root> module of the currently-open file.
              if (message.selectedModule) {
                setSelectedModule(message.selectedModule);
              } else {
                const currentFilePath: string | undefined = message.currentFile;

                if (currentFilePath) {
                  // Look for a module id ending with `:<root>` that belongs
                  // to the current file.
                  const maybeRootId = Object.keys(
                    message.netlist.instances
                  ).find(
                    (id) =>
                      id.startsWith(currentFilePath) && id.includes(":<root>")
                  );

                  setSelectedModule(maybeRootId || `${currentFilePath}:<root>`);
                }
              }
            }

            // Always record the current file path even if netlist is invalid
            setCurrentFile(message.currentFile);
            break;
          default:
            console.warn("Unknown command received:", message);
        }
      };

      window.addEventListener("message", messageHandler);

      return () => {
        window.removeEventListener("message", messageHandler);
      };
    } else {
      // Browser environment - use demo data
      setIsLoading(false);
      setNetlistData(demoData as any);
      setCurrentFile("/Users/lenny/Downloads/PI0009 3/PI0009.star");
      setSelectedModule("/Users/lenny/Downloads/PI0009 3/PI0009.star:<root>");

      return () => {};
    }
  }, []);

  return (
    <div className="App">
      <main style={{ padding: "0" }}>
        {/* Toggle button - only show in browser environment */}
        {!vscode && (
          <div style={{ padding: "10px", borderBottom: "1px solid #ccc" }}>
            <button
              onClick={() => setShowSymbolTest(!showSymbolTest)}
              style={{
                padding: "8px 16px",
                backgroundColor: showSymbolTest ? "#4CAF50" : "#008CBA",
                color: "white",
                border: "none",
                borderRadius: "4px",
                cursor: "pointer",
                fontSize: "14px",
              }}
            >
              {showSymbolTest ? "Show Schematic View" : "Show Symbol Test View"}
            </button>
          </div>
        )}

        {!vscode && showSymbolTest ? (
          <SymbolTestView />
        ) : isLoading ? (
          <div className="loading">Loading netlist data...</div>
        ) : loadError ? (
          <div className="error-message">
            <h3>Error Loading Data</h3>
            <p>{loadError}</p>
            <button onClick={() => setLoadError(null)}>Dismiss</button>
          </div>
        ) : !netlistData ? (
          <div className="loading">Waiting for netlist data...</div>
        ) : (
          <SchematicContainer
            netlistData={netlistData}
            currentFile={currentFile ?? ""}
            selectedModule={selectedModule}
          />
        )}
      </main>
    </div>
  );
}

// Symbol Test View Component
function SymbolTestView() {
  const canvasRef = React.useRef<HTMLCanvasElement>(null);
  const [renderError, setRenderError] = useState<string | null>(null);
  const [symbolInfo, setSymbolInfo] = useState<any>(null);
  const [debugBounds, setDebugBounds] = useState(true);
  const [tightBounds, setTightBounds] = useState(false);
  const [includePinTextInBounds, setIncludePinTextInBounds] = useState(true);
  const [showPinEndpoints, setShowPinEndpoints] = useState(true);
  const [showFields, setShowFields] = useState(false);

  useEffect(() => {
    const renderSymbol = async () => {
      if (!canvasRef.current) return;

      try {
        // Get symbol info first
        const info = getKicadSymbolInfo(LED_SYMBOL, "LED", {
          unit: 1,
          bodyStyle: 1,
          tightBounds,
          debugBounds,
          includePinTextInBounds,
        });
        setSymbolInfo(info);

        await renderKicadSymbol(canvasRef.current, LED_SYMBOL, "LED", {
          scale: 10,
          padding: 50,
          showPinNames: true,
          showPinNumbers: true,
          showPinEndpoints,
          showFields,
          tightBounds,
          debugBounds,
          includePinTextInBounds,
        });
      } catch (error) {
        console.error("Error rendering symbol:", error);
        setRenderError(
          error instanceof Error ? error.message : "Unknown error"
        );
      }
    };

    renderSymbol();
  }, [
    debugBounds,
    tightBounds,
    includePinTextInBounds,
    showPinEndpoints,
    showFields,
  ]);

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        padding: "20px",
        backgroundColor: "#f5f5f5",
        minHeight: "calc(100vh - 60px)",
      }}
    >
      <h2>KiCad Symbol Renderer Test</h2>
      <p>STM32G431K6U6 Microcontroller</p>

      {/* Debug Controls */}
      <div
        style={{
          marginBottom: "20px",
          padding: "15px",
          backgroundColor: "white",
          borderRadius: "8px",
          boxShadow: "0 2px 4px rgba(0,0,0,0.1)",
        }}
      >
        <h3>Debug Options:</h3>
        <div style={{ display: "flex", gap: "20px", flexWrap: "wrap" }}>
          <label>
            <input
              type="checkbox"
              checked={debugBounds}
              onChange={(e) => setDebugBounds(e.target.checked)}
            />
            Debug Bounds
          </label>
          <label>
            <input
              type="checkbox"
              checked={tightBounds}
              onChange={(e) => setTightBounds(e.target.checked)}
            />
            Tight Bounds
          </label>
          <label>
            <input
              type="checkbox"
              checked={includePinTextInBounds}
              onChange={(e) => setIncludePinTextInBounds(e.target.checked)}
            />
            Include Pin Text in Bounds
          </label>
          <label>
            <input
              type="checkbox"
              checked={showPinEndpoints}
              onChange={(e) => setShowPinEndpoints(e.target.checked)}
            />
            Show Pin Endpoints
          </label>
          <label>
            <input
              type="checkbox"
              checked={showFields}
              onChange={(e) => setShowFields(e.target.checked)}
            />
            Show Fields
          </label>
        </div>
      </div>

      {renderError ? (
        <div style={{ color: "red", marginBottom: "10px" }}>
          Error: {renderError}
        </div>
      ) : null}
      <div
        style={{
          borderRadius: "8px",
          backgroundColor: "white",
          padding: "20px",
          boxShadow: "0 2px 4px rgba(0,0,0,0.1)",
        }}
      >
        <canvas
          ref={canvasRef}
          style={{
            display: "block",
            border: "2px dashed #888888",
            borderRadius: "4px",
          }}
        />
      </div>
      {symbolInfo && (
        <div style={{ marginTop: "20px", textAlign: "left" }}>
          <h3>Symbol Information:</h3>
          <p>
            <strong>Bounding Box:</strong> x: {symbolInfo.bbox.x.toFixed(2)}, y:{" "}
            {symbolInfo.bbox.y.toFixed(2)}, width:{" "}
            {symbolInfo.bbox.w.toFixed(2)}, height:{" "}
            {symbolInfo.bbox.h.toFixed(2)}
          </p>
          <p>
            <strong>Pin Count:</strong> {symbolInfo.pinEndpoints.length}
          </p>
          <details>
            <summary style={{ cursor: "pointer" }}>
              Pin Endpoints (click to expand)
            </summary>
            <ul style={{ fontSize: "12px", fontFamily: "monospace" }}>
              {symbolInfo.pinEndpoints.map((pin: any, index: number) => (
                <li key={index}>
                  Pin {pin.number} ({pin.name}): ({pin.position.x.toFixed(2)},{" "}
                  {pin.position.y.toFixed(2)}) - {pin.orientation}
                </li>
              ))}
            </ul>
          </details>
        </div>
      )}
    </div>
  );
}

export default App;
