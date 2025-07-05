#!/usr/bin/env ts-node

import * as puppeteer from "puppeteer";
import * as fs from "fs";
import * as path from "path";
import * as esbuild from "esbuild";

// Parse command line arguments
const args = process.argv.slice(2);

if (args.length < 1) {
  console.error(
    "Usage: ts-node render-kicad-sym.ts <kicad_sym_file> [output_png] [options]"
  );
  console.error("");
  console.error("Options:");
  console.error(
    "  --symbol <name>     Symbol name to render (default: first symbol)"
  );
  console.error("  --unit <number>     Unit number to render (default: 1)");
  console.error("  --style <number>    Body style to render (default: 1)");
  console.error("  --scale <number>    Scale factor (default: 10)");
  console.error("  --padding <number>  Padding in pixels (default: 50)");
  console.error("  --no-pin-names      Hide pin names");
  console.error("  --no-pin-numbers    Hide pin numbers");
  console.error("  --width <number>    Canvas width (default: auto)");
  console.error("  --height <number>   Canvas height (default: auto)");
  process.exit(1);
}

// Parse arguments
const inputFile = args[0];
let outputFile = args[1];
let symbolName: string | undefined;
let options = {
  unit: 1,
  bodyStyle: 1,
  scale: 10,
  padding: 50,
  showPinNames: true,
  showPinNumbers: true,
};
let canvasWidth: number | undefined;
let canvasHeight: number | undefined;

// Parse options
for (let i = 1; i < args.length; i++) {
  switch (args[i]) {
    case "--symbol":
      symbolName = args[++i];
      break;
    case "--unit":
      options.unit = parseInt(args[++i]);
      break;
    case "--style":
      options.bodyStyle = parseInt(args[++i]);
      break;
    case "--scale":
      options.scale = parseFloat(args[++i]);
      break;
    case "--padding":
      options.padding = parseInt(args[++i]);
      break;
    case "--no-pin-names":
      options.showPinNames = false;
      break;
    case "--no-pin-numbers":
      options.showPinNumbers = false;
      break;
    case "--width":
      canvasWidth = parseInt(args[++i]);
      break;
    case "--height":
      canvasHeight = parseInt(args[++i]);
      break;
  }
}

// Default output file if not specified
if (!outputFile || outputFile.startsWith("--")) {
  const baseName = path.basename(inputFile, ".kicad_sym");
  outputFile = `${baseName}.png`;
}

// Read and bundle the renderer code
async function getRendererBundle(): Promise<string> {
  const rendererPath = path.join(__dirname, "../src/renderer/kicad_sym.ts");

  try {
    // Bundle the renderer with esbuild
    const result = await esbuild.build({
      entryPoints: [rendererPath],
      bundle: true,
      format: "iife",
      globalName: "KicadSymbolRenderer",
      platform: "browser",
      target: "es2020",
      write: false,
      loader: {
        ".ts": "ts",
      },
      define: {
        "process.env.NODE_ENV": '"production"',
      },
      minify: false, // Keep it readable for debugging
      sourcemap: false,
    });

    // Get the bundled code
    const bundledCode = result.outputFiles[0].text;

    // Wrap it to expose the renderKicadSymbol function globally
    return (
      bundledCode +
      `
      // Expose the render function globally
      window.renderKicadSymbol = KicadSymbolRenderer.renderKicadSymbol;
    `
    );
  } catch (error) {
    console.warn("Failed to bundle renderer, using fallback:", error);

    // Fallback to a simple implementation
    return `
      // Fallback renderer
      window.renderKicadSymbol = async function(canvas, content, symbolName, options) {
        const ctx = canvas.getContext('2d');
        canvas.width = 800;
        canvas.height = 600;
        
        ctx.fillStyle = 'white';
        ctx.fillRect(0, 0, canvas.width, canvas.height);
        
        ctx.fillStyle = 'red';
        ctx.font = '20px sans-serif';
        ctx.textAlign = 'center';
        ctx.fillText('Failed to load full renderer', canvas.width/2, canvas.height/2);
        ctx.fillText('Symbol: ' + (symbolName || 'unknown'), canvas.width/2, canvas.height/2 + 30);
      };
    `;
  }
}

async function renderSymbolToPng() {
  let browser: puppeteer.Browser | null = null;

  try {
    // Read the kicad_sym file
    const content = fs.readFileSync(inputFile, "utf-8");

    // Launch puppeteer
    browser = await puppeteer.launch({
      headless: true,
      args: ["--no-sandbox", "--disable-setuid-sandbox"],
    });

    const page = await browser.newPage();

    // Set viewport size
    await page.setViewport({
      width: canvasWidth || 1200,
      height: canvasHeight || 800,
      deviceScaleFactor: 1,
    });

    // Create HTML with inlined renderer
    const html = `
      <!DOCTYPE html>
      <html>
      <head>
        <style>
          body { margin: 0; padding: 0; background: white; }
          canvas { display: block; }
        </style>
      </head>
      <body>
        <canvas id="canvas"></canvas>
        <script>
          ${await getRendererBundle()}
          
          // Symbol data
          const kicadSymContent = ${JSON.stringify(content)};
          const symbolName = ${JSON.stringify(symbolName)};
          const options = ${JSON.stringify(options)};
          
          // Render the symbol
          async function render() {
            const canvas = document.getElementById('canvas');
            try {
              await renderKicadSymbol(canvas, kicadSymContent, symbolName, options);
              window.renderComplete = true;
            } catch (error) {
              console.error('Render error:', error);
              window.renderError = error.message;
            }
          }
          
          render();
        </script>
      </body>
      </html>
    `;

    // Enable console logging
    page.on("console", (msg) => console.log("Browser console:", msg.text()));
    page.on("pageerror", (error) => console.error("Browser error:", error));

    // Set the page content
    await page.setContent(html, { waitUntil: "networkidle0" });

    // Wait for rendering to complete
    await page.waitForFunction(
      () => (window as any).renderComplete || (window as any).renderError,
      { timeout: 30000 }
    );

    // Check for errors
    const error = await page.evaluate(() => (window as any).renderError);
    if (error) {
      throw new Error(`Rendering failed: ${error}`);
    }

    // Get canvas dimensions
    const dimensions = await page.evaluate(() => {
      const canvas = document.getElementById("canvas") as HTMLCanvasElement;
      return {
        width: canvas.width,
        height: canvas.height,
      };
    });

    console.log(
      `Rendered symbol with dimensions: ${dimensions.width}x${dimensions.height}`
    );

    // Take screenshot of just the canvas element
    const canvasElement = await page.$("#canvas");
    if (!canvasElement) {
      throw new Error("Canvas element not found");
    }

    await canvasElement.screenshot({
      path: outputFile as `${string}.png`,
      omitBackground: false,
    });

    console.log(`Saved to: ${outputFile}`);
  } catch (error) {
    console.error("Error:", error);
    process.exit(1);
  } finally {
    if (browser) {
      await browser.close();
    }
  }
}

// Run the renderer
renderSymbolToPng();
