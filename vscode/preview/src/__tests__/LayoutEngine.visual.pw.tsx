import { test, expect } from "@playwright/experimental-ct-react";
import SchematicContainer from "../components/SchematicContainer";
import type { Netlist } from "../types/NetlistTypes";
import * as fs from "fs";
import * as path from "path";
import { execSync } from "child_process";

test.describe("Schematic Visual Tests", () => {
  const examplesDir = path.join(__dirname, "../../../../examples");

  // Helper function to build netlist from example
  async function buildNetlistFromExample(
    exampleName: string
  ): Promise<Netlist> {
    const examplePath = path.join(examplesDir, exampleName);
    const starFile = path.join(examplePath, `${exampleName}.zen`);

    if (!fs.existsSync(starFile)) {
      throw new Error(`Example file not found: ${starFile}`);
    }

    // Run cargo to build netlist
    try {
      const output = execSync(`cargo run -- build --netlist "${starFile}"`, {
        cwd: path.join(__dirname, "../../../../"),
        stdio: "pipe",
        encoding: "utf-8",
      });

      // Parse the output directly from stdout
      return JSON.parse(output) as Netlist;
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
        const args = [];
        for (const arg of msg.args()) {
          try {
            // Try to get the JSON value (works for objects, arrays, primitives)
            const value = await arg.jsonValue();
            args.push(value);
          } catch {
            // If jsonValue fails, fall back to string representation
            args.push(arg.toString());
          }
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

      const netlist = await buildNetlistFromExample(exampleName);

      // Mount the component
      const component = await mount(
        <>
          <style>{`
            /* Ensure the mount root has full height */
            #root {
              width: 800px;
              height: 600px;
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
          <SchematicContainer
            netlistData={netlist}
            currentFile={netlist.root_ref.split(":")[0]}
            selectedModule={netlist.root_ref}
          />
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
          scale: "device",
        }
      );
    });
  }
});
