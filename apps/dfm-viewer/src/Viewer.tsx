import { createElement, memo, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import type { PointerEvent as ReactPointerEvent, ReactNode, RefObject } from 'react';
import {
  fitBounds,
  panCamera,
  placeLabel,
  pointToScreen,
  scaleBar,
  screenToPoint,
  visibleBounds,
  zoomCamera,
} from './camera';
import type { Bounds, Camera, Point, Size } from './camera';
import {
  breadcrumb,
  dimensionFor,
  evidenceOf,
  measurementOf,
  measurementValue,
  number,
  passApplies,
  pretty,
  siteBounds,
  unionBounds,
} from './model';
import type { Entry, Model } from './model';
import { compileMaterialPass } from './scene';
import type { CompiledPass, SvgNode } from './scene';
import { EvidenceGeometry } from './EvidenceGeometry';

function useSize<T extends HTMLElement | SVGSVGElement>(
  ref: RefObject<T | null>,
  enabled = true,
): Size {
  const [size, setSize] = useState<Size>({ width: 0, height: 0 });
  useLayoutEffect(() => {
    if (!enabled || !ref.current) return;
    const observer = new ResizeObserver(([entry]) =>
      setSize({ width: entry.contentRect.width, height: entry.contentRect.height }),
    );
    observer.observe(ref.current);
    return () => observer.disconnect();
  }, [ref, enabled]);
  return size;
}
const transform = (camera: Camera) =>
  `translate(${camera.x} ${camera.y}) scale(${camera.scale} ${-camera.scale})`;
function screenRect(bounds: Bounds, camera: Camera) {
  const point = pointToScreen({ x: bounds.min.x, y: bounds.max.y }, camera);
  return {
    x: point.x,
    y: point.y,
    width: (bounds.max.x - bounds.min.x) * camera.scale,
    height: (bounds.max.y - bounds.min.y) * camera.scale,
  };
}
function renderNode(node: SvgNode, index: number): ReactNode {
  return createElement(node.tag, { ...node.attrs, key: index }, node.children.map(renderNode));
}
export const SceneDefinitions = memo(function SceneDefinitions({
  passes,
}: {
  passes: CompiledPass[];
}) {
  return (
    <svg width="0" height="0" className="scene-definitions" aria-hidden="true">
      <defs>
        {passes.map((pass) => (
          <g id={pass.id} key={pass.id}>
            {pass.nodes.map(renderNode)}
          </g>
        ))}
      </defs>
    </svg>
  );
});
const SceneUses = memo(function SceneUses({ passes }: { passes: CompiledPass[] }) {
  return (
    <g className="scene-context">
      {passes.map((pass) => (
        <use key={pass.id} href={`#${pass.id}`} data-layer={pass.label} />
      ))}
    </g>
  );
});

function Scale({ camera, size, name }: { camera: Camera; size: Size; name: string }) {
  const bar = scaleBar(camera.scale, Math.min(110, size.width * 0.32));
  const x = 15,
    y = size.height - 15;
  return (
    <g
      className="scale-bar"
      aria-label={`${name} scale: ${bar.label}`}
      data-mm={bar.lengthMm}
      data-pixels={bar.pixels}
      data-pixels-per-mm={camera.scale}
    >
      <rect
        x={x - 7}
        y={y - 30}
        width={Math.max(bar.pixels + 14, 70)}
        height={38}
        rx="2"
        className="scale-background"
      />
      <text x={x} y={y - 12}>
        {bar.label}
      </text>
      <path d={`M${x} ${y - 5}v5h${bar.pixels}v-5`} fill="none" />
    </g>
  );
}

function MeasurementOverlay({ entry, camera, size }: { entry: Entry; camera: Camera; size: Size }) {
  const labelRef = useRef<SVGTextElement>(null);
  const [labelSize, setLabelSize] = useState({ width: 120, height: 28 });
  const site = entry.site!;
  const dimension = dimensionFor(site);
  const b = site.bounding_box;
  const anchorWorld = dimension
    ? { x: (dimension[0].x + dimension[1].x) / 2, y: (dimension[0].y + dimension[1].y) / 2 }
    : { x: (b.min.x + b.max.x) / 2, y: (b.min.y + b.max.y) / 2 };
  const anchor = pointToScreen(anchorWorld, camera);
  const label =
    site.measurement_kind === 'missing_copper'
      ? 'Required copper missing'
      : `${['diameter', 'inscribed_width'].includes(site.measurement_kind) ? '⌀ ' : ''}${measurementValue(measurementOf(entry))}`;
  useLayoutEffect(() => {
    const text = labelRef.current;
    if (!text) return;
    const measure = () => {
      const width = Math.ceil(text.getComputedTextLength()) + 16;
      setLabelSize((current) => (current.width === width ? current : { width, height: 28 }));
    };
    measure();
    // Fonts and styles may finish resolving after the first layout.
    const observer = new ResizeObserver(measure);
    observer.observe(text);
    return () => observer.disconnect();
  }, [label]);
  const rect = screenRect(b, camera);
  const position = placeLabel(anchor, labelSize, size, {
    min: { x: rect.x, y: rect.y },
    max: { x: rect.x + rect.width, y: rect.y + rect.height },
  });
  const leaderEnd = {
    x: Math.max(position.x, Math.min(position.x + labelSize.width, anchor.x)),
    y: Math.max(position.y, Math.min(position.y + labelSize.height, anchor.y)),
  };
  const anchorVisible =
    anchor.x >= 0 && anchor.x <= size.width && anchor.y >= 0 && anchor.y <= size.height;
  const points = dimension?.map((point) => pointToScreen(point, camera));
  return (
    <g className="measurement-overlay" pointerEvents="none">
      {points && (
        <g className="dimension-line">
          <line x1={points[0].x} y1={points[0].y} x2={points[1].x} y2={points[1].y} />
          {points.map((p, index) => (
            <circle key={index} cx={p.x} cy={p.y} r="2.5" />
          ))}
        </g>
      )}
      <g className="measurement-callout" visibility={anchorVisible ? 'visible' : 'hidden'}>
        <line x1={anchor.x} y1={anchor.y} x2={leaderEnd.x} y2={leaderEnd.y} />
        <rect
          x={position.x}
          y={position.y}
          width={labelSize.width}
          height={labelSize.height}
          rx="3"
        />
        <text ref={labelRef} x={position.x + 8} y={position.y + 18}>
          {label}
        </text>
      </g>
    </g>
  );
}

function MiniMap({
  model,
  entry,
  entries,
  passes,
  camera,
  size,
  onCenter,
}: {
  model: Model;
  entry: Entry | null;
  entries: Entry[];
  passes: CompiledPass[];
  camera: Camera;
  size: Size;
  onCenter: (world: Point) => void;
}) {
  const ref = useRef<SVGSVGElement>(null);
  const mapSize = useSize(ref);
  const view = visibleBounds(camera, size);
  // Keep both the layout and the entire current view in sight, even after panning off-board.
  const mapCamera = fitBounds(unionBounds([model.bounds, view]), mapSize, 17);
  const capturedCamera = useRef<Camera | null>(null);
  const at = (event: ReactPointerEvent<SVGSVGElement>, current: Camera) => {
    const rect = event.currentTarget.getBoundingClientRect();
    onCenter(screenToPoint({ x: event.clientX - rect.left, y: event.clientY - rect.top }, current));
  };
  const roi = entry?.site ? screenRect(entry.site.bounding_box, mapCamera) : null;
  const center = roi ? { x: roi.x + roi.width / 2, y: roi.y + roi.height / 2 } : null;
  const markers = useMemo(() => {
    if (!entry) return '';
    const bins = new Map<string, Point>();
    for (const candidate of entries) {
      if (
        !candidate.site ||
        candidate.family !== entry.family ||
        !candidate.layers.some((layer) => entry.layers.includes(layer))
      )
        continue;
      const b = candidate.site.bounding_box;
      const p = pointToScreen(
        { x: (b.min.x + b.max.x) / 2, y: (b.min.y + b.max.y) / 2 },
        mapCamera,
      );
      // Do not cover the selected region with nearby or coincident site dots.
      if (center && Math.abs(p.x - center.x) < 4 && Math.abs(p.y - center.y) < 4) continue;
      bins.set(`${Math.round(p.x / 4)},${Math.round(p.y / 4)}`, p);
    }
    return [...bins.values()].map((p) => `M${p.x} ${p.y}h0.01`).join(' ');
  }, [entries, entry, mapCamera.x, mapCamera.y, mapCamera.scale]);
  // Extend to the map edges, leaving the exact finding box unobscured.
  const crosshair =
    roi && center
      ? [
          `M0 ${center.y}H${Math.max(0, roi.x - 3)}`,
          `M${Math.min(mapSize.width, roi.x + roi.width + 3)} ${center.y}H${mapSize.width}`,
          `M${center.x} 0V${Math.max(0, roi.y - 3)}`,
          `M${center.x} ${Math.min(mapSize.height, roi.y + roi.height + 3)}V${mapSize.height}`,
        ].join(' ')
      : '';
  return (
    <section className="minimap-section" aria-label="Location in layout">
      <div className="section-title">
        <h3>Location</h3>
        <span className="map-hint">Click / drag to navigate</span>
      </div>
      <svg
        ref={ref}
        className="minimap"
        aria-label="Layout minimap; click or drag to move the main view"
        viewBox={`0 0 ${mapSize.width || 1} ${mapSize.height || 1}`}
        onPointerDown={(event) => {
          if (event.button !== 0) return;
          capturedCamera.current = mapCamera;
          event.currentTarget.setPointerCapture(event.pointerId);
          at(event, mapCamera);
        }}
        onPointerMove={(event) => {
          if (capturedCamera.current) at(event, capturedCamera.current);
        }}
        onPointerUp={() => {
          capturedCamera.current = null;
        }}
        onPointerCancel={() => {
          capturedCamera.current = null;
        }}
      >
        <g transform={transform(mapCamera)}>
          <SceneUses passes={passes} />
        </g>
        <path d={markers} className="other-sites" />
        {center && <path d={crosshair} className="finding-crosshair" />}
        <rect
          {...screenRect(view, mapCamera)}
          className="viewport-marker"
          data-world-min-x={view.min.x}
          data-world-min-y={view.min.y}
          data-world-max-x={view.max.x}
          data-world-max-y={view.max.y}
        />
        {roi && <rect {...roi} className="finding-marker" />}
        <Scale camera={mapCamera} size={mapSize} name="Minimap" />
      </svg>
      <div className="map-legend" aria-label="Minimap legend">
        <span title="Selected finding region; crosshair points to its center">
          <i className="finding-key" aria-hidden="true" /> Finding
        </span>
        <span title="The area currently visible in the main viewer">
          <i className="view-key" aria-hidden="true" /> Current view
        </span>
        <span title="Other sites of the same diagnostic family on matching layers">
          <i className="site-key" aria-hidden="true" /> Other sites
        </span>
      </div>
      {entry && (
        <p
          className="occurrence-label"
          title={entry.occurrences.map((index) => breadcrumb(model, index)).join(' ↔ ')}
        >
          {entry.occurrences.length
            ? entry.occurrences
                .map((index) => {
                  const instance = model.instances.get(index)!;
                  return `${instance.step} [${instance.repeat_index_x + 1},${instance.repeat_index_y + 1}] #${index}`;
                })
                .join(' ↔ ')
            : model.report.layout.selected_step || 'Checked layout'}
        </p>
      )}
    </section>
  );
}

export function Viewer({
  model,
  entry,
  entries,
  passes,
  inspector,
  navigation,
}: {
  model: Model;
  entry: Entry | null;
  entries: Entry[];
  passes: CompiledPass[];
  inspector: ReactNode;
  navigation: ReactNode;
}) {
  const ref = useRef<SVGSVGElement>(null);
  const size = useSize(ref, !entry || !!entry.site);
  const [camera, setCamera] = useState<Camera>({ x: 0, y: 0, scale: 1 });
  const cameraRef = useRef(camera);
  const previous = useRef<{ id: string | null; size: Size } | null>(null);
  const [evidence, setEvidence] = useState(true);
  const [hidden, setHidden] = useState<Set<string>>(new Set());
  const [mode, setMode] = useState<'trackpad' | 'mouse'>(() =>
    navigator.platform.includes('Mac') ? 'trackpad' : 'mouse',
  );
  const [dragging, setDragging] = useState(false);
  const pointers = useRef(new Map<number, Point>());
  const isNonspatial = !!entry && !entry.site;
  const available = useMemo(
    () =>
      passes.filter(
        (pass) => !entry || passApplies(pass, entry) || pass.feature === 'board_outlines',
      ),
    [passes, entry],
  );
  const visible = useMemo(
    () => available.filter((pass) => !hidden.has(pass.id)),
    [available, hidden],
  );
  const materials = useMemo(() => {
    const layers = new Set(
      entry && evidence
        ? evidenceOf(entry).flatMap((item) =>
            item.display?.kind === 'circle_minus_layer' ? [item.display.layer] : [],
          )
        : [],
    );
    return passes
      .filter((pass) => pass.feature === 'copper' && pass.layer && layers.has(pass.layer))
      .map((pass) => compileMaterialPass(pass, `${pass.id}-material`));
  }, [entry, evidence, passes]);
  // Hiding a context layer must not remove material used by a diagnostic mask.
  const definitions = useMemo(() => [...visible, ...materials], [visible, materials]);
  const applyCamera = (next: Camera | ((current: Camera) => Camera)) => {
    const value = typeof next === 'function' ? next(cameraRef.current) : next;
    cameraRef.current = value;
    setCamera(value);
  };
  const fitFinding = () =>
    applyCamera(fitBounds(entry?.site ? siteBounds(entry.site) : model.bounds, size, 35));
  const fitLayout = () => applyCamera(fitBounds(model.bounds, size, 35));
  const limits = { min: Math.max(1e-6, fitBounds(model.bounds, size).scale / 50), max: 1e6 };
  const zoom = (factor: number, anchor: Point = { x: size.width / 2, y: size.height / 2 }) =>
    applyCamera((current) => zoomCamera(current, factor, anchor, limits.min, limits.max));

  useLayoutEffect(() => {
    if (!size.width || !size.height) return;
    if (!previous.current || previous.current.id !== (entry?.id || null)) {
      applyCamera(fitBounds(entry?.site ? siteBounds(entry.site) : model.bounds, size, 35));
      pointers.current.clear();
      setDragging(false);
    } else {
      applyCamera((current) =>
        panCamera(current, {
          x: (size.width - previous.current!.size.width) / 2,
          y: (size.height - previous.current!.size.height) / 2,
        }),
      );
    }
    previous.current = { id: entry?.id || null, size };
  }, [entry, size.width, size.height, model.bounds]);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    const wheel = (event: WheelEvent) => {
      event.preventDefault();
      const multiplier = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? size.height : 1;
      const dx = event.deltaX * multiplier,
        dy = event.deltaY * multiplier;
      if (event.ctrlKey || event.metaKey || mode === 'mouse') {
        const bounds = element.getBoundingClientRect();
        zoom(Math.exp(-dy * (event.ctrlKey ? 0.012 : 0.003)), {
          x: event.clientX - bounds.left,
          y: event.clientY - bounds.top,
        });
      } else applyCamera((current) => panCamera(current, { x: -dx, y: -dy }));
    };
    element.addEventListener('wheel', wheel, { passive: false });
    return () => element.removeEventListener('wheel', wheel);
  }, [mode, size.width, size.height, limits.min, isNonspatial]);
  useEffect(() => {
    const key = (event: KeyboardEvent) => {
      if (
        (event.target as Element).closest('input,select,textarea,[contenteditable="true"]') ||
        event.ctrlKey ||
        event.metaKey ||
        event.altKey ||
        isNonspatial
      )
        return;
      if (event.key.toLowerCase() === 'f') {
        event.preventDefault();
        fitFinding();
      }
      if (event.key.toLowerCase() === 'a' || event.key === '0') {
        event.preventDefault();
        fitLayout();
      }
      if (event.key === '+' || event.key === '=') {
        event.preventDefault();
        zoom(1.5);
      }
      if (event.key === '-') {
        event.preventDefault();
        zoom(1 / 1.5);
      }
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  }, [entry, size.width, size.height, isNonspatial]);
  const pointFromEvent = (event: ReactPointerEvent<SVGSVGElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    return { x: event.clientX - rect.left, y: event.clientY - rect.top };
  };
  const move = (event: ReactPointerEvent<SVGSVGElement>) => {
    if (!pointers.current.has(event.pointerId)) return;
    const before = [...pointers.current.values()];
    pointers.current.set(event.pointerId, pointFromEvent(event));
    const after = [...pointers.current.values()];
    if (after.length >= 2) {
      const center = (points: Point[]) => ({
        x: (points[0].x + points[1].x) / 2,
        y: (points[0].y + points[1].y) / 2,
      });
      const distance = (points: Point[]) =>
        Math.hypot(points[1].x - points[0].x, points[1].y - points[0].y);
      const oldCenter = center(before),
        newCenter = center(after);
      applyCamera((current) =>
        panCamera(
          zoomCamera(
            current,
            distance(after) / Math.max(1, distance(before)),
            oldCenter,
            limits.min,
            limits.max,
          ),
          { x: newCenter.x - oldCenter.x, y: newCenter.y - oldCenter.y },
        ),
      );
    } else
      applyCamera((current) =>
        panCamera(current, { x: after[0].x - before[0].x, y: after[0].y - before[0].y }),
      );
  };
  const end = (event: ReactPointerEvent<SVGSVGElement>) => {
    pointers.current.delete(event.pointerId);
    setDragging(pointers.current.size > 0);
  };
  const view = visibleBounds(camera, size);
  return (
    <div className="viewer-workspace">
      <SceneDefinitions passes={definitions} />
      <section className="detail-pane" aria-label="Selected diagnostic">
        <div className="selection-header">
          <h2>
            {entry?.rule.view.title ||
              (model.report.findings.length ? 'No matching findings' : 'No reported violations')}
          </h2>
          {entry && (
            <span className={`badge ${entry.status}`}>
              {entry.status}
              {entry.rule.tier === 'preferred' ? ' · preferred' : ''}
            </span>
          )}
        </div>
        <div className="navigation-toolbar">{navigation}</div>
        {!isNonspatial && (
          <>
            <div className="view-toolbar">
              <div className="button-group">
                <button onClick={fitFinding} disabled={!entry?.site} title="Fit finding (F)">
                  Fit finding
                </button>
                <button onClick={fitLayout} title="Fit full layout (A)">
                  Fit layout
                </button>
              </div>
              <div className="button-group">
                <button onClick={() => zoom(1 / 1.5)} aria-label="Zoom out">
                  −
                </button>
                <button onClick={() => zoom(1.5)} aria-label="Zoom in">
                  +
                </button>
              </div>
              <label className="input-mode">
                Input
                <select
                  aria-label="Camera input mode"
                  value={mode}
                  onChange={(event) => setMode(event.target.value as typeof mode)}
                >
                  <option value="trackpad">Trackpad</option>
                  <option value="mouse">Mouse</option>
                </select>
              </label>
            </div>
            <div className="layer-controls" aria-label="Visible layers and features">
              {available.map((pass) => (
                <label key={pass.id}>
                  <input
                    type="checkbox"
                    checked={!hidden.has(pass.id)}
                    onChange={(event) =>
                      setHidden((current) => {
                        const next = new Set(current);
                        if (event.target.checked) next.delete(pass.id);
                        else next.add(pass.id);
                        return next;
                      })
                    }
                  />
                  <i style={{ background: pass.color }} />
                  {pass.label}
                </label>
              ))}
              {entry?.site && (
                <label className="evidence-toggle">
                  <input
                    type="checkbox"
                    checked={evidence}
                    onChange={(event) => setEvidence(event.target.checked)}
                  />
                  Evidence
                </label>
              )}
            </div>
          </>
        )}
        {!model.report.scene && !isNonspatial && (
          <p className="notice compact">
            Board geometry is absent. Showing check evidence only. Regenerate with{' '}
            <code>--include-geometry</code> for full context.
          </p>
        )}
        {isNonspatial ? (
          <div className="nonspatial">
            <p>This check applies to the shared physical stackup.</p>
            <div className="stack">
              {entry.finding.layers.map((layer, index) => (
                <div className="stack-layer" key={layer.name}>
                  <strong>
                    {index + 1}. {layer.name}
                  </strong>
                  <span>{pretty(layer.side || layer.function)}</span>
                </div>
              ))}
            </div>
          </div>
        ) : (
          <>
            <svg
              ref={ref}
              className={`main-view ${dragging ? 'dragging' : ''}`}
              aria-label="Main board viewer"
              tabIndex={0}
              viewBox={`0 0 ${size.width || 1} ${size.height || 1}`}
              data-camera-x={camera.x}
              data-camera-y={camera.y}
              data-camera-scale={camera.scale}
              data-view-width-mm={size.width / camera.scale}
              data-view-height-mm={size.height / camera.scale}
              onContextMenu={(event) => event.preventDefault()}
              onPointerDown={(event) => {
                if (event.button !== 0 && event.button !== 1) return;
                event.preventDefault();
                event.currentTarget.focus();
                event.currentTarget.setPointerCapture(event.pointerId);
                pointers.current.set(event.pointerId, pointFromEvent(event));
                setDragging(true);
              }}
              onPointerMove={move}
              onPointerUp={end}
              onPointerCancel={end}
              onLostPointerCapture={end}
              onDoubleClick={(event) => {
                const rect = event.currentTarget.getBoundingClientRect();
                zoom(event.shiftKey ? 0.5 : 2, {
                  x: event.clientX - rect.left,
                  y: event.clientY - rect.top,
                });
              }}
            >
              <g transform={transform(camera)}>
                <SceneUses passes={visible} />
                {entry?.site && evidence && (
                  <EvidenceGeometry
                    entry={entry}
                    materials={materials}
                    view={view}
                    scale={camera.scale}
                  />
                )}
              </g>
              {entry?.site && evidence && (
                <MeasurementOverlay entry={entry} camera={camera} size={size} />
              )}
              <Scale camera={camera} size={size} name="Main view" />
            </svg>
            <div className="view-footer">
              <span>
                {mode === 'trackpad'
                  ? 'Scroll to pan · pinch / Ctrl+scroll to zoom'
                  : 'Drag to pan · wheel to zoom'}{' '}
                · F finding · A layout
              </span>
              <span
                title={`X ${number(view.min.x)}…${number(view.max.x)}, Y ${number(view.min.y)}…${number(view.max.y)} mm`}
              >
                {number(size.width / camera.scale)} × {number(size.height / camera.scale)} mm
              </span>
            </div>
          </>
        )}
      </section>
      <aside className="inspector" aria-label="Diagnostic details">
        {!isNonspatial && (
          <MiniMap
            model={model}
            entry={entry}
            entries={entries}
            passes={visible}
            camera={camera}
            size={size}
            onCenter={(world) =>
              applyCamera((current) => ({
                ...current,
                x: size.width / 2 - world.x * current.scale,
                y: size.height / 2 + world.y * current.scale,
              }))
            }
          />
        )}
        {inspector}
      </aside>
    </div>
  );
}
