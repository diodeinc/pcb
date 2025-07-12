// KiCad Global Label Renderer
// Renders a KiCad-style global net label to a canvas element

import { Color } from "../third_party/kicanvas/base/color";
import { Angle, BBox, Matrix3, Vec2 } from "../third_party/kicanvas/base/math";
import { Canvas2DRenderer } from "../third_party/kicanvas/graphics/canvas2d";
import { Polygon, Polyline } from "../third_party/kicanvas/graphics/shapes";
import { StrokeFont } from "../third_party/kicanvas/kicad/text/stroke-font";
import { SchText } from "../third_party/kicanvas/kicad/text/sch-text";
import { At, Effects } from "../third_party/kicanvas/kicad/common";
import { DefaultValues } from "../third_party/kicanvas/kicad/schematic";
import type { SchematicTheme } from "../third_party/kicanvas/kicad/theme";
import { DEFAULT_THEME } from "./kicad_sym";

export type LabelDirection = "left" | "right" | "up" | "down";
export type LabelShape =
  | "input"
  | "output"
  | "bidirectional"
  | "tri_state"
  | "passive";

export interface GlobalLabelOptions {
  direction?: LabelDirection;
  shape?: LabelShape;
  theme?: Partial<SchematicTheme>;
  scale?: number;
  padding?: number;
  fontSize?: number; // in mm
}

export interface GlobalLabelInfo {
  bbox: BBox; // Bounding box in label coordinates
  width: number; // Canvas width
  height: number; // Canvas height
}

// Custom Canvas2DRenderer that enables alpha channel
class AlphaCanvas2DRenderer extends Canvas2DRenderer {
  override async setup() {
    const ctx2d = this.canvas.getContext("2d", {
      alpha: true,
      desynchronized: true,
    });

    if (ctx2d == null) {
      throw new Error("Unable to create Canvas2d context");
    }

    this.ctx2d = ctx2d;
    this.update_canvas_size();
  }

  override clear_canvas() {
    // Don't update canvas size here - we set it manually
    this.ctx2d!.setTransform();

    // Always clear with transparent background
    this.ctx2d!.clearRect(0, 0, this.canvas.width, this.canvas.height);
    this.ctx2d!.lineCap = "round";
    this.ctx2d!.lineJoin = "round";
  }
}

export class KicadGlobalLabelRenderer {
  private renderer: AlphaCanvas2DRenderer;
  private theme: SchematicTheme;

  constructor(private canvas: HTMLCanvasElement) {
    this.renderer = new AlphaCanvas2DRenderer(canvas);
    this.theme = DEFAULT_THEME;
  }

  async setup() {
    await this.renderer.setup();
  }

  dispose() {
    this.renderer.dispose();
  }

  /**
   * Get the rotation angle in degrees based on direction
   */
  private getRotationFromDirection(direction: LabelDirection): number {
    switch (direction) {
      case "right":
        return 0;
      case "up":
        return 90;
      case "left":
        return 180;
      case "down":
        return 270;
    }
  }

  /**
   * Calculate the bounding box for a global label
   */
  calculateLabelBBox(text: string, options: GlobalLabelOptions = {}): BBox {
    const {
      direction = "right",
      shape = "input",
      fontSize = 1.27, // Default KiCad font size in mm
    } = options;

    // Create SchText to calculate text dimensions
    const schtext = new SchText(text);
    const at = new At();
    at.rotation = this.getRotationFromDirection(direction);

    schtext.apply_at(at);
    const effects = new Effects();
    // Set font size in internal units
    effects.font.size = new Vec2(fontSize, fontSize);
    // Set a reasonable thickness
    effects.font.thickness = fontSize * 0.15; // 15% of font size
    schtext.apply_effects(effects);

    // Get text dimensions - these are already in internal units
    const textBox = schtext.get_text_box();

    // Convert to mm for calculations
    const textWidthMm = textBox.w / 10000;
    const textHeightMm = schtext.text_size.y / 10000;

    // Calculate margin based on text height
    const marginMm = DefaultValues.label_size_ratio * textHeightMm;
    const halfSizeMm = textHeightMm / 2 + marginMm;
    const symbolLengthMm = textWidthMm + 2 * marginMm;

    // Calculate shape dimensions in mm
    const xMm = symbolLengthMm + 0.3; // 0.3mm padding
    const yMm = halfSizeMm + 0.3; // 0.3mm padding

    // Create bounding box based on direction (in mm)
    // The bbox should represent the full extent of the shape
    let bbox: BBox;
    if (direction === "right" || direction === "left") {
      // For horizontal labels
      if (
        shape === "input" ||
        shape === "bidirectional" ||
        shape === "tri_state"
      ) {
        // These shapes extend from -halfSizeMm to xMm (due to the arrow)
        bbox = new BBox(-halfSizeMm, -yMm, xMm + halfSizeMm, 2 * yMm);
      } else {
        // Output and passive shapes don't have the arrow extension
        bbox = new BBox(0, -yMm, xMm, 2 * yMm);
      }
    } else {
      // For vertical labels
      if (
        shape === "input" ||
        shape === "bidirectional" ||
        shape === "tri_state"
      ) {
        // These shapes extend from -halfSizeMm to xMm (due to the arrow)
        bbox = new BBox(-yMm, -halfSizeMm, 2 * yMm, xMm + halfSizeMm);
      } else {
        // Output and passive shapes don't have the arrow extension
        bbox = new BBox(-yMm, 0, 2 * yMm, xMm);
      }
    }

    return bbox;
  }

  /**
   * Get label info including dimensions
   */
  getLabelInfo(
    text: string,
    options: GlobalLabelOptions = {}
  ): GlobalLabelInfo {
    const { scale = 1, padding = 2 } = options;

    const bbox = this.calculateLabelBBox(text, options);
    // Don't multiply by device pixel ratio here - that's handled by the renderer
    const width = Math.ceil((bbox.w + padding * 2) * scale);
    const height = Math.ceil((bbox.h + padding * 2) * scale);

    return {
      bbox,
      width,
      height,
    };
  }

  /**
   * Render a global label to the canvas
   */
  async renderLabel(
    text: string,
    options: GlobalLabelOptions = {}
  ): Promise<GlobalLabelInfo> {
    const {
      direction = "right",
      shape = "input",
      theme = {},
      scale = 1,
      padding = 2,
      fontSize = 1.27,
    } = options;

    // Merge theme
    this.theme = { ...DEFAULT_THEME, ...theme };

    // Get label info
    const info = this.getLabelInfo(text, options);
    const { bbox } = info;

    // Calculate actual canvas dimensions
    const canvasWidth = Math.ceil((bbox.w + padding * 2) * scale);
    const canvasHeight = Math.ceil((bbox.h + padding * 2) * scale);

    // Setup canvas
    this.canvas.width = canvasWidth;
    this.canvas.height = canvasHeight;

    // Update info with actual dimensions
    info.width = canvasWidth;
    info.height = canvasHeight;

    // Clear canvas with transparent background
    if (this.renderer.ctx2d) {
      this.renderer.background_color = new Color(0, 0, 0, 0);
      this.renderer.clear_canvas();
    }

    // Setup transformation
    const ctx = this.renderer.ctx2d!;
    ctx.save();

    // Calculate translation based on direction and shape
    // For input shapes, we need to position the arrow point at the correct edge
    let translateX = canvasWidth / 2;
    let translateY = canvasHeight / 2;

    // Adjust translation so the connection point is at the correct edge
    if (
      shape === "input" ||
      shape === "bidirectional" ||
      shape === "tri_state"
    ) {
      // These shapes have an arrow point that should align with the port
      switch (direction) {
        case "left":
          // Arrow points left, so the point (at x=0 in shape coords) should be at the right edge
          translateX = canvasWidth - padding * scale;
          break;
        case "right":
          // Arrow points right, so the point should be at the left edge
          translateX = padding * scale;
          break;
        case "up":
          // Arrow points up, so the point should be at the bottom edge
          translateY = canvasHeight - padding * scale;
          break;
        case "down":
          // Arrow points down, so the point should be at the top edge
          translateY = padding * scale;
          break;
      }
    }

    // Apply translation and scale
    ctx.translate(translateX, translateY);
    ctx.scale(scale, scale);

    // Start rendering
    this.renderer.start_layer("label");

    // Create label data structure
    const at = new At();
    at.rotation = this.getRotationFromDirection(direction);

    // Create SchText for rendering
    const schtext = new SchText(text);
    schtext.apply_at(at);

    const effects = new Effects();
    effects.font.size = new Vec2(fontSize, fontSize);
    effects.font.thickness = fontSize * 0.15;
    schtext.apply_effects(effects);

    // Calculate offsets (adapted from GlobalLabelPainter)
    // const textHeightMm = schtext.text_size.y / 10000;
    const textHeightMm = 1.27; // 50 mils
    let horzMm = this.getBoxExpansion(schtext);
    let vertMm = textHeightMm * 0.0715; // Magic number from KiCad

    if (["input", "bidirectional", "tri_state"].includes(shape)) {
      horzMm += textHeightMm * 0.75;
    }

    // Calculate text position offset based on rotation (in mm)
    let textOffset: Vec2;

    // Get shape dimensions
    const marginMm = this.getBoxExpansion(schtext);
    const textBox = schtext.get_text_box();
    const textWidthMm = textBox.w / 10000;
    const symbolLengthMm = textWidthMm + 2 * marginMm;

    // For input shapes, calculate where the center of the shape body is
    // The shape extends from the arrow point (at origin) into the body
    let shapeCenterOffset = new Vec2(0, 0);

    if (
      shape === "input" ||
      shape === "bidirectional" ||
      shape === "tri_state"
    ) {
      // The shape body center is offset from the arrow point
      switch (direction) {
        case "left":
          // Shape extends right from arrow, center is to the right
          shapeCenterOffset.x = (symbolLengthMm + 0.3) / 2;
          break;
        case "right":
          // Shape extends left from arrow, center is to the left
          shapeCenterOffset.x = -(symbolLengthMm + 0.3) / 2;
          break;
        case "up":
          // Shape extends down from arrow, center is below
          shapeCenterOffset.y = (symbolLengthMm + 0.3) / 2;
          break;
        case "down":
          // Shape extends up from arrow, center is above
          shapeCenterOffset.y = -(symbolLengthMm + 0.3) / 2;
          break;
      }
    }

    // Now calculate text offset relative to the shape center
    switch (at.rotation) {
      case 0: // Right-pointing
        textOffset = new Vec2(horzMm, vertMm);
        break;
      case 90: // Up-pointing
        textOffset = new Vec2(vertMm, -horzMm);
        break;
      case 180: // Left-pointing
        textOffset = new Vec2(-horzMm, vertMm);
        break;
      case 270: // Down-pointing
        textOffset = new Vec2(vertMm, horzMm);
        break;
      default:
        textOffset = new Vec2(0, 0);
    }

    // Draw the label shape
    const shapePoints = this.createShape(at.rotation, shape, schtext);
    if (shapePoints.length > 0) {
      this.renderer.state.push();
      this.renderer.state.stroke = this.theme.label_global;
      this.renderer.state.fill = this.theme.background;

      // Fill the shape with the background color
      this.renderer.polygon(
        new Polygon(
          shapePoints.slice(0, -1), // Remove last duplicate point
          this.theme.background
        )
      );

      // Draw the outline
      this.renderer.line(
        new Polyline(
          shapePoints,
          fontSize * 0.15, // Stroke width in mm
          this.theme.label_global
        )
      );

      this.renderer.state.pop();
    }

    // Draw the text
    this.renderer.state.push();
    this.renderer.state.stroke = this.theme.label_global;
    this.renderer.state.fill = this.theme.label_global;

    const textAttributes = schtext.attributes.copy();

    // Draw the label text
    let finalTextOffset = textOffset.copy();

    if (
      shape === "input" ||
      shape === "bidirectional" ||
      shape === "tri_state"
    ) {
      // The shape was shifted so the arrow is at origin instead of center
      // We need to move the text in the opposite direction of where we moved the shape
      switch (direction) {
        case "left":
          // Arrow at origin, shape extends right, text needs to move right by halfSize
          finalTextOffset.x -= symbolLengthMm / 2;
          break;
        case "right":
          // Arrow at origin, shape extends left, text needs to move left by halfSize
          finalTextOffset.x += symbolLengthMm / 2;
          break;
        case "up":
          // Arrow at origin, shape extends down, text needs to move down by halfSize
          finalTextOffset.y -= symbolLengthMm / 2;
          break;
        case "down":
          // Arrow at origin, shape extends up, text needs to move up by halfSize
          finalTextOffset.y += symbolLengthMm / 2;
          break;
      }
    }

    const textPosInternal = finalTextOffset.multiply(10000); // Convert mm to internal units
    StrokeFont.default().draw(
      this.renderer,
      schtext.shown_text,
      textPosInternal,
      textAttributes
    );

    this.renderer.state.pop();

    // End rendering
    const layer = this.renderer.end_layer();
    layer.render(Matrix3.identity(), 0);

    ctx.restore();

    return info;
  }

  /**
   * Get box expansion for label (in mm)
   * From SCH_LABEL_BASE::GetLabelBoxExpansion
   */
  private getBoxExpansion(schtext: SchText): number {
    // Convert text size from internal units to mm
    const textHeightMm = schtext.text_size.y / 10000;
    return DefaultValues.label_size_ratio * textHeightMm;
  }

  /**
   * Create the label shape outline
   * Adapted from SCH_GLOBALLABEL::CreateGraphicShape
   */
  private createShape(
    rotation: number,
    shape: LabelShape,
    schtext: SchText
  ): Vec2[] {
    // KiCad draws the label shape with an extra 180° rotation relative to
    // the label’s logical direction.  Replicate that here so the text offset
    // logic lines up with the outline.
    const angle = Angle.from_degrees(rotation + 180);

    // Convert dimensions to mm
    const textHeightMm = schtext.text_size.y / 10000;
    const textBox = schtext.get_text_box();
    const textWidthMm = textBox.w / 10000;
    const marginMm = this.getBoxExpansion(schtext);
    const halfSizeMm = textHeightMm / 2 + marginMm;
    const symbolLengthMm = textWidthMm + 2 * marginMm;

    const x = symbolLengthMm + 0.3; // 0.3mm padding
    const y = halfSizeMm + 0.3; // 0.3mm padding

    let pts = [
      new Vec2(0, 0),
      new Vec2(0, -y),
      new Vec2(-x, -y),
      new Vec2(-x, 0),
      new Vec2(-x, y),
      new Vec2(0, y),
      new Vec2(0, 0),
    ];

    const offset = new Vec2();

    switch (shape) {
      case "input":
        offset.x = -halfSizeMm;
        pts[0]!.x += halfSizeMm;
        pts[6]!.x += halfSizeMm;
        break;
      case "output":
        pts[3]!.x -= halfSizeMm;
        break;
      case "bidirectional":
      case "tri_state":
        offset.x = -halfSizeMm;
        pts[0]!.x += halfSizeMm;
        pts[6]!.x += halfSizeMm;
        pts[3]!.x -= halfSizeMm;
        break;
      default:
        break;
    }

    // Transform points - shape is centered at origin
    pts = pts.map((pt) => {
      return pt.add(offset).rotate(angle);
    });

    return pts;
  }
}

// Export convenience functions
export async function renderGlobalLabel(
  canvas: HTMLCanvasElement,
  text: string,
  options?: GlobalLabelOptions
): Promise<GlobalLabelInfo> {
  const renderer = new KicadGlobalLabelRenderer(canvas);
  await renderer.setup();

  try {
    const info = await renderer.renderLabel(text, options);
    renderer.dispose();
    return info;
  } catch (error) {
    renderer.dispose();
    throw error;
  }
}

export function getGlobalLabelInfo(
  text: string,
  options?: GlobalLabelOptions
): GlobalLabelInfo {
  const renderer = new KicadGlobalLabelRenderer(
    document.createElement("canvas")
  );

  try {
    return renderer.getLabelInfo(text, options);
  } catch (error) {
    throw error;
  }
}
