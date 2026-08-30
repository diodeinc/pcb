import { useDeferredValue, useEffect, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import {
  breadcrumb,
  createModel,
  evidenceOf,
  filterEntries,
  measurementOf,
  measurementValue,
  number,
  occurrenceName,
  pretty,
  requiredValue,
  subjectsOf,
} from './model';
import type { Entry, Filters, Model } from './model';
import { compilePass } from './scene';
import type { Report, Subject } from './types';
import { Viewer } from './Viewer';

let sceneSequence = 0;
const countLabel = (count: number, noun: string) => `${count} ${noun}${count === 1 ? '' : 's'}`;
function Properties({ children }: { children: ReactNode }) {
  return (
    <table className="properties">
      <tbody>{children}</tbody>
    </table>
  );
}
function Property({ label, value }: { label: string; value: ReactNode }) {
  return value == null || value === '' ? null : (
    <tr>
      <th>{label}</th>
      <td>{value}</td>
    </tr>
  );
}

function SubjectDetails({ subject, model }: { subject: Subject; model: Model }) {
  const source = subject.provenance || subject.source;
  const occurrence =
    source?.instance_index != null && model.instances.has(source.instance_index)
      ? breadcrumb(model, source.instance_index)
      : source?.step === model.report.layout.selected_step
        ? 'Checked root'
        : 'Unavailable';
  const sourceSummary = source
    ? [
        source.step,
        source.layer,
        source.instance_index != null ? `#${source.instance_index}` : null,
      ]
        .filter(Boolean)
        .join(' · ') || 'Source attribution unavailable'
    : 'Source attribution unavailable';
  return (
    <div className="subject">
      <div>
        <span className="muted">{pretty(subject.role)} · </span>
        <strong>{subject.reference_designator || subject.name || pretty(subject.kind)}</strong>
        {subject.pin && <span>.{subject.pin}</span>}
        {subject.net && <span className="subject-net">{subject.net}</span>}
      </div>
      <p className="subject-source" title={occurrence}>
        {sourceSummary}
      </p>
      {subject.drill_span && (
        <p className="subject-source">
          Copper span {subject.drill_span.first_copper_index + 1}–
          {subject.drill_span.last_copper_index + 1}
          {' · '}
          {pretty(subject.drill_span.interpretation)}
        </p>
      )}
      <details>
        <summary>Source record</summary>
        <Properties>
          <Property label="Kind" value={pretty(subject.kind)} />
          <Property label="Padstack" value={subject.padstack_ref} />
          <Property label="Occurrence" value={occurrence} />
          <Property
            label={subject.provenance ? 'Set / feature' : 'Flattened set / feature'}
            value={
              source
                ? `${source.set_index ?? '—'} / ${source.feature_index ?? '—'}`
                : 'Source attribution unavailable'
            }
          />
        </Properties>
      </details>
    </div>
  );
}
const explanations: Record<string, string> = {
  diameter: 'Diameter of the drilled circular opening.',
  nominal_width:
    'The stated nominal slot width; the materialized outline is checked for consistency.',
  inscribed_width:
    'Width is the diameter of the inscribed disk. The chord between boundary witnesses is not necessarily that diameter.',
  clearance: 'Shortest separation between the stated physical boundaries.',
  radial_enclosure:
    'Signed copper enclosure from the drilled-hole edge. A negative value means copper is missing inside that edge.',
  overlap: 'The subjects touch or overlap; a positive separation does not exist.',
  missing_copper:
    'Required copper is missing. A nominal radial measurement does not indicate an existing copper ring.',
};
function Inspector({ model, entry }: { model: Model; entry: Entry | null }) {
  const { report } = model;
  const m = entry ? measurementOf(entry) : null;
  const margin = m && ('margin_mm' in m ? m.margin_mm : m.margin_count);
  return (
    <>
      {entry && m && (
        <>
          <section className="measurement-section">
            <div className="section-title">
              <h3>Measurement</h3>
              <span className="muted">{entry.rule.tier}</span>
            </div>
            <p className="diagnostic-reason">{entry.finding.title}</p>
            <Properties>
              <Property
                label="Actual"
                value={<strong className={entry.status}>{measurementValue(m)}</strong>}
              />
              <Property
                label={entry.rule.tier === 'preferred' ? 'Preferred' : 'Required'}
                value={`${entry.rule.comparison === 'maximum' ? '≤' : '≥'} ${requiredValue(m)}`}
              />
              <Property
                label={
                  (margin || 0) < 0
                    ? entry.rule.comparison === 'maximum'
                      ? 'Excess'
                      : 'Shortfall'
                    : 'Margin'
                }
                value={`${number(Math.abs(margin || 0))} ${'actual_mm' in m ? 'mm' : 'layers'}`}
              />
              <Property
                label="Uncertainty"
                value={entry.site ? `± ${number(entry.site.uncertainty_mm)} mm` : null}
              />
            </Properties>
            <p className="measurement-meaning">
              {entry.site?.note ||
                (entry.site
                  ? explanations[entry.site.measurement_kind] || pretty(entry.site.measurement_kind)
                  : 'Count of conductive layers in the shared physical stackup.')}
            </p>
            {entry.finding.waived && (
              <p className="notice compact">
                Waived: {entry.finding.waiver_reason || 'No reason supplied'}
              </p>
            )}
          </section>
          <section className="check-context">
            <div className="section-title">
              <h3>Check context</h3>
            </div>
            <Properties>
              <Property label="Rule" value={entry.rule.title} />
              <Property label="Method" value={pretty(entry.rule.method)} />
              <Property
                label="Layers"
                value={entry.layers.length ? entry.layers.join(', ') : 'Physical layout boundaries'}
              />
              <Property
                label="Region (mm)"
                value={
                  entry.site
                    ? `[${number(entry.site.bounding_box.min.x)}, ${number(entry.site.bounding_box.min.y)}] → [${number(entry.site.bounding_box.max.x)}, ${number(entry.site.bounding_box.max.y)}]`
                    : null
                }
              />
            </Properties>
            {evidenceOf(entry).some((item) => item.role === 'candidate_region') && (
              <p className="measurement-meaning">
                Amber: search candidates. Red: verified measurement. Blue: required envelope.
              </p>
            )}
          </section>
          <section>
            <div className="section-title">
              <h3>Subjects</h3>
            </div>
            {subjectsOf(entry).map((subject, index) => (
              <SubjectDetails key={index} subject={subject} model={model} />
            ))}
          </section>
          <details className="inspector-details diagnostic-record">
            <summary>Diagnostic record</summary>
            <p>
              {entry.finding.sites.length > 1 && (
                <span className="muted">Finding summary (all sites): </span>
              )}
              {entry.finding.message}
            </p>
            {entry.site?.note && explanations[entry.site.measurement_kind] && (
              <p>{explanations[entry.site.measurement_kind]}</p>
            )}
            <Properties>
              <Property label="Quantity" value={pretty(entry.rule.quantity)} />
              <Property
                label="Construction"
                value={entry.site ? pretty(entry.site.measurement_kind) : 'Physical layer count'}
              />
              <Property label="PDK value" value={entry.rule.limit.pdk_value} />
              <Property label="Finding" value={entry.finding.id} />
              <Property label="Site" value={entry.site?.id} />
            </Properties>
            {evidenceOf(entry).map((item, index) => (
              <div className="evidence-item" key={index}>
                {pretty(item.role)}{' '}
                <span>
                  {item.kind === 'circle' && item.diameter != null
                    ? `⌀ ${number(item.diameter)} mm`
                    : item.paths.length
                      ? `${item.paths.length} ${item.kind === 'region' ? 'contours' : 'paths'}`
                      : pretty(item.kind)}
                </span>
              </div>
            ))}
          </details>
        </>
      )}
      <details className="inspector-details checks" open={!entry || undefined}>
        <summary>
          {report.rules.length} checks · {report.summary.rules_skipped} skipped
        </summary>
        {report.rules.map((rule) => (
          <div className="check-row" key={rule.id}>
            <div>
              <strong>{rule.title}</strong>
              <span className={`badge ${rule.status}`}>{rule.status}</span>
            </div>
            <p>
              {rule.comparison === 'maximum' ? '≤' : '≥'} {rule.limit.pdk_value} · {rule.checked}{' '}
              {pretty(rule.subject)} checked
              {rule.finding_count ? ` · ${countLabel(rule.finding_count, 'finding')}` : ''}
              {rule.waived_count ? ` · ${rule.waived_count} waived` : ''}
            </p>
            {rule.skip_reason && <p className="skip-reason">{rule.skip_reason}</p>}
          </div>
        ))}
      </details>
      <details className="inspector-details">
        <summary>Run information</summary>
        <Properties>
          <Property label="Input" value={report.input.path} />
          <Property
            label="Scope"
            value={`${pretty(report.layout.kind)} · ${report.layout.selected_step}`}
          />
          <Property label="Frame" value={pretty(report.layout.coordinate_frame)} />
          <Property label="Coordinates" value="mm · X right, Y up" />
          <Property label="Generated" value={report.generated_at} />
          <Property label="Tool" value={`${report.tool.name} ${report.tool.version}`} />
          <Property label="PDK" value={`${report.pdk.name} · ${report.pdk.revision}`} />
          <Property label="PDK source" value={report.pdk.path} />
          <Property label="Input SHA-256" value={report.input.sha256} />
          <Property label="PDK SHA-256" value={report.pdk.sha256} />
          <Property label="Waivers" value={report.waivers?.path} />
        </Properties>
        <p className="muted">
          A pass applies only to the configured checks and the selected scope.
        </p>
        {report.layout.coordinate_frame === 'selected_board' && (
          <p>
            Canonical board scope checks the selected board design, not other designs or panel
            support features.
          </p>
        )}
      </details>
    </>
  );
}

interface Family {
  key: string;
  entries: Entry[];
  buckets: Entry[][];
}
function FindingGroup({
  family,
  selected,
  onSelect,
}: {
  family: Family;
  selected: Entry | null;
  onSelect: (entry: Entry) => void;
}) {
  const active = selected?.family === family.key;
  const [expanded, setExpanded] = useState(active);
  const [page, setPage] = useState(0);
  const pageSize = 60;
  useEffect(() => {
    if (active) {
      setExpanded(true);
      const index = family.buckets.findIndex((bucket) =>
        bucket.some((entry) => entry.id === selected?.id),
      );
      setPage(Math.max(0, Math.floor(index / pageSize)));
    }
  }, [active, selected?.id, family.buckets]);
  const rule = family.entries[0].rule;
  const actualPage = Math.min(page, Math.floor((family.buckets.length - 1) / pageSize));
  return (
    <section className="finding-group">
      <button
        className="family-heading"
        onClick={() => setExpanded(!expanded)}
        aria-expanded={expanded}
      >
        <span className="chevron">{expanded ? '▾' : '▸'}</span>
        <strong>
          {rule.view.title}
          {rule.tier === 'preferred' ? ' · preferred' : ''}
        </strong>
        <span>{countLabel(family.entries.length, rule.view.spatial ? 'site' : 'finding')}</span>
      </button>
      {expanded && (
        <>
          {family.buckets
            .slice(actualPage * pageSize, (actualPage + 1) * pageSize)
            .map((bucket) => {
              const current = bucket.find((entry) => entry.id === selected?.id);
              const entry = current || bucket[0];
              const occurrences = new Set(bucket.flatMap((e) => e.occurrences)).size;
              const metadata = [
                bucket.length > 1 ? `${bucket.length} sites` : '',
                occurrences > 1 ? `${occurrences} placements` : '',
                entry.status === 'waived' ? 'waived' : '',
              ]
                .filter(Boolean)
                .join(' · ');
              return (
                <button
                  type="button"
                  key={entry.cause}
                  className={`finding-row ${current ? 'selected' : ''}`}
                  aria-current={!!current}
                  onClick={() => onSelect(entry)}
                  title={entry.subject}
                >
                  <span className="row-title">
                    <i className={`status-dot ${entry.status}`} />
                    <span>{entry.subject}</span>
                  </span>
                  <span className="row-values">
                    <span>{measurementValue(measurementOf(entry))}</span>
                    <span>{metadata}</span>
                  </span>
                </button>
              );
            })}
          {family.buckets.length > pageSize && (
            <div className="list-pager">
              <button
                onClick={() => setPage(actualPage - 1)}
                disabled={actualPage === 0}
                aria-label="Previous findings page"
              >
                ←
              </button>
              <span>
                {actualPage + 1} / {Math.ceil(family.buckets.length / pageSize)}
              </span>
              <button
                onClick={() => setPage(actualPage + 1)}
                disabled={(actualPage + 1) * pageSize >= family.buckets.length}
                aria-label="Next findings page"
              >
                →
              </button>
            </div>
          )}
        </>
      )}
    </section>
  );
}

function hashSelection() {
  try {
    return decodeURIComponent(location.hash.slice(1));
  } catch {
    return '';
  }
}
export function ReportView({ report }: { report: Report }) {
  const model = useMemo(() => createModel(report), [report]);
  const passes = useMemo(() => {
    const namespace = `scene-${++sceneSequence}`;
    return (report.scene?.passes || []).map((pass, index) =>
      compilePass(pass, `${namespace}-p${index}`),
    );
  }, [report]);
  const [filters, setFilters] = useState<Filters>({
    query: '',
    status: 'all',
    layer: '',
    occurrence: '',
  });
  const query = useDeferredValue(filters.query);
  const entries = useMemo(
    () => filterEntries(model.entries, { ...filters, query }),
    [model.entries, query, filters.status, filters.layer, filters.occurrence],
  );
  const [selection, setSelection] = useState(hashSelection);
  const selected =
    entries.find((entry) => entry.id === selection || entry.finding.id === selection) ||
    entries[0] ||
    null;
  const select = (entry: Entry) => {
    setSelection(entry.id);
    const url = new URL(location.href);
    url.hash = encodeURIComponent(entry.id);
    history.replaceState(null, '', url);
  };
  useEffect(() => {
    const read = () => setSelection(hashSelection());
    window.addEventListener('hashchange', read);
    return () => window.removeEventListener('hashchange', read);
  }, []);
  const navigate = (direction: number, byFinding = false) => {
    if (!selected) return;
    let index = entries.indexOf(selected) + direction;
    while (byFinding && entries[index]?.finding.id === selected.finding.id) index += direction;
    if (entries[index]) select(entries[index]);
  };
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (
        (event.target as Element).closest('input,select,textarea,[contenteditable="true"]') ||
        event.ctrlKey ||
        event.metaKey ||
        event.altKey
      )
        return;
      if (event.key === 'ArrowLeft' || event.key.toLowerCase() === 'k') {
        event.preventDefault();
        navigate(-1, event.key.toLowerCase() === 'k');
      }
      if (event.key === 'ArrowRight' || event.key.toLowerCase() === 'j') {
        event.preventDefault();
        navigate(1, event.key.toLowerCase() === 'j');
      }
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  }, [entries, selected]);
  const families = useMemo(() => {
    const groups = new Map<string, { entries: Entry[]; causes: Map<string, Entry[]> }>();
    for (const entry of entries) {
      if (!groups.has(entry.family)) groups.set(entry.family, { entries: [], causes: new Map() });
      const group = groups.get(entry.family)!;
      group.entries.push(entry);
      if (!group.causes.has(entry.cause)) group.causes.set(entry.cause, []);
      group.causes.get(entry.cause)!.push(entry);
    }
    return [...groups].map(([key, group]) => ({
      key,
      entries: group.entries,
      buckets: [...group.causes.values()],
    }));
  }, [entries]);
  const occurrenceOptions = useMemo(
    () =>
      [...new Set(model.entries.flatMap((entry) => [...entry.scopes]))].sort((a, b) => {
        const aa = [...model.ancestors.get(a)!].reverse(),
          bb = [...model.ancestors.get(b)!].reverse();
        for (let i = 0; i < Math.min(aa.length, bb.length); i++)
          if (aa[i] !== bb[i]) return aa[i] - bb[i];
        return aa.length - bb.length;
      }),
    [model],
  );
  const bucket = selected ? entries.filter((entry) => entry.cause === selected.cause) : [];
  const position = selected ? entries.indexOf(selected) : -1;
  return (
    <div className="report-layout">
      <aside className="finding-pane" aria-label="Diagnostic groups">
        <div className="filters">
          <input
            type="search"
            aria-label="Find diagnostics"
            placeholder="Find net, component, rule…"
            value={filters.query}
            onChange={(event) => setFilters({ ...filters, query: event.target.value })}
          />
          <div className="filter-row">
            <label>
              Status
              <select
                value={filters.status}
                onChange={(event) => setFilters({ ...filters, status: event.target.value })}
              >
                <option value="all">All</option>
                <option value="active">Not waived</option>
                <option value="error">Errors</option>
                <option value="warning">Warnings</option>
                <option value="waived">Waived</option>
              </select>
            </label>
            <label>
              Layer
              <select
                value={filters.layer}
                onChange={(event) => setFilters({ ...filters, layer: event.target.value })}
              >
                <option value="">All layers</option>
                {model.layers.map((layer) => (
                  <option key={layer}>{layer}</option>
                ))}
              </select>
            </label>
          </div>
          {occurrenceOptions.length > 0 && (
            <label>
              Occurrence
              <select
                value={filters.occurrence}
                onChange={(event) => setFilters({ ...filters, occurrence: event.target.value })}
              >
                <option value="">All (includes children)</option>
                {occurrenceOptions.map((index) => (
                  <option key={index} value={index}>
                    {'\u00a0\u00a0'.repeat(Math.max(0, model.ancestors.get(index)!.length - 1))}
                    {occurrenceName(model.instances.get(index)!)}
                  </option>
                ))}
              </select>
            </label>
          )}
          <div className="filter-count">
            {new Set(entries.map((entry) => entry.finding.id)).size} /{' '}
            {countLabel(report.findings.length, 'finding')}
            {' · '}
            {countLabel(entries.filter((entry) => entry.site).length, 'site')}
          </div>
        </div>
        <nav className="groups" aria-label="Findings">
          {families.map((family) => (
            <FindingGroup key={family.key} family={family} selected={selected} onSelect={select} />
          ))}
          {!families.length && (
            <p className="empty">
              {report.findings.length
                ? 'No matches. Adjust the filters.'
                : 'No reported violations. Review the check coverage and any skipped checks.'}
            </p>
          )}
        </nav>
        <div className="keyboard-help">← / → sites · J / K findings</div>
      </aside>
      <Viewer
        model={model}
        entry={selected}
        entries={entries}
        passes={passes}
        inspector={<Inspector model={model} entry={selected} />}
        navigation={
          <>
            <button
              onClick={() => navigate(-1)}
              disabled={position <= 0}
              aria-label="Previous site"
            >
              ←
            </button>
            <button
              onClick={() => navigate(1)}
              disabled={position < 0 || position >= entries.length - 1}
              aria-label="Next site"
            >
              →
            </button>
            <span className="site-position">
              {position >= 0 ? `${position + 1} / ${entries.length}` : '0 sites'}
            </span>
            {selected && (
              <span className="selected-subject" title={selected.subject}>
                {selected.subject}
              </span>
            )}
            {bucket.length > 1 && (
              <label className="site-choice">
                Site
                <select
                  aria-label="Site or repeated occurrence"
                  value={selected?.id}
                  onChange={(event) => {
                    const entry = bucket.find((entry) => entry.id === event.target.value);
                    if (entry) select(entry);
                  }}
                >
                  {bucket.map((entry, index) => (
                    <option key={entry.id} value={entry.id}>
                      {index + 1}. {entry.layers.join(', ')}
                      {entry.occurrences.length
                        ? ` · ${entry.occurrences.map((id) => `#${id}`).join(' ↔ ')}`
                        : ''}
                    </option>
                  ))}
                </select>
              </label>
            )}
          </>
        }
      />
    </div>
  );
}
