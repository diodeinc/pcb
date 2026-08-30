import { describe, expect, it } from 'vitest';
import { createModel, dimensionFor, filterEntries, parseReport, passApplies } from './model';
import { findingFixture, reportFixture, ruleFixture, siteFixture } from './test-fixtures';
import type { EvidenceDisplay, Occurrence, Report } from './types';

const fields = (value: object) => value as Record<string, unknown>;

describe('native evidence display boundary', () => {
  const curve = 'M2 0 A2 2 0 0 1 -2 0 A2 2 0 0 1 2 0 Z';
  const circle = { center: { x: 41, y: 21 }, diameter: 0.4 };
  const displays: EvidenceDisplay[] = [
    { kind: 'path', paths: [curve, 'M0 0 H1 V1 H0 Z'], fill_rule: 'evenodd' },
    { kind: 'path', paths: [curve], fill_rule: 'nonzero' },
    {
      kind: 'round_stroke',
      paths: [
        [
          { x: 1, y: 2 },
          { x: 3, y: 4 },
        ],
      ],
      width_mm: 0.2,
    },
    { kind: 'circle_minus_layer', ...circle, layer: 'F.Cu' },
    { kind: 'circle_intersection', first: circle, second: { ...circle, diameter: 0.5 } },
  ];
  const withDisplay = (display: unknown) => {
    const report = reportFixture();
    const evidence = { ...report.findings[0].sites[0].evidence[0] };
    fields(evidence).display = display;
    report.findings[0].sites[0].evidence = [evidence];
    return report;
  };

  it.each(displays)(
    'retains a valid $kind construction without replacing measured evidence',
    (display) => {
      const report = withDisplay(display);
      const parsed = parseReport(JSON.stringify(report)) as Report;
      expect(parsed.findings[0].sites[0].evidence[0]).toEqual(
        report.findings[0].sites[0].evidence[0],
      );
      expect(parsed.findings[0].id).toBe(report.findings[0].id);
      expect(parsed.findings[0].sites[0].id).toBe(report.findings[0].sites[0].id);
    },
  );

  it('accepts circle/layer metadata without a scene and leaves legacy fallback geometry available', () => {
    const report = withDisplay({ kind: 'circle_minus_layer', ...circle, layer: 'F.Cu' });
    delete report.scene;
    const legacy = report.findings[0].sites[0].evidence[0];
    legacy.kind = 'region';
    legacy.paths = [
      [
        { x: 41, y: 21 },
        { x: 41.1, y: 21 },
        { x: 41.1, y: 21.1 },
      ],
    ];
    const parsed = parseReport(JSON.stringify(report)) as Report;
    expect(parsed.scene).toBeUndefined();
    expect(parsed.findings[0].sites[0].evidence[0].paths).toEqual(legacy.paths);
  });

  it('validates display recipes in aggregate findings as well as individual sites', () => {
    const report = reportFixture();
    const evidence = { ...report.findings[0].evidence[0] };
    fields(evidence).display = { kind: 'round_stroke', width_mm: -1, paths: [] };
    report.findings[0].evidence = [evidence];
    expect(() => parseReport(JSON.stringify(report))).toThrow(/Finding .*round stroke/);
  });

  it.each([
    ['null', null],
    ['unknown kind', { kind: 'spline' }],
    [
      'unknown fields',
      { kind: 'path', paths: [curve], fill_rule: 'nonzero', href: 'https://example.com/shape' },
    ],
    ['missing fill rule', { kind: 'path', paths: [curve] }],
    ['unknown fill rule', { kind: 'path', paths: [curve], fill_rule: 'winding' }],
    ['no native paths', { kind: 'path', paths: [], fill_rule: 'nonzero' }],
    ['empty native path', { kind: 'path', paths: [' '], fill_rule: 'nonzero' }],
    ['single path string', { kind: 'path', paths: curve, fill_rule: 'nonzero' }],
    ['nonfinite native path', { kind: 'path', paths: ['M0 0 L1e309 0'], fill_rule: 'nonzero' }],
    ['malformed arc', { kind: 'path', paths: ['M0 0 A2 2 0 2 0 1 1'], fill_rule: 'nonzero' }],
    ['markup in path', { kind: 'path', paths: ['M0 0</path><script/>'], fill_rule: 'nonzero' }],
    [
      'zero width',
      {
        kind: 'round_stroke',
        width_mm: 0,
        paths: [
          [
            { x: 0, y: 0 },
            { x: 1, y: 1 },
          ],
        ],
      },
    ],
    [
      'negative width',
      {
        kind: 'round_stroke',
        width_mm: -0.1,
        paths: [
          [
            { x: 0, y: 0 },
            { x: 1, y: 1 },
          ],
        ],
      },
    ],
    [
      'infinite width',
      {
        kind: 'round_stroke',
        width_mm: Infinity,
        paths: [
          [
            { x: 0, y: 0 },
            { x: 1, y: 1 },
          ],
        ],
      },
    ],
    ['empty stroke', { kind: 'round_stroke', width_mm: 0.2, paths: [] }],
    ['incomplete stroke', { kind: 'round_stroke', width_mm: 0.2, paths: [[{ x: 0, y: 0 }]] }],
    [
      'nonfinite stroke',
      {
        kind: 'round_stroke',
        width_mm: 0.2,
        paths: [
          [
            { x: 0, y: 0 },
            { x: Infinity, y: 1 },
          ],
        ],
      },
    ],
    [
      'nonfinite center',
      { kind: 'circle_minus_layer', center: { x: Infinity, y: 0 }, diameter: 1, layer: 'F.Cu' },
    ],
    [
      'zero diameter',
      { kind: 'circle_minus_layer', center: { x: 0, y: 0 }, diameter: 0, layer: 'F.Cu' },
    ],
    ['missing layer', { kind: 'circle_minus_layer', ...circle }],
    ['missing second circle', { kind: 'circle_intersection', first: circle }],
    [
      'negative circle diameter',
      { kind: 'circle_intersection', first: circle, second: { ...circle, diameter: -1 } },
    ],
    [
      'extra circle fields',
      { kind: 'circle_intersection', first: circle, second: { ...circle, radius: 1 } },
    ],
  ])('rejects %s rather than silently reverting a malformed construction', (_name, display) => {
    expect(() => parseReport(JSON.stringify(withDisplay(display)))).toThrow();
  });

  it('rejects subtracting an undeclared layer even if another scene layer exists', () => {
    const report = withDisplay({ kind: 'circle_minus_layer', ...circle, layer: 'B.Cu' });
    report.scene!.passes.push({ ...report.scene!.passes[1], layer: 'B.Cu' });
    expect(() => parseReport(JSON.stringify(report))).toThrow(/undeclared layer B.Cu/);
  });

  it.each(['missing', 'mask_openings'])(
    'requires copper material when the scene is present (%s)',
    (feature) => {
      const report = withDisplay({ kind: 'circle_minus_layer', ...circle, layer: 'F.Cu' });
      if (feature === 'missing') report.scene!.passes = [];
      else report.scene!.passes[1].feature = feature;
      expect(() => parseReport(JSON.stringify(report))).toThrow(/missing copper scene layer F.Cu/);
    },
  );
});

describe('diagnostic file boundary', () => {
  it('loads ordinary JSON without inventing board context', () => {
    const report = reportFixture();
    delete report.scene;
    const parsed = parseReport(JSON.stringify(report)) as Report;
    expect(parsed.scene).toBeUndefined();
    expect(createModel(parsed).entries).toHaveLength(2);
  });

  it('keeps an incomplete run distinct from a passing empty report', () => {
    const report = parseReport(
      JSON.stringify({
        schema_version: 1,
        verdict: 'incomplete',
        input: { path: 'bad.xml' },
        error: { message: 'Invalid IPC geometry' },
      }),
    );
    expect(report.verdict).toBe('incomplete');
    expect(report).not.toHaveProperty('summary');
  });

  it.each([
    [
      'report schema',
      (report: Report) => {
        report.schema_version = 2;
      },
    ],
    [
      'scene schema',
      (report: Report) => {
        report.scene!.schema_version = 2;
      },
    ],
    [
      'units',
      (report: Report) => {
        report.coordinate_system.unit = 'mil';
      },
    ],
    [
      'axes',
      (report: Report) => {
        report.coordinate_system.axes = 'x_right_y_down';
      },
    ],
    [
      'inverted bounds',
      (report: Report) => {
        report.findings[0].sites[0].bounding_box.min.x = 200;
      },
    ],
    [
      'missing sites',
      (report: Report) => {
        report.findings[0].sites = [];
      },
    ],
    [
      'unknown rule',
      (report: Report) => {
        report.findings[0].rule_id = 'missing';
      },
    ],
    [
      'duplicate finding',
      (report: Report) => {
        report.findings.push(report.findings[0]);
      },
    ],
    [
      'duplicate site',
      (report: Report) => {
        report.findings[0].sites.push(report.findings[0].sites[0]);
      },
    ],
    [
      'invalid witness',
      (report: Report) => {
        report.findings[0].sites[0].witnesses[0].point.x = Infinity;
      },
    ],
  ])('rejects %s instead of showing misleading geometry', (_name, mutate) => {
    const report = reportFixture();
    mutate(report);
    expect(() => parseReport(JSON.stringify(report))).toThrow();
  });

  it.each(['false', 0, null, undefined])('rejects a nonboolean waived flag (%s)', (waived) => {
    const report = reportFixture();
    fields(report.findings[0]).waived = waived;
    expect(() => parseReport(JSON.stringify(report))).toThrow(/boolean waived flag/);
  });

  it.each(['info', 'ERROR', {}, null])('rejects invalid finding severity (%s)', (severity) => {
    const report = reportFixture();
    fields(report.findings[0]).severity = severity;
    expect(() => parseReport(JSON.stringify(report))).toThrow(/error\/warning severity/);
  });

  it.each([
    {},
    { path: 'waivers.json', applied: 0 },
    { path: {}, applied: 0, expired: [], unmatched: [] },
    { path: 'waivers.json', applied: '0', expired: [], unmatched: [] },
    { path: 'waivers.json', applied: -1, expired: [], unmatched: [] },
    { path: 'waivers.json', applied: 0, expired: 'finding', unmatched: [] },
    { path: 'waivers.json', applied: 0, expired: [], unmatched: [{}] },
  ])('rejects malformed waiver metadata before the app header reads it (%j)', (waivers) => {
    const report = reportFixture();
    fields(report).waivers = waivers;
    expect(() => parseReport(JSON.stringify(report))).toThrow(/invalid waiver metadata/);
  });

  it('accepts real waivers without suppressing other active findings', () => {
    const report = reportFixture();
    report.waivers = {
      path: 'waivers.json',
      applied: 1,
      expired: ['expired'],
      unmatched: ['unknown'],
    };
    report.findings[0].waived = true;
    report.summary.waived = 1;
    report.summary.errors = 1;
    const parsed = parseReport(JSON.stringify(report)) as Report;
    expect(parsed.waivers).toEqual(report.waivers);
    const model = createModel(parsed);
    const active = filterEntries(model.entries, {
      query: '',
      status: 'active',
      layer: '',
      occurrence: '',
    });
    expect(active.map((entry) => entry.finding.id)).toEqual(['finding-stackup']);
  });

  it.each([
    [
      'PDK name',
      (r: Report) => {
        fields(r.pdk).name = { text: 'PDK' };
      },
      /PDK metadata/,
    ],
    [
      'tool version',
      (r: Report) => {
        fields(r.tool).version = [];
      },
      /tool metadata/,
    ],
    [
      'summary count',
      (r: Report) => {
        fields(r.summary).errors = '0';
      },
      /summary counts/,
    ],
    [
      'missing summary count',
      (r: Report) => {
        delete fields(r.summary).warnings;
      },
      /summary counts/,
    ],
    [
      'fractional summary count',
      (r: Report) => {
        r.summary.waived = 0.5;
      },
      /summary counts/,
    ],
    [
      'input hash',
      (r: Report) => {
        fields(r.input).sha256 = {};
      },
      /input identity/,
    ],
    [
      'run timestamp',
      (r: Report) => {
        fields(r).generated_at = [];
      },
      /run metadata/,
    ],
    [
      'layout kind',
      (r: Report) => {
        fields(r.layout).kind = {};
      },
      /layout metadata/,
    ],
  ] as const)('rejects invalid %s metadata', (_name, mutate, message) => {
    const report = reportFixture();
    mutate(report);
    expect(() => parseReport(JSON.stringify(report))).toThrow(message);
  });

  it.each([
    [
      'finding layer',
      (r: Report) => {
        fields(r.findings[0].layers[0]).name = {};
      },
    ],
    [
      'subject text',
      (r: Report) => {
        fields(r.findings[0].subjects[0]).net = {};
      },
    ],
    [
      'source index',
      (r: Report) => {
        r.findings[0].subjects[0].source!.set_index = 0.5;
      },
    ],
    [
      'unresolved source occurrence',
      (r: Report) => {
        r.findings[0].subjects[0].source!.instance_index = 99;
      },
    ],
    [
      'unresolved site provenance',
      (r: Report) => {
        r.findings[0].sites[0].subjects[0].provenance = {
          ...r.findings[0].subjects[0].source!,
          instance_index: 99,
        };
      },
    ],
    [
      'drill span',
      (r: Report) => {
        r.findings[0].subjects[0].drill_span = {
          first_copper_index: 3,
          last_copper_index: 1,
          interpretation: 'through',
        };
      },
    ],
    [
      'site note',
      (r: Report) => {
        fields(r.findings[0].sites[0]).note = {};
      },
    ],
    [
      'witness role',
      (r: Report) => {
        fields(r.findings[0].sites[0].witnesses[0]).role = {};
      },
    ],
    [
      'evidence role',
      (r: Report) => {
        fields(r.findings[0].evidence[0]).role = {};
      },
    ],
    [
      'view features',
      (r: Report) => {
        fields(r.rules[0].view).features = ['copper', {}];
      },
    ],
    [
      'rule comparison',
      (r: Report) => {
        r.rules[0].comparison = 'less';
      },
    ],
  ] as const)('rejects invalid %s instead of failing during rendering', (_name, mutate) => {
    const report = reportFixture();
    mutate(report);
    expect(() => parseReport(JSON.stringify(report))).toThrow();
  });

  it.each([
    {
      actual_mm: 0.1,
      required_mm: 0.2,
      margin_mm: -0.1,
      actual_count: 1,
      required_count: 2,
      margin_count: -1,
    },
    { actual_mm: null, actual_count: 1, required_count: 2, margin_count: -1 },
    { required_mm: 0.2, actual_count: 1, required_count: 2, margin_count: -1 },
    { actual_count: 1.5, required_count: 2, margin_count: -0.5 },
  ])('rejects mixed or malformed measurement variants (%j)', (measurement) => {
    const report = reportFixture();
    fields(report.findings[0]).measurement = measurement;
    expect(() => parseReport(JSON.stringify(report))).toThrow(/measurements/);
    const siteReport = reportFixture();
    fields(siteReport.findings[0].sites[0]).measurement = measurement;
    expect(() => parseReport(JSON.stringify(siteReport))).toThrow(/geometry or measurement/);
  });
});

describe('layout hierarchy boundary', () => {
  const occurrence = (index: number, parent_index: number | null): Occurrence => ({
    index,
    parent_index,
    step: `step-${index}`,
    kind: parent_index === null ? 'panel' : 'board',
    purpose: 'product',
    transform: [1, 0, 0, 1, 0, 0],
    bounding_box: null,
    repeat_index_x: 0,
    repeat_index_y: 0,
  });
  const hierarchyReport = () => {
    const report = reportFixture();
    report.layout = {
      ...report.layout,
      kind: 'fab_panel',
      coordinate_frame: 'root_layout',
      instances: [occurrence(2, 1), occurrence(1, 0), occurrence(0, null)],
    };
    report.layout_target = 'board_array';
    report.findings[0].subjects[0].source!.instance_index = 2;
    return report;
  };

  it('accepts parents declared after children and retains descendant filtering', () => {
    const parsed = parseReport(JSON.stringify(hierarchyReport())) as Report;
    const model = createModel(parsed);
    expect(model.ancestors.get(2)).toEqual([2, 1, 0]);
    expect(
      filterEntries(model.entries, { query: '', status: 'all', layer: '', occurrence: '0' }),
    ).toHaveLength(1);
  });

  it.each([
    [
      'duplicate IDs',
      (r: Report) => {
        r.layout.instances.push(occurrence(2, null));
      },
      /duplicate layout occurrence/,
    ],
    [
      'unknown parent',
      (r: Report) => {
        r.layout.instances[0].parent_index = 99;
      },
      /unknown parent/,
    ],
    [
      'self parent',
      (r: Report) => {
        r.layout.instances[0].parent_index = 2;
      },
      /cyclic parent/,
    ],
    [
      'parent cycle',
      (r: Report) => {
        r.layout.instances[2].parent_index = 2;
      },
      /cyclic parent/,
    ],
    [
      'fractional ID',
      (r: Report) => {
        r.layout.instances[0].index = 2.5;
      },
      /occurrence index/,
    ],
    [
      'missing parent',
      (r: Report) => {
        delete fields(r.layout.instances[0]).parent_index;
      },
      /invalid metadata/,
    ],
    [
      'wrong transform length',
      (r: Report) => {
        r.layout.instances[0].transform = [1, 0, 0, 1];
      },
      /invalid metadata/,
    ],
    [
      'nonfinite transform',
      (r: Report) => {
        r.layout.instances[0].transform[0] = Infinity;
      },
      /invalid metadata/,
    ],
    [
      'negative repeat index',
      (r: Report) => {
        r.layout.instances[0].repeat_index_x = -1;
      },
      /invalid metadata/,
    ],
    [
      'object step name',
      (r: Report) => {
        fields(r.layout.instances[0]).step = {};
      },
      /invalid metadata/,
    ],
  ] as const)(
    'rejects %s before constructing misleading occurrence filters',
    (_name, mutate, message) => {
      const report = hierarchyReport();
      mutate(report);
      expect(() => parseReport(JSON.stringify(report))).toThrow(message);
    },
  );
});

describe('finding identity and scope', () => {
  it('does not collapse active errors into waived copies of the same proven cause', () => {
    const report = reportFixture({
      findings: [
        findingFixture({ group_key: 'same-source' }),
        findingFixture({ id: 'waived-copy', waived: true, group_key: 'same-source' }),
      ],
    });
    const [first, second] = createModel(report).entries;
    expect(first.cause).not.toBe(second.cause);
    expect(new Set([first.status, second.status])).toEqual(new Set(['error', 'waived']));
  });

  it('keeps preferred assessments separate from required limits', () => {
    const report = reportFixture({
      rules: [
        ruleFixture(),
        ruleFixture({ id: 'preferred', tier: 'preferred', severity: 'warning' }),
      ],
      findings: [
        findingFixture(),
        findingFixture({ id: 'preferred-finding', rule_id: 'preferred', severity: 'warning' }),
      ],
    });
    const entries = createModel(report).entries;
    expect(new Set(entries.map((entry) => entry.family)).size).toBe(2);
  });

  it('never groups findings solely because their values and subjects match', () => {
    const entries = createModel(
      reportFixture({
        findings: [findingFixture(), findingFixture({ id: 'independent-finding' })],
      }),
    ).entries;
    expect(entries[0].subject).toBe(entries[1].subject);
    expect(entries[0].cause).not.toBe(entries[1].cause);
  });

  it('filters descendants through the real occurrence hierarchy and uses canonical provenance', () => {
    const occurrence = (index: number, parent_index: number | null, kind: string): Occurrence => ({
      index,
      parent_index,
      kind,
      step: `step-${index}`,
      purpose: 'product',
      transform: [1, 0, 0, 1, 0, 0],
      bounding_box: null,
      repeat_index_x: 0,
      repeat_index_y: 0,
    });
    const site = siteFixture();
    site.subjects[0].source!.instance_index = 3;
    site.subjects[0].provenance = { ...site.subjects[0].source!, instance_index: 2 };
    const report = reportFixture({ findings: [findingFixture({ sites: [site] })] });
    report.layout.instances = [
      occurrence(0, null, 'panel'),
      occurrence(1, 0, 'panel'),
      occurrence(2, 1, 'board'),
      occurrence(3, null, 'board'),
    ];
    const model = createModel(report);
    expect(model.entries[0].occurrences).toEqual([2]);
    expect([...model.entries[0].scopes]).toEqual([2, 1, 0]);
    const filters = { query: '', status: 'all', layer: '', occurrence: '0' };
    expect(filterEntries(model.entries, filters)).toHaveLength(1);
    expect(filterEntries(model.entries, { ...filters, occurrence: '3' })).toHaveLength(0);
  });

  it('selects the declared layer and semantic feature, not every similarly named pass', () => {
    const entry = createModel(reportFixture()).entries.find((entry) => entry.site)!;
    expect(passApplies({ feature: 'copper', layer: 'F.Cu' }, entry)).toBe(true);
    expect(passApplies({ feature: 'copper', layer: 'B.Cu' }, entry)).toBe(false);
    expect(passApplies({ feature: 'mask_openings', layer: 'F.Cu' }, entry)).toBe(false);
    expect(passApplies({ feature: 'board_outlines', layer: null }, entry)).toBe(true);
  });
});

describe('measurement construction', () => {
  it('draws the actual width disk diameter, not its shorter witness chord', () => {
    const site = siteFixture({
      measurement_kind: 'inscribed_width',
      witnesses: [
        { role: 'a', point: { x: 1, y: 1 } },
        { role: 'b', point: { x: 1.01, y: 1 } },
      ],
      evidence: [
        {
          role: 'inscribed_width_disk',
          kind: 'circle',
          center: { x: 1, y: 1 },
          diameter: 0.1,
          paths: [],
          start: null,
          end: null,
          bounding_box: null,
        },
      ],
    });
    const [a, b] = dimensionFor(site)!;
    expect(Math.hypot(b.x - a.x, b.y - a.y)).toBeCloseTo(0.1);
    expect((a.x + b.x) / 2).toBe(1);
  });

  it.each(['overlap', 'missing_copper'])(
    'does not fabricate a length for %s',
    (measurement_kind) => {
      expect(dimensionFor(siteFixture({ measurement_kind }))).toBeNull();
    },
  );
});
