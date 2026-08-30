import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Camera, Point, Size } from './camera';
import { createModel, type Entry, type Model } from './model';
import { compilePass, type CompiledPass } from './scene';
import { reportFixture } from './test-fixtures';
import type { Evidence } from './types';
import { Viewer } from './Viewer';

let mainSize: Size;
let minimapSize: Size;
const observers = new Set<TestResizeObserver>();

function boundsFor(element: Element): DOMRect {
  const size = element.classList.contains('main-view') ? mainSize : minimapSize;
  const offset = element.classList.contains('main-view') ? { x: 120, y: 80 } : { x: 1100, y: 150 };
  return new DOMRect(offset.x, offset.y, size.width, size.height);
}

class TestResizeObserver {
  readonly targets = new Set<Element>();
  constructor(private callback: ResizeObserverCallback) {
    observers.add(this);
  }
  observe(element: Element) {
    this.targets.add(element);
    this.emit(element);
  }
  unobserve(element: Element) {
    this.targets.delete(element);
  }
  disconnect() {
    this.targets.clear();
    observers.delete(this);
  }
  emit(element: Element) {
    this.callback(
      [{ target: element, contentRect: boundsFor(element) } as ResizeObserverEntry],
      this as unknown as ResizeObserver,
    );
  }
}

let root: Root;
let container: HTMLDivElement;
let model: Model;
let passes: CompiledPass[];
let spatial: Entry;
let nonspatial: Entry;
let originalTextLength: PropertyDescriptor | undefined;
let originalPointerCapture: PropertyDescriptor | undefined;

beforeEach(() => {
  mainSize = { width: 900, height: 600 };
  minimapSize = { width: 280, height: 180 };
  observers.clear();
  vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
  vi.stubGlobal('ResizeObserver', TestResizeObserver);
  vi.spyOn(Element.prototype, 'getBoundingClientRect').mockImplementation(function (this: Element) {
    return boundsFor(this);
  });
  originalTextLength = Object.getOwnPropertyDescriptor(
    SVGElement.prototype,
    'getComputedTextLength',
  );
  originalPointerCapture = Object.getOwnPropertyDescriptor(Element.prototype, 'setPointerCapture');
  Object.defineProperty(SVGElement.prototype, 'getComputedTextLength', {
    configurable: true,
    value(this: SVGElement) {
      return (this.textContent?.length || 0) * 7;
    },
  });
  Object.defineProperty(Element.prototype, 'setPointerCapture', { configurable: true, value() {} });
  container = document.createElement('div');
  document.body.append(container);
  root = createRoot(container);
  model = createModel(reportFixture());
  passes = model.report.scene!.passes.map((pass, index) =>
    compilePass(pass, `test-scene-${index}`),
  );
  spatial = model.entries.find((entry) => entry.site)!;
  nonspatial = model.entries.find((entry) => !entry.site)!;
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  observers.clear();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  if (originalTextLength)
    Object.defineProperty(SVGElement.prototype, 'getComputedTextLength', originalTextLength);
  else delete (SVGElement.prototype as unknown as Record<string, unknown>).getComputedTextLength;
  if (originalPointerCapture)
    Object.defineProperty(Element.prototype, 'setPointerCapture', originalPointerCapture);
  else delete (Element.prototype as unknown as Record<string, unknown>).setPointerCapture;
});

async function render(entry: Entry = spatial) {
  await act(async () =>
    root.render(
      <Viewer
        model={model}
        entry={entry}
        entries={model.entries}
        passes={passes}
        inspector={null}
        navigation={null}
      />,
    ),
  );
}

function get<T extends Element = Element>(selector: string): T {
  const element = container.querySelector<T>(selector);
  expect(element, `Missing ${selector}`).not.toBeNull();
  return element!;
}

function numberAttribute(element: Element, name: string): number {
  const value = element.getAttribute(name);
  expect(value, `Missing ${name}`).not.toBeNull();
  const number = Number(value);
  expect(Number.isFinite(number)).toBe(true);
  return number;
}

function camera(): Camera {
  const svg = get('.main-view');
  return {
    x: numberAttribute(svg, 'data-camera-x'),
    y: numberAttribute(svg, 'data-camera-y'),
    scale: numberAttribute(svg, 'data-camera-scale'),
  };
}

function worldAt(point: Point, view = camera()): Point {
  return { x: (point.x - view.x) / view.scale, y: (view.y - point.y) / view.scale };
}

function expectPoint(actual: Point, expected: Point) {
  expect(actual.x).toBeCloseTo(expected.x, 8);
  expect(actual.y).toBeCloseTo(expected.y, 8);
}

async function click(label: string) {
  const button = [...container.querySelectorAll('button')].find(
    (button) => button.getAttribute('aria-label') === label || button.textContent === label,
  );
  expect(button, `Missing button ${label}`).toBeDefined();
  await act(async () => button!.click());
}

async function toggle(label: string) {
  const input = [...container.querySelectorAll<HTMLInputElement>('.layer-controls input')].find(
    (input) => input.closest('label')!.textContent!.trim() === label,
  );
  expect(input, `Missing checkbox ${label}`).toBeDefined();
  await act(async () => input!.click());
}

function region(display?: Evidence['display']): Evidence {
  return {
    role: 'missing_copper',
    kind: 'region',
    center: null,
    diameter: null,
    start: null,
    end: null,
    bounding_box: null,
    paths: [
      [
        { x: 40, y: 20 },
        { x: 42, y: 20 },
        { x: 41, y: 22 },
      ],
    ],
    ...(display ? { display } : {}),
  };
}

async function mode(value: 'mouse' | 'trackpad') {
  await act(async () => {
    const select = get<HTMLSelectElement>('select[aria-label="Camera input mode"]');
    select.value = value;
    select.dispatchEvent(new Event('change', { bubbles: true }));
  });
}

async function wheel(options: WheelEventInit = {}, anchor = { x: 170, y: 120 }) {
  const element = get('.main-view');
  const rect = element.getBoundingClientRect();
  await act(async () =>
    element.dispatchEvent(
      new WheelEvent('wheel', {
        bubbles: true,
        cancelable: true,
        clientX: rect.left + anchor.x,
        clientY: rect.top + anchor.y,
        ...options,
      }),
    ),
  );
}

async function pointer(element: Element, type: string, point: Point, pointerId = 1) {
  const rect = element.getBoundingClientRect();
  const event = new MouseEvent(type, {
    bubbles: true,
    cancelable: true,
    button: 0,
    buttons: type === 'pointerup' ? 0 : 1,
    clientX: rect.left + point.x,
    clientY: rect.top + point.y,
  });
  Object.defineProperty(event, 'pointerId', { value: pointerId });
  await act(async () => element.dispatchEvent(event));
}

async function resize(size: Size) {
  mainSize = size;
  await act(async () => {
    for (const observer of [...observers])
      for (const element of observer.targets) {
        if (element.isConnected && element.classList.contains('main-view')) observer.emit(element);
      }
  });
}

function expectAccurateScale(svg: Element) {
  const bar = svg.querySelector('.scale-bar')!;
  const mm = numberAttribute(bar, 'data-mm');
  const pixels = numberAttribute(bar, 'data-pixels');
  const scale = numberAttribute(bar, 'data-pixels-per-mm');
  expect(pixels).toBeCloseTo(mm * scale, 8);
  expect(bar.querySelector('text')?.textContent).toMatch(/\S+ (µm|mm|m)$/);
  return { mm, pixels, scale };
}

function expectMinimapMatchesCamera() {
  const view = camera();
  const marker = get('.viewport-marker');
  const expected = {
    minX: -view.x / view.scale,
    minY: (view.y - mainSize.height) / view.scale,
    maxX: (mainSize.width - view.x) / view.scale,
    maxY: view.y / view.scale,
  };
  expect(numberAttribute(marker, 'data-world-min-x')).toBeCloseTo(expected.minX, 8);
  expect(numberAttribute(marker, 'data-world-min-y')).toBeCloseTo(expected.minY, 8);
  expect(numberAttribute(marker, 'data-world-max-x')).toBeCloseTo(expected.maxX, 8);
  expect(numberAttribute(marker, 'data-world-max-y')).toBeCloseTo(expected.maxY, 8);
  const mini = expectAccurateScale(get('.minimap'));
  expect(numberAttribute(marker, 'width')).toBeCloseTo(
    (expected.maxX - expected.minX) * mini.scale,
    8,
  );
  expect(numberAttribute(marker, 'height')).toBeCloseTo(
    (expected.maxY - expected.minY) * mini.scale,
    8,
  );
  expect(expectAccurateScale(get('.main-view')).scale).toBe(view.scale);
}

describe('interactive board viewer', () => {
  it('zooms out beyond the finding and whole board, then fits either independently', async () => {
    await render();
    const initial = camera();
    for (let i = 0; i < 10; i++) await click('Zoom out');
    expect(numberAttribute(get('.main-view'), 'data-view-width-mm')).toBeGreaterThan(100);
    expect(camera().scale).toBeLessThan(initial.scale / 40);
    expect(get('.main-view .scene-context').querySelectorAll('use')).toHaveLength(2);
    await click('Fit layout');
    expect(numberAttribute(get('.main-view'), 'data-view-width-mm')).toBeGreaterThan(100);
    expect(numberAttribute(get('.main-view'), 'data-view-width-mm')).toBeLessThan(130);
    await click('Fit finding');
    expect(camera().scale).toBeCloseTo(initial.scale);
    expectPoint(camera(), initial);
  });

  it('supports unclamped dragging and trackpad panning with physical wheel units', async () => {
    await render();
    const original = camera();
    const svg = get('.main-view');
    await pointer(svg, 'pointerdown', { x: 100, y: 100 });
    await pointer(svg, 'pointermove', { x: 1800, y: 950 });
    await pointer(svg, 'pointerup', { x: 1800, y: 950 });
    expectPoint(camera(), { x: original.x + 1700, y: original.y + 850 });
    expect(camera().scale).toBe(original.scale);
    await mode('trackpad');
    await wheel({ deltaX: 2500, deltaY: -1700 });
    expectPoint(camera(), { x: original.x - 800, y: original.y + 2550 });
    expect(camera().scale).toBe(original.scale);
    const beforeLines = camera();
    await wheel({ deltaX: 1, deltaY: 2, deltaMode: 1 });
    expectPoint(camera(), { x: beforeLines.x - 16, y: beforeLines.y - 32 });
    expectMinimapMatchesCamera();
  });

  it('anchors mouse-wheel and trackpad-pinch zoom under the pointer', async () => {
    await render();
    await mode('mouse');
    const anchor = { x: 170, y: 120 };
    const fixed = worldAt(anchor);
    const initialScale = camera().scale;
    await wheel({ deltaY: 120 }, anchor);
    expect(camera().scale).toBeLessThan(initialScale);
    expectPoint(worldAt(anchor), fixed);
    await mode('trackpad');
    const beforePan = camera();
    await wheel({ deltaY: 120 }, anchor);
    expect(camera().scale).toBe(beforePan.scale);
    expect(camera().y).toBeCloseTo(beforePan.y - 120);
    const pinchAnchor = worldAt(anchor);
    const beforePinch = camera().scale;
    await wheel({ deltaY: -35, ctrlKey: true }, anchor);
    expect(camera().scale).toBeGreaterThan(beforePinch);
    expectPoint(worldAt(anchor), pinchAnchor);
    expectMinimapMatchesCamera();
  });

  it('updates the live minimap viewport and both scale bars throughout navigation', async () => {
    await render();
    expectMinimapMatchesCamera();
    const initialWidth = numberAttribute(get('.viewport-marker'), 'width');
    const initialMiniScale = expectAccurateScale(get('.minimap')).scale;
    await click('Zoom in');
    expect(numberAttribute(get('.viewport-marker'), 'width')).toBeLessThan(initialWidth);
    expectMinimapMatchesCamera();
    expect(get('.finding-crosshair').getAttribute('d')).toBeTruthy();
    await mode('trackpad');
    await wheel({ deltaX: 50000, deltaY: 20000 });
    expect(expectAccurateScale(get('.minimap')).scale).toBeLessThan(initialMiniScale);
    expectMinimapMatchesCamera();
  });

  it('preserves the world center and scale on resize while expanding the visible extent', async () => {
    await render();
    await mode('trackpad');
    await wheel({ deltaX: 80, deltaY: -40 });
    const center = worldAt({ x: 450, y: 300 });
    const previousScale = camera().scale;
    await resize({ width: 1200, height: 400 });
    expectPoint(worldAt({ x: 600, y: 200 }), center);
    expect(camera().scale).toBe(previousScale);
    expect(get('.main-view').getAttribute('viewBox')).toBe('0 0 1200 400');
    expect(numberAttribute(get('.main-view'), 'data-view-width-mm')).toBeCloseTo(
      1200 / previousScale,
    );
    expect(numberAttribute(get('.main-view'), 'data-view-height-mm')).toBeCloseTo(
      400 / previousScale,
    );
    expectMinimapMatchesCamera();
  });

  it('recenters from a minimap click without changing the main zoom', async () => {
    await render();
    const scale = camera().scale;
    // Layout bounds dominate the initial minimap; its center is the board center.
    const map = get('.minimap');
    await pointer(map, 'pointerdown', { x: 140, y: 90 });
    await pointer(map, 'pointerup', { x: 140, y: 90 });
    expectPoint(worldAt({ x: 450, y: 300 }), { x: 50, y: 30 });
    expect(camera().scale).toBe(scale);
    expectMinimapMatchesCamera();
  });

  it('reattaches sizing and wheel input after a spatial → stackup → spatial transition', async () => {
    await render();
    await mode('mouse');
    const firstSvg = get('.main-view');
    await render(nonspatial);
    expect(container.querySelector('.main-view')).toBeNull();
    expect(container.textContent).toContain('shared physical stackup');
    await render(spatial);
    expect(get('.main-view')).not.toBe(firstSvg);
    const beforeWheel = camera().scale;
    await wheel({ deltaY: -100 });
    expect
      .soft(camera().scale, 'Wheel listener must bind to the remounted SVG')
      .toBeGreaterThan(beforeWheel);
    await resize({ width: 1100, height: 450 });
    expect(
      get('.main-view').getAttribute('viewBox'),
      'ResizeObserver must follow the remounted SVG',
    ).toBe('0 0 1100 450');
    expectMinimapMatchesCamera();
  });

  it('can start on a nonspatial finding and then initialize the spatial viewer', async () => {
    await render(nonspatial);
    expect(container.querySelector('.main-view')).toBeNull();
    await render(spatial);
    expect(get('.main-view').getAttribute('viewBox')).toBe('0 0 900 600');
    expect(camera().scale).toBeGreaterThan(1);
    expectMinimapMatchesCamera();
  });

  it('keeps native material evidence when its context layer is hidden and reuses it on navigation', async () => {
    spatial.site!.evidence = [
      region({
        kind: 'circle_minus_layer',
        center: { x: 41, y: 21 },
        diameter: 3,
        layer: 'F.Cu',
      }),
    ];
    await render();
    const material = get('#test-scene-1-material');
    const native = get('[data-display-kind="circle_minus_layer"]');
    expect(native.querySelector('mask use')!.getAttribute('href')).toBe('#test-scene-1-material');
    expect(numberAttribute(native.querySelector('circle')!, 'r')).toBe(1.5);
    await toggle('F.Cu');
    expect(container.querySelector('#test-scene-1')).toBeNull();
    expect(container.querySelector('.main-view .scene-context [data-layer="F.Cu"]')).toBeNull();
    expect(get('#test-scene-1-material')).toBe(material);
    expect(get('[data-display-kind="circle_minus_layer"]')).toBe(native);
    await click('Zoom in');
    await mode('trackpad');
    await wheel({ deltaX: 20, deltaY: 30 });
    expect(get('#test-scene-1-material')).toBe(material);
    expectMinimapMatchesCamera();
    await toggle('Evidence');
    expect(container.querySelector('#test-scene-1-material')).toBeNull();
    expect(container.querySelector('.evidence-geometry')).toBeNull();
  });

  it('keeps native clearance widths physical while bounding high-zoom filter surfaces to the viewport', async () => {
    spatial.site!.evidence = [
      {
        ...region({
          kind: 'round_stroke',
          paths: [
            [
              { x: -100000, y: 21 },
              { x: 100000, y: 21 },
            ],
          ],
          width_mm: 0.4,
        }),
        role: 'required_clearance_band',
      },
    ];
    await render();
    const shape = get('[data-display-kind="round_stroke"] path');
    const path = shape.getAttribute('d');
    const nodeCount = container.querySelectorAll('*').length;
    const checkBounds = () => {
      const filter = get('[data-display-kind="round_stroke"] filter');
      const scale = camera().scale;
      expect(numberAttribute(filter, 'width') * scale).toBeLessThanOrEqual(mainSize.width + 6.001);
      expect(numberAttribute(filter, 'height') * scale).toBeLessThanOrEqual(
        mainSize.height + 6.001,
      );
      expect(numberAttribute(filter.querySelector('feMorphology')!, 'radius') * scale).toBeCloseTo(
        0.85,
      );
      expect(numberAttribute(shape, 'stroke-width')).toBe(0.4);
      expect(shape.hasAttribute('vector-effect')).toBe(false);
      expect(get('[data-display-kind="round_stroke"] path')).toBe(shape);
      expect(shape.getAttribute('d')).toBe(path);
      expect(container.querySelectorAll('*')).toHaveLength(nodeCount);
    };
    checkBounds();
    for (let i = 0; i < 24; i++) {
      await click('Zoom in');
      checkBounds();
    }
    expect(camera().scale).toBe(1e6);
    await mode('trackpad');
    await wheel({ deltaX: 200, deltaY: 50 });
    checkBounds();
    expectMinimapMatchesCamera();
  });

  it('preserves independent native contour fill before painting the combined slot evidence', async () => {
    const paths = ['M40 20 H42 A1 1 0 0 1 42 22 H40 Z', 'M41 20 H42 V22 H41 Z'];
    spatial.site!.evidence = [
      {
        ...region({ kind: 'path', paths, fill_rule: 'evenodd' }),
        role: 'nominal_slot',
      },
    ];
    await render();
    const native = get('[data-display-kind="path"]');
    expect([...native.querySelectorAll('path')].map((path) => path.getAttribute('d'))).toEqual(
      paths,
    );
    expect(native.querySelector('g[fill-rule="evenodd"]')).not.toBeNull();
    expect(native.querySelectorAll('filter')).toHaveLength(1);
    expect(native.querySelectorAll('[fill-opacity]')).toHaveLength(0);
    await click('Zoom in');
    expect([...native.querySelectorAll('path')].map((path) => path.getAttribute('d'))).toEqual(
      paths,
    );
  });

  it('keeps circle annotations native with constant screen weight and dash spacing through zoom', async () => {
    spatial.site!.evidence = [
      {
        ...region(),
        role: 'required_copper_envelope',
        kind: 'circle',
        paths: [],
        center: { x: 41, y: 21 },
        diameter: 0.55,
      },
    ];
    await render();
    const circle = get('.evidence-geometry circle');
    const screenStyle = () => ({
      width: numberAttribute(circle, 'stroke-width') * camera().scale,
      dash: circle
        .getAttribute('stroke-dasharray')!
        .split(' ')
        .map(Number)
        .map((n) => n * camera().scale),
    });
    const initial = screenStyle();
    for (let i = 0; i < 8; i++) {
      await click('Zoom in');
      expect(screenStyle().width).toBeCloseTo(initial.width);
      screenStyle().dash.forEach((length, index) =>
        expect(length).toBeCloseTo(initial.dash[index]),
      );
      expect(numberAttribute(circle, 'r')).toBe(0.275);
      expect(circle.hasAttribute('vector-effect')).toBe(false);
    }
    await click('Fit layout');
    expect(screenStyle().width).toBeCloseTo(initial.width);
    expect(circle).toBe(get('.evidence-geometry circle'));
  });

  it('retains measured polygon evidence for old JSON or an absent material scene', async () => {
    passes = [];
    model.report.scene = undefined;
    spatial.site!.evidence = [
      region({
        kind: 'circle_minus_layer',
        center: { x: 41, y: 21 },
        diameter: 3,
        layer: 'F.Cu',
      }),
    ];
    await render();
    const fallback = get('.evidence-geometry path');
    expect(fallback.getAttribute('d')).toBe('M40 20 L42 20 L41 22 Z');
    expect(container.querySelector('[data-display-kind]')).toBeNull();
    expect(container.textContent).toContain('Board geometry is absent');
    spatial = { ...spatial, site: { ...spatial.site!, evidence: [region()] } };
    await render();
    expect(get('.evidence-geometry path').getAttribute('d')).toBe(fallback.getAttribute('d'));
  });
});
