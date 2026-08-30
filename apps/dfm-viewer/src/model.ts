import type { Bounds, Point } from './camera';
import { validatePath } from './scene';
import type {
  DiagnosticReport,
  Evidence,
  Finding,
  Measurement,
  Occurrence,
  Report,
  Rule,
  Site,
  Subject,
} from './types';

export const pretty = (value: string | null | undefined) => (value || '').replaceAll('_', ' ');
export const number = (value: number) =>
  Number.isFinite(value) ? Number(value.toFixed(6)).toString() : '—';
export const basename = (path: string) => path.split(/[\\/]/).pop() || path;
export const measurementValue = (m: Measurement) =>
  'actual_mm' in m ? `${number(m.actual_mm)} mm` : `${m.actual_count} layers`;
export const requiredValue = (m: Measurement) =>
  'required_mm' in m ? `${number(m.required_mm)} mm` : `${m.required_count} layers`;

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}
function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}
const finite = (v: unknown): v is number => typeof v === 'number' && Number.isFinite(v);
const count = (v: unknown): v is number => finite(v) && Number.isSafeInteger(v) && v >= 0;
const string = (v: unknown): v is string => typeof v === 'string';
const nullableString = (v: unknown) => v == null || string(v);
const strings = (v: unknown): v is string[] => Array.isArray(v) && v.every(string);
const severity = (v: unknown) => v === 'error' || v === 'warning';
const point = (v: unknown): v is Point => object(v) && finite(v.x) && finite(v.y);
export const validBounds = (v: unknown): v is Bounds =>
  object(v) && point(v.min) && point(v.max) && v.max.x >= v.min.x && v.max.y >= v.min.y;
function validMeasurement(value: unknown): value is Measurement {
  if (!object(value)) return false;
  const distanceFields = ['actual_mm', 'required_mm', 'margin_mm'];
  const countFields = ['actual_count', 'required_count', 'margin_count'];
  // The renderer selects a variant by field presence. Never accept a malformed
  // distance variant just because a second, valid count variant is also present.
  if (distanceFields.some((key) => key in value)) {
    return (
      !countFields.some((key) => key in value) && distanceFields.every((key) => finite(value[key]))
    );
  }
  return (
    count(value.actual_count) &&
    count(value.required_count) &&
    finite(value.margin_count) &&
    Number.isSafeInteger(value.margin_count)
  );
}

function validLayer(value: unknown) {
  return (
    object(value) && string(value.name) && string(value.function) && nullableString(value.side)
  );
}

function validSource(value: unknown, instances: Set<number>) {
  return (
    value == null ||
    (object(value) &&
      nullableString(value.step) &&
      nullableString(value.layer) &&
      ['set_index', 'feature_index'].every((key) => value[key] == null || count(value[key])) &&
      (value.instance_index == null ||
        (count(value.instance_index) && instances.has(value.instance_index))))
  );
}

function validSubject(value: unknown, instances: Set<number>) {
  if (!object(value) || !string(value.role) || !string(value.kind)) return false;
  const span = value.drill_span;
  return (
    ['name', 'reference_designator', 'pin', 'net', 'padstack_ref'].every((key) =>
      nullableString(value[key]),
    ) &&
    validSource(value.source, instances) &&
    validSource(value.provenance, instances) &&
    (span == null ||
      (object(span) &&
        count(span.first_copper_index) &&
        count(span.last_copper_index) &&
        span.first_copper_index <= span.last_copper_index &&
        string(span.interpretation)))
  );
}

function validateDisplay(
  value: unknown,
  owner: string,
  layers: string[],
  materialLayers: Map<string, string>,
) {
  assert(object(value), `${owner} has invalid native display metadata.`);
  const fields = (names: string[]) =>
    assert(
      Object.keys(value).every((key) => names.includes(key)),
      `${owner} has unsupported native display fields.`,
    );
  const positive = (number: unknown) => finite(number) && number > 0;
  const circle = (v: unknown) =>
    object(v) &&
    point(v.center) &&
    positive(v.diameter) &&
    Object.keys(v).every((key) => key === 'center' || key === 'diameter');
  switch (value.kind) {
    case 'path':
      fields(['kind', 'paths', 'fill_rule']);
      assert(
        strings(value.paths) &&
          value.paths.length > 0 &&
          value.paths.every((path) => path.trim().length > 0) &&
          (value.fill_rule === 'nonzero' || value.fill_rule === 'evenodd'),
        `${owner} has invalid native paths or fill rule.`,
      );
      for (const path of value.paths) {
        try {
          validatePath(path);
        } catch (error) {
          throw new Error(
            `${owner} has an invalid native path: ${error instanceof Error ? error.message : String(error)}`,
          );
        }
      }
      break;
    case 'round_stroke':
      fields(['kind', 'paths', 'width_mm']);
      assert(
        positive(value.width_mm) &&
          Array.isArray(value.paths) &&
          value.paths.length > 0 &&
          value.paths.every((path) => Array.isArray(path) && path.length >= 2 && path.every(point)),
        `${owner} has an invalid round stroke; expected finite paths and a positive width in mm.`,
      );
      break;
    case 'circle_minus_layer':
      fields(['kind', 'center', 'diameter', 'layer']);
      assert(
        point(value.center) &&
          positive(value.diameter) &&
          string(value.layer) &&
          value.layer.length > 0,
        `${owner} has an invalid circle/layer construction.`,
      );
      assert(layers.includes(value.layer), `${owner} subtracts undeclared layer ${value.layer}.`);
      materialLayers.set(value.layer, owner);
      break;
    case 'circle_intersection':
      fields(['kind', 'first', 'second']);
      assert(
        circle(value.first) && circle(value.second),
        `${owner} has an invalid circle intersection; expected finite centers and positive diameters.`,
      );
      break;
    default:
      throw new Error(`${owner} has an unsupported native display kind.`);
  }
}

function validateEvidence(
  evidence: unknown,
  owner: string,
  layers: string[],
  materialLayers: Map<string, string>,
) {
  assert(Array.isArray(evidence), `${owner} is missing its evidence array.`);
  for (const item of evidence) {
    assert(
      object(item) && string(item.kind) && string(item.role) && Array.isArray(item.paths),
      `${owner} has invalid evidence.`,
    );
    assert(
      item.paths.every((path) => Array.isArray(path) && path.every(point)),
      `${owner} has invalid evidence paths.`,
    );
    assert(
      ['center', 'start', 'end'].every((key) => item[key] == null || point(item[key])) &&
        (item.bounding_box == null || validBounds(item.bounding_box)) &&
        (item.diameter == null || (finite(item.diameter) && item.diameter >= 0)),
      `${owner} has nonfinite evidence geometry.`,
    );
    if ('display' in item) validateDisplay(item.display, owner, layers, materialLayers);
  }
}

function validateInstances(instances: unknown[]): Set<number> {
  const parents = new Map<number, number | null>();
  for (const instance of instances) {
    assert(
      object(instance) && count(instance.index) && !parents.has(instance.index),
      'Invalid or duplicate layout occurrence index.',
    );
    assert(
      (instance.parent_index === null || count(instance.parent_index)) &&
        ['step', 'kind', 'purpose'].every((key) => string(instance[key])) &&
        count(instance.repeat_index_x) &&
        count(instance.repeat_index_y) &&
        Array.isArray(instance.transform) &&
        instance.transform.length === 6 &&
        instance.transform.every(finite) &&
        (instance.bounding_box == null || validBounds(instance.bounding_box)),
      `Layout occurrence ${instance.index} has invalid metadata or geometry.`,
    );
    parents.set(instance.index, instance.parent_index);
  }
  for (const [index, parent] of parents) {
    assert(
      parent === null || parents.has(parent),
      `Layout occurrence ${index} references an unknown parent ${parent}.`,
    );
  }
  // Check each chain once, accepting parents declared after their children.
  const complete = new Set<number>();
  for (const index of parents.keys()) {
    const path = new Set<number>();
    let current: number | null = index;
    while (current !== null && !complete.has(current)) {
      assert(!path.has(current), `Layout occurrence ${current} has a cyclic parent hierarchy.`);
      path.add(current);
      current = parents.get(current)!;
    }
    for (const visited of path) complete.add(visited);
  }
  return new Set(parents.keys());
}

/** Validate the version and geometry at the file boundary, before creating a camera. */
export function parseReport(text: string): DiagnosticReport {
  const value: unknown = JSON.parse(text);
  assert(
    object(value) && value.schema_version === 1,
    'Unsupported DFM report. Expected schema_version 1.',
  );
  assert(
    object(value.input) && typeof value.input.path === 'string',
    'The report is missing its input identity.',
  );
  if (value.verdict === 'incomplete') {
    assert(
      object(value.error) && typeof value.error.message === 'string',
      'Incomplete report is missing its error message.',
    );
    return value as unknown as DiagnosticReport;
  }
  assert(
    value.verdict === 'pass' || value.verdict === 'fail',
    'The report has no valid check verdict.',
  );
  assert(
    object(value.coordinate_system) &&
      value.coordinate_system.unit === 'mm' &&
      value.coordinate_system.axes === 'x_right_y_up',
    'The viewer requires millimeters with X right / Y up coordinates.',
  );
  assert(
    object(value.summary) && object(value.pdk) && object(value.tool),
    'The report is missing run information.',
  );
  assert(
    string(value.generated_at) &&
      string(value.layout_target) &&
      string(value.coordinate_system.origin),
    'The report has invalid run metadata.',
  );
  assert(
    string(value.input.sha256) && count(value.input.size_bytes),
    'The report has invalid input identity metadata.',
  );
  assert(
    ['name', 'version'].every((key) => string((value.tool as Record<string, unknown>)[key])),
    'The report has invalid tool metadata.',
  );
  assert(
    ['id', 'name', 'revision', 'path', 'sha256'].every((key) =>
      string((value.pdk as Record<string, unknown>)[key]),
    ),
    'The report has invalid PDK metadata.',
  );
  assert(
    [
      'rules_configured',
      'rules_passed',
      'rules_warned',
      'rules_failed',
      'rules_skipped',
      'findings',
      'errors',
      'warnings',
      'waived',
    ].every((key) => count((value.summary as Record<string, unknown>)[key])),
    'The report has invalid summary counts. Expected nonnegative integers.',
  );
  if (value.waivers != null) {
    assert(
      object(value.waivers) &&
        string(value.waivers.path) &&
        count(value.waivers.applied) &&
        strings(value.waivers.expired) &&
        strings(value.waivers.unmatched),
      'The report has invalid waiver metadata. Expected path, applied count, and expired/unmatched string arrays.',
    );
  }
  assert(
    object(value.layout) && Array.isArray(value.layout.instances),
    'The report is missing its checked layout.',
  );
  assert(
    value.layout.bounding_box == null || validBounds(value.layout.bounding_box),
    'The layout has invalid bounds.',
  );
  assert(
    string(value.layout.kind) &&
      string(value.layout.coordinate_frame) &&
      nullableString(value.layout.selected_step),
    'The report has invalid layout metadata.',
  );
  const instances = validateInstances(value.layout.instances);
  assert(
    Array.isArray(value.rules) && Array.isArray(value.findings),
    'Expected DFM rules and findings arrays.',
  );
  const rules = new Map<string, Record<string, unknown>>();
  for (const rule of value.rules) {
    assert(
      object(rule) && typeof rule.id === 'string' && !rules.has(rule.id),
      'Invalid or duplicate DFM rule.',
    );
    assert(
      object(rule.view) &&
        string(rule.view.kind) &&
        string(rule.view.title) &&
        strings(rule.view.features) &&
        typeof rule.view.spatial === 'boolean',
      'This report lacks diagnostic view recipes. Regenerate it with the current pcb CLI.',
    );
    assert(
      object(rule.limit) &&
        string(rule.limit.pdk_value) &&
        finite(rule.limit.normalized_value) &&
        string(rule.limit.normalized_unit),
      `Rule ${rule.id} has no limit.`,
    );
    assert(
      ['title', 'subject', 'quantity', 'method'].every((key) => string(rule[key])) &&
        severity(rule.severity) &&
        ['pass', 'warning', 'fail', 'skipped'].includes(rule.status as string) &&
        ['minimum', 'maximum'].includes(rule.comparison as string) &&
        ['required', 'preferred'].includes(rule.tier as string) &&
        ['checked', 'finding_count', 'waived_count'].every((key) => count(rule[key])) &&
        nullableString(rule.skip_reason),
      `Rule ${rule.id} has invalid status or metadata.`,
    );
    rules.set(rule.id, rule);
  }
  const ids = new Set<string>();
  const materialLayers = new Map<string, string>();
  for (const finding of value.findings) {
    assert(
      object(finding) && typeof finding.id === 'string' && !ids.has(finding.id),
      'Invalid or duplicate finding ID.',
    );
    ids.add(finding.id);
    assert(
      typeof finding.waived === 'boolean' && severity(finding.severity),
      `Finding ${finding.id} must have a boolean waived flag and error/warning severity.`,
    );
    assert(
      string(finding.title) &&
        string(finding.message) &&
        nullableString(finding.waiver_reason) &&
        nullableString(finding.group_key),
      `Finding ${finding.id} has invalid text metadata.`,
    );
    assert(
      typeof finding.rule_id === 'string' && rules.has(finding.rule_id),
      `Finding ${finding.id} references an unknown rule.`,
    );
    assert(
      validMeasurement(finding.measurement) &&
        Array.isArray(finding.sites) &&
        Array.isArray(finding.layers) &&
        Array.isArray(finding.subjects),
      `Finding ${finding.id} lacks diagnostic measurements or sites.`,
    );
    assert(
      finding.layers.every(validLayer) &&
        finding.subjects.every((subject) => validSubject(subject, instances)),
      `Finding ${finding.id} has invalid layers or subjects (including source occurrence references).`,
    );
    validateEvidence(
      finding.evidence,
      `Finding ${finding.id}`,
      finding.layers.map((layer) => layer.name as string),
      materialLayers,
    );
    const rule = rules.get(finding.rule_id)!;
    assert(
      !(rule.view as { spatial: boolean }).spatial || finding.sites.length > 0,
      `Spatial finding ${finding.id} has no geometry sites.`,
    );
    const siteIds = new Set<string>();
    for (const site of finding.sites) {
      assert(
        object(site) && typeof site.id === 'string' && !siteIds.has(site.id),
        `Finding ${finding.id} has invalid or duplicate sites.`,
      );
      siteIds.add(site.id);
      assert(
        validBounds(site.bounding_box) &&
          validMeasurement(site.measurement) &&
          finite(site.uncertainty_mm) &&
          site.uncertainty_mm >= 0 &&
          string(site.measurement_kind) &&
          nullableString(site.note),
        `Site ${site.id} has invalid geometry or measurement.`,
      );
      assert(
        Array.isArray(site.witnesses) &&
          Array.isArray(site.layers) &&
          site.layers.every(validLayer) &&
          Array.isArray(site.subjects) &&
          site.subjects.every((subject) => validSubject(subject, instances)),
        `Site ${site.id} has invalid witnesses, layers, or subjects (including source occurrence references).`,
      );
      assert(
        site.witnesses.every((w) => object(w) && string(w.role) && point(w.point)),
        `Site ${site.id} has invalid witnesses.`,
      );
      validateEvidence(
        site.evidence,
        `Site ${site.id}`,
        site.layers.map((layer) => layer.name as string),
        materialLayers,
      );
    }
  }
  if (value.scene != null) {
    assert(
      object(value.scene) &&
        value.scene.schema_version === 1 &&
        validBounds(value.scene.bounds) &&
        Array.isArray(value.scene.passes),
      'Unsupported or malformed DFM scene.',
    );
    for (const pass of value.scene.passes) {
      assert(
        object(pass) &&
          typeof pass.label === 'string' &&
          typeof pass.feature === 'string' &&
          typeof pass.svg === 'string' &&
          string(pass.color) &&
          (pass.layer == null || typeof pass.layer === 'string'),
        'Invalid DFM scene pass.',
      );
    }
    const copperLayers = new Set(
      value.scene.passes.filter((pass) => pass.feature === 'copper').map((pass) => pass.layer),
    );
    for (const [layer, owner] of materialLayers) {
      assert(copperLayers.has(layer), `${owner} requires missing copper scene layer ${layer}.`);
    }
  }
  return value as unknown as Report;
}

export interface Entry {
  id: string;
  finding: Finding;
  site: Site | null;
  rule: Rule;
  family: string;
  cause: string;
  status: 'error' | 'warning' | 'waived';
  layers: string[];
  occurrences: number[];
  scopes: Set<number>;
  subject: string;
  search: string;
}
export interface Model {
  report: Report;
  entries: Entry[];
  instances: Map<number, Occurrence>;
  ancestors: Map<number, number[]>;
  bounds: Bounds;
  layers: string[];
}
export function subjectsOf(entry: Entry): Subject[] {
  return entry.site?.subjects || entry.finding.subjects;
}
export function evidenceOf(entry: Entry): Evidence[] {
  return entry.site?.evidence || entry.finding.evidence;
}
export function measurementOf(entry: Entry): Measurement {
  return entry.site?.measurement || entry.finding.measurement;
}
export function occurrenceName(instance: Occurrence): string {
  return `${instance.step} [${instance.repeat_index_x + 1},${instance.repeat_index_y + 1}] #${instance.index}`;
}
export function breadcrumb(model: Model, id: number): string {
  return [
    model.report.layout.selected_step,
    ...[...(model.ancestors.get(id) || [])]
      .reverse()
      .map((index) => occurrenceName(model.instances.get(index)!)),
  ]
    .filter(Boolean)
    .join(' / ');
}
export function createModel(report: Report): Model {
  const rules = new Map(report.rules.map((rule) => [rule.id, rule]));
  const instances = new Map(report.layout.instances.map((instance) => [instance.index, instance]));
  const ancestors = new Map<number, number[]>();
  for (const index of instances.keys()) {
    const chain: number[] = [];
    let current: number | null = index;
    while (current !== null && instances.has(current) && !chain.includes(current)) {
      chain.push(current);
      current = instances.get(current)!.parent_index;
    }
    ancestors.set(index, chain);
  }
  const entries: Entry[] = [];
  for (const finding of report.findings) {
    const rule = rules.get(finding.rule_id)!;
    const status = finding.waived ? 'waived' : finding.severity;
    const family = `${rule.view.kind}:${rule.tier}`;
    for (const site of finding.sites.length ? finding.sites : [null]) {
      const subjects = site?.subjects || finding.subjects;
      const occurrences = [
        ...new Set(
          subjects
            .map((subject) => (subject.provenance || subject.source)?.instance_index)
            .filter((id): id is number => id != null && instances.has(id)),
        ),
      ];
      const subject =
        [
          ...new Set(
            subjects.map((s) =>
              s.reference_designator
                ? `${s.reference_designator}${s.pin ? `.${s.pin}` : ''}`
                : s.net || s.name || pretty(s.kind),
            ),
          ),
        ].join(' ↔ ') || finding.title;
      const layers = (site?.layers || finding.layers).map((layer) => layer.name);
      entries.push({
        id: site ? `${finding.id}/${site.id}` : finding.id,
        finding,
        site,
        rule,
        family,
        cause: `${family}:${status}:${finding.group_key || finding.id}`,
        status,
        layers,
        occurrences,
        scopes: new Set(occurrences.flatMap((index) => ancestors.get(index) || [])),
        subject,
        search: [
          subject,
          finding.id,
          finding.message,
          rule.title,
          ...layers,
          ...occurrences.flatMap(
            (index) => ancestors.get(index)?.map((i) => occurrenceName(instances.get(i)!)) || [],
          ),
          ...subjects.flatMap((s) => [s.padstack_ref, s.reference_designator, s.pin, s.net]),
        ]
          .join(' ')
          .toLowerCase(),
      });
    }
  }
  entries.sort((a, b) => a.family.localeCompare(b.family) || a.cause.localeCompare(b.cause));
  return {
    report,
    entries,
    instances,
    ancestors,
    bounds:
      report.scene?.bounds ||
      report.layout.bounding_box ||
      unionBounds(entries.flatMap((entry) => (entry.site ? [entry.site.bounding_box] : []))),
    layers: [...new Set(entries.flatMap((entry) => entry.layers))].sort(),
  };
}
export function unionBounds(bounds: Bounds[]): Bounds {
  if (!bounds.length) return { min: { x: 0, y: 0 }, max: { x: 100, y: 100 } };
  return bounds.reduce((a, b) => ({
    min: { x: Math.min(a.min.x, b.min.x), y: Math.min(a.min.y, b.min.y) },
    max: { x: Math.max(a.max.x, b.max.x), y: Math.max(a.max.y, b.max.y) },
  }));
}
export function siteBounds(site: Site): Bounds {
  const b = site.bounding_box;
  const minimum =
    'required_mm' in site.measurement ? Math.max(0.25, site.measurement.required_mm * 4) : 0.25;
  const width = Math.max((b.max.x - b.min.x) * 1.5, minimum),
    height = Math.max((b.max.y - b.min.y) * 1.5, minimum);
  const x = (b.min.x + b.max.x) / 2,
    y = (b.min.y + b.max.y) / 2;
  return {
    min: { x: x - width / 2, y: y - height / 2 },
    max: { x: x + width / 2, y: y + height / 2 },
  };
}
export function dimensionFor(site: Site): [Point, Point] | null {
  if (site.measurement_kind === 'inscribed_width') {
    const disk = site.evidence.find(
      (item) =>
        item.role === 'inscribed_width_disk' && item.center && item.diameter && item.diameter > 0,
    );
    return disk?.center && disk.diameter
      ? [
          { x: disk.center.x - disk.diameter / 2, y: disk.center.y },
          { x: disk.center.x + disk.diameter / 2, y: disk.center.y },
        ]
      : null;
  }
  if (site.measurement_kind === 'nominal_width') {
    const construction = site.evidence.find(
      (item) => item.role === 'nominal_width_dimension' && item.start && item.end,
    );
    return construction?.start && construction.end ? [construction.start, construction.end] : null;
  }
  if (
    ['diameter', 'clearance', 'radial_enclosure'].includes(site.measurement_kind) &&
    site.witnesses.length >= 2
  ) {
    const [a, b] = site.witnesses.map((witness) => witness.point);
    return a.x !== b.x || a.y !== b.y ? [a, b] : null;
  }
  return null;
}

export interface Filters {
  query: string;
  status: string;
  layer: string;
  occurrence: string;
}
export function filterEntries(entries: Entry[], filters: Filters): Entry[] {
  const query = filters.query.trim().toLowerCase();
  return entries.filter(
    (entry) =>
      (!query || entry.search.includes(query)) &&
      (filters.status === 'all' ||
        (filters.status === 'active' ? !entry.finding.waived : entry.status === filters.status)) &&
      (!filters.layer || entry.layers.includes(filters.layer)) &&
      (!filters.occurrence || entry.scopes.has(Number(filters.occurrence))),
  );
}
export function passApplies(
  pass: { feature: string; layer: string | null },
  entry: Entry,
): boolean {
  return (
    entry.rule.view.features.includes(pass.feature) &&
    (!pass.layer || entry.layers.includes(pass.layer))
  );
}
