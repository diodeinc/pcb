import React, { useEffect, useRef, useState } from "react";
import { renderKicadSymbol } from "../../../src/renderer/kicad_sym";
import type { RenderOptions } from "../../../src/renderer/kicad_sym";

// Component to render a KiCad symbol
interface KicadSymbolRendererProps {
  symbolContent: string;
  symbolName?: string;
  options?: RenderOptions;
  width?: number;
  height?: number;
}

const KicadSymbolRenderer: React.FC<KicadSymbolRendererProps> = ({
  symbolContent,
  symbolName,
  options = {},
  width = 600,
  height = 600,
}) => {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [error, setError] = useState<string | null>(null);
  const [actualDimensions, setActualDimensions] = useState<{
    width: number;
    height: number;
  } | null>(null);

  useEffect(() => {
    const renderSymbol = async () => {
      if (!canvasRef.current) return;

      try {
        setError(null);
        const { width, height } = await renderKicadSymbol(
          canvasRef.current,
          symbolContent,
          symbolName,
          options
        );
        setActualDimensions({ width, height });
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
        console.error("Failed to render symbol:", err);
      }
    };

    renderSymbol();
  }, [symbolContent, symbolName, options]);

  return (
    <div
      style={{
        padding: "20px",
        backgroundColor: "#f0f0f0",
        display: "inline-block",
      }}
    >
      {error && (
        <div style={{ color: "red", marginBottom: "10px" }}>Error: {error}</div>
      )}
      <canvas
        ref={canvasRef}
        style={{
          border: "1px solid #ccc",
          backgroundColor: "white",
          maxWidth: `${width}px`,
          maxHeight: `${height}px`,
        }}
      />
      {actualDimensions && (
        <div style={{ marginTop: "10px", fontSize: "12px", color: "#666" }}>
          Canvas: {actualDimensions.width}x{actualDimensions.height}px
        </div>
      )}
    </div>
  );
};

export default KicadSymbolRenderer;
