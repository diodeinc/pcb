import type { Finding, Report, Rule, Site } from './types';

export function siteFixture(overrides: Partial<Site> = {}): Site {
  return {
    id: 'site-clearance',
    measurement: { actual_mm: 0.1, required_mm: 0.2, margin_mm: -0.1 },
    measurement_kind: 'clearance',
    uncertainty_mm: 0.001,
    witnesses: [
      { role: 'first_boundary', point: { x: 41, y: 21 } },
      { role: 'second_boundary', point: { x: 41.1, y: 21 } },
    ],
    bounding_box: { min: { x: 40, y: 20 }, max: { x: 42, y: 22 } },
    layers: [{ name: 'F.Cu', function: 'conductor', side: 'top' }],
    subjects: [
      {
        role: 'first_boundary',
        kind: 'pad',
        name: null,
        reference_designator: 'U1',
        pin: '1',
        net: 'VCC',
        padstack_ref: 'pad-1',
        drill_span: null,
        provenance: null,
        source: {
          step: 'Fixture board',
          layer: 'F.Cu',
          set_index: 0,
          feature_index: 0,
          instance_index: null,
        },
      },
    ],
    evidence: [
      {
        role: 'first_boundary',
        kind: 'segment',
        center: null,
        diameter: null,
        start: { x: 41, y: 21 },
        end: { x: 41.1, y: 21 },
        bounding_box: null,
        paths: [],
      },
    ],
    note: null,
    ...overrides,
  };
}

export function findingFixture(overrides: Partial<Finding> = {}): Finding {
  const site = siteFixture();
  return {
    id: 'finding-clearance',
    rule_id: 'rule-clearance',
    severity: 'error',
    waived: false,
    waiver_reason: null,
    title: 'Copper clearance',
    message: 'Copper clearance is below the required minimum.',
    measurement: site.measurement,
    layers: site.layers,
    subjects: site.subjects,
    evidence: site.evidence,
    sites: [site],
    group_key: null,
    ...overrides,
  };
}

export function ruleFixture(overrides: Partial<Rule> = {}): Rule {
  return {
    id: 'rule-clearance',
    title: 'Copper clearance',
    severity: 'error',
    status: 'fail',
    comparison: 'minimum',
    limit: { pdk_value: '0.2 mm', normalized_value: 0.2, normalized_unit: 'mm' },
    subject: 'copper',
    quantity: 'clearance',
    method: 'physical_boundaries',
    checked: 1,
    finding_count: 1,
    waived_count: 0,
    skip_reason: null,
    tier: 'required',
    view: {
      kind: 'copper_clearance',
      title: 'Copper clearance',
      spatial: true,
      features: ['copper', 'board_outlines'],
    },
    ...overrides,
  };
}

/** A full layout with a small spatial finding and a nonspatial stackup finding. */
export function reportFixture(overrides: Partial<Report> = {}): Report {
  const bounds = { min: { x: 0, y: 0 }, max: { x: 100, y: 60 } };
  const svg = (body: string) =>
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 -60 100 60"><g transform="scale(1 -1)">${body}</g></svg>`;
  return {
    schema_version: 1,
    generated_at: '2026-08-29T12:00:00Z',
    verdict: 'fail',
    tool: { name: 'pcb', version: 'test' },
    input: { path: 'fixture.xml', sha256: 'fixture-input', size_bytes: 100 },
    pdk: {
      id: 'fixture',
      name: 'Fixture PDK',
      revision: '1',
      path: 'fixture.toml',
      sha256: 'fixture-pdk',
    },
    layout_target: 'board',
    coordinate_system: { unit: 'mm', axes: 'x_right_y_up', origin: 'ipc2581' },
    layout: {
      kind: 'board',
      selected_step: 'Fixture board',
      coordinate_frame: 'selected_board',
      bounding_box: bounds,
      instances: [],
    },
    waivers: null,
    summary: {
      rules_configured: 2,
      rules_passed: 0,
      rules_warned: 0,
      rules_failed: 2,
      rules_skipped: 0,
      findings: 2,
      errors: 2,
      warnings: 0,
      waived: 0,
    },
    rules: [
      ruleFixture(),
      ruleFixture({
        id: 'rule-stackup',
        title: 'Layer count',
        subject: 'stackup',
        quantity: 'layer_count',
        method: 'physical_stackup',
        limit: { pdk_value: '4 layers', normalized_value: 4, normalized_unit: 'layers' },
        view: { kind: 'layer_count', title: 'Layer count', spatial: false, features: [] },
      }),
    ],
    findings: [
      findingFixture(),
      findingFixture({
        id: 'finding-stackup',
        rule_id: 'rule-stackup',
        title: 'Layer count',
        message: 'The stackup has too few layers.',
        measurement: { actual_count: 2, required_count: 4, margin_count: -2 },
        layers: [
          { name: 'F.Cu', function: 'conductor', side: 'top' },
          { name: 'B.Cu', function: 'conductor', side: 'bottom' },
        ],
        subjects: [],
        evidence: [],
        sites: [],
      }),
    ],
    scene: {
      schema_version: 1,
      bounds,
      passes: [
        {
          label: 'Board outline',
          feature: 'board_outlines',
          layer: null,
          color: '#333333',
          svg: svg(
            '<path d="M0 0 H100 V60 H0 Z" fill="none" stroke="#333333" stroke-width="0.1"/>',
          ),
        },
        {
          label: 'F.Cu',
          feature: 'copper',
          layer: 'F.Cu',
          color: '#d87822',
          svg: svg(
            '<rect x="40" y="20" width="2" height="2" fill="#d87822"/><circle cx="90" cy="50" r="3" fill="#d87822"/>',
          ),
        },
      ],
    },
    ...overrides,
  };
}
