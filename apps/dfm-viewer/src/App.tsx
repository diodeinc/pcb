import { Component, useEffect, useRef, useState } from 'react';
import type { ErrorInfo, ReactNode } from 'react';
import { basename, parseReport, pretty } from './model';
import type { DiagnosticReport } from './types';
import { ReportView } from './ReportView';

interface LoadedReport {
  id: number;
  name: string;
  report: DiagnosticReport;
}
interface Source {
  name: string;
  read: () => Promise<string>;
}
let sequence = 0;
class ReportBoundary extends Component<{ children: ReactNode }, { error: string | null }> {
  state = { error: null as string | null };
  static getDerivedStateFromError(error: Error) {
    return { error: error.message };
  }
  componentDidCatch(_error: Error, _info: ErrorInfo) {
    /* Shown below; never send report data to telemetry. */
  }
  render() {
    return this.state.error ? (
      <div className="load-error" role="alert">
        <h2>Cannot display this report</h2>
        <p>{this.state.error}</p>
        <p>
          The diagnostic verdict has not been changed. Open a fresh export from the current CLI.
        </p>
      </div>
    ) : (
      this.props.children
    );
  }
}

export function App() {
  const input = useRef<HTMLInputElement>(null);
  const [reports, setReports] = useState<LoadedReport[]>([]);
  const [active, setActive] = useState<number | null>(null);
  const [loading, setLoading] = useState('');
  const [error, setError] = useState('');
  const [drop, setDrop] = useState(false);
  const loadGeneration = useRef(0);
  const initialLoad = useRef(false);
  const loaded = reports.find((report) => report.id === active);
  const report = loaded?.report;
  const load = async (sources: Source[]) => {
    const generation = ++loadGeneration.current;
    setError('');
    setActive(null);
    const next: LoadedReport[] = [],
      errors: string[] = [];
    for (const source of sources) {
      setLoading(`Loading ${source.name}…`);
      try {
        const text = await source.read();
        if (generation !== loadGeneration.current) return;
        // Allow the loading state to paint before parsing a large local report.
        await new Promise((resolve) => requestAnimationFrame(resolve));
        next.push({ id: ++sequence, name: source.name, report: parseReport(text) });
      } catch (cause) {
        errors.push(`${source.name}: ${cause instanceof Error ? cause.message : String(cause)}`);
      }
    }
    if (generation !== loadGeneration.current) return;
    setReports((current) => [...current, ...next]);
    setActive(next[0]?.id ?? null);
    setError(errors.join('\n'));
    setLoading('');
  };
  const openFiles = (files: FileList | File[]) => {
    const url = new URL(location.href);
    url.search = '';
    url.hash = '';
    history.replaceState(null, '', url);
    void load(Array.from(files).map((file) => ({ name: file.name, read: () => file.text() })));
  };
  useEffect(() => {
    if (initialLoad.current) return;
    initialLoad.current = true;
    const urls = new URLSearchParams(location.search).getAll('report');
    if (!urls.length) return;
    void load(
      urls.map((path) => ({
        name: basename(path),
        read: async () => {
          const url = new URL(path, location.href);
          if (url.origin !== location.origin)
            throw new Error(
              'Report links must be on this app’s origin. Open local files with Open JSON.',
            );
          const response = await fetch(url, { credentials: 'omit' });
          if (!response.ok) throw new Error(`Report request failed (${response.status}).`);
          return response.text();
        },
      })),
    );
  }, []);
  useEffect(() => {
    document.title = loaded
      ? `${loaded.name.replace(/\.dfm\.json$|\.json$/i, '')} — PCB DFM`
      : 'PCB DFM';
  }, [loaded]);
  const download = () => {
    if (!loaded) return;
    const url = URL.createObjectURL(
      new Blob([JSON.stringify(loaded.report)], { type: 'application/json' }),
    );
    const link = document.createElement('a');
    link.href = url;
    link.download = loaded.name.endsWith('.json') ? loaded.name : `${loaded.name}.json`;
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  return (
    <div
      className={`app ${drop ? 'drag-over' : ''}`}
      onDragOver={(event) => {
        if (event.dataTransfer.types.includes('Files')) {
          event.preventDefault();
          setDrop(true);
        }
      }}
      onDragLeave={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node)) setDrop(false);
      }}
      onDrop={(event) => {
        event.preventDefault();
        setDrop(false);
        if (event.dataTransfer.files.length) openFiles(event.dataTransfer.files);
      }}
    >
      <header className="masthead">
        <div className="heading">
          <h1>PCB DFM</h1>
          {report && (
            <span className={`badge ${report.verdict}`}>
              {report.verdict === 'incomplete' ? 'Check incomplete' : report.verdict}
            </span>
          )}
          {reports.length > 1 ? (
            <select
              className="report-select"
              aria-label="Loaded report"
              value={active ?? ''}
              onChange={(event) => {
                setActive(Number(event.target.value));
                const url = new URL(location.href);
                url.hash = '';
                history.replaceState(null, '', url);
              }}
            >
              <option value="" disabled>
                Select report
              </option>
              {reports.map((item) => (
                <option key={item.id} value={item.id}>
                  {item.name}
                </option>
              ))}
            </select>
          ) : (
            loaded && (
              <strong className="report-name" title={loaded.name}>
                {loaded.name.replace(/\.dfm\.json$|\.json$/i, '')}
              </strong>
            )
          )}
          <div className="file-actions">
            {loaded && <button onClick={download}>Save JSON</button>}
            <button onClick={() => input.current?.click()}>Open JSON</button>
            <input
              ref={input}
              type="file"
              accept=".json,application/json"
              multiple
              hidden
              onChange={(event) => {
                if (event.target.files?.length) openFiles(event.target.files);
                event.target.value = '';
              }}
            />
          </div>
        </div>
        {report && report.verdict !== 'incomplete' && (
          <div className="run-line">
            <span className="scope">{pretty(report.layout.kind)}</span>
            <span title={report.pdk.path}>{report.pdk.name}</span>
            <span className="run-counts">
              <strong className="error">{report.summary.errors} errors</strong>
              {report.summary.warnings > 0 && (
                <span className="warning">{report.summary.warnings} warnings</span>
              )}
              {report.summary.waived > 0 && <span>{report.summary.waived} waived</span>}
              <span>{report.summary.rules_skipped} skipped</span>
            </span>
          </div>
        )}
      </header>
      {error && (
        <div className="notice error-notice" role="alert">
          {error}
          <button onClick={() => setError('')} aria-label="Dismiss error">
            ×
          </button>
        </div>
      )}
      {report?.verdict !== 'incomplete' &&
        report?.waivers &&
        (report.waivers.expired.length > 0 || report.waivers.unmatched.length > 0) && (
          <div className="notice compact">
            Waivers: {report.waivers.expired.length} expired, {report.waivers.unmatched.length}{' '}
            unmatched. Expired waivers do not suppress errors.
          </div>
        )}
      {loading ? (
        <div className="landing" role="status">
          <h2>{loading}</h2>
          <p>Reading diagnostic data and vector geometry locally.</p>
        </div>
      ) : report?.verdict === 'incomplete' ? (
        <div className="load-error">
          <h2>Check incomplete</h2>
          <p>The check stopped before it could produce a verdict.</p>
          <pre>{report.error.message}</pre>
          <p className="muted">{report.input.path}</p>
        </div>
      ) : report ? (
        <ReportBoundary key={loaded!.id}>
          <ReportView report={report} />
        </ReportBoundary>
      ) : (
        <main className="landing">
          <h2>Open a DFM report</h2>
          <p>
            Drop diagnostic JSON here, or choose{' '}
            <button onClick={() => input.current?.click()}>Open JSON</button>. Files stay in your
            browser.
          </p>
          <pre>pcb ipc dfm check board.xml --pdk standard --include-geometry -o board.dfm.json</pre>
          <p className="muted">
            Full vector layers, per-site evidence, and board / array / panel scope.
            <br />
            Plain diagnostic JSON also works; board context requires <code>--include-geometry</code>
            .
          </p>
        </main>
      )}
      {drop && <div className="drop-overlay">Drop DFM JSON to open</div>}
    </div>
  );
}
