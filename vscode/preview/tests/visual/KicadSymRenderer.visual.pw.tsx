import { test, expect } from "@playwright/experimental-ct-react";
import React from "react";
import type { RenderOptions } from "../../src/renderer/kicad_sym";
import * as fs from "fs";
import * as path from "path";
import KicadSymbolRenderer from "../fixtures/components/KicadSymbolRenderer";

test.describe("KiCad Symbol Renderer Visual Tests", () => {
  // Load symbol files from resources
  const resourcesDir = path.join(__dirname, "../fixtures/resources");

  // Find all .kicad_sym files in the resources directory
  const symbolFiles = fs
    .readdirSync(resourcesDir)
    .filter((file) => file.endsWith(".kicad_sym"))
    .sort(); // Sort for consistent test order

  // Test each symbol with all debug options enabled
  const debugOptions: RenderOptions = {
    showPinEndpoints: true,
    showFields: true,
    debugBounds: true,
    includePinTextInBounds: true,
    scale: 10, // Scale to make symbols clearly visible
    padding: 20, // Small padding
  };

  for (const symbolFile of symbolFiles) {
    const symbolName = path.basename(symbolFile, ".kicad_sym");
    const symbolContent = fs.readFileSync(
      path.join(resourcesDir, symbolFile),
      "utf-8"
    );

    test(`${symbolName} with debug`, async ({ mount, page }) => {
      // Capture console logs
      page.on("console", async (msg) => {
        const args: any[] = [];
        for (const arg of msg.args()) {
          try {
            const value = await arg.jsonValue();
            args.push(value);
          } catch {
            args.push(arg.toString());
          }
        }

        const type = msg.type();
        if (type === "log") {
          console.log("[Browser]", ...args);
        } else if (type === "error") {
          console.error("[Browser Error]", ...args);
        }
      });

      // Mount the component
      const component = await mount(
        <KicadSymbolRenderer
          symbolContent={symbolContent}
          symbolName={symbolName}
          options={debugOptions}
        />
      );

      // Wait for canvas to be rendered
      await page.waitForSelector("canvas", {
        state: "visible",
        timeout: 5000,
      });

      // Give time for rendering to complete
      await page.waitForTimeout(500);

      // Check for errors
      const errorElement = await page.$("div[style*='color: red']");
      if (errorElement) {
        const errorText = await errorElement.textContent();
        throw new Error(`Rendering error for ${symbolName}: ${errorText}`);
      }

      // Take screenshot
      await expect(component).toHaveScreenshot(`${symbolName}.png`, {
        animations: "disabled",
        scale: "device",
      });
    });
  }
});
