import {
  sexpr,
  quoted,
  atom,
  xy,
  at,
  property,
  uuid,
  SExpr,
  SExprString,
  SExprAtom,
} from "./sexpr";
import type { ElkGraph, ElkNode } from "./LayoutEngine";
import { NodeType, snapToGrid } from "./LayoutEngine";
import { v5 as uuidv5 } from "uuid";

interface ParsedProperty {
  name: string;
  value: string;
  at: { x: number; y: number; angle: number };
  effects?: any;
  hide?: boolean;
}

export class KicadSchematicGenerator {
  private symbolCounter = 0;
  private usedSymbols = new Map<string, string>(); // Map from lib_id to symbol content
  private parsedSymbols = new Map<string, Map<string, ParsedProperty>>(); // Map from lib_id to property map

  // UUID namespace for URL (same as Rust's Uuid::NAMESPACE_URL)
  private static readonly UUID_NAMESPACE_URL =
    "6ba7b811-9dad-11d1-80b4-00c04fd430c8";

  generate(graph: ElkGraph, fullSchematic: boolean = false): string {
    if (fullSchematic) {
      return this.generateFullSchematic(graph);
    } else {
      // Generate the snippet containing lib_symbols and symbols
      const elements = this.generateSchematicElements(graph);
      return elements.map((expr) => expr.toString({ pretty: true })).join("\n");
    }
  }

  generateFullSchematic(graph: ElkGraph): string {
    // Generate a UUID for the schematic based on the current timestamp
    const schematicUuid = this.generateUUID(`schematic_${Date.now()}`);

    // Create the root kicad_sch expression
    const kicadSch = sexpr(
      "kicad_sch",
      sexpr("version", 20250114),
      sexpr("generator", quoted("eeschema")),
      sexpr("generator_version", quoted("9.0")),
      uuid(schematicUuid),
      sexpr("paper", quoted("A4"))
    );

    // Get all schematic elements
    const elements = this.generateSchematicElements(graph);

    // Add all elements to the root expression
    elements.forEach((element) => kicadSch.add(element));

    // Add sheet instances
    kicadSch.add(
      sexpr(
        "sheet_instances",
        sexpr("path", quoted("/"), sexpr("page", quoted("1")))
      )
    );

    // Add embedded fonts flag
    kicadSch.add(sexpr("embedded_fonts", atom("no")));

    // Convert to string with pretty printing
    return kicadSch.toString({ pretty: true });
  }

  private collectUsedSymbols(graph: ElkGraph): void {
    const collectFromNodes = (nodes: ElkNode[] = []) => {
      for (const node of nodes) {
        if (node.type === NodeType.SYMBOL) {
          // Check if this is a net symbol
          const isNetSymbol = node.properties?.isNetSymbol === "true";

          if (isNetSymbol && node.net) {
            // For net symbols, get the symbol content from the net
            const symbolContent = this.getNetSymbolContent(node);
            if (symbolContent) {
              const libId = this.getNetSymbolLibId(node);
              if (libId) {
                this.usedSymbols.set(libId, symbolContent);
                // Parse the symbol to extract property positions
                this.parseSymbolProperties(libId, symbolContent);
              }
            }
          } else if (node.instance) {
            // For regular component symbols
            const symbolContent = this.getSymbolContent(node);
            if (symbolContent) {
              const libId = this.getLibId(node);
              if (libId) {
                this.usedSymbols.set(libId, symbolContent);
                // Parse the symbol to extract property positions
                this.parseSymbolProperties(libId, symbolContent);
              }
            }
          }
        }

        // Recursively collect from children
        if (node.children) {
          collectFromNodes(node.children);
        }
      }
    };

    collectFromNodes(graph.children);
  }

  private parseSymbolProperties(libId: string, symbolContent: string): void {
    try {
      const symbolExpr = SExpr.parse(symbolContent);
      const properties = new Map<string, ParsedProperty>();

      // Find all property expressions in the symbol
      const findProperties = (expr: SExpr) => {
        if (expr.name === "property" && expr.values.length >= 3) {
          // Property format: (property "name" "value" (at x y angle) ...)
          const name = this.unquote(expr.values[0]);
          const value = this.unquote(expr.values[1]);

          // Find the "at" expression
          let at = { x: 0, y: 0, angle: 0 };
          let effects: any = null;
          let hide = false;

          for (const val of expr.values.slice(2)) {
            if (val instanceof SExpr) {
              if (val.name === "at" && val.values.length >= 2) {
                at.x = Number(val.values[0]) || 0;
                at.y = Number(val.values[1]) || 0;
                at.angle = Number(val.values[2]) || 0;
              } else if (val.name === "effects") {
                effects = val;
                // Check if hide is set in effects
                for (const effect of val.values) {
                  if (
                    effect instanceof SExpr &&
                    effect.name === "hide" &&
                    effect.values[0] instanceof SExprAtom &&
                    (effect.values[0] as SExprAtom).value === "yes"
                  ) {
                    hide = true;
                  }
                }
              }
            }
          }

          properties.set(name, { name, value, at, effects, hide });
        }

        // Recursively search in child expressions
        for (const val of expr.values) {
          if (val instanceof SExpr) {
            findProperties(val);
          }
        }
      };

      findProperties(symbolExpr);
      this.parsedSymbols.set(libId, properties);
    } catch (error) {
      console.error(`Failed to parse symbol properties for ${libId}:`, error);
    }
  }

  private unquote(value: any): string {
    if (value instanceof SExprString) {
      return value.value;
    }
    if (typeof value === "string") {
      // Remove quotes if present
      if (value.startsWith('"') && value.endsWith('"')) {
        return value.slice(1, -1);
      }
      return value;
    }
    return String(value);
  }

  private getSymbolContent(node: ElkNode): string | null {
    if (!node.instance) return null;

    // Check for __symbol_value attribute
    const symbolValueAttr = node.instance.attributes.__symbol_value;

    if (typeof symbolValueAttr === "string") {
      return symbolValueAttr;
    } else if (
      symbolValueAttr &&
      typeof symbolValueAttr === "object" &&
      "String" in symbolValueAttr
    ) {
      return (symbolValueAttr as any).String;
    }

    return null;
  }

  private getNetSymbolContent(node: ElkNode): string | null {
    if (!node.net) return null;

    // Access the net information directly from the node
    const net = node.net;
    const symbolValueAttr = net.properties?.__symbol_value;

    if (typeof symbolValueAttr === "string") {
      return symbolValueAttr;
    } else if (
      symbolValueAttr &&
      typeof symbolValueAttr === "object" &&
      "String" in symbolValueAttr
    ) {
      return (symbolValueAttr as any).String;
    }

    return null;
  }

  private getLibId(node: ElkNode): string | null {
    if (!node.instance) return null;

    // Check for symbol_path attribute
    const symbolPath = this.getAttributeValue(
      node.instance.attributes.symbol_path
    );
    const symbolName = this.getAttributeValue(
      node.instance.attributes.symbol_name
    );

    if (symbolPath && symbolName) {
      // Parse the symbol path to extract library name from .kicad_sym filename
      // Expected format: "path/to/Device.kicad_sym"
      const match = symbolPath.match(/([^/]+)\.kicad_sym$/);
      if (match) {
        const libraryName = match[1]; // e.g., "Device"
        return `${libraryName}:${symbolName}`;
      }
    }

    return null;
  }

  private getNetSymbolLibId(node: ElkNode): string | null {
    if (!node.net) return null;

    // First check for symbol_path and symbol_name attributes in net properties
    const symbolPath = this.getAttributeValue(node.net.properties?.symbol_path);
    const symbolName = this.getAttributeValue(node.net.properties?.symbol_name);

    if (symbolPath && symbolName) {
      // Parse the symbol path to extract library name from .kicad_sym filename
      // Expected format: "path/to/Device.kicad_sym"
      const match = symbolPath.match(/([^/]+)\.kicad_sym$/);
      if (match) {
        const libraryName = match[1]; // e.g., "Device"
        return `${libraryName}:${symbolName}`;
      }
    }

    return null;
  }

  private getAttributeValue(attr: any): string | null {
    if (typeof attr === "string") return attr;
    if (attr && typeof attr === "object" && "String" in attr) {
      return attr.String;
    }
    return null;
  }

  private transformFootprintPath(footprintPath: string): string {
    // Transform footprint path from "path/to/something.pretty/file.kicad_mod" to "something:file"
    const match = footprintPath.match(/([^/]+)\.pretty\/([^/]+)\.kicad_mod$/);
    if (match) {
      const libraryName = match[1];
      const footprintName = match[2];
      return `${libraryName}:${footprintName}`;
    }
    // If it doesn't match the pattern, return as-is
    return footprintPath;
  }

  private generateLibSymbols(): SExpr {
    const libSymbols = sexpr("lib_symbols");

    // Add each used symbol
    for (const [libId, symbolContent] of this.usedSymbols) {
      try {
        // Parse the symbol content
        const symbolExpr = SExpr.parse(symbolContent);

        // Ensure the symbol has the correct lib_id
        // The parsed symbol might not have the lib_id in the expected format
        // so we need to update it
        if (symbolExpr.name === "symbol") {
          // Update the first value (the symbol name) to match our lib_id
          if (symbolExpr.values.length > 0) {
            symbolExpr.values[0] = quoted(libId);
          } else {
            symbolExpr.values.unshift(quoted(libId));
          }

          // Remove any pin_names structure
          symbolExpr.removeWhere((value) => {
            return value instanceof SExpr && value.name === "pin_names";
          });
        }

        libSymbols.add(symbolExpr);
      } catch (error) {
        console.error(`Failed to parse symbol content for ${libId}:`, error);
      }
    }

    return libSymbols;
  }

  private generateSchematicElements(graph: ElkGraph): SExpr[] {
    const result: SExpr[] = [];

    // Collect all symbols used in the graph
    this.collectUsedSymbols(graph);

    // Generate lib_symbols section
    const libSymbols = this.generateLibSymbols();
    if (libSymbols.values.length > 0) {
      result.push(libSymbols);
    }

    // Generate symbol instances
    const symbols = this.generateSymbols(graph);
    result.push(...symbols);

    // Generate wires (edges)
    const wires = this.generateWires(graph);
    result.push(...wires);

    // Generate junctions
    const junctions = this.generateJunctions(graph);
    result.push(...junctions);

    // Generate global labels for port net references
    const globalLabels = this.generateGlobalLabels(graph);
    result.push(...globalLabels);

    return result;
  }

  private generateSymbols(graph: ElkGraph): SExpr[] {
    const symbols: SExpr[] = [];

    // The graph structure from LayoutEngine is flat - all nodes are at the top level
    // and their positions are already absolute, not relative to any parent
    if (graph.children) {
      for (const node of graph.children) {
        if (node.type === NodeType.SYMBOL) {
          const isNetSymbol = node.properties?.isNetSymbol === "true";

          if (isNetSymbol) {
            // Generate net symbol
            const netSymbol = this.generateNetSymbol(node, 0, 0);
            if (netSymbol) {
              symbols.push(netSymbol);
            }
          } else if (node.instance) {
            // Generate regular component symbol
            const symbol = this.generateSymbol(node, 0, 0);
            if (symbol) {
              symbols.push(symbol);
            }
          }
        }
      }
    }

    return symbols;
  }

  private generateSymbol(
    node: ElkNode,
    parentX: number,
    parentY: number
  ): SExpr | null {
    const libId = this.getLibId(node);
    if (!libId || !node.instance) return null;

    // Make sure we have the symbol content
    const symbolContent = this.getSymbolContent(node);
    if (!symbolContent) return null;

    // Calculate absolute position
    const x = parentX + (node.x || 0);
    const y = parentY + (node.y || 0);
    const rotation = node.rotation || 0;

    // Convert from top-left corner (React Flow) to center (KiCad)
    // Start with the geometric center
    let centerX = x + (node.width || 0) / 2;
    let centerY = y + (node.height || 0) / 2;

    // If we have the actual symbol bbox center, apply it as an offset
    if (node.symbolBboxCenter) {
      // The symbolBboxCenter is in symbol units, and the LayoutEngine scales by 10
      // We need to subtract the bbox center to properly position the symbol
      const symbolScale = 10;
      centerX -= node.symbolBboxCenter.x * symbolScale;
      centerY -= node.symbolBboxCenter.y * symbolScale;
    }

    // Convert from pixels to mm (KiCad uses mm)
    // The LayoutEngine uses a scale factor of 10 to convert symbol units to pixels
    // KiCad uses mm, where 1mm = 1 unit in the schematic editor
    // So we need to divide by 10 to get back to mm
    const scale = 0.1; // Convert from layout pixels to mm
    const kicadX = centerX * scale;
    const kicadY = centerY * scale;

    // Generate UUID based on hierarchical name (node ID)
    const symbolUuid = this.generateUUID(node.id);

    const symbol = sexpr(
      "symbol",
      sexpr("lib_id", quoted(libId)),
      at(kicadX, kicadY, rotation),
      sexpr("unit", 1),
      sexpr("exclude_from_sim", atom("no")),
      sexpr("in_bom", atom("yes")),
      sexpr("on_board", atom("yes")),
      sexpr("dnp", atom("no")),
      uuid(symbolUuid)
    );

    // Get parsed properties for this lib_id (for effects only)
    const parsedProps = this.parsedSymbols.get(libId);

    // Add properties
    const refDes =
      node.instance.reference_designator || `U${++this.symbolCounter}`;

    // Find the reference label in node.labels
    const refLabel = node.labels?.find((label) => label.text === refDes);

    // Add Reference property
    if (refLabel) {
      // Use the label position from our autoplace algorithm
      // Label positions are relative to the node's top-left corner
      const labelRelX = refLabel.x || 0;
      const labelRelY = refLabel.y || 0;

      // Apply rotation to the label position if the component is rotated
      const nodeWidth = node.width || 0;
      const nodeHeight = node.height || 0;
      const rotatedPos = this.rotateLabelPosition(
        labelRelX,
        labelRelY,
        nodeWidth,
        nodeHeight,
        rotation
      );

      const labelX = ((node.x || 0) + rotatedPos.x) * scale;
      const labelY = ((node.y || 0) + rotatedPos.y) * scale;

      // Labels should always be upright (rotation 0) for readability
      const propAngle = 0;

      const refProp = parsedProps?.get("Reference");
      const refEffects = refProp?.effects
        ? (SExpr.parse(refProp.effects.toString()) as SExpr)
        : sexpr("effects", sexpr("font", sexpr("size", 1.27, 1.27)));

      // Always add justify left top
      refEffects.add(sexpr("justify", atom("left"), atom("top")));

      symbol.add(
        property("Reference", refDes, at(labelX, labelY, propAngle), refEffects)
      );
    } else {
      // Fallback to parsed position or hardcoded
      const refProp = parsedProps?.get("Reference");
      if (refProp) {
        const propX =
          kicadX + this.rotateX(refProp.at.x, refProp.at.y, rotation);
        const propY =
          kicadY + this.rotateY(refProp.at.x, refProp.at.y, rotation);
        const propAngle = (refProp.at.angle + rotation) % 360;

        const refEffects = refProp.effects
          ? (SExpr.parse(refProp.effects.toString()) as SExpr)
          : sexpr("effects", sexpr("font", sexpr("size", 1.27, 1.27)));

        // Always add justify left top
        refEffects.add(sexpr("justify", atom("left"), atom("top")));

        symbol.add(
          property("Reference", refDes, at(propX, propY, propAngle), refEffects)
        );
      } else {
        symbol.add(
          property(
            "Reference",
            refDes,
            at(kicadX + 2, kicadY - 2, 0),
            sexpr(
              "effects",
              sexpr("font", sexpr("size", 1.27, 1.27)),
              sexpr("justify", atom("left"), atom("top"))
            )
          )
        );
      }
    }

    // Add Value property
    const value =
      this.getAttributeValue(node.instance.attributes.Value) ||
      this.getAttributeValue(node.instance.attributes.value) ||
      "";
    if (value) {
      // Find the value label in node.labels
      const valueLabel = node.labels?.find((label) => label.text === value);

      if (valueLabel) {
        // Use the label position from our autoplace algorithm
        // Label positions are relative to the node's top-left corner
        const labelRelX = valueLabel.x || 0;
        const labelRelY = valueLabel.y || 0;

        // Apply rotation to the label position if the component is rotated
        const nodeWidth = node.width || 0;
        const nodeHeight = node.height || 0;
        const rotatedPos = this.rotateLabelPosition(
          labelRelX,
          labelRelY,
          nodeWidth,
          nodeHeight,
          rotation
        );

        const labelX = ((node.x || 0) + rotatedPos.x) * scale;
        const labelY = ((node.y || 0) + rotatedPos.y) * scale;

        // Labels should always be upright (rotation 0) for readability
        const propAngle = 0;

        const valueProp = parsedProps?.get("Value");
        const valueEffects = valueProp?.effects
          ? (SExpr.parse(valueProp.effects.toString()) as SExpr)
          : sexpr("effects", sexpr("font", sexpr("size", 1.27, 1.27)));

        // Always add justify left top
        valueEffects.add(sexpr("justify", atom("left"), atom("top")));

        symbol.add(
          property("Value", value, at(labelX, labelY, propAngle), valueEffects)
        );
      } else {
        // Fallback to parsed position or hardcoded
        const valueProp = parsedProps?.get("Value");
        if (valueProp) {
          const propX =
            kicadX + this.rotateX(valueProp.at.x, valueProp.at.y, rotation);
          const propY =
            kicadY + this.rotateY(valueProp.at.x, valueProp.at.y, rotation);
          const propAngle = (valueProp.at.angle + rotation) % 360;

          const valueEffects = valueProp.effects
            ? (SExpr.parse(valueProp.effects.toString()) as SExpr)
            : sexpr("effects", sexpr("font", sexpr("size", 1.27, 1.27)));

          // Always add justify left top
          valueEffects.add(sexpr("justify", atom("left"), atom("top")));

          symbol.add(
            property("Value", value, at(propX, propY, propAngle), valueEffects)
          );
        } else {
          symbol.add(
            property(
              "Value",
              value,
              at(kicadX + 2, kicadY + 2, 0),
              sexpr(
                "effects",
                sexpr("font", sexpr("size", 1.27, 1.27)),
                sexpr("justify", atom("left"), atom("top"))
              )
            )
          );
        }
      }
    }

    // Add Footprint property
    const footprint =
      this.getAttributeValue(node.instance.attributes.footprint) || "";
    if (footprint) {
      // Transform footprint path to KiCad library format
      const transformedFootprint = this.transformFootprintPath(footprint);

      // Find the footprint label in node.labels (if visible)
      const footprintLabel = node.labels?.find(
        (label) =>
          label.text === footprint || label.text === transformedFootprint
      );

      if (footprintLabel) {
        // Use the label position from our autoplace algorithm
        // Label positions are relative to the node's top-left corner
        const labelRelX = footprintLabel.x || 0;
        const labelRelY = footprintLabel.y || 0;

        // Apply rotation to the label position if the component is rotated
        const nodeWidth = node.width || 0;
        const nodeHeight = node.height || 0;
        const rotatedPos = this.rotateLabelPosition(
          labelRelX,
          labelRelY,
          nodeWidth,
          nodeHeight,
          rotation
        );

        const labelX = ((node.x || 0) + rotatedPos.x) * scale;
        const labelY = ((node.y || 0) + rotatedPos.y) * scale;

        // Labels should always be upright (rotation 0) for readability
        const propAngle = 0;

        const footprintProp = parsedProps?.get("Footprint");
        const propExpr = property(
          "Footprint",
          transformedFootprint,
          at(labelX, labelY, propAngle)
        );

        // Add effects, but don't hide if we have a visible label
        const footprintEffects = footprintProp?.effects
          ? (SExpr.parse(footprintProp.effects.toString()) as SExpr)
          : sexpr("effects", sexpr("font", sexpr("size", 1.27, 1.27)));

        // Always add justify left top
        footprintEffects.add(sexpr("justify", atom("left"), atom("top")));

        propExpr.add(footprintEffects);

        symbol.add(propExpr);
      } else {
        // Fallback to parsed position or hardcoded (hidden)
        const footprintProp = parsedProps?.get("Footprint");

        if (footprintProp) {
          const propX =
            kicadX +
            this.rotateX(footprintProp.at.x, footprintProp.at.y, rotation);
          const propY =
            kicadY +
            this.rotateY(footprintProp.at.x, footprintProp.at.y, rotation);
          const propAngle = (footprintProp.at.angle + rotation) % 360;

          const propExpr = property(
            "Footprint",
            transformedFootprint,
            at(propX, propY, propAngle)
          );

          // Add effects if available, ensuring hide is set
          const footprintEffects = footprintProp.effects
            ? (SExpr.parse(footprintProp.effects.toString()) as SExpr)
            : sexpr("effects", sexpr("font", sexpr("size", 1.27, 1.27)));

          // Always add justify left top and hide
          footprintEffects.add(sexpr("justify", atom("left"), atom("top")));
          footprintEffects.add(sexpr("hide", atom("yes")));

          propExpr.add(footprintEffects);

          symbol.add(propExpr);
        } else {
          symbol.add(
            property(
              "Footprint",
              transformedFootprint,
              at(kicadX, kicadY, 0),
              sexpr(
                "effects",
                sexpr("font", sexpr("size", 1.27, 1.27)),
                sexpr("justify", atom("left"), atom("top")),
                sexpr("hide", atom("yes"))
              )
            )
          );
        }
      }
    }

    // Add pin instances
    if (node.ports) {
      for (const port of node.ports) {
        const pinNumber = this.extractPinNumber(port.id);
        if (pinNumber) {
          // Generate UUID for pin based on port ID
          const pinUuid = this.generateUUID(port.id);
          symbol.add(sexpr("pin", quoted(pinNumber), uuid(pinUuid)));
        }
      }
    }

    return symbol;
  }

  private generateNetSymbol(
    node: ElkNode,
    parentX: number,
    parentY: number
  ): SExpr | null {
    const libId = this.getNetSymbolLibId(node);
    if (!libId || !node.net) return null;

    // Make sure we have the symbol content
    const symbolContent = this.getNetSymbolContent(node);
    if (!symbolContent) return null;

    // Calculate absolute position
    const x = parentX + (node.x || 0);
    const y = parentY + (node.y || 0);
    const rotation = node.rotation || 0;

    // Convert from top-left corner (React Flow) to center (KiCad)
    // Start with the geometric center
    let centerX = x + (node.width || 0) / 2;
    let centerY = y + (node.height || 0) / 2;

    // If we have the actual symbol bbox center, apply it as an offset
    if (node.symbolBboxCenter) {
      // The symbolBboxCenter is in symbol units, and the LayoutEngine scales by 10
      // We need to subtract the bbox center to properly position the symbol
      const symbolScale = 10;
      centerX -= node.symbolBboxCenter.x * symbolScale;
      centerY -= node.symbolBboxCenter.y * symbolScale;
    }

    // Convert from pixels to mm (KiCad uses mm)
    const scale = 0.1; // Convert from layout pixels to mm
    const kicadX = centerX * scale;
    const kicadY = centerY * scale;

    // Generate UUID based on node ID
    const symbolUuid = this.generateUUID(node.id);

    const symbol = sexpr(
      "symbol",
      sexpr("lib_id", quoted(libId)),
      at(kicadX, kicadY, rotation),
      sexpr("unit", 1),
      sexpr("exclude_from_sim", atom("no")),
      sexpr("in_bom", atom("no")), // Power symbols typically not in BOM
      sexpr("on_board", atom("yes")),
      sexpr("dnp", atom("no")),
      uuid(symbolUuid)
    );

    // Get parsed properties for this lib_id
    const parsedProps = this.parsedSymbols.get(libId);

    // Add properties
    // For net symbols, use the net name as the value
    const netName = node.net.name || node.netId || "";
    const refDes = `#PWR${++this.symbolCounter}`;

    // Add Reference property (always hidden for power symbols)
    const refProp = parsedProps?.get("Reference");
    if (refProp) {
      // Apply rotation to the property position
      const propX = kicadX + this.rotateX(refProp.at.x, refProp.at.y, rotation);
      const propY = kicadY + this.rotateY(refProp.at.x, refProp.at.y, rotation);
      const propAngle = (refProp.at.angle + rotation) % 360;

      const propExpr = property(
        "Reference",
        refDes,
        at(propX, propY, propAngle)
      );

      // Add effects, ensuring hide is set for power symbols
      const refEffects = refProp.effects
        ? (SExpr.parse(refProp.effects.toString()) as SExpr)
        : sexpr("effects", sexpr("font", sexpr("size", 1.27, 1.27)));

      // Always add justify left top and hide for power symbols
      refEffects.add(sexpr("justify", atom("left"), atom("top")));
      refEffects.add(sexpr("hide", atom("yes")));

      propExpr.add(refEffects);

      symbol.add(propExpr);
    } else {
      // Fallback to hardcoded position
      symbol.add(
        property(
          "Reference",
          refDes,
          at(kicadX, kicadY - 5, 0),
          sexpr(
            "effects",
            sexpr("font", sexpr("size", 1.27, 1.27)),
            sexpr("justify", atom("left"), atom("top")),
            sexpr("hide", atom("yes"))
          )
        )
      );
    }

    // Add Value property (net name)
    // Find the net name label in node.labels
    const netNameLabel = node.labels?.find((label) => label.text === netName);

    if (netNameLabel) {
      // Use the label position from our autoplace algorithm
      // Label positions are relative to the node's top-left corner
      const labelRelX = netNameLabel.x || 0;
      const labelRelY = netNameLabel.y || 0;

      // Apply rotation to the label position if the component is rotated
      const nodeWidth = node.width || 0;
      const nodeHeight = node.height || 0;
      const rotatedPos = this.rotateLabelPosition(
        labelRelX,
        labelRelY,
        nodeWidth,
        nodeHeight,
        rotation
      );

      const labelX = ((node.x || 0) + rotatedPos.x) * scale;
      const labelY = ((node.y || 0) + rotatedPos.y) * scale;

      // Labels should always be upright (rotation 0) for readability
      const propAngle = 0;

      const valueProp = parsedProps?.get("Value");
      const valueEffects = valueProp?.effects
        ? (SExpr.parse(valueProp.effects.toString()) as SExpr)
        : sexpr("effects", sexpr("font", sexpr("size", 1.27, 1.27)));

      // Always add justify left top
      valueEffects.add(sexpr("justify", atom("left"), atom("top")));

      symbol.add(
        property("Value", netName, at(labelX, labelY, propAngle), valueEffects)
      );
    } else {
      // Fallback to parsed position or hardcoded
      const valueProp = parsedProps?.get("Value");
      if (valueProp) {
        const propX =
          kicadX + this.rotateX(valueProp.at.x, valueProp.at.y, rotation);
        const propY =
          kicadY + this.rotateY(valueProp.at.x, valueProp.at.y, rotation);
        const propAngle = (valueProp.at.angle + rotation) % 360;

        const valueEffects = valueProp.effects
          ? (SExpr.parse(valueProp.effects.toString()) as SExpr)
          : sexpr("effects", sexpr("font", sexpr("size", 1.27, 1.27)));

        // Always add justify left top
        valueEffects.add(sexpr("justify", atom("left"), atom("top")));

        symbol.add(
          property("Value", netName, at(propX, propY, propAngle), valueEffects)
        );
      } else {
        symbol.add(
          property(
            "Value",
            netName,
            at(kicadX, kicadY + 2, 0),
            sexpr(
              "effects",
              sexpr("font", sexpr("size", 1.27, 1.27)),
              sexpr("justify", atom("left"), atom("top"))
            )
          )
        );
      }
    }

    // Add pin instances
    if (node.ports) {
      for (const port of node.ports) {
        // For net symbols, we typically use pin "1"
        const pinNumber = "1";
        const pinUuid = this.generateUUID(port.id);
        symbol.add(sexpr("pin", quoted(pinNumber), uuid(pinUuid)));
      }
    }

    return symbol;
  }

  private extractPinNumber(portId: string): string | null {
    // Extract pin number from port ID
    // e.g., "instance.P1" -> "1", "instance.P2" -> "2"
    const match = portId.match(/\.P(\d+)$/);
    return match ? match[1] : null;
  }

  private generateWires(graph: ElkGraph): SExpr[] {
    const wires: SExpr[] = [];

    // KiCad grid size in mm (typically 1.27mm = 50mil)
    const kicadGridSize = 1.27;

    for (const edge of graph.edges) {
      if (edge.sections) {
        for (const section of edge.sections) {
          // Build points array from section data
          const points = [
            section.startPoint,
            ...(section.bendPoints || []),
            section.endPoint,
          ];

          // Generate separate wire segments between each pair of points
          for (let i = 0; i < points.length - 1; i++) {
            const startPoint = points[i];
            const endPoint = points[i + 1];

            // Convert to KiCad coordinates (mm) and snap to grid
            const startX = snapToGrid(startPoint.x * 0.1, kicadGridSize);
            const startY = snapToGrid(startPoint.y * 0.1, kicadGridSize);
            const endX = snapToGrid(endPoint.x * 0.1, kicadGridSize);
            const endY = snapToGrid(endPoint.y * 0.1, kicadGridSize);

            // Generate UUID for this wire segment
            const wireUuid = this.generateUUID(
              `${edge.id}_wire_${section.id || "0"}_segment_${i}`
            );

            const wire = sexpr(
              "wire",
              sexpr("pts", xy(startX, startY), xy(endX, endY)),
              sexpr(
                "stroke",
                sexpr("width", 0),
                sexpr("type", atom("default"))
              ),
              uuid(wireUuid)
            );
            wires.push(wire);
          }
        }
      }
    }

    return wires;
  }

  private generateJunctions(graph: ElkGraph): SExpr[] {
    const junctions: SExpr[] = [];

    // KiCad grid size in mm (typically 1.27mm = 50mil)
    const kicadGridSize = 1.27;

    // Add junctions from edges
    for (const edge of graph.edges) {
      if (edge.junctionPoints) {
        for (let i = 0; i < edge.junctionPoints.length; i++) {
          const point = edge.junctionPoints[i];

          // Convert to KiCad coordinates (mm) and snap to grid
          const x = snapToGrid(point.x * 0.1, kicadGridSize);
          const y = snapToGrid(point.y * 0.1, kicadGridSize);

          // Generate UUID for junction based on edge ID and index
          const junctionUuid = this.generateUUID(`${edge.id}_junction_${i}`);

          const junction = sexpr(
            "junction",
            at(x, y),
            sexpr("diameter", 0),
            sexpr("color", 0, 0, 0, 0),
            uuid(junctionUuid)
          );
          junctions.push(junction);
        }
      }
    }

    return junctions;
  }

  private generateGlobalLabels(graph: ElkGraph): SExpr[] {
    const globalLabels: SExpr[] = [];

    // KiCad grid size in mm (typically 1.27mm = 50mil)
    const kicadGridSize = 1.27;

    // Iterate through all nodes
    if (graph.children) {
      for (const node of graph.children) {
        if (!node.ports) continue;

        const nodeRotation = node.rotation || 0;
        const nodeX = node.x || 0;
        const nodeY = node.y || 0;
        const nodeCenterX = nodeX + (node.width || 0) / 2;
        const nodeCenterY = nodeY + (node.height || 0) / 2;

        for (const port of node.ports) {
          // Check if port has labels with net references
          if (!port.labels) continue;

          for (const label of port.labels) {
            // Only generate global labels for net reference labels
            if (label.properties?.labelType !== "netReference") continue;

            // Use the port position, not the label position
            const portX = port.x || 0;
            const portY = port.y || 0;

            // Apply rotation if node is rotated
            let absoluteX: number;
            let absoluteY: number;

            if (nodeRotation !== 0) {
              // Convert rotation to radians
              const angleRad = (nodeRotation * Math.PI) / 180;
              const cos = Math.cos(angleRad);
              const sin = Math.sin(angleRad);

              // Rotate around node center
              const dx = portX - (node.width || 0) / 2;
              const dy = portY - (node.height || 0) / 2;

              const rotatedX = dx * cos - dy * sin;
              const rotatedY = dx * sin + dy * cos;

              absoluteX = nodeCenterX + rotatedX;
              absoluteY = nodeCenterY + rotatedY;
            } else {
              absoluteX = nodeX + portX;
              absoluteY = nodeY + portY;
            }

            // Convert to KiCad coordinates (mm) and snap to grid
            const kicadX = snapToGrid(absoluteX * 0.1, kicadGridSize);
            const kicadY = snapToGrid(absoluteY * 0.1, kicadGridSize);

            // Generate UUID for the global label
            const labelUuid = this.generateUUID(`${port.id}_global_label`);

            let shape = "input";

            // Calculate label rotation based on node rotation and port side
            let labelRotation = 0;
            const portSide = port.properties?.["port.side"] || "WEST";

            // Base rotation for label orientation based on port side
            // Labels should point AWAY from the component (opposite of port direction)
            let baseRotation = 0;
            switch (portSide) {
              case "WEST":
                baseRotation = 180; // Port faces left, label points right
                break;
              case "EAST":
                baseRotation = 0; // Port faces right, label points left
                break;
              case "NORTH":
                baseRotation = 270; // Port faces up, label points down
                break;
              case "SOUTH":
                baseRotation = 90; // Port faces down, label points up
                break;
            }

            // Add node rotation to base rotation
            labelRotation = (baseRotation + nodeRotation) % 360;

            // Determine justification based on final rotation
            let justification = "left";
            const normalizedRotation = labelRotation % 360;
            if (normalizedRotation === 180) {
              justification = "right";
            } else if (normalizedRotation === 90) {
              justification = "bottom";
            } else if (normalizedRotation === 270) {
              justification = "top";
            }

            const globalLabel = sexpr(
              "global_label",
              quoted(label.text),
              sexpr("shape", atom(shape)),
              at(kicadX, kicadY, labelRotation),
              sexpr("fields_autoplaced", atom("yes")),
              sexpr(
                "effects",
                sexpr("font", sexpr("size", 1.27, 1.27)),
                sexpr("justify", atom(justification))
              ),
              uuid(labelUuid)
            );

            // Add Intersheetrefs property
            // Adjust offset based on justification
            let intersheetOffsetX = 6.5;
            let intersheetOffsetY = 0;
            if (justification === "right") {
              intersheetOffsetX = -6.5;
            } else if (justification === "bottom") {
              intersheetOffsetX = 0;
              intersheetOffsetY = 6.5;
            } else if (justification === "top") {
              intersheetOffsetX = 0;
              intersheetOffsetY = -6.5;
            }

            globalLabel.add(
              property(
                "Intersheetrefs",
                `\${INTERSHEET_REFS}`,
                at(kicadX + intersheetOffsetX, kicadY + intersheetOffsetY, 0),
                sexpr(
                  "effects",
                  sexpr("font", sexpr("size", 1.27, 1.27)),
                  sexpr("justify", atom(justification)),
                  sexpr("hide", atom("yes"))
                )
              )
            );

            globalLabels.push(globalLabel);
          }
        }
      }
    }

    return globalLabels;
  }

  private generateUUID(hierarchicalName: string): string {
    // Generate UUID v5 using the same namespace as the Rust implementation
    return uuidv5(hierarchicalName, KicadSchematicGenerator.UUID_NAMESPACE_URL);
  }

  private rotateX(x: number, y: number, angle: number): number {
    const rad = (angle * Math.PI) / 180;
    return x * Math.cos(rad) - y * Math.sin(rad);
  }

  private rotateY(x: number, y: number, angle: number): number {
    const rad = (angle * Math.PI) / 180;
    return x * Math.sin(rad) + y * Math.cos(rad);
  }

  private rotateLabelPosition(
    labelRelX: number,
    labelRelY: number,
    nodeWidth: number,
    nodeHeight: number,
    rotation: number
  ): { x: number; y: number } {
    if (rotation === 0) {
      return { x: labelRelX, y: labelRelY };
    }

    const nodeCenterX = nodeWidth / 2;
    const nodeCenterY = nodeHeight / 2;

    // Convert rotation to radians
    // Note: In screen coordinates (Y down), we need to negate the angle for correct rotation
    const angleRad = (rotation * Math.PI) / 180;
    const cos = Math.cos(angleRad);
    const sin = Math.sin(angleRad);

    // Translate to origin (component center)
    const dx = labelRelX - nodeCenterX;
    const dy = labelRelY - nodeCenterY;

    // Rotate
    const rotatedX = dx * cos - dy * sin;
    const rotatedY = dx * sin + dy * cos;

    // Translate back
    return {
      x: nodeCenterX + rotatedX,
      y: nodeCenterY + rotatedY,
    };
  }
}
