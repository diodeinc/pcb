/**
 * S-Expression Serialization Library
 *
 * A compact library for building and serializing s-expressions
 * with human-readable output formatting.
 */

/**
 * Represents a string value that should always be quoted
 */
export class SExprString {
  constructor(public readonly value: string) {}
}

/**
 * Represents an atom value that is quoted only when necessary
 */
export class SExprAtom {
  constructor(public readonly value: string) {}
}

/**
 * Represents an s-expression value that can be serialized
 */
export type SExprValue =
  | string // Legacy support - treated as atom
  | number
  | SExprString
  | SExprAtom
  | SExpr
  | SExprValue[];

/**
 * Main s-expression class for building and serializing s-expressions
 */
export class SExpr {
  private _name: string;
  private _values: SExprValue[];

  constructor(name: string, ...values: SExprValue[]) {
    this._name = name;
    this._values = values;
  }

  /**
   * Get the name of this s-expression
   */
  get name(): string {
    return this._name;
  }

  /**
   * Get the values of this s-expression
   */
  get values(): SExprValue[] {
    return this._values;
  }

  /**
   * Get a value as a string, handling both raw strings and SExprString/SExprAtom
   */
  getStringValue(index: number): string | undefined {
    const value = this._values[index];
    if (typeof value === "string") return value;
    if (value instanceof SExprString) return value.value;
    if (value instanceof SExprAtom) return value.value;
    return undefined;
  }

  /**
   * Add values to this s-expression
   */
  add(...values: SExprValue[]): this {
    this._values.push(...values);
    return this;
  }

  /**
   * Remove values that match a predicate
   */
  removeWhere(predicate: (value: SExprValue, index: number) => boolean): this {
    this._values = this._values.filter(
      (value, index) => !predicate(value, index)
    );
    return this;
  }

  /**
   * Create a child s-expression and add it to this one
   */
  child(name: string, ...values: SExprValue[]): SExpr {
    const child = new SExpr(name, ...values);
    this._values.push(child);
    return child;
  }

  /**
   * Find a child s-expression by name
   */
  findChild(name: string): SExpr | undefined {
    for (const value of this._values) {
      if (value instanceof SExpr && value.name === name) {
        return value;
      }
    }
    return undefined;
  }

  /**
   * Find all child s-expressions by name
   */
  findChildren(name: string): SExpr[] {
    const children: SExpr[] = [];
    for (const value of this._values) {
      if (value instanceof SExpr && value.name === name) {
        children.push(value);
      }
    }
    return children;
  }

  /**
   * Serialize to string with optional formatting
   */
  toString(options?: SerializeOptions): string {
    const opts = { ...defaultOptions, ...options };
    return serializeToString(this, opts, 0);
  }

  /**
   * Static factory method for creating s-expressions
   */
  static create(name: string, ...values: SExprValue[]): SExpr {
    return new SExpr(name, ...values);
  }

  /**
   * Static method to serialize any value
   */
  static serialize(value: SExprValue, options?: SerializeOptions): string {
    const opts = { ...defaultOptions, ...options };
    return serializeValue(value, opts, 0);
  }

  /**
   * Parse an s-expression string into an SExpr object
   */
  static parse(input: string): SExpr {
    const tokens = tokenize(input);
    const result = parseExpression(tokens);
    if (!(result instanceof SExpr)) {
      throw new Error("Input does not contain a valid s-expression");
    }
    return result;
  }
}

/**
 * Options for serialization
 */
export interface SerializeOptions {
  /** Use pretty printing with indentation */
  pretty?: boolean;
  /** Indentation string (default: "  ") */
  indent?: string;
  /** Maximum line width before wrapping (default: 80) */
  maxWidth?: number;
  /** Quote all strings, even if they don't need it */
  quoteAll?: boolean;
  /** Use single line for simple expressions */
  compact?: boolean;
}

const defaultOptions: Required<SerializeOptions> = {
  pretty: true,
  indent: "  ",
  maxWidth: 80,
  quoteAll: false,
  compact: true,
};

/**
 * Check if a string needs quoting
 */
function needsQuoting(str: string): boolean {
  // Empty strings need quotes
  if (str.length === 0) return true;

  // Check for special characters that require quoting
  const specialChars = /[\s()"\\]/;
  if (specialChars.test(str)) return true;

  // Check if it could be confused with a number
  if (/^-?\d+(\.\d+)?$/.test(str)) return true;

  return false;
}

/**
 * Quote a string value
 */
function quoteString(str: string): string {
  // Escape special characters
  const escaped = str
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\n/g, "\\n")
    .replace(/\r/g, "\\r")
    .replace(/\t/g, "\\t");

  return `"${escaped}"`;
}

/**
 * Serialize a value to string
 */
function serializeValue(
  value: SExprValue,
  options: Required<SerializeOptions>,
  depth: number
): string {
  if (value === null || value === undefined) {
    return "nil";
  }

  if (value instanceof SExprString) {
    // Always quote SExprString values
    return quoteString(value.value);
  }

  if (value instanceof SExprAtom) {
    // Quote atoms only when necessary
    return needsQuoting(value.value) ? quoteString(value.value) : value.value;
  }

  if (typeof value === "string") {
    // Legacy support: treat raw strings as atoms
    return options.quoteAll || needsQuoting(value) ? quoteString(value) : value;
  }

  if (typeof value === "number") {
    // Format numbers nicely
    if (Number.isInteger(value)) {
      return value.toString();
    } else {
      // Use a reasonable precision for floats
      return value.toFixed(6).replace(/\.?0+$/, "");
    }
  }

  if (value instanceof SExpr) {
    return serializeToString(value, options, depth);
  }

  if (Array.isArray(value)) {
    // Serialize arrays as space-separated values
    return value.map((v) => serializeValue(v, options, depth)).join(" ");
  }

  // Fallback for unknown types
  return String(value);
}

/**
 * Calculate the length of a serialized s-expression on a single line
 */
function calculateSingleLineLength(
  sexpr: SExpr,
  options: Required<SerializeOptions>
): number {
  let length = 1 + sexpr.name.length + 1; // "(name "

  for (let i = 0; i < sexpr.values.length; i++) {
    if (i > 0) length += 1; // space separator

    const value = sexpr.values[i];
    if (value instanceof SExpr) {
      length += calculateSingleLineLength(value, options);
    } else if (value instanceof SExprString) {
      // Always quoted
      length += quoteString(value.value).length;
    } else if (value instanceof SExprAtom) {
      // May or may not be quoted
      length += needsQuoting(value.value)
        ? quoteString(value.value).length
        : value.value.length;
    } else {
      length += serializeValue(value, options, 0).length;
    }
  }

  length += 1; // closing ")"
  return length;
}

/**
 * Check if an s-expression is simple (no nested s-expressions)
 */
function isSimpleExpr(sexpr: SExpr): boolean {
  return sexpr.values.every((v) => !(v instanceof SExpr));
}

/**
 * Check if an s-expression should be formatted on a single line
 */
function shouldUseSingleLine(
  sexpr: SExpr,
  options: Required<SerializeOptions>
): boolean {
  // Always use single line if not pretty printing
  if (!options.pretty) return true;

  // Calculate single line length
  const singleLineLength = calculateSingleLineLength(sexpr, options);

  // Use single line if it fits within max width
  if (singleLineLength <= options.maxWidth) {
    // For very simple expressions (like "unit 1"), always use single line
    if (isSimpleExpr(sexpr) && sexpr.values.length <= 2) {
      return true;
    }

    // For other expressions, use single line if compact mode is on
    if (options.compact) {
      return true;
    }
  }

  return false;
}

/**
 * Serialize an s-expression to string
 */
function serializeToString(
  sexpr: SExpr,
  options: Required<SerializeOptions>,
  depth: number
): string {
  const nextIndent = options.pretty ? options.indent.repeat(depth + 1) : "";

  // Check if we should use single-line format
  const useSingleLine = shouldUseSingleLine(sexpr, options);

  if (useSingleLine) {
    // Single line format
    let result = `(${sexpr.name}`;

    for (const value of sexpr.values) {
      result += " " + serializeValue(value, options, depth + 1);
    }

    result += ")";
    return result;
  } else {
    // Multi-line format with closing paren on same line as last element
    let result = `(${sexpr.name}`;

    for (let i = 0; i < sexpr.values.length; i++) {
      const value = sexpr.values[i];
      result += "\n" + nextIndent;
      result += serializeValue(value, options, depth + 1);

      // Add closing paren on same line as last element
      if (i === sexpr.values.length - 1) {
        result += ")";
      }
    }

    // If there are no values, close on the same line
    if (sexpr.values.length === 0) {
      result += ")";
    }

    return result;
  }
}

/**
 * Helper function to create an s-expression
 */
export function sexpr(name: string, ...values: SExprValue[]): SExpr {
  return new SExpr(name, ...values);
}

/**
 * Helper function to create a quoted string value
 */
export function quoted(str: string): SExprString {
  return new SExprString(str);
}

/**
 * Helper function to create an atom value
 */
export function atom(str: string): SExprAtom {
  return new SExprAtom(str);
}

/**
 * Helper function to format a coordinate pair
 */
export function xy(x: number, y: number): SExpr {
  return new SExpr("xy", x, y);
}

/**
 * Helper function to format a coordinate pair with "at" prefix
 */
export function at(x: number, y: number, angle?: number): SExpr {
  return angle !== undefined
    ? new SExpr("at", x, y, angle)
    : new SExpr("at", x, y);
}

/**
 * Helper function for property expressions
 */
export function property(
  key: string,
  value: string,
  ...attrs: SExprValue[]
): SExpr {
  // Properties should always have quoted keys and values
  return new SExpr("property", quoted(key), quoted(value), ...attrs);
}

/**
 * Helper to create a UUID
 * @param uuid - A UUID string to be quoted
 */
export function uuid(uuid: string): SExpr {
  return new SExpr("uuid", quoted(uuid));
}

/**
 * Token types for parsing
 */
enum TokenType {
  LPAREN = "LPAREN",
  RPAREN = "RPAREN",
  STRING = "STRING",
  NUMBER = "NUMBER",
  SYMBOL = "SYMBOL",
  EOF = "EOF",
}

/**
 * Token structure
 */
interface Token {
  type: TokenType;
  value: string;
}

/**
 * Tokenize an s-expression string
 */
function tokenize(input: string): Token[] {
  const tokens: Token[] = [];
  let i = 0;

  while (i < input.length) {
    // Skip whitespace
    while (i < input.length && /\s/.test(input[i])) {
      i++;
    }

    if (i >= input.length) break;

    const char = input[i];

    // Left parenthesis
    if (char === "(") {
      tokens.push({ type: TokenType.LPAREN, value: "(" });
      i++;
      continue;
    }

    // Right parenthesis
    if (char === ")") {
      tokens.push({ type: TokenType.RPAREN, value: ")" });
      i++;
      continue;
    }

    // Quoted string
    if (char === '"') {
      let value = "";
      i++; // Skip opening quote
      while (i < input.length && input[i] !== '"') {
        if (input[i] === "\\") {
          i++;
          if (i < input.length) {
            // Handle escape sequences
            switch (input[i]) {
              case "n":
                value += "\n";
                break;
              case "r":
                value += "\r";
                break;
              case "t":
                value += "\t";
                break;
              case "\\":
                value += "\\";
                break;
              case '"':
                value += '"';
                break;
              default:
                value += input[i];
            }
          }
        } else {
          value += input[i];
        }
        i++;
      }
      i++; // Skip closing quote
      tokens.push({ type: TokenType.STRING, value });
      continue;
    }

    // Number or symbol
    let value = "";
    while (i < input.length && !/[\s()]/.test(input[i])) {
      value += input[i];
      i++;
    }

    // Check if it's a number
    if (/^-?\d+(\.\d+)?$/.test(value)) {
      tokens.push({ type: TokenType.NUMBER, value });
    } else {
      tokens.push({ type: TokenType.SYMBOL, value });
    }
  }

  tokens.push({ type: TokenType.EOF, value: "" });
  return tokens;
}

/**
 * Parse tokens into s-expressions
 */
function parseExpression(tokens: Token[]): SExprValue {
  let index = 0;

  function peek(): Token {
    return tokens[index] || { type: TokenType.EOF, value: "" };
  }

  function consume(): Token {
    return tokens[index++];
  }

  function parseValue(): SExprValue {
    const token = peek();

    switch (token.type) {
      case TokenType.LPAREN: {
        consume(); // consume '('

        // Check for empty list
        if (peek().type === TokenType.RPAREN) {
          consume(); // consume ')'
          return new SExpr("list");
        }

        // Parse the name
        const nameToken = consume();
        if (
          nameToken.type !== TokenType.SYMBOL &&
          nameToken.type !== TokenType.STRING
        ) {
          throw new Error(
            `Expected symbol or string for s-expression name, got ${nameToken.type}`
          );
        }

        const sexpr = new SExpr(nameToken.value);

        // Parse values until we hit ')'
        while (
          peek().type !== TokenType.RPAREN &&
          peek().type !== TokenType.EOF
        ) {
          sexpr.add(parseValue());
        }

        if (peek().type !== TokenType.RPAREN) {
          throw new Error("Expected closing parenthesis");
        }
        consume(); // consume ')'

        return sexpr;
      }

      case TokenType.STRING:
        consume();
        // Parsed strings are always SExprString
        return new SExprString(token.value);

      case TokenType.NUMBER:
        consume();
        return parseFloat(token.value);

      case TokenType.SYMBOL:
        consume();
        // Handle special symbols
        if (token.value === "nil") return "";
        // All symbols are atoms
        return new SExprAtom(token.value);

      default:
        throw new Error(`Unexpected token type: ${token.type}`);
    }
  }

  return parseValue();
}
