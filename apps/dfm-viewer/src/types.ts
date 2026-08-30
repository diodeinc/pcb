import type { Bounds, Point } from './camera';

export type Measurement =
  | { actual_mm: number; required_mm: number; margin_mm: number }
  | { actual_count: number; required_count: number; margin_count: number };
export interface Layer {
  name: string;
  function: string;
  side: string | null;
}
export interface Source {
  step: string | null;
  layer: string | null;
  set_index: number | null;
  feature_index: number | null;
  instance_index: number | null;
}
export interface Subject {
  role: string;
  kind: string;
  name: string | null;
  reference_designator: string | null;
  pin: string | null;
  net: string | null;
  padstack_ref: string | null;
  source: Source | null;
  provenance: Source | null;
  drill_span: {
    first_copper_index: number;
    last_copper_index: number;
    interpretation: string;
  } | null;
}
export interface Evidence {
  role: string;
  kind: string;
  center: Point | null;
  diameter: number | null;
  start: Point | null;
  end: Point | null;
  bounding_box: Bounds | null;
  paths: Point[][];
  /** Optional native construction. Legacy measured paths remain the fallback. */
  display?: EvidenceDisplay;
}
export type EvidenceDisplay =
  | { kind: 'path'; paths: string[]; fill_rule: 'nonzero' | 'evenodd' }
  | { kind: 'round_stroke'; paths: Point[][]; width_mm: number }
  | { kind: 'circle_minus_layer'; center: Point; diameter: number; layer: string }
  | {
      kind: 'circle_intersection';
      first: { center: Point; diameter: number };
      second: { center: Point; diameter: number };
    };
export interface Site {
  id: string;
  measurement: Measurement;
  measurement_kind: string;
  uncertainty_mm: number;
  witnesses: { role: string; point: Point }[];
  bounding_box: Bounds;
  layers: Layer[];
  subjects: Subject[];
  evidence: Evidence[];
  note: string | null;
}
export interface Finding {
  id: string;
  rule_id: string;
  severity: 'error' | 'warning';
  waived: boolean;
  waiver_reason: string | null;
  title: string;
  message: string;
  measurement: Measurement;
  layers: Layer[];
  subjects: Subject[];
  evidence: Evidence[];
  sites: Site[];
  group_key: string | null;
}
export interface Rule {
  id: string;
  title: string;
  severity: string;
  status: string;
  comparison: string;
  limit: { pdk_value: string; normalized_value: number; normalized_unit: string };
  subject: string;
  quantity: string;
  method: string;
  checked: number;
  finding_count: number;
  waived_count: number;
  skip_reason: string | null;
  tier: string;
  view: { kind: string; title: string; spatial: boolean; features: string[] };
}
export interface Occurrence {
  index: number;
  parent_index: number | null;
  step: string;
  kind: string;
  purpose: string;
  transform: number[];
  bounding_box: Bounds | null;
  repeat_index_x: number;
  repeat_index_y: number;
}
export interface ScenePass {
  label: string;
  feature: string;
  layer: string | null;
  color: string;
  svg: string;
}
export interface Report {
  schema_version: number;
  generated_at: string;
  verdict: 'pass' | 'fail';
  tool: { name: string; version: string };
  input: { path: string; sha256: string; size_bytes: number };
  pdk: { id: string; name: string; revision: string; path: string; sha256: string };
  layout_target: string;
  coordinate_system: { unit: string; axes: string; origin: string };
  layout: {
    kind: string;
    selected_step: string | null;
    coordinate_frame: string;
    bounding_box: Bounds | null;
    instances: Occurrence[];
  };
  waivers: { path: string; applied: number; expired: string[]; unmatched: string[] } | null;
  summary: {
    rules_configured: number;
    rules_passed: number;
    rules_warned: number;
    rules_failed: number;
    rules_skipped: number;
    findings: number;
    errors: number;
    warnings: number;
    waived: number;
  };
  rules: Rule[];
  findings: Finding[];
  scene?: { schema_version: number; bounds: Bounds; passes: ScenePass[] };
}
export interface IncompleteReport {
  schema_version: number;
  generated_at: string;
  verdict: 'incomplete';
  input: { path: string };
  pdk: { path: string };
  layout_target: string;
  error: { message: string };
}
export type DiagnosticReport = Report | IncompleteReport;
