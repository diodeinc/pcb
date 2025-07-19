import { test, expect } from "@playwright/experimental-ct-react";
import React from "react";
import SchematicWithPositions from "./SchematicWithPositions";
import type { Netlist } from "../../src/types/NetlistTypes";
import type { NodePositions } from "../../src/LayoutEngine";
import * as fs from "fs";
import * as path from "path";
import { execSync } from "child_process";

test.describe("Schematic Visual Tests", () => {
  const examplesDir = path.join(__dirname, "../../../../examples");

  // Helper function to parse saved positions from a zen file
  function parseSavedPositions(zenFilePath: string): NodePositions {
    const positions: NodePositions = {};
    const fileContent = fs.readFileSync(zenFilePath, "utf-8");
    const lines = fileContent.split("\n");

    // Pattern to match position comments: # pcb:sch <id> x=<x> y=<y> rot=<rotation>
    const positionPattern =
      /^#\s*pcb:sch\s+(\S+)\s+x=([^\s]+)\s+y=([^\s]+)\s+rot=([^\s]+)/;

    for (const line of lines) {
      const match = line.match(positionPattern);
      if (match) {
        const [, id, x, y, rotation] = match;
        positions[id] = {
          x: parseFloat(x),
          y: parseFloat(y),
          rotation: parseFloat(rotation),
        };
      }
    }

    return positions;
  }

  // Helper function to build netlist from example
  async function buildNetlistFromExample(
    exampleName: string
  ): Promise<{ netlist: Netlist; positions: NodePositions }> {
    const examplePath = path.join(examplesDir, exampleName);
    const starFile = path.join(examplePath, `${exampleName}.zen`);

    if (!fs.existsSync(starFile)) {
      throw new Error(`Example file not found: ${starFile}`);
    }

    // Parse saved positions from the zen file
    const positions = parseSavedPositions(starFile);

    // Run cargo to build netlist
    try {
      const output = execSync(`cargo run -- build --netlist "${starFile}"`, {
        cwd: path.join(__dirname, "../../../../"),
        stdio: "pipe",
        encoding: "utf-8",
      });

      // Parse the output directly from stdout
      const netlist = JSON.parse(output) as Netlist;

      return { netlist, positions };
    } catch (error: any) {
      console.error(
        `Failed to build netlist for ${exampleName}:`,
        error.stderr?.toString()
      );
      throw error;
    }
  }

  // Get all example directories
  const exampleDirs = fs
    .readdirSync(examplesDir)
    .filter((name) => fs.statSync(path.join(examplesDir, name)).isDirectory());

  // Dynamically generate a test for each example
  for (const exampleName of exampleDirs) {
    test(`${exampleName}`, async ({ mount, page }) => {
      // Capture console logs from the browser
      page.on("console", async (msg) => {
        const type = msg.type();

        // Get all arguments passed to console.log/error/etc
        const args: string[] = [];
        for (const arg of msg.args()) {
          args.push(arg.toString());
        }

        if (msg.text().includes("kicanvas")) {
          return;
        }

        // Log to Node.js console with appropriate formatting
        if (type === "log") {
          console.log("[Browser Console]", ...args);
        } else if (type === "error") {
          console.error("[Browser Console Error]", ...args);
        } else if (type === "warning") {
          console.warn("[Browser Console Warning]", ...args);
        } else if (type === "info") {
          console.info("[Browser Console Info]", ...args);
        }
      });

      // Also capture page errors
      page.on("pageerror", (error) => {
        console.error(`[Page Error] ${error.message}`);
        console.error(error.stack);
      });

      const { netlist, positions } = await buildNetlistFromExample(exampleName);

      // Mount the component
      const component = await mount(
        <>
          <style>{`
            /* Ensure the mount root has full height */
            #root {
              width: 1920px;
              height: 1080px;
              position: relative;
              overflow: hidden;
            }
            /* Override the 100vh in SchematicContainer to use parent height */
            .schematic-layout {
              height: 100% !important;
            }
            .schematic-viewer-container {
              height: 100% !important;
            }
            /* Ensure React Flow fills the container */
            .react-flow {
              height: 100% !important;
              width: 100% !important;
            }
            .react-flow__renderer {
              height: 100% !important;
              width: 100% !important;
            }
            .react-flow__viewport {
              height: 100% !important;
              width: 100% !important;
            }
            .schematic-viewer {
              height: 100% !important;
              width: 100% !important;
            }
            .react-flow-schematic-viewer {
              height: 100% !important;
              width: 100% !important;
            }
          `}</style>
          <SchematicWithPositions netlist={netlist} positions={positions} />
        </>
      );

      // Wait for React Flow to finish rendering
      await page.waitForSelector(".react-flow__renderer", {
        state: "attached",
        timeout: 10000,
      });

      // Wait for nodes to be rendered
      await page.waitForSelector(".react-flow__node", {
        state: "visible",
        timeout: 10000,
      });

      // Give time for layout to stabilize and animations to complete
      await page.waitForTimeout(1000);

      // Check for any error messages
      const errorElement = await page.$(".error-message");
      if (errorElement) {
        const errorText = await errorElement.textContent();
        throw new Error(`Error message found in ${exampleName}: ${errorText}`);
      }

      // Take screenshot
      await expect(component).toHaveScreenshot(
        `${exampleName.toLowerCase()}.png`,
        {
          animations: "disabled",
          scale: "css",
        }
      );
    });
  }

  // Test rotation functionality
  test("VoltageDivider with rotation", async ({ mount, page }) => {
    // Capture console logs from the browser
    page.on("console", async (msg) => {
      const type = msg.type();
      const args: string[] = [];
      for (const arg of msg.args()) {
        args.push(arg.toString());
      }
      if (msg.text().includes("kicanvas")) {
        return;
      }
      if (type === "log") {
        console.log("[Browser Console]", ...args);
      } else if (type === "error") {
        console.error("[Browser Console Error]", ...args);
      }
    });

    page.on("pageerror", (error) => {
      console.error(`[Page Error] ${error.message}`);
      console.error(error.stack);
    });

    const { netlist, positions } = await buildNetlistFromExample(
      "VoltageDivider"
    );

    // Mount the component
    const component = await mount(
      <>
        <style>{`
          /* Ensure the mount root has full height */
          #root {
            width: 1920px;
            height: 1080px;
            position: relative;
            overflow: hidden;
          }
          /* Override the 100vh in SchematicContainer to use parent height */
          .schematic-layout {
            height: 100% !important;
          }
          .schematic-viewer-container {
            height: 100% !important;
          }
          /* Ensure React Flow fills the container */
          .react-flow {
            height: 100% !important;
            width: 100% !important;
          }
          .react-flow__renderer {
            height: 100% !important;
            width: 100% !important;
          }
          .react-flow__viewport {
            height: 100% !important;
            width: 100% !important;
          }
          .schematic-viewer {
            height: 100% !important;
            width: 100% !important;
          }
          .react-flow-schematic-viewer {
            height: 100% !important;
            width: 100% !important;
          }
        `}</style>
        <SchematicWithPositions netlist={netlist} positions={positions} />
      </>
    );

    // Wait for React Flow to finish rendering
    await page.waitForSelector(".react-flow__renderer", {
      state: "attached",
      timeout: 10000,
    });

    // Wait for nodes to be rendered
    await page.waitForSelector(".react-flow__node", {
      state: "visible",
      timeout: 10000,
    });

    // Give time for layout to stabilize
    await page.waitForTimeout(1000);

    // Find and click on a resistor node (R1 or R2)
    const resistorNode = await page
      .locator('.react-flow__node[data-id*="R1"]')
      .first();

    // Click to select the resistor
    await resistorNode.click();

    // Wait a bit for selection to register
    await page.waitForTimeout(100);

    // Press 'R' to rotate
    await page.keyboard.press("r");

    // Wait for rotation animation and re-layout
    await page.waitForTimeout(500);

    // Take screenshot with rotated component
    await expect(component).toHaveScreenshot("voltagedivider-rotated.png", {
      animations: "disabled",
      scale: "css",
    });

    // Rotate again (should be 180 degrees now)
    await page.keyboard.press("r");
    await page.waitForTimeout(500);

    await expect(component).toHaveScreenshot("voltagedivider-rotated-180.png", {
      animations: "disabled",
      scale: "css",
    });

    // Rotate again (should be 270 degrees now)
    await page.keyboard.press("r");
    await page.waitForTimeout(500);

    await expect(component).toHaveScreenshot("voltagedivider-rotated-270.png", {
      animations: "disabled",
      scale: "css",
    });

    // Rotate once more (should be back to 0 degrees)
    await page.keyboard.press("r");
    await page.waitForTimeout(500);

    await expect(component).toHaveScreenshot("voltagedivider-rotated-360.png", {
      animations: "disabled",
      scale: "css",
    });
  });
});
