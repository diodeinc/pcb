// KiCad Symbol Renderer
// Renders a kicad_sym file to a canvas element

import { Color } from "kicanvas/base/color";
import { Angle, BBox, Matrix3, Vec2, Arc as MathArc } from "kicanvas/base/math";
import { Canvas2DRenderer } from "kicanvas/graphics/canvas2d";
import { Arc, Circle, Polygon, Polyline } from "kicanvas/graphics/shapes";
import {
  LibSymbol,
  LibText,
  Arc as SchematicArc,
  Circle as SchematicCircle,
  Rectangle,
  Polyline as SchematicPolyline,
  PinDefinition,
  type PinElectricalType,
  type PinShape,
  DefaultValues,
  Property,
  SchematicSymbol,
} from "kicanvas/kicad/schematic";
import { StrokeFont } from "kicanvas/kicad/text/stroke-font";
import { LibText as LibTextRenderer } from "kicanvas/kicad/text/lib-text";
import { SchField } from "kicanvas/kicad/text/sch-field";
import { parse_expr, P, T } from "kicanvas/kicad/parser";
import type { SchematicTheme } from "kicanvas/kicad/theme";
import { LayerNames } from "kicanvas/viewers/schematic/layers";
import {
  PinPainter,
  PinShapeInternals,
  PinLabelInternals,
  type PinInfo,
} from "kicanvas/viewers/schematic/painters/pin";
import type { SymbolTransform } from "kicanvas/viewers/schematic/painters/symbol";

// Default theme colors matching kicanvas defaults
const DEFAULT_THEME: SchematicTheme = {
  background: new Color(1, 1, 1, 1), // White background
  component_outline: new Color(0.5, 0, 0, 1), // Dark red outlines
  component_body: new Color(1, 1, 0.8, 1), // Light yellow fill
  pin: new Color(0.5, 0, 0, 1), // Dark red pins
  pin_name: new Color(0.5, 0, 0, 1),
  pin_number: new Color(0.5, 0, 0, 1),
  reference: new Color(0, 0.5, 0.5, 1), // Cyan reference
  value: new Color(0, 0.5, 0.5, 1), // Cyan value
  fields: new Color(0, 0.5, 0.5, 1), // Cyan fields
  wire: new Color(0, 0.5, 0, 1),
  bus: new Color(0, 0, 0.5, 1),
  junction: new Color(0, 0.5, 0, 1),
  label_local: new Color(0, 0, 0, 1),
  label_global: new Color(0.5, 0, 0, 1),
  label_hier: new Color(0.5, 0.25, 0, 1),
  no_connect: new Color(0, 0, 0.5, 1),
  note: new Color(0, 0, 0.5, 1),
  sheet_background: new Color(1, 1, 0.8, 1),
  sheet: new Color(0.5, 0, 0, 1),
  sheet_label: new Color(0.5, 0.25, 0, 1),
  sheet_fields: new Color(0.5, 0, 0.5, 1),
  sheet_filename: new Color(0.5, 0.25, 0, 1),
  sheet_name: new Color(0, 0.5, 0.5, 1),
  erc_warning: new Color(1, 0.5, 0, 1),
  erc_error: new Color(1, 0, 0, 1),
  grid: new Color(0.5, 0.5, 0.5, 1),
  grid_axes: new Color(0, 0, 0.5, 1),
  hidden: new Color(0.5, 0.5, 0.5, 1),
  brightened: new Color(1, 0, 1, 1),
  worksheet: new Color(0.5, 0, 0, 1),
  cursor: new Color(0, 0, 0, 1),
  aux_items: new Color(0, 0, 0, 1),
  anchor: new Color(0, 0, 1, 1),
  shadow: new Color(0.5, 0.5, 0.5, 0.5),
  bus_junction: new Color(0, 0.5, 0, 1),
};

export interface RenderOptions {
  unit?: number;
  bodyStyle?: number;
  showPinNames?: boolean;
  showPinNumbers?: boolean;
  showPinEndpoints?: boolean; // If true, render dots at pin endpoints
  showFields?: boolean; // If true, render symbol fields (Reference, Value, etc.)
  theme?: Partial<SchematicTheme>;
  scale?: number;
  padding?: number;
  tightBounds?: boolean; // If true, calculate bounds based on symbol body only
  debugBounds?: boolean; // If true, show debug visualization for bounds
  includePinTextInBounds?: boolean; // If true, include pin names/numbers in bounds calculation
}

export interface PinEndpoint {
  name: string;
  number: string;
  position: Vec2; // Position relative to symbol origin
  orientation: "right" | "left" | "up" | "down";
  type: PinElectricalType;
  shape: PinShape;
}

export interface SymbolInfo {
  bbox: BBox; // Bounding box in symbol coordinates
  pinEndpoints: PinEndpoint[]; // Array of pin endpoints
}

// Custom Canvas2DRenderer that enables alpha channel
class AlphaCanvas2DRenderer extends Canvas2DRenderer {
  override async setup() {
    const ctx2d = this.canvas.getContext("2d", {
      alpha: true, // Enable alpha channel
      desynchronized: true,
    });

    if (ctx2d == null) {
      throw new Error("Unable to create Canvas2d context");
    }

    this.ctx2d = ctx2d;
    this.update_canvas_size();
  }

  override clear_canvas() {
    this.update_canvas_size();

    this.ctx2d!.setTransform();
    this.ctx2d!.scale(window.devicePixelRatio, window.devicePixelRatio);

    // Clear with transparent background if alpha is 0
    if (this.background_color.a === 0) {
      this.ctx2d!.clearRect(0, 0, this.canvas.width, this.canvas.height);
    } else {
      this.ctx2d!.fillStyle = this.background_color.to_css();
      this.ctx2d!.fillRect(0, 0, this.canvas.width, this.canvas.height);
    }
    this.ctx2d!.lineCap = "round";
    this.ctx2d!.lineJoin = "round";
  }
}

export class KicadSymRenderer {
  private renderer: AlphaCanvas2DRenderer;
  private theme: SchematicTheme;
  private currentSymbolTransform?: SymbolTransform;
  private pinEndpoints: PinEndpoint[] = [];
  private showPinEndpoints: boolean = false;
  private showFields: boolean = false;
  private debugBounds: boolean = false;
  private debugPoints: Vec2[] = [];

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
   * Parse a kicad_sym file content and extract symbols
   */
  parseKicadSym(content: string): LibSymbol[] {
    // A kicad_sym file has the structure:
    // (kicad_symbol_lib (version ...) (generator ...)
    //   (symbol "name" ...)
    //   (symbol "name2" ...)
    // )

    const parsed = parse_expr(
      content,
      P.start("kicad_symbol_lib"),
      P.pair("version", T.number),
      P.pair("generator", T.string),
      P.collection("symbols", "symbol", T.item(LibSymbol))
    );

    return parsed["symbols"] || [];
  }

  /**
   * Get symbol information including bounding box and pin endpoints
   */
  getSymbolInfo(symbol: LibSymbol, options: RenderOptions = {}): SymbolInfo {
    const {
      unit = 1,
      bodyStyle = 1,
      tightBounds = false,
      includePinTextInBounds = false,
    } = options;

    // Create a mock SchematicSymbol for calculations
    const mockSymbol = this.createMockSchematicSymbol(symbol, unit, bodyStyle);
    this.currentSymbolTransform = this.getSymbolTransform(mockSymbol);

    // Calculate bounding box
    const bbox = this.calculateSymbolBBox(
      mockSymbol,
      tightBounds,
      includePinTextInBounds
    );

    // Collect pin endpoints
    const pinEndpoints: PinEndpoint[] = [];
    const libSymbol = symbol;

    // Helper function to process pins
    const processPins = (pins: PinDefinition[]) => {
      for (const pin of pins) {
        if (pin.unit === 0 || pin.unit === unit) {
          if (!pin.hide) {
            const pinInfo: PinInfo = {
              pin: { definition: pin, parent: mockSymbol } as any,
              def: pin,
              position: pin.at.position.copy(),
              orientation: this.angleToOrientation(pin.at.rotation),
            };

            // Apply symbol transformations
            if (this.currentSymbolTransform) {
              PinPainter.apply_symbol_transformations(
                pinInfo,
                this.currentSymbolTransform
              );
            }

            // The pinInfo.position is the outer end of the pin (connection point)
            // after transformations have been applied (includes Y-flip)
            // Keep it in the same coordinate system as the bbox
            pinEndpoints.push({
              name: pin.name.text,
              number: pin.number.text,
              position: pinInfo.position,
              orientation: pinInfo.orientation,
              type: pin.type,
              shape: pin.shape,
            });
          }
        }
      }
    };

    // Process pins from main symbol
    processPins(libSymbol.pins);

    // Also check children for pins (handles nested symbols)
    for (const child of libSymbol.children) {
      if (
        (child.unit === 0 || child.unit === unit) &&
        (child.style === 0 || child.style === bodyStyle)
      ) {
        processPins(child.pins);
      }
    }

    return {
      bbox,
      pinEndpoints,
    };
  }

  /**
   * Render a LibSymbol to the canvas
   * Returns the actual canvas dimensions
   */
  async renderSymbol(
    symbol: LibSymbol,
    options: RenderOptions = {}
  ): Promise<{ width: number; height: number }> {
    const {
      unit = 1,
      bodyStyle = 1,
      showPinEndpoints = false,
      showFields = false,
      theme = {},
      scale = 1,
      padding = 50,
      tightBounds = false,
      debugBounds = false,
    } = options;

    // Store flags
    this.showPinEndpoints = showPinEndpoints;
    this.showFields = showFields;
    this.debugBounds = debugBounds;

    // Merge theme
    this.theme = { ...DEFAULT_THEME, ...theme };

    // Create a mock SchematicSymbol for rendering
    const mockSymbol = this.createMockSchematicSymbol(symbol, unit, bodyStyle);
    this.currentSymbolTransform = this.getSymbolTransform(mockSymbol);

    // Get symbol info including pin endpoints
    const symbolInfo = this.getSymbolInfo(symbol, {
      unit,
      bodyStyle,
      tightBounds,
      includePinTextInBounds: options.includePinTextInBounds,
    });
    this.pinEndpoints = symbolInfo.pinEndpoints;
    const bbox = symbolInfo.bbox;

    // No need to transform bbox since points are already transformed
    if (this.debugBounds) {
      console.log("=== Canvas Setup ===");
      console.log("Symbol bbox (with transformed points):", bbox);
      console.log("Symbol bbox center:", bbox.center);
    }

    // Setup canvas size using the bbox directly
    const width = (bbox.w + padding * 2) * scale;
    const height = (bbox.h + padding * 2) * scale;

    this.canvas.width = width;
    this.canvas.height = height;

    // Clear canvas
    if (this.renderer.ctx2d) {
      // Always use transparent background regardless of theme
      this.renderer.background_color = new Color(0, 0, 0, 0); // Fully transparent
      this.renderer.clear_canvas();
    }

    // Setup transformation to center the symbol in the canvas
    const ctx = this.renderer.ctx2d!;
    ctx.save();

    // Move to center of canvas
    ctx.translate(width / 2, height / 2);

    // Apply scale
    ctx.scale(scale, scale);

    // The symbol transform will flip Y axis
    // Center the symbol by translating to its center
    // Use the original bbox center since the transform will handle the flip
    ctx.translate(-bbox.center.x, -bbox.center.y);

    // Start rendering
    this.renderer.start_layer("symbol");

    // Render using kicanvas approach
    this.renderLibSymbol(mockSymbol);

    // Render pin endpoints if requested
    if (this.showPinEndpoints) {
      this.renderPinEndpoints();
    }

    // Render debug bounds if requested
    if (this.debugBounds) {
      this.renderDebugBounds(bbox);
    }

    // End rendering
    const layer = this.renderer.end_layer();
    layer.render(Matrix3.identity(), 0);

    ctx.restore();

    // Return the actual canvas dimensions
    return { width, height };
  }

  private createMockSchematicSymbol(
    libSymbol: LibSymbol,
    unit: number,
    bodyStyle: number
  ): SchematicSymbol {
    // Create a mock SchematicSymbol that wraps the LibSymbol
    // We need to create a minimal expression that the constructor can parse
    const mockExpr = [
      "symbol",
      ["lib_id", libSymbol.name],
      ["at", 0, 0, 0], // Fixed: at expects numbers directly, not strings
      ["unit", unit],
      ["convert", bodyStyle],
      ["uuid", "mock-uuid"],
      [
        "default_instance",
        ["reference", "U?"],
        ["unit", "1"],
        ["value", libSymbol.name],
        ["footprint", ""],
      ],
    ];

    // Create a mock parent with resolve_text_var method
    const mockParent = {
      lib_symbols: {
        by_name: (name: string): LibSymbol | undefined =>
          name === libSymbol.name ? libSymbol : undefined,
      },
      resolve_text_var: (name: string): undefined => undefined,
    } as any;

    const symbol = new SchematicSymbol(mockExpr, mockParent);

    // Override properties from the LibSymbol
    symbol.properties = new Map(libSymbol.properties);

    return symbol;
  }

  private getSymbolTransform(symbol: SchematicSymbol): SymbolTransform {
    // For standalone rendering, we need the standard KiCad transform
    // which flips the Y axis
    const zero_deg_matrix = new Matrix3([1, 0, 0, 0, -1, 0, 0, 0, 1]);

    return {
      matrix: zero_deg_matrix,
      position: symbol.at.position,
      rotations: 0,
      mirror_x: false,
      mirror_y: false,
    };
  }

  private renderLibSymbol(symbol: SchematicSymbol) {
    const libSymbol = symbol.lib_symbol;
    const unit = symbol.unit || 1;
    const bodyStyle = symbol.convert || 1;

    // Apply symbol transformation
    this.renderer.state.push();
    this.renderer.state.matrix = this.currentSymbolTransform!.matrix;

    // Render background fills first
    // First render common unit (0)
    this.renderSymbolLayer(
      libSymbol,
      0,
      bodyStyle,
      LayerNames.symbol_background
    );
    // Then render specific unit
    this.renderSymbolLayer(
      libSymbol,
      unit,
      bodyStyle,
      LayerNames.symbol_background
    );

    // Then render foreground strokes
    // First render common unit (0)
    this.renderSymbolLayer(
      libSymbol,
      0,
      bodyStyle,
      LayerNames.symbol_foreground
    );
    // Then render specific unit
    this.renderSymbolLayer(
      libSymbol,
      unit,
      bodyStyle,
      LayerNames.symbol_foreground
    );

    // Pop transformation before rendering pins and properties
    this.renderer.state.pop();

    // Render pins
    this.renderPins(symbol);

    // Render properties (fields) if enabled
    if (this.showFields) {
      this.renderProperties(symbol);
    }
  }

  private renderSymbolLayer(
    libSymbol: LibSymbol,
    unit: number,
    bodyStyle: number,
    layerName: LayerNames
  ) {
    // For unit 0 (common), render drawings from the main symbol
    if (unit === 0 || libSymbol.units.size === 0) {
      for (const drawing of libSymbol.drawings) {
        this.renderDrawing(drawing, layerName);
      }
    }

    // For specific units, check the children
    for (const child of libSymbol.children) {
      if (
        (child.unit === 0 || child.unit === unit) &&
        (child.style === 0 || child.style === bodyStyle)
      ) {
        for (const drawing of child.drawings) {
          this.renderDrawing(drawing, layerName);
        }
      }
    }
  }

  private renderDrawing(drawing: any, layerName: LayerNames) {
    if (drawing instanceof SchematicArc) {
      this.renderArc(drawing, layerName);
    } else if (drawing instanceof SchematicCircle) {
      this.renderCircle(drawing, layerName);
    } else if (drawing instanceof Rectangle) {
      this.renderRectangle(drawing, layerName);
    } else if (drawing instanceof SchematicPolyline) {
      this.renderPolyline(drawing, layerName);
    } else if (drawing instanceof LibText) {
      if (layerName === LayerNames.symbol_foreground) {
        this.renderLibText(drawing);
      }
    }
  }

  private renderArc(arc: SchematicArc, layerName: LayerNames) {
    const mathArc = MathArc.from_three_points(
      arc.start,
      arc.mid,
      arc.end,
      arc.stroke?.width || DefaultValues.line_width
    );

    const { width, color } = this.determineStroke(layerName, arc);
    const fillColor = this.determineFill(layerName, arc);

    if (fillColor && layerName === LayerNames.symbol_background) {
      this.renderer.polygon(new Polygon(mathArc.to_polygon(), fillColor));
    }

    if (width && color && layerName === LayerNames.symbol_foreground) {
      this.renderer.arc(
        new Arc(
          mathArc.center,
          mathArc.radius,
          mathArc.start_angle,
          mathArc.end_angle,
          width,
          color
        )
      );
    }
  }

  private renderCircle(circle: SchematicCircle, layerName: LayerNames) {
    const { width, color } = this.determineStroke(layerName, circle);
    const fillColor = this.determineFill(layerName, circle);

    if (fillColor && layerName === LayerNames.symbol_background) {
      this.renderer.circle(new Circle(circle.center, circle.radius, fillColor));
    }

    if (width && color && layerName === LayerNames.symbol_foreground) {
      this.renderer.arc(
        new Arc(
          circle.center,
          circle.radius,
          new Angle(0),
          new Angle(Math.PI * 2),
          width,
          color
        )
      );
    }
  }

  private renderRectangle(rect: Rectangle, layerName: LayerNames) {
    const pts = [
      rect.start,
      new Vec2(rect.end.x, rect.start.y),
      rect.end,
      new Vec2(rect.start.x, rect.end.y),
      rect.start,
    ];

    const { width, color } = this.determineStroke(layerName, rect);
    const fillColor = this.determineFill(layerName, rect);

    if (fillColor && layerName === LayerNames.symbol_background) {
      this.renderer.polygon(new Polygon(pts.slice(0, -1), fillColor));
    }

    if (width && color && layerName === LayerNames.symbol_foreground) {
      this.renderer.line(new Polyline(pts, width, color));
    }
  }

  private renderPolyline(polyline: SchematicPolyline, layerName: LayerNames) {
    const { width, color } = this.determineStroke(layerName, polyline);
    const fillColor = this.determineFill(layerName, polyline);

    if (fillColor && layerName === LayerNames.symbol_background) {
      this.renderer.polygon(new Polygon(polyline.pts, fillColor));
    }

    if (width && color && layerName === LayerNames.symbol_foreground) {
      this.renderer.line(new Polyline(polyline.pts, width, color));
    }
  }

  private renderLibText(text: LibText) {
    if (text.effects.hide || !text.text) {
      return;
    }

    const libtext = new LibTextRenderer(text.shown_text);
    libtext.apply_effects(text.effects);
    libtext.apply_at(text.at);

    if (this.currentSymbolTransform) {
      libtext.apply_symbol_transformations(this.currentSymbolTransform);
    }

    libtext.attributes.color = this.theme.component_outline;

    // Get the absolute world coordinates where the text should be drawn
    const pos = libtext.world_pos;

    // world_pos already applies v_align, so set it to center
    libtext.attributes.v_align = "center";

    this.renderer.state.push();
    this.renderer.state.matrix = Matrix3.identity();

    StrokeFont.default().draw(
      this.renderer,
      libtext.shown_text,
      pos,
      libtext.attributes
    );

    this.renderer.state.pop();
  }

  private renderPins(symbol: SchematicSymbol) {
    const libSymbol = symbol.lib_symbol;
    const unit = symbol.unit || 1;
    const bodyStyle = symbol.convert || 1;

    // Render pins from main symbol
    for (const pin of libSymbol.pins) {
      if (pin.unit === 0 || pin.unit === unit) {
        this.renderPin(pin, symbol);
      }
    }

    // Also check children for pins (handles nested symbols like LED_1_1)
    for (const child of libSymbol.children) {
      if (
        (child.unit === 0 || child.unit === unit) &&
        (child.style === 0 || child.style === bodyStyle)
      ) {
        for (const pin of child.pins) {
          this.renderPin(pin, symbol);
        }
      }
    }
  }

  private renderPin(pin: PinDefinition, symbol: SchematicSymbol) {
    if (pin.hide) {
      return;
    }

    const pinInfo: PinInfo = {
      pin: { definition: pin, parent: symbol } as any,
      def: pin,
      position: pin.at.position.copy(),
      orientation: this.angleToOrientation(pin.at.rotation),
    };

    if (this.currentSymbolTransform) {
      PinPainter.apply_symbol_transformations(
        pinInfo,
        this.currentSymbolTransform
      );
    }

    const color = this.theme.pin;
    const strokeWidth = DefaultValues.line_width; // Use consistent stroke width

    this.renderer.state.push();
    this.renderer.state.matrix = Matrix3.identity();
    this.renderer.state.stroke = color;
    this.renderer.state.stroke_width = strokeWidth; // Set consistent stroke width

    // Draw pin shape
    const { p0, dir } = PinShapeInternals.stem(
      pinInfo.position,
      pinInfo.orientation,
      pin.length
    );

    PinShapeInternals.draw(
      this.renderer,
      pin.type,
      pin.shape,
      pinInfo.position,
      p0,
      dir
    );

    // Draw pin name and number
    this.drawPinLabels(pinInfo, symbol);

    this.renderer.state.pop();
  }

  private drawPinLabels(pinInfo: PinInfo, symbol: SchematicSymbol) {
    const def = pinInfo.def;
    const libsym = symbol.lib_symbol;
    const name = def.name.text;
    const number = def.number.text;
    const pin_length = def.length;
    const hide_pin_names = libsym.pin_names.hide || !name || name === "~";
    const hide_pin_numbers =
      libsym.pin_numbers.hide || !number || number === "~";
    const pin_thickness = DefaultValues.line_width;
    const pin_name_offset = libsym.pin_names.offset;
    const text_margin = 0.6096 * DefaultValues.text_offset_ratio;
    const num_thickness = def.number.effects.font.thickness || pin_thickness;
    const name_thickness = def.name.effects.font.thickness || pin_thickness;

    let name_placement;
    let num_placement;

    if (pin_name_offset > 0) {
      // Names inside, numbers above
      name_placement = hide_pin_names
        ? undefined
        : PinLabelInternals.place_inside(
            pin_name_offset,
            name_thickness,
            pin_length,
            pinInfo.orientation
          );
      num_placement = hide_pin_numbers
        ? undefined
        : PinLabelInternals.place_above(
            text_margin,
            pin_thickness,
            num_thickness,
            pin_length,
            pinInfo.orientation
          );
    } else {
      // Names above, numbers below
      name_placement = hide_pin_names
        ? undefined
        : PinLabelInternals.place_above(
            text_margin,
            pin_thickness,
            name_thickness,
            pin_length,
            pinInfo.orientation
          );
      num_placement = hide_pin_numbers
        ? undefined
        : PinLabelInternals.place_below(
            text_margin,
            pin_thickness,
            name_thickness,
            pin_length,
            pinInfo.orientation
          );
    }

    if (name_placement) {
      PinLabelInternals.draw(
        this.renderer,
        name,
        pinInfo.position,
        name_placement,
        def.name.effects,
        this.theme.pin_name
      );
    }

    if (num_placement) {
      PinLabelInternals.draw(
        this.renderer,
        number,
        pinInfo.position,
        num_placement,
        def.number.effects,
        this.theme.pin_number
      );
    }
  }

  private renderProperties(symbol: SchematicSymbol) {
    for (const [name, property] of symbol.properties) {
      if (!property.effects.hide && property.text) {
        this.renderProperty(property, name, symbol);
      }
    }
  }

  private renderProperty(
    property: Property,
    name: string,
    symbol: SchematicSymbol
  ) {
    let color = this.theme.fields;

    switch (name) {
      case "Reference":
        color = this.theme.reference;
        break;
      case "Value":
        color = this.theme.value;
        break;
    }

    const transform = this.currentSymbolTransform;
    const matrix = transform?.matrix ?? Matrix3.identity();

    let text = property.shown_text;
    if (name === "Reference" && symbol.unit) {
      text += String.fromCharCode(64 + symbol.unit); // A, B, C, etc.
    }

    const schfield = new SchField(text, {
      position: symbol.at.position.multiply(10000),
      transform: matrix,
      is_symbol: true,
    });

    schfield.apply_effects(property.effects);
    schfield.attributes.angle = Angle.from_degrees(property.at.rotation);

    // Position relative to parent
    let rel_position = property.at.position
      .multiply(10000)
      .sub(schfield.parent!.position);
    rel_position = matrix.inverse().transform(rel_position);
    rel_position = rel_position.add(schfield.parent!.position);

    schfield.text_pos = rel_position;

    const orient = schfield.draw_rotation;
    const bbox = schfield.bounding_box;
    const pos = bbox.center;

    schfield.attributes.angle = orient;
    schfield.attributes.h_align = "center";
    schfield.attributes.v_align = "center";
    schfield.attributes.stroke_width = schfield.get_effective_text_thickness(
      DefaultValues.line_width * 10000
    );
    schfield.attributes.color = color;

    this.renderer.state.push();
    StrokeFont.default().draw(
      this.renderer,
      schfield.shown_text,
      pos,
      schfield.attributes
    );
    this.renderer.state.pop();
  }

  private angleToOrientation(
    angleDeg: number
  ): "right" | "left" | "up" | "down" {
    switch (angleDeg) {
      case 0:
        return "right";
      case 90:
        return "up";
      case 180:
        return "left";
      case 270:
        return "down";
      default:
        return "right";
    }
  }

  private determineStroke(layer: LayerNames, item: any) {
    const width = item.stroke?.width || DefaultValues.line_width;

    if (width < 0) {
      return { width: 0, color: null };
    }

    // For symbol foreground, default to "default" stroke type, otherwise "none"
    const stroke_type =
      item.stroke?.type ??
      (layer === LayerNames.symbol_foreground ? "default" : "none");

    if (stroke_type === "none") {
      return { width: 0, color: null };
    }

    const default_stroke =
      layer === LayerNames.symbol_foreground
        ? this.theme.component_outline
        : this.theme.note;

    const color = item.stroke?.color ?? default_stroke;

    return { width, color };
  }

  private determineFill(layer: LayerNames, item: any) {
    const fill_type = item.fill?.type ?? "background"; // Default to "background" instead of "none"

    if (fill_type === "none") {
      return null;
    }

    if (fill_type === "background" && layer !== LayerNames.symbol_background) {
      return null;
    }

    let color;

    switch (fill_type) {
      case "background":
        color = this.theme.component_body;
        break;
      case "outline":
        color = this.theme.component_outline;
        break;
      case "color":
        color = item.fill!.color;
        break;
    }

    return color;
  }

  private calculateSymbolBBox(
    symbol: SchematicSymbol,
    tightBounds: boolean = false,
    includePinTextInBounds: boolean = false
  ): BBox {
    const points: Vec2[] = [];
    const libSymbol = symbol.lib_symbol;
    const unit = symbol.unit || 1;
    const bodyStyle = symbol.convert || 1;

    // Clear debug points
    this.debugPoints = [];

    if (this.debugBounds) {
      console.log("=== Calculating Symbol BBox ===");
      console.log("Symbol:", libSymbol.name);
      console.log("Unit:", unit, "BodyStyle:", bodyStyle);
      console.log("TightBounds:", tightBounds);
    }

    // Collect points from all drawings
    const collectDrawingPoints = (drawing: any) => {
      const transform =
        this.currentSymbolTransform?.matrix || Matrix3.identity();

      if (drawing instanceof SchematicArc) {
        const arc = MathArc.from_three_points(
          drawing.start,
          drawing.mid,
          drawing.end
        );
        const arcPoints = arc.to_polyline();
        // Transform arc points
        const transformedPoints = arcPoints.map((p) => transform.transform(p));
        points.push(...transformedPoints);
        if (this.debugBounds) {
          console.log("Arc points:", arcPoints.length, "points");
        }
      } else if (drawing instanceof SchematicCircle) {
        const r = drawing.radius;
        const c = drawing.center;
        const circlePoints = [
          c.add(new Vec2(r, r)),
          c.add(new Vec2(-r, r)),
          c.add(new Vec2(r, -r)),
          c.add(new Vec2(-r, -r)),
        ];
        // Transform circle points
        const transformedPoints = circlePoints.map((p) =>
          transform.transform(p)
        );
        points.push(...transformedPoints);
        if (this.debugBounds) {
          console.log("Circle center:", c, "radius:", r);
          console.log("Circle bounds (transformed):", transformedPoints);
        }
      } else if (drawing instanceof Rectangle) {
        // Apply the symbol transformation to the rectangle points
        const transform =
          this.currentSymbolTransform?.matrix || Matrix3.identity();
        const rectPoints = [
          drawing.start,
          drawing.end,
          new Vec2(drawing.start.x, drawing.end.y),
          new Vec2(drawing.end.x, drawing.start.y),
        ];

        // Transform each point using the symbol transformation matrix
        const transformedPoints = rectPoints.map((p) => transform.transform(p));
        points.push(...transformedPoints);

        if (this.debugBounds) {
          console.log("Rectangle start:", drawing.start, "end:", drawing.end);
          console.log("Rectangle corners (original):", rectPoints);
          console.log("Rectangle corners (transformed):", transformedPoints);
          console.log(
            "Rectangle min Y (original):",
            Math.min(drawing.start.y, drawing.end.y)
          );
          console.log(
            "Rectangle max Y (original):",
            Math.max(drawing.start.y, drawing.end.y)
          );
          const minY = Math.min(...transformedPoints.map((p) => p.y));
          const maxY = Math.max(...transformedPoints.map((p) => p.y));
          console.log("Rectangle min/max Y (transformed):", minY, maxY);
        }
      } else if (drawing instanceof SchematicPolyline) {
        // Transform polyline points
        const transformedPoints = drawing.pts.map((p) =>
          transform.transform(p)
        );
        points.push(...transformedPoints);
        if (this.debugBounds) {
          console.log("Polyline:", drawing.pts.length, "points");
        }
      } else if (drawing instanceof LibText) {
        // For text, transform the position
        const transformedPos = transform.transform(drawing.at.position);
        points.push(transformedPos);
        if (this.debugBounds) {
          console.log(
            "Text at (original):",
            drawing.at.position,
            "text:",
            drawing.text
          );
          console.log("Text at (transformed):", transformedPos);
        }
      }
    };

    // Collect from main symbol drawings (common to all units)
    for (const drawing of libSymbol.drawings) {
      collectDrawingPoints(drawing);
    }

    // Collect from children for specific unit/style
    for (const child of libSymbol.children) {
      if (
        (child.unit === 0 || child.unit === unit) &&
        (child.style === 0 || child.style === bodyStyle)
      ) {
        for (const drawing of child.drawings) {
          collectDrawingPoints(drawing);
        }
      }
    }

    // Pins - only include if not using tight bounds
    if (!tightBounds) {
      // Helper function to process pins
      const processPinBounds = (pins: PinDefinition[]) => {
        for (const pin of pins) {
          if (pin.unit === 0 || pin.unit === unit) {
            const pinInfo: PinInfo = {
              pin: { definition: pin, parent: symbol } as any,
              def: pin,
              position: pin.at.position.copy(),
              orientation: this.angleToOrientation(pin.at.rotation),
            };

            // Apply symbol transformations
            if (this.currentSymbolTransform) {
              PinPainter.apply_symbol_transformations(
                pinInfo,
                this.currentSymbolTransform
              );
            }

            // Include the outer pin position (connection point)
            points.push(pinInfo.position);

            // Also include the inner end for complete bounds
            const { p0 } = PinShapeInternals.stem(
              pinInfo.position,
              pinInfo.orientation,
              pin.length
            );
            points.push(p0);

            // Include pin text bounds if requested
            // if (
            //   includePinTextInBounds &&
            //   (this.showPinNames || this.showPinNumbers)
            // ) {
            //   const textPoints = this.calculatePinTextBounds(pinInfo, symbol);
            //   points.push(...textPoints);
            // }
          }
        }
      };

      // Process pins from main symbol
      processPinBounds(libSymbol.pins);

      // Also check children for pins (handles nested symbols)
      for (const child of libSymbol.children) {
        if (
          (child.unit === 0 || child.unit === unit) &&
          (child.style === 0 || child.style === bodyStyle)
        ) {
          processPinBounds(child.pins);
        }
      }
    }

    // If no points found, create a default bbox
    if (points.length === 0) {
      return new BBox(-10, -10, 20, 20);
    }

    // Store debug points
    this.debugPoints = [...points];

    const bbox = BBox.from_points(points);

    // Expand the bbox by a small amount to prevent border clipping
    const expansionAmount = 0.1; // Expand by 0.1 pixels in each direction
    const expandedBBox = new BBox(
      bbox.x - expansionAmount,
      bbox.y - expansionAmount,
      bbox.w + expansionAmount * 2,
      bbox.h + expansionAmount * 2
    );

    if (this.debugBounds) {
      console.log("Total points:", points.length);
      console.log(
        "Min point:",
        points.reduce(
          (min, p) => new Vec2(Math.min(min.x, p.x), Math.min(min.y, p.y)),
          new Vec2(Infinity, Infinity)
        )
      );
      console.log(
        "Max point:",
        points.reduce(
          (max, p) => new Vec2(Math.max(max.x, p.x), Math.max(max.y, p.y)),
          new Vec2(-Infinity, -Infinity)
        )
      );
      console.log("Calculated BBox:", bbox);
      console.log("Expanded BBox:", expandedBBox);
      console.log("BBox center:", expandedBBox.center);
      console.log("=== End BBox Calculation ===");
    }

    // Return expanded bbox to prevent border clipping
    return expandedBBox;
  }

  private renderPinEndpoints() {
    // Render small dots at each pin endpoint
    const dotRadius = 0.5; // Small dot radius
    const dotColor = new Color(1, 0, 0, 1); // Red color for visibility

    this.renderer.state.push();
    this.renderer.state.matrix = Matrix3.identity();

    for (const endpoint of this.pinEndpoints) {
      // Draw a filled circle at the pin endpoint
      this.renderer.circle(new Circle(endpoint.position, dotRadius, dotColor));

      // Also draw a small outline for better visibility
      this.renderer.arc(
        new Arc(
          endpoint.position,
          dotRadius,
          new Angle(0),
          new Angle(Math.PI * 2),
          0.1,
          new Color(0, 0, 0, 1) // Black outline
        )
      );
    }

    this.renderer.state.pop();
  }

  private renderDebugBounds(bbox: BBox) {
    this.renderer.state.push();
    this.renderer.state.matrix = Matrix3.identity();

    // Draw the bounding box
    const boxColor = new Color(0, 0, 1, 0.3); // Semi-transparent blue
    const boxStrokeColor = new Color(0, 0, 1, 1); // Blue

    // Draw filled rectangle
    const boxPts = [
      new Vec2(bbox.x, bbox.y),
      new Vec2(bbox.x + bbox.w, bbox.y),
      new Vec2(bbox.x + bbox.w, bbox.y + bbox.h),
      new Vec2(bbox.x, bbox.y + bbox.h),
    ];
    this.renderer.polygon(new Polygon(boxPts, boxColor));

    // Draw outline
    this.renderer.line(
      new Polyline([...boxPts, boxPts[0]], 0.2, boxStrokeColor)
    );

    // Draw center cross
    const centerColor = new Color(1, 0, 0, 1); // Red
    const crossSize = 2;
    this.renderer.line(
      new Polyline(
        [
          new Vec2(bbox.center.x - crossSize, bbox.center.y),
          new Vec2(bbox.center.x + crossSize, bbox.center.y),
        ],
        0.2,
        centerColor
      )
    );
    this.renderer.line(
      new Polyline(
        [
          new Vec2(bbox.center.x, bbox.center.y - crossSize),
          new Vec2(bbox.center.x, bbox.center.y + crossSize),
        ],
        0.2,
        centerColor
      )
    );

    // Draw all the points that contributed to the bbox
    const pointColor = new Color(0, 1, 0, 1); // Green
    for (const pt of this.debugPoints) {
      this.renderer.circle(new Circle(pt, 0.3, pointColor));
    }

    // Draw coordinate axes at origin
    const axisColor = new Color(0.5, 0.5, 0.5, 1); // Gray
    const axisLength = 5;
    // X axis
    this.renderer.line(
      new Polyline(
        [new Vec2(-axisLength, 0), new Vec2(axisLength, 0)],
        0.1,
        axisColor
      )
    );
    // Y axis
    this.renderer.line(
      new Polyline(
        [new Vec2(0, -axisLength), new Vec2(0, axisLength)],
        0.1,
        axisColor
      )
    );

    this.renderer.state.pop();
  }
}

// Export a simple function for easy usage
export async function renderKicadSymbol(
  canvas: HTMLCanvasElement,
  kicadSymContent: string,
  symbolName?: string,
  options?: RenderOptions
): Promise<{ width: number; height: number }> {
  const renderer = new KicadSymRenderer(canvas);
  await renderer.setup();

  try {
    const symbols = renderer.parseKicadSym(kicadSymContent);
    if (symbols.length === 0) {
      throw new Error("No symbols found in file");
    }

    // Find the symbol to render
    let symbol: LibSymbol | undefined;
    if (symbolName) {
      symbol = symbols.find((s) => s.name === symbolName) || symbols[0];
    } else {
      symbol = symbols[0];
    }

    if (!symbol) {
      throw new Error("No symbol found to render");
    }

    const dimensions = await renderer.renderSymbol(symbol, options);
    renderer.dispose();
    return dimensions;
  } catch (error) {
    renderer.dispose();
    throw error;
  }
}

// Export a function to get symbol info without rendering
export function getKicadSymbolInfo(
  kicadSymContent: string,
  symbolName?: string,
  options?: RenderOptions
): SymbolInfo {
  const renderer = new KicadSymRenderer(document.createElement("canvas"));

  try {
    const symbols = renderer.parseKicadSym(kicadSymContent);
    if (symbols.length === 0) {
      throw new Error("No symbols found in file");
    }

    // Find the symbol
    let symbol: LibSymbol | undefined;
    if (symbolName) {
      symbol = symbols.find((s) => s.name === symbolName) || symbols[0];
    } else {
      symbol = symbols[0];
    }

    if (!symbol) {
      throw new Error("No symbol found");
    }

    return renderer.getSymbolInfo(symbol, options);
  } catch (error) {
    throw error;
  }
}
