import type { ScenePass } from './types';

export interface SvgNode {
  tag: string;
  attrs: Record<string, string | number>;
  children: SvgNode[];
}
export interface CompiledPass extends Omit<ScenePass, 'svg'> {
  id: string;
  nodes: SvgNode[];
}
const tags = new Set([
  'g',
  'defs',
  'path',
  'use',
  'mask',
  'clipPath',
  'rect',
  'circle',
  'ellipse',
  'line',
  'polyline',
  'polygon',
]);
const names: Record<string, string> = {
  'fill-rule': 'fillRule',
  'stroke-width': 'strokeWidth',
  'stroke-linecap': 'strokeLinecap',
  'stroke-linejoin': 'strokeLinejoin',
  'stroke-miterlimit': 'strokeMiterlimit',
  'fill-opacity': 'fillOpacity',
  'stroke-opacity': 'strokeOpacity',
  'stroke-dasharray': 'strokeDasharray',
  'stroke-dashoffset': 'strokeDashoffset',
  'clip-rule': 'clipRule',
  'clip-path': 'clipPath',
  'vector-effect': 'vectorEffect',
  'xlink:href': 'href',
};
const numeric = new Set([
  'x',
  'y',
  'x1',
  'y1',
  'x2',
  'y2',
  'cx',
  'cy',
  'r',
  'rx',
  'ry',
  'width',
  'height',
  'opacity',
  'fill-opacity',
  'stroke-opacity',
  'stroke-width',
  'stroke-miterlimit',
  'stroke-dashoffset',
]);
const identifier = /^[A-Za-z_][A-Za-z0-9_.:-]*$/;
const numericList = /^[\s,+.\deE-]+$/;
const numberToken = '[-+]?(?:\\d+\\.?\\d*|\\.\\d+)(?:[eE][-+]?\\d+)?';

function validateTransform(value: string) {
  const arities: Record<string, number[]> = {
    matrix: [6],
    translate: [1, 2],
    scale: [1, 2],
    rotate: [1, 3],
    skewX: [1],
    skewY: [1],
  };
  const expressions = value.matchAll(/(matrix|translate|scale|rotate|skewX|skewY)\(([^()]*)\)\s*/g);
  let end = 0;
  for (const expression of expressions) {
    if (expression.index !== end) throw new Error('Invalid vector transform.');
    const parameters = expression[2].trim();
    if (!new RegExp(`^${numberToken}(?:(?:\\s*,\\s*|\\s+)${numberToken})*$`).test(parameters))
      throw new Error('Invalid vector transform parameters.');
    const values = parameters.match(new RegExp(numberToken, 'g'))!.map(Number);
    if (!arities[expression[1]].includes(values.length) || !values.every(Number.isFinite))
      throw new Error('Invalid vector transform arity or coordinate.');
    end = expression.index + expression[0].length;
  }
  if (end !== value.length || end === 0) throw new Error('Invalid vector transform.');
}

export function validatePath(value: string) {
  const token = new RegExp(`[MmLlHhVvCcSsQqTtAaZz]|${numberToken}`, 'g');
  if (value.replace(token, '').replace(/[\s,]/g, '')) throw new Error('Invalid vector path.');
  const tokens = value.match(token) || [];
  if (!tokens.length) return;
  if (tokens[0]!.toUpperCase() !== 'M') throw new Error('A vector path must start with a move.');
  const arities: Record<string, number> = {
    M: 2,
    L: 2,
    H: 1,
    V: 1,
    C: 6,
    S: 4,
    Q: 4,
    T: 2,
    A: 7,
    Z: 0,
  };
  let command = '',
    values: number[] = [];
  const finish = () => {
    if (!command) return;
    const arity = arities[command];
    if (
      !values.every(Number.isFinite) ||
      (arity === 0 ? values.length !== 0 : !values.length || values.length % arity !== 0)
    )
      throw new Error('Invalid or nonfinite vector path coordinates.');
    if (command === 'A')
      for (let i = 0; i < values.length; i += 7) {
        if (
          values[i] < 0 ||
          values[i + 1] < 0 ||
          ![0, 1].includes(values[i + 3]) ||
          ![0, 1].includes(values[i + 4])
        )
          throw new Error('Invalid arc parameters.');
      }
  };
  for (const item of tokens) {
    if (/^[A-Za-z]$/.test(item)) {
      finish();
      command = item.toUpperCase();
      values = [];
    } else values.push(Number(item));
  }
  finish();
}

/** Import only inert vector primitives, never HTML, styles, scripts or remote resources.
 * The source renderer's single screen flip is removed: the camera owns that flip.
 * Namespaced local references keep aperture/mask IDs independent between layers.
 */
export function compilePass(pass: ScenePass, prefix: string): CompiledPass {
  if (!/^[A-Za-z][A-Za-z0-9_-]*$/.test(prefix)) throw new Error('Invalid scene namespace.');
  if (/<!DOCTYPE|<!ENTITY/i.test(pass.svg))
    throw new Error('Scene SVG cannot contain document entities.');
  const doc = new DOMParser().parseFromString(pass.svg, 'image/svg+xml');
  const root = doc.documentElement;
  if (root.localName !== 'svg' || doc.querySelector('parsererror'))
    throw new Error(`Invalid vector geometry in ${pass.label}.`);
  for (const attribute of [...root.attributes]) {
    if (!['xmlns', 'xmlns:xlink', 'viewBox', 'width', 'height', 'version'].includes(attribute.name))
      throw new Error(`Unsupported SVG root attribute ${attribute.name}.`);
  }
  const ids = new Set<string>();
  for (const element of root.querySelectorAll('[id]')) {
    const id = element.getAttribute('id')!;
    if (!identifier.test(id) || ids.has(id))
      throw new Error(`Invalid or duplicate SVG ID in ${pass.label}.`);
    ids.add(id);
  }
  const localId = (id: string) => {
    if (!ids.has(id)) throw new Error(`Unknown local vector reference in ${pass.label}.`);
    return `${prefix}-${id}`;
  };
  const read = (element: Element, depth: number): SvgNode | null => {
    if (element.localName === 'title' || element.localName === 'desc') return null;
    if (!tags.has(element.localName))
      throw new Error(`Unsupported SVG element ${element.localName} in ${pass.label}.`);
    const attrs: SvgNode['attrs'] = {};
    for (const attribute of [...element.attributes]) {
      const { name, value } = attribute;
      if (name === 'id') attrs.id = localId(value);
      else if (name === 'href' || name === 'xlink:href') {
        if (!value.startsWith('#'))
          throw new Error('Scene geometry cannot load external resources.');
        attrs.href = `#${localId(value.slice(1))}`;
      } else if (name === 'mask' || name === 'clip-path') {
        const match = /^url\(#([A-Za-z_][A-Za-z0-9_.:-]*)\)$/.exec(value);
        if (!match) throw new Error('Only local geometry masks are supported.');
        attrs[names[name] || name] = `url(#${localId(match[1])})`;
      } else if (name === 'transform') {
        if (depth === 0 && /^scale\(1[ ,]+-1\)$/.test(value)) continue;
        validateTransform(value);
        attrs.transform = value;
      } else if (name === 'd') {
        validatePath(value);
        attrs.d = value;
      } else if (name === 'points' || name === 'stroke-dasharray') {
        if (!numericList.test(value)) throw new Error('Invalid vector coordinates.');
        attrs[names[name] || name] = value;
      } else if (numeric.has(name)) {
        if (!Number.isFinite(Number(value))) throw new Error('Nonfinite vector coordinate.');
        attrs[names[name] || name] = Number(value);
      } else if (name === 'fill' || name === 'stroke') {
        if (!/^(#[0-9a-fA-F]{3,8}|none|inherit|currentColor|white|black)$/.test(value))
          throw new Error('Only solid geometry paints are supported.');
        attrs[name] = value;
      } else if (
        ['fill-rule', 'clip-rule'].includes(name) &&
        ['nonzero', 'evenodd'].includes(value)
      )
        attrs[names[name]] = value;
      else if (name === 'stroke-linecap' && ['butt', 'round', 'square'].includes(value))
        attrs.strokeLinecap = value;
      else if (name === 'stroke-linejoin' && ['miter', 'round', 'bevel'].includes(value))
        attrs.strokeLinejoin = value;
      else if (
        ['maskUnits', 'maskContentUnits', 'clipPathUnits'].includes(name) &&
        ['userSpaceOnUse', 'objectBoundingBox'].includes(value)
      )
        attrs[name] = value;
      else if (name === 'vector-effect' && value === 'non-scaling-stroke')
        attrs.vectorEffect = value;
      else if (name === 'data-board-outline' && value === 'true') {
        /* presentation width below */
      } else throw new Error(`Unsupported SVG attribute ${name} in ${pass.label}.`);
    }
    if (
      element.getAttribute('data-board-outline') === 'true' ||
      (['board_outlines', 'array_outlines', 'scores'].includes(pass.feature) &&
        element.localName === 'path')
    ) {
      attrs.vectorEffect = 'non-scaling-stroke';
      attrs.strokeWidth = pass.feature === 'scores' ? 1 : 1.2;
    }
    return {
      tag: element.localName,
      attrs,
      children: [...element.children]
        .map((child) => read(child, depth + 1))
        .filter((child): child is SvgNode => child !== null),
    };
  };
  const groups = [...root.children].filter(
    (element) => !['title', 'desc'].includes(element.localName),
  );
  if (
    groups.length !== 1 ||
    groups[0].localName !== 'g' ||
    !/^scale\(1[ ,]+-1\)$/.test(groups[0].getAttribute('transform') || '')
  )
    throw new Error(`Scene ${pass.label} must use the PCB IR world coordinate frame.`);
  return {
    label: pass.label,
    feature: pass.feature,
    layer: pass.layer,
    color: /^#[0-9a-f]{6}$/i.test(pass.color) ? pass.color : '#555555',
    id: prefix,
    nodes: groups.map((group) => read(group, 0)).filter((node): node is SvgNode => node !== null),
  };
}

/** A separately namespaced, opaque black silhouette for a constructive mask.
 * Foreground paint loses its presentation color/opacity; native masks and clips
 * keep their original paints. A definition used in both contexts therefore gets
 * a separate black foreground variant, compiled once per referenced definition.
 * The source pass is never mutated and need not remain mounted.
 */
export function compileMaterialPass(pass: CompiledPass, prefix: string): CompiledPass {
  if (!/^[A-Za-z][A-Za-z0-9_-]*$/.test(prefix)) throw new Error('Invalid material namespace.');
  const originals = new Map<string, SvgNode>();
  const collect = (node: SvgNode) => {
    if (node.attrs.id != null) originals.set(String(node.attrs.id), node);
    node.children.forEach(collect);
  };
  pass.nodes.forEach(collect);
  const sourceId = (id: string) => {
    if (!originals.has(id))
      throw new Error(`Unknown material geometry reference in ${pass.label}.`);
    return `${prefix}-source-${id}`;
  };
  const sourceAttrs = (node: SvgNode): SvgNode['attrs'] => {
    const attrs = { ...node.attrs };
    if (attrs.id != null) attrs.id = sourceId(String(attrs.id));
    if (attrs.href != null) attrs.href = `#${sourceId(String(attrs.href).slice(1))}`;
    for (const key of ['mask', 'clipPath']) {
      if (attrs[key] != null) attrs[key] = `url(#${sourceId(String(attrs[key]).slice(5, -1))})`;
    }
    return attrs;
  };
  const sourceNode = (node: SvgNode): SvgNode => ({
    tag: node.tag,
    attrs: sourceAttrs(node),
    children: node.children.map(sourceNode),
  });
  const variants: SvgNode[] = [];
  const compiled = new Map<string, string>();
  const pending = new Set<string>();
  const nonPaint = new Set(['defs', 'mask', 'clipPath']);
  const foregroundNode = (node: SvgNode): SvgNode | null => {
    if (nonPaint.has(node.tag)) return null;
    const attrs = sourceAttrs(node);
    // Foreground IDs live only on demand-compiled definitions. Inline copies
    // must not duplicate those IDs when a group is also referenced elsewhere.
    delete attrs.id;
    for (const key of ['fill', 'stroke']) {
      if (attrs[key] != null && attrs[key] !== 'none') attrs[key] = '#000000';
    }
    delete attrs.opacity;
    delete attrs.fillOpacity;
    delete attrs.strokeOpacity;
    if (node.attrs.href != null)
      attrs.href = `#${foregroundReference(String(node.attrs.href).slice(1))}`;
    return {
      tag: node.tag,
      attrs,
      children: node.children
        .map(foregroundNode)
        .filter((child): child is SvgNode => child !== null),
    };
  };
  const foregroundReference = (id: string): string => {
    const cached = compiled.get(id);
    if (cached) return cached;
    if (pending.has(id)) throw new Error(`Cyclic material geometry reference in ${pass.label}.`);
    const original = originals.get(id);
    if (!original) throw new Error(`Unknown material geometry reference in ${pass.label}.`);
    pending.add(id);
    const node = foregroundNode(original);
    if (!node) throw new Error(`Unsupported material geometry reference in ${pass.label}.`);
    const materialId = `${prefix}-paint-${id}`;
    node.attrs.id = materialId;
    variants.push(node);
    compiled.set(id, materialId);
    pending.delete(id);
    return materialId;
  };
  const foreground = pass.nodes
    .map(foregroundNode)
    .filter((node): node is SvgNode => node !== null);
  return {
    ...pass,
    id: prefix,
    color: '#000000',
    nodes: [
      // Keep original definitions under <defs> so mask/clip references retain
      // polarity without painting a second copy of the colored layer.
      { tag: 'defs', attrs: {}, children: [...pass.nodes.map(sourceNode), ...variants] },
      { tag: 'g', attrs: { fill: '#000000' }, children: foreground },
    ],
  };
}
