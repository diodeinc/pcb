import { memo, useId, useMemo } from 'react';
import type { ReactNode } from 'react';
import type { Bounds, Point } from './camera';
import { evidenceOf, pretty } from './model';
import type { Entry } from './model';
import type { CompiledPass } from './scene';
import type { Evidence } from './types';

function pathData(paths: Point[][], closed = false) {
  return paths
    .filter((path) => path.length)
    .map(
      (path) => `M${path.map((point) => `${point.x} ${point.y}`).join(' L')}${closed ? ' Z' : ''}`,
    )
    .join(' ');
}

function rect(bounds: Bounds) {
  return {
    x: bounds.min.x,
    y: bounds.min.y,
    width: bounds.max.x - bounds.min.x,
    height: bounds.max.y - bounds.min.y,
  };
}

function circleBounds(center: Point, diameter: number): Bounds {
  const r = diameter / 2;
  return { min: { x: center.x - r, y: center.y - r }, max: { x: center.x + r, y: center.y + r } };
}

function intersection(a: Bounds, b: Bounds): Bounds {
  return {
    min: { x: Math.max(a.min.x, b.min.x), y: Math.max(a.min.y, b.min.y) },
    max: { x: Math.min(a.max.x, b.max.x), y: Math.min(a.max.y, b.max.y) },
  };
}

function expand(bounds: Bounds, amount: number): Bounds {
  return {
    min: { x: bounds.min.x - amount, y: bounds.min.y - amount },
    max: { x: bounds.max.x + amount, y: bounds.max.y + amount },
  };
}

/** Native geometry is opaque here; the common region painter supplies its color. */
const NativeShape = memo(function NativeShape({
  evidence,
  material,
  id,
  clip,
}: {
  evidence: Evidence;
  material?: CompiledPass;
  id: string;
  clip: Bounds;
}) {
  const display = evidence.display!;
  const strokePath = useMemo(
    () => (display.kind === 'round_stroke' ? pathData(display.paths) : ''),
    [display],
  );
  switch (display.kind) {
    case 'path':
      return (
        <g fill="white" stroke="none" fillRule={display.fill_rule}>
          {display.paths.map((d, index) => (
            <path key={index} d={d} />
          ))}
        </g>
      );
    case 'round_stroke':
      return (
        <path
          d={strokePath}
          fill="none"
          stroke="white"
          strokeWidth={display.width_mm}
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      );
    case 'circle_intersection': {
      const { first, second } = display;
      return (
        <>
          <defs>
            <clipPath id={`${id}-intersection`} clipPathUnits="userSpaceOnUse">
              <circle cx={second.center.x} cy={second.center.y} r={second.diameter / 2} />
            </clipPath>
          </defs>
          <circle
            cx={first.center.x}
            cy={first.center.y}
            r={first.diameter / 2}
            fill="white"
            clipPath={`url(#${id}-intersection)`}
          />
        </>
      );
    }
    case 'circle_minus_layer':
      return (
        <>
          <defs>
            <mask
              id={`${id}-difference`}
              maskUnits="userSpaceOnUse"
              maskContentUnits="userSpaceOnUse"
              {...rect(clip)}
              style={{ maskType: 'luminance' }}
            >
              <rect {...rect(clip)} fill="white" />
              <use href={`#${material!.id}`} />
            </mask>
          </defs>
          <circle
            cx={display.center.x}
            cy={display.center.y}
            r={display.diameter / 2}
            fill="white"
            mask={`url(#${id}-difference)`}
          />
        </>
      );
  }
});

/** Outline the composed region, not each operand (which would introduce seams).
 * Filter/mask surfaces are clipped to the visible area plus a few screen pixels;
 * zooming into a large band never allocates a board-sized offscreen image.
 */
function RegionPaint({
  id,
  clip,
  scale,
  color,
  opacity,
  children,
}: {
  id: string;
  clip: Bounds;
  scale: number;
  color: string;
  opacity: number;
  children: ReactNode;
}) {
  return (
    <>
      <defs>
        <filter
          id={`${id}-paint`}
          filterUnits="userSpaceOnUse"
          primitiveUnits="userSpaceOnUse"
          {...rect(clip)}
          colorInterpolationFilters="sRGB"
        >
          <feMorphology in="SourceAlpha" operator="dilate" radius={0.85 / scale} result="outer" />
          <feMorphology in="SourceAlpha" operator="erode" radius={0.85 / scale} result="inner" />
          <feComposite in="outer" in2="inner" operator="out" result="edge" />
          <feFlood floodColor={color} result="ink" />
          <feComposite in="ink" in2="edge" operator="in" result="border" />
          <feFlood floodColor={color} floodOpacity={opacity} result="wash" />
          <feComposite in="wash" in2="SourceAlpha" operator="in" result="fill" />
          <feMerge>
            <feMergeNode in="fill" />
            <feMergeNode in="border" />
          </feMerge>
        </filter>
      </defs>
      <g filter={`url(#${id}-paint)`}>{children}</g>
    </>
  );
}

const NativeRegion = memo(function NativeRegion({
  evidence,
  material,
  bounds,
  view,
  scale,
  color,
  opacity,
}: {
  evidence: Evidence;
  material?: CompiledPass;
  bounds: Bounds;
  view: Bounds;
  scale: number;
  color: string;
  opacity: number;
}) {
  const id = `evidence-${useId().replaceAll(':', '')}`;
  const regionBounds = useMemo(() => {
    const display = evidence.display!;
    switch (display.kind) {
      case 'path':
        return bounds;
      case 'circle_minus_layer':
        return circleBounds(display.center, display.diameter);
      case 'circle_intersection':
        return intersection(
          circleBounds(display.first.center, display.first.diameter),
          circleBounds(display.second.center, display.second.diameter),
        );
      case 'round_stroke': {
        const result = { min: { x: Infinity, y: Infinity }, max: { x: -Infinity, y: -Infinity } };
        for (const path of display.paths)
          for (const point of path) {
            result.min.x = Math.min(result.min.x, point.x);
            result.min.y = Math.min(result.min.y, point.y);
            result.max.x = Math.max(result.max.x, point.x);
            result.max.y = Math.max(result.max.y, point.y);
          }
        return expand(result, display.width_mm / 2);
      }
    }
  }, [evidence, bounds]);
  const padding = 3 / scale;
  const clip = intersection(expand(regionBounds, padding), expand(view, padding));
  if (clip.max.x <= clip.min.x || clip.max.y <= clip.min.y) return null;
  return (
    <g data-display-kind={evidence.display!.kind}>
      <title>{pretty(evidence.role)}</title>
      <RegionPaint id={id} clip={clip} scale={scale} color={color} opacity={opacity}>
        <NativeShape evidence={evidence} material={material} id={id} clip={clip} />
      </RegionPaint>
    </g>
  );
});

const EvidenceShape = memo(function EvidenceShape({
  evidence,
  material,
  bounds,
  view,
  scale,
}: {
  evidence: Evidence;
  material?: CompiledPass;
  bounds: Bounds;
  view: Bounds;
  scale: number;
}) {
  const required = /required|envelope/.test(evidence.role);
  const candidate = evidence.role === 'candidate_region';
  const context = evidence.kind === 'bounds' || /subject|outline/.test(evidence.role);
  const color = candidate
    ? '#a87318'
    : required
      ? '#186eae'
      : /second_boundary|second_hole/.test(evidence.role)
        ? '#783b96'
        : context
          ? '#48515a'
          : '#bd252a';
  const d = useMemo(() => pathData(evidence.paths, evidence.kind === 'region'), [evidence]);
  // Old JSON and geometry-free reports retain their measured polygon evidence.
  if (evidence.display && (evidence.display.kind !== 'circle_minus_layer' || material))
    return (
      <NativeRegion
        evidence={evidence}
        material={material}
        bounds={bounds}
        view={view}
        scale={scale}
        color={color}
        opacity={required || candidate ? 0.07 : 0.17}
      />
    );
  const attrs = {
    fill: 'none',
    stroke: color,
    // The camera has uniform scale. Inverse widths keep annotations fixed in
    // pixels while retaining the browser's native circle/curve paint path.
    strokeWidth: (context ? 1.2 : 1.7) / scale,
    strokeLinejoin: 'round' as const,
    strokeLinecap: 'round' as const,
    strokeDasharray:
      required || candidate || evidence.kind === 'bounds' ? `${5 / scale} ${3 / scale}` : undefined,
  };
  const title = <title>{pretty(evidence.role)}</title>;
  if (evidence.kind === 'circle' && evidence.center && evidence.diameter && evidence.diameter > 0)
    return (
      <circle {...attrs} cx={evidence.center.x} cy={evidence.center.y} r={evidence.diameter / 2}>
        {title}
      </circle>
    );
  if (evidence.kind === 'segment' && evidence.start && evidence.end)
    return (
      <line
        {...attrs}
        x1={evidence.start.x}
        y1={evidence.start.y}
        x2={evidence.end.x}
        y2={evidence.end.y}
      >
        {title}
      </line>
    );
  if ((evidence.kind === 'path' || evidence.kind === 'region') && d)
    return (
      <path
        {...attrs}
        d={d}
        fill={evidence.kind === 'region' ? color : 'none'}
        fillOpacity={required || candidate ? 0.07 : 0.17}
        fillRule="nonzero"
      >
        {title}
      </path>
    );
  if (evidence.kind === 'bounds' && evidence.bounding_box)
    return (
      <rect {...attrs} {...rect(evidence.bounding_box)}>
        {title}
      </rect>
    );
  return null;
});

export const EvidenceGeometry = memo(function EvidenceGeometry({
  entry,
  materials,
  view,
  scale,
}: {
  entry: Entry;
  materials: CompiledPass[];
  view: Bounds;
  scale: number;
}) {
  return (
    <g className="evidence-geometry">
      {evidenceOf(entry).map((evidence, index) => {
        const display = evidence.display;
        const material =
          display?.kind === 'circle_minus_layer'
            ? materials.find((pass) => pass.layer === display.layer)
            : undefined;
        return (
          <EvidenceShape
            key={index}
            evidence={evidence}
            material={material}
            bounds={entry.site!.bounding_box}
            view={view}
            scale={scale}
          />
        );
      })}
    </g>
  );
});
