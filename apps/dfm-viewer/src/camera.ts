export interface Point {
  x: number;
  y: number;
}

export interface Bounds {
  min: Point;
  max: Point;
}

export interface Size {
  width: number;
  height: number;
}

/** World coordinates are millimeters, X right / Y up; scale is CSS px/mm. */
export interface Camera {
  x: number;
  y: number;
  scale: number;
}

const finite = (value: number, fallback = 0): number => (Number.isFinite(value) ? value : fallback);

const positive = (value: number, fallback: number): number =>
  Number.isFinite(value) && value > 0 ? value : fallback;

const dimension = (value: number): number => Math.max(0, finite(value));

function normalizedBounds(bounds: Bounds): Bounds {
  const first = { x: finite(bounds.min.x), y: finite(bounds.min.y) };
  const second = { x: finite(bounds.max.x), y: finite(bounds.max.y) };
  return {
    min: { x: Math.min(first.x, second.x), y: Math.min(first.y, second.y) },
    max: { x: Math.max(first.x, second.x), y: Math.max(first.y, second.y) },
  };
}

function normalizedCamera(camera: Camera): Camera {
  return {
    x: finite(camera.x),
    y: finite(camera.y),
    scale: positive(camera.scale, 1),
  };
}

export function fitBounds(bounds: Bounds, size: Size, paddingPx = 24): Camera {
  const box = normalizedBounds(bounds);
  const width = dimension(size.width);
  const height = dimension(size.height);
  const padding = dimension(paddingPx);
  const spanX = box.max.x - box.min.x;
  const spanY = box.max.y - box.min.y;
  // A point has no intrinsic extent. Give it a one-millimeter initial view;
  // a horizontal/vertical line still fits according to its nonzero extent.
  const point = spanX === 0 && spanY === 0;
  const scaleX =
    spanX > 0 || point ? Math.max(1, width - padding * 2) / (point ? 1 : spanX) : Infinity;
  const scaleY =
    spanY > 0 || point ? Math.max(1, height - padding * 2) / (point ? 1 : spanY) : Infinity;
  const scale = positive(Math.min(scaleX, scaleY), 1);
  const centerX = box.min.x / 2 + box.max.x / 2;
  const centerY = box.min.y / 2 + box.max.y / 2;
  return {
    x: finite(width / 2 - centerX * scale),
    y: finite(height / 2 + centerY * scale),
    scale,
  };
}

export function pointToScreen(point: Point, camera: Camera): Point {
  const view = normalizedCamera(camera);
  return {
    x: view.x + finite(point.x) * view.scale,
    y: view.y - finite(point.y) * view.scale,
  };
}

export function screenToPoint(point: Point, camera: Camera): Point {
  const view = normalizedCamera(camera);
  return {
    x: (finite(point.x) - view.x) / view.scale,
    y: (view.y - finite(point.y)) / view.scale,
  };
}

/** Actual visible world extent, including space around an aspect-fitted ROI. */
export function visibleBounds(camera: Camera, size: Size): Bounds {
  const topLeft = screenToPoint({ x: 0, y: 0 }, camera);
  const bottomRight = screenToPoint(
    {
      x: dimension(size.width),
      y: dimension(size.height),
    },
    camera,
  );
  return {
    min: { x: topLeft.x, y: bottomRight.y },
    max: { x: bottomRight.x, y: topLeft.y },
  };
}

/** Panning has no bounds clamp: the full layout remains freely navigable. */
export function panCamera(camera: Camera, deltaScreen: Point): Camera {
  const view = normalizedCamera(camera);
  return {
    ...view,
    x: finite(view.x + finite(deltaScreen.x), view.x),
    y: finite(view.y + finite(deltaScreen.y), view.y),
  };
}

export function zoomCamera(
  camera: Camera,
  factor: number,
  anchorScreen: Point,
  minScale: number,
  maxScale: number,
): Camera {
  const view = normalizedCamera(camera);
  if (!Number.isFinite(factor) || factor <= 0) return view;
  const minimum = positive(minScale, 1e-9);
  const maximum = Math.max(minimum, positive(maxScale, 1e9));
  const scale = Math.min(maximum, Math.max(minimum, view.scale * factor));
  const anchor = screenToPoint(anchorScreen, view);
  return {
    x: finite(anchorScreen.x) - anchor.x * scale,
    y: finite(anchorScreen.y) + anchor.y * scale,
    scale,
  };
}

export function scaleBar(
  scale: number,
  targetPx = 100,
): { lengthMm: number; pixels: number; label: string } {
  const pixelsPerMm = positive(scale, 1);
  const target = positive(targetPx, 100);
  const ideal = Math.max(Number.MIN_VALUE, Math.min(Number.MAX_VALUE, target / pixelsPerMm));
  const magnitude = Math.max(Number.MIN_VALUE, 10 ** Math.floor(Math.log10(ideal)));
  const fraction = ideal / magnitude;
  const step = fraction >= 5 - 1e-12 ? 5 : fraction >= 2 - 1e-12 ? 2 : 1;
  const lengthMm = magnitude * step;
  const unit = lengthMm < 1 ? 'µm' : lengthMm >= 1000 ? 'm' : 'mm';
  const value = unit === 'µm' ? lengthMm * 1000 : unit === 'm' ? lengthMm / 1000 : lengthMm;
  return {
    lengthMm,
    pixels: lengthMm * pixelsPerMm,
    label: `${Number(value.toPrecision(12))} ${unit}`,
  };
}

/** Place a fixed-size screen label near its anchor, outside evidence if possible. */
export function placeLabel(
  anchorScreen: Point,
  labelSize: Size,
  viewportSize: Size,
  avoidBoundsScreen?: Bounds,
): Point {
  const anchor = { x: finite(anchorScreen.x), y: finite(anchorScreen.y) };
  const width = dimension(labelSize.width);
  const height = dimension(labelSize.height);
  const gap = 14;
  const availableX = Math.max(0, dimension(viewportSize.width) - width);
  const availableY = Math.max(0, dimension(viewportSize.height) - height);
  const insetX = Math.min(8, availableX);
  const insetY = Math.min(8, availableY);
  const clamp = (point: Point): Point => ({
    x: Math.max(insetX, Math.min(Math.max(insetX, availableX - 8), point.x)),
    y: Math.max(insetY, Math.min(Math.max(insetY, availableY - 8), point.y)),
  });
  const avoid = normalizedBounds(avoidBoundsScreen ?? { min: anchor, max: anchor });
  const candidates: Point[] = [
    { x: anchor.x - width / 2, y: anchor.y - height - gap },
    { x: anchor.x + gap, y: anchor.y - height / 2 },
    { x: anchor.x - width / 2, y: anchor.y + gap },
    { x: anchor.x - width - gap, y: anchor.y - height / 2 },
    { x: anchor.x + gap, y: anchor.y - height - gap },
    { x: anchor.x - width - gap, y: anchor.y - height - gap },
    { x: anchor.x + gap, y: anchor.y + gap },
    { x: anchor.x - width - gap, y: anchor.y + gap },
    { x: anchor.x - width / 2, y: avoid.min.y - height - gap },
    { x: avoid.max.x + gap, y: anchor.y - height / 2 },
    { x: anchor.x - width / 2, y: avoid.max.y + gap },
    { x: avoid.min.x - width - gap, y: anchor.y - height / 2 },
  ];
  let best: Point = { x: 0, y: 0 };
  let bestOverlap = Infinity;
  let bestDistance = Infinity;
  for (const candidate of candidates) {
    const point = clamp(candidate);
    // Four pixels around a segment/point also count as occupied space.
    const overlapX = Math.max(
      0,
      Math.min(point.x + width, avoid.max.x + 4) - Math.max(point.x, avoid.min.x - 4),
    );
    const overlapY = Math.max(
      0,
      Math.min(point.y + height, avoid.max.y + 4) - Math.max(point.y, avoid.min.y - 4),
    );
    const overlap = overlapX * overlapY;
    const distance = (point.x + width / 2 - anchor.x) ** 2 + (point.y + height / 2 - anchor.y) ** 2;
    if (overlap < bestOverlap || (overlap === bestOverlap && distance < bestDistance)) {
      best = point;
      bestOverlap = overlap;
      bestDistance = distance;
    }
  }
  return best;
}
