/// <reference lib="esnext.disposable" />

/** Options are strict: unknown keys and unsupported enum values throw Error. */
export interface ImportOptions { name?: string; validate?: boolean; }
export type LayoutTarget = "board" | "board-array";
export type SideFilter = "both" | "top" | "bottom";
export type ViewMode = "bom" | "assembly" | "fabrication" | "stackup" | "test" | "stencil" | "dfx";

export interface IpcInfo {
  revision: string;
  mode: string;
  level: string | null;
  source_units?: string | null;
  board_dimensions?: { width_mm: number; height_mm: number; width_inch: number; height_inch: number };
  board_array?: { step_name: string; board_count: number; board_instances: number; [key: string]: unknown };
  components?: { total: number; smt: number; tht: number; other: number };
  [key: string]: unknown;
}

export type ExportOptions =
  | { format: "ipc2581"; mode?: ViewMode }
  | { format: "gerber"; layoutTarget?: LayoutTarget; zip?: boolean }
  | { format: "svg"; layer: string; layoutTarget?: LayoutTarget }
  | { format: "png"; layer: string; layoutTarget?: LayoutTarget }
  | { format: "dxf"; layoutTarget?: LayoutTarget }
  | { format: "bom" }
  | { format: "cpl"; side?: SideFilter; excludeDnp?: boolean }
  | { format: "ict"; side?: SideFilter }
  | { format: "html" };
export interface ExportFile { name: string; mediaType: string; data: Uint8Array<ArrayBuffer>; }

/** Names are report labels only. No method reads paths or fetches URLs. */
export interface TextInput { source: string; name?: string; }
export type PdkInput = string | TextInput;
export interface BuiltinPdk { name: string; source: string; }
export interface DfmOptions {
  /** A built-in name (default: standard) or custom TOML. */
  pdk?: PdkInput;
  waivers?: TextInput;
  layoutTarget?: LayoutTarget;
  /** RFC 3339; defaults to the host clock. Controls waiver expiry in UTC. */
  generatedAt?: string;
}
export interface FileIdentity { path: string; sha256: string; size_bytes: number; }
export interface PdkIdentity {
  id: string; name: string; revision: string;
  manufacturer: string | null; process: string | null;
  path: string; sha256: string; source: string;
}
export interface DfmSummary {
  rules_configured: number; rules_passed: number; rules_warned: number;
  rules_failed: number; rules_skipped: number; findings: number;
  errors: number; warnings: number; waived: number;
}
export interface DfmFinding {
  id: string; rule_id: string; severity: "error" | "warning";
  message: string; waived: boolean; waiver_reason: string | null;
  measurement: Record<string, unknown>;
  layers: Array<Record<string, unknown>>;
  subjects: Array<Record<string, unknown>>;
  evidence: unknown;
  sites: Array<{ id: string; bounding_box: DfmBounds; [key: string]: unknown }>;
  group_key: string | null;
  [key: string]: unknown;
}
export interface DfmBounds { min: { x: number; y: number }; max: { x: number; y: number }; }
export interface DfmScene {
  schema_version: number;
  bounds: DfmBounds;
  passes: Array<{ label: string; feature: string; layer: string | null; color: string; svg: string }>;
}
/** Preserves the native DFM JSON schema, including snake_case field names. */
export interface DfmReport {
  schema_version: number;
  generated_at: string;
  verdict: "pass" | "fail";
  input: FileIdentity;
  pdk: PdkIdentity;
  layout_target: "board" | "board_array";
  layout: {
    kind: string; selected_step: string | null; coordinate_frame: string;
    bounding_box: DfmBounds | null; instances: Array<Record<string, unknown>>;
  };
  scene: DfmScene;
  summary: DfmSummary;
  findings: DfmFinding[];
  rules: Array<{ id: string; status: "pass" | "warning" | "fail" | "skipped"; [key: string]: unknown }>;
  waivers: null | { path: string; sha256: string; applied: number; expired: string[]; unmatched: string[] };
  [key: string]: unknown;
}
