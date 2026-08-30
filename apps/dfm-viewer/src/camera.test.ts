import { describe, expect, it } from 'vitest';
import {
  fitBounds,
  panCamera,
  placeLabel,
  pointToScreen,
  scaleBar,
  screenToPoint,
  visibleBounds,
  zoomCamera,
  type Bounds,
  type Camera,
  type Point,
  type Size,
} from './camera';

function expectPoint(actual: Point, expected: Point): void {
  expect(actual.x).toBeCloseTo(expected.x, 9);
  expect(actual.y).toBeCloseTo(expected.y, 9);
}

describe('world / screen camera', () => {
  it('keeps Y up in millimeters and round-trips coordinates at different scales', () => {
    for (const scale of [0.02, 1, 17, 1e5]) {
      const camera = { x: 123, y: 234, scale };
      const point = { x: -2.5, y: 8.25 };
      const screen = pointToScreen(point, camera);
      expect(screen.y).toBeLessThan(camera.y);
      expectPoint(screenToPoint(screen, camera), point);
    }
  });

  it('fits the board with uniform scale and accounts for unused viewport space', () => {
    const bounds = { min: { x: 0, y: 0 }, max: { x: 100, y: 50 } };
    const size = { width: 1000, height: 600 };
    const camera = fitBounds(bounds, size, 20);
    expect(camera.scale).toBeCloseTo(9.6);
    expectPoint(pointToScreen(bounds.min, camera), { x: 20, y: 540 });
    expectPoint(pointToScreen(bounds.max, camera), { x: 980, y: 60 });
    const visible = visibleBounds(camera, size);
    expectPoint(visible.min, { x: -20 / 9.6, y: -60 / 9.6 });
    expectPoint(visible.max, { x: 100 + 20 / 9.6, y: 50 + 60 / 9.6 });
  });

  it.each([
    { width: 1000, height: 300 },
    { width: 300, height: 1000 },
  ])('preserves physical aspect ratio in viewport %j', (size) => {
    const bounds = { min: { x: -20, y: -40 }, max: { x: 120, y: 10 } };
    const camera = fitBounds(bounds, size, 24);
    const topLeft = pointToScreen({ x: bounds.min.x, y: bounds.max.y }, camera);
    const bottomRight = pointToScreen({ x: bounds.max.x, y: bounds.min.y }, camera);
    expect(topLeft.x).toBeGreaterThanOrEqual(24 - 1e-9);
    expect(topLeft.y).toBeGreaterThanOrEqual(24 - 1e-9);
    expect(bottomRight.x).toBeLessThanOrEqual(size.width - 24 + 1e-9);
    expect(bottomRight.y).toBeLessThanOrEqual(size.height - 24 + 1e-9);
    const view = visibleBounds(camera, size);
    expect((view.max.x - view.min.x) / (view.max.y - view.min.y)).toBeCloseTo(
      size.width / size.height,
    );
  });

  it('updates visible bounds on resize without changing the camera', () => {
    const camera = { x: 20, y: 40, scale: 2 };
    expect(visibleBounds(camera, { width: 200, height: 100 })).toEqual({
      min: { x: -10, y: -30 },
      max: { x: 90, y: 20 },
    });
    expect(visibleBounds(camera, { width: 800, height: 600 })).toEqual({
      min: { x: -10, y: -280 },
      max: { x: 390, y: 20 },
    });
  });

  it('permits panning far outside the original region', () => {
    expect(panCamera({ x: 100, y: 200, scale: 12 }, { x: -5000, y: 8000 })).toEqual({
      x: -4900,
      y: 8200,
      scale: 12,
    });
  });

  it.each([0.001, 0.5, 2, 1e10])(
    'holds the cursor anchor while zooming by %s, including at limits',
    (factor) => {
      const camera = { x: 123, y: 456, scale: 3 };
      const anchor = { x: 322, y: 166 };
      const worldPoint = screenToPoint(anchor, camera);
      const zoomed = zoomCamera(camera, factor, anchor, 1, 20);
      expectPoint(pointToScreen(worldPoint, zoomed), anchor);
      expect(zoomed.scale).toBeGreaterThanOrEqual(1);
      expect(zoomed.scale).toBeLessThanOrEqual(20);
      expect(zoomed.scale).toBe(Math.min(20, Math.max(1, 3 * factor)));
    },
  );

  it('fits point, line, reversed, and not-yet-mounted bounds without invalid numbers', () => {
    const cases: Array<[Bounds, Size]> = [
      [
        { min: { x: 12, y: 34 }, max: { x: 12, y: 34 } },
        { width: 800, height: 600 },
      ],
      [
        { min: { x: 12, y: 0 }, max: { x: 12, y: 100 } },
        { width: 800, height: 600 },
      ],
      [
        { min: { x: 0, y: 12 }, max: { x: 100, y: 12 } },
        { width: 800, height: 600 },
      ],
      [
        { min: { x: 100, y: 50 }, max: { x: 0, y: 0 } },
        { width: 800, height: 600 },
      ],
      [
        { min: { x: 0, y: 0 }, max: { x: 1e-8, y: 1e-8 } },
        { width: 1, height: 1 },
      ],
      [
        { min: { x: -1e8, y: -1e8 }, max: { x: 1e8, y: 1e8 } },
        { width: 800, height: 600 },
      ],
      [
        { min: { x: 0, y: 0 }, max: { x: 100, y: 50 } },
        { width: 0, height: 0 },
      ],
      [
        { min: { x: NaN, y: 0 }, max: { x: 0, y: Infinity } },
        { width: NaN, height: -1 },
      ],
    ];
    for (const [bounds, size] of cases) {
      const camera = fitBounds(bounds, size);
      expect(Object.values(camera).every(Number.isFinite)).toBe(true);
      expect(camera.scale).toBeGreaterThan(0);
    }
    const point = { x: 12, y: 34 };
    expectPoint(
      pointToScreen(point, fitBounds({ min: point, max: point }, { width: 800, height: 600 })),
      { x: 400, y: 300 },
    );
  });

  it('ignores invalid zoom factors and recovers invalid camera inputs', () => {
    const camera: Camera = { x: 10, y: 20, scale: 2 };
    for (const factor of [0, -1, NaN, Infinity]) {
      expect(zoomCamera(camera, factor, { x: 50, y: 50 }, 0.1, 100)).toEqual(camera);
    }
    expect(panCamera({ x: NaN, y: Infinity, scale: 0 }, { x: NaN, y: Infinity })).toEqual({
      x: 0,
      y: 0,
      scale: 1,
    });
  });
});

describe('physical scale bars', () => {
  it.each([1e-12, 0.0001, 0.1, 0.4, 1, 2, 3, 7.5, 250, 5000, 2e6, 1e12])(
    'matches the camera at %s CSS pixels per millimeter',
    (scale) => {
      const bar = scaleBar(scale);
      expect(bar.pixels).toBe(bar.lengthMm * scale);
      expect(bar.pixels).toBeGreaterThanOrEqual(40 - 1e-9);
      expect(bar.pixels).toBeLessThanOrEqual(100 + 1e-9);
      const magnitude = 10 ** Math.floor(Math.log10(bar.lengthMm));
      expect([1, 2, 5]).toContain(Math.round(bar.lengthMm / magnitude));
      expect(bar.label).not.toMatch(/^0 /);
    },
  );

  it('uses readable units without losing small dimensions', () => {
    expect(scaleBar(1).label).toBe('100 mm');
    expect(scaleBar(250).label).toBe('200 µm');
    expect(scaleBar(0.02).label).toBe('5 m');
    expect(scaleBar(2e6).label).toBe('0.05 µm');
    expect(scaleBar(1e12).label).toBe('1e-7 µm');
    const compact = scaleBar(3, 60);
    expect(compact).toEqual({ lengthMm: 20, pixels: 60, label: '20 mm' });
  });

  it('keeps scale bar results finite for transient invalid and extreme scales', () => {
    for (const scale of [0, -1, NaN, Infinity, 1e-310, 1e308]) {
      const bar = scaleBar(scale, NaN);
      expect(Number.isFinite(bar.lengthMm)).toBe(true);
      expect(Number.isFinite(bar.pixels)).toBe(true);
      expect(bar.lengthMm).toBeGreaterThan(0);
      expect(bar.pixels).toBeGreaterThan(0);
    }
  });
});

describe('screen-space evidence labels', () => {
  it.each([
    { x: 0, y: 0 },
    { x: 800, y: 600 },
    { x: 400, y: 0 },
    { x: 400, y: 600 },
    { x: -100, y: 900 },
  ])('keeps a label in the viewport near anchor %j', (anchor) => {
    const placed = placeLabel(anchor, { width: 140, height: 28 }, { width: 800, height: 600 });
    expect(placed.x).toBeGreaterThanOrEqual(0);
    expect(placed.y).toBeGreaterThanOrEqual(0);
    expect(placed.x + 140).toBeLessThanOrEqual(800);
    expect(placed.y + 28).toBeLessThanOrEqual(600);
  });

  it('moves beyond a measured region instead of covering it', () => {
    const region = { min: { x: 350, y: 250 }, max: { x: 450, y: 350 } };
    const placed = placeLabel(
      { x: 400, y: 300 },
      { width: 140, height: 28 },
      { width: 800, height: 600 },
      region,
    );
    const separated =
      placed.x + 140 < region.min.x ||
      placed.x > region.max.x ||
      placed.y + 28 < region.min.y ||
      placed.y > region.max.y;
    expect(separated).toBe(true);
  });

  it('keeps labels outside zero-height dimension lines too', () => {
    const region = { min: { x: 300, y: 300 }, max: { x: 500, y: 300 } };
    const placed = placeLabel(
      { x: 400, y: 300 },
      { width: 140, height: 28 },
      { width: 800, height: 600 },
      region,
    );
    expect(placed.y + 28 < 300 || placed.y > 300).toBe(true);
  });

  it('has a finite origin when the viewport is smaller than the label', () => {
    expect(
      placeLabel({ x: 10, y: 10 }, { width: 140, height: 28 }, { width: 20, height: 10 }),
    ).toEqual({ x: 0, y: 0 });
    expect(
      placeLabel({ x: NaN, y: Infinity }, { width: 140, height: 28 }, { width: 0, height: 0 }),
    ).toEqual({ x: 0, y: 0 });
  });
});
