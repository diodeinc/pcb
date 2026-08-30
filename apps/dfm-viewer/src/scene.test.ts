import { describe, expect, it } from 'vitest';
import { compileMaterialPass, compilePass, type SvgNode } from './scene';
import type { ScenePass } from './types';

function svg(body: string, rootAttributes = ''): string {
  return `<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="-2 -20 24 24" ${rootAttributes}>
    <title>F.Cu &amp; routed slots</title>
    <g transform="scale(1 -1)">${body}</g>
  </svg>`;
}

function pass(body: string, feature = 'copper'): ScenePass {
  return { label: 'F.Cu', layer: 'F.Cu', feature, color: '#d87822', svg: svg(body) };
}

function descendants(nodes: SvgNode[]): SvgNode[] {
  return nodes.flatMap((node) => [node, ...descendants(node.children)]);
}

// The native IR renderer paints runs in order. The first clear mask erases
// the base; a later flash repaints part of it, and the final drill removes
// material from both. Apertures and mask paints must not inherit UI colors.
const circle = 'M2 0 A2 2 0 0 1 -2 0 A2 2 0 0 1 2 0 Z';
const nativeArtwork = `
  <defs>
    <path id="a0" d="${circle}" fill-rule="nonzero" stroke="none"/>
    <mask id="m0" maskUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
      <rect x="0" y="0" width="20" height="20" fill="#ffffff"/>
      <g fill="#000000" stroke="#000000"><circle cx="10" cy="10" r="4"/></g>
    </mask>
    <mask id="m1" maskUnits="userSpaceOnUse" x="0" y="0" width="20" height="20">
      <rect x="0" y="0" width="20" height="20" fill="#ffffff"/>
      <g fill="#000000" stroke="#000000"><circle cx="10" cy="10" r="0.5"/></g>
    </mask>
  </defs>
  <g fill="#d87822" stroke="#d87822" opacity="0.9">
    <g id="material" mask="url(#m1)">
      <g id="base-run" mask="url(#m0)">
        <path d="M0 0 L20 0 L20 20 L0 20 Z" fill-rule="nonzero" stroke="none"/>
      </g>
      <use href="#a0" transform="matrix(1 0 0 1 10 10)"/>
    </g>
  </g>`;

describe('native IR scene geometry', () => {
  it('retains analytic arcs and their world coordinates instead of tessellating or cropping', () => {
    const d =
      'M134.425001 -94.24 A0.300001 0.300001 0 0 1 134.125 -93.939999 A0.300001 0.300001 0 1 1 134.425001 -94.24 Z';
    const compiled = compilePass(pass(`<path d="${d}" fill="#d87822"/>`), 'detail-copper');
    const path = descendants(compiled.nodes).find((node) => node.tag === 'path');
    expect(path?.attrs.d).toBe(d);
    expect(compiled.nodes[0].attrs.transform).toBeUndefined();
  });

  it('keeps local aperture and mask references separate for every layer and viewer', () => {
    const scenes = ['detail-copper', 'detail-mask', 'mini-copper', 'mini-mask'].map((prefix) =>
      compilePass(pass(nativeArtwork), prefix),
    );
    const allIds: string[] = [];
    for (const scene of scenes) {
      const nodes = descendants(scene.nodes);
      const ids = nodes.flatMap((node) => (node.attrs.id ? [String(node.attrs.id)] : []));
      allIds.push(...ids);
      expect(ids.length).toBe(5);
      for (const node of nodes) {
        if (node.attrs.href) expect(ids).toContain(String(node.attrs.href).slice(1));
        if (node.attrs.mask) expect(ids).toContain(String(node.attrs.mask).slice(5, -1));
      }
      expect(nodes.find((node) => node.tag === 'use')?.attrs.href).toBe(`#${scene.id}-a0`);
      expect(nodes.find((node) => node.attrs.id === `${scene.id}-material`)?.attrs.mask).toBe(
        `url(#${scene.id}-m1)`,
      );
    }
    expect(new Set(allIds).size).toBe(allIds.length);
  });

  it('removes only the renderer screen flip while retaining local mirrors and rotations', () => {
    const mirrorAndRotation = 'matrix(0 -1 -1 0 27.25 -14.75)';
    const compiled = compilePass(
      pass(`
      <g transform="scale(1 -1)">
        <g transform="${mirrorAndRotation}"><path d="${circle}"/></g>
      </g>`),
      'detail-copper',
    );
    const transforms = descendants(compiled.nodes).flatMap((node) =>
      node.attrs.transform ? [node.attrs.transform] : [],
    );
    expect(transforms).toEqual(['scale(1 -1)', mirrorAndRotation]);
    expect(descendants(compiled.nodes).find((node) => node.tag === 'path')?.attrs.d).toBe(circle);
  });

  it('preserves clear masks, later repaint, final cutouts, and group opacity in paint order', () => {
    const compiled = compilePass(pass(nativeArtwork), 'cu');
    const nodes = descendants(compiled.nodes);
    const material = nodes.find((node) => node.attrs.id === 'cu-material')!;
    expect(material.attrs.mask).toBe('url(#cu-m1)');
    expect(material.children.map((node) => node.tag)).toEqual(['g', 'use']);
    expect(material.children[0].attrs.mask).toBe('url(#cu-m0)');
    expect(material.children[1].attrs.href).toBe('#cu-a0');
    expect(nodes.filter((node) => node.tag === 'mask')).toHaveLength(2);
    for (const mask of nodes.filter((node) => node.tag === 'mask')) {
      expect(mask.attrs.maskUnits).toBe('userSpaceOnUse');
      expect(mask.children[0].attrs.fill).toBe('#ffffff');
      expect(mask.children[1].attrs).toMatchObject({ fill: '#000000', stroke: '#000000' });
    }
    expect(nodes.find((node) => node.attrs.opacity === 0.9)?.attrs).toMatchObject({
      fill: '#d87822',
      stroke: '#d87822',
      opacity: 0.9,
    });
    expect(nodes.find((node) => node.attrs.id === 'cu-a0')?.attrs.stroke).toBe('none');
  });

  it.each(['nonzero', 'evenodd'])(
    'keeps compound hole contours together under %s filling',
    (rule) => {
      const compound = 'M0 0 L10 0 L10 10 L0 10 Z M3 3 L3 7 L7 7 L7 3 Z';
      const compiled = compilePass(
        pass(`<path d="${compound}" fill-rule="${rule}" stroke="none"/>`),
        'cu',
      );
      const paths = descendants(compiled.nodes).filter((node) => node.tag === 'path');
      expect(paths).toHaveLength(1);
      expect(paths[0].attrs).toMatchObject({ d: compound, fillRule: rule, stroke: 'none' });
    },
  );

  it('retains physical trace stroke widths and caps, but gives profile overlays a screen width', () => {
    const compiled = compilePass(
      pass(`
      <path d="M1 2 L4 5" fill="none" stroke-width="0.127" stroke-linecap="round" stroke-linejoin="round"/>
      <path d="M0 0 L20 0 L20 20 Z" fill="none" stroke-width="0.1" data-board-outline="true"/>`),
      'cu',
    );
    const paths = descendants(compiled.nodes).filter((node) => node.tag === 'path');
    expect(paths[0].attrs).toMatchObject({
      strokeWidth: 0.127,
      strokeLinecap: 'round',
      strokeLinejoin: 'round',
    });
    expect(paths[0].attrs.vectorEffect).toBeUndefined();
    expect(paths[1].attrs.vectorEffect).toBe('non-scaling-stroke');
    expect(paths[1].attrs.strokeWidth).toBeGreaterThan(0);
  });

  it('supports legacy local xlink references without allowing an external URL', () => {
    const compiled = compilePass(
      pass(`<defs><path id="a0" d="${circle}"/></defs><use xlink:href="#a0"/>`),
      'cu',
    );
    expect(descendants(compiled.nodes).find((node) => node.tag === 'use')?.attrs.href).toBe(
      '#cu-a0',
    );
  });
});

describe('opaque material operands', () => {
  function idsAndReferences(nodes: SvgNode[]) {
    const all = descendants(nodes);
    const ids = all.flatMap((node) => (node.attrs.id ? [String(node.attrs.id)] : []));
    expect(new Set(ids).size).toBe(ids.length);
    for (const node of all) {
      if (node.attrs.href) expect(ids).toContain(String(node.attrs.href).slice(1));
      for (const key of ['mask', 'clipPath']) {
        if (node.attrs[key]) expect(ids).toContain(String(node.attrs[key]).slice(5, -1));
      }
    }
    return ids;
  }

  it('preserves clear/repaint/final-cutout ordering while removing foreground opacity', () => {
    const original = compilePass(pass(nativeArtwork), 'copper');
    const before = JSON.stringify(original);
    const material = compileMaterialPass(original, 'missing-ring');
    expect(material.id).toBe('missing-ring');
    expect(material.color).toBe('#000000');
    expect(JSON.stringify(original)).toBe(before);
    idsAndReferences(material.nodes);
    const foreground = descendants(material.nodes.slice(1));
    expect(foreground.some((node) => node.attrs.fill === '#000000')).toBe(true);
    for (const node of foreground) {
      expect(node.attrs.opacity).toBeUndefined();
      expect(node.attrs.fillOpacity).toBeUndefined();
      expect(node.attrs.strokeOpacity).toBeUndefined();
      for (const key of ['fill', 'stroke']) {
        if (node.attrs[key]) expect(['#000000', 'none']).toContain(node.attrs[key]);
      }
    }
    const outer = foreground.find(
      (node) => node.attrs.mask === 'url(#missing-ring-source-copper-m1)',
    )!;
    expect(outer.children.map((node) => node.tag)).toEqual(['g', 'use']);
    expect(outer.children[0].attrs.mask).toBe('url(#missing-ring-source-copper-m0)');
    expect(outer.children[1].attrs).toMatchObject({
      href: '#missing-ring-paint-copper-a0',
      transform: 'matrix(1 0 0 1 10 10)',
    });
    const masks = descendants(material.nodes).filter((node) => node.tag === 'mask');
    expect(masks).toHaveLength(2);
    for (const mask of masks) {
      expect(mask.attrs.maskUnits).toBe('userSpaceOnUse');
      expect(mask.children[0].attrs.fill).toBe('#ffffff');
      expect(mask.children[1].attrs).toMatchObject({ fill: '#000000', stroke: '#000000' });
    }
    const aperture = descendants(material.nodes).find(
      (node) => node.attrs.id === 'missing-ring-paint-copper-a0',
    )!;
    expect(aperture.attrs).toMatchObject({ d: circle, stroke: 'none', fillRule: 'nonzero' });
  });

  it('separates a shared white mask aperture from its black foreground variant', () => {
    const transform = 'matrix(0 -1 -1 0 27.25 -14.75)';
    const compound = 'M0 0 H10 V10 H0 Z M2 2 H8 V8 H2 Z';
    const original = compilePass(
      pass(`
      <defs>
        <g id="shared" fill="#ffffff" stroke="#ffffff" opacity="0.4" fill-opacity="0.6" stroke-opacity="0.7" transform="${transform}">
          <path d="${compound}" fill-rule="evenodd" stroke="none"/>
        </g>
        <mask id="keep" maskUnits="userSpaceOnUse" maskContentUnits="userSpaceOnUse" x="-50" y="-50" width="100" height="100">
          <rect x="-50" y="-50" width="100" height="100" fill="#000000"/>
          <use href="#shared" opacity="0.8"/>
        </mask>
        <clipPath id="clip" clipPathUnits="userSpaceOnUse" transform="translate(1 2)"><use href="#shared"/></clipPath>
      </defs>
      <g fill="#d87822" opacity="0.9" mask="url(#keep)" clip-path="url(#clip)">
        <use href="#shared"/><use href="#shared" transform="translate(5 6)"/>
      </g>`),
      'cu',
    );
    const before = JSON.stringify(original);
    const compiled = compileMaterialPass(original, 'mono');
    idsAndReferences(compiled.nodes);
    expect(JSON.stringify(original)).toBe(before);
    const all = descendants(compiled.nodes);
    const maskSource = all.find((node) => node.attrs.id === 'mono-source-cu-shared')!;
    expect(maskSource.attrs).toMatchObject({
      fill: '#ffffff',
      stroke: '#ffffff',
      opacity: 0.4,
      fillOpacity: 0.6,
      strokeOpacity: 0.7,
      transform,
    });
    const materialSource = all.find((node) => node.attrs.id === 'mono-paint-cu-shared')!;
    expect(materialSource.attrs).toMatchObject({ fill: '#000000', stroke: '#000000', transform });
    expect(materialSource.attrs.opacity).toBeUndefined();
    expect(materialSource.attrs.fillOpacity).toBeUndefined();
    expect(materialSource.attrs.strokeOpacity).toBeUndefined();
    expect(materialSource.children[0].attrs).toMatchObject({
      d: compound,
      fillRule: 'evenodd',
      stroke: 'none',
    });
    const mask = all.find((node) => node.attrs.id === 'mono-source-cu-keep')!;
    expect(mask.children[1].attrs).toMatchObject({ href: '#mono-source-cu-shared', opacity: 0.8 });
    const clip = all.find((node) => node.attrs.id === 'mono-source-cu-clip')!;
    expect(clip.attrs).toMatchObject({
      clipPathUnits: 'userSpaceOnUse',
      transform: 'translate(1 2)',
    });
    expect(clip.children[0].attrs.href).toBe('#mono-source-cu-shared');
    const visibleUses = descendants(compiled.nodes.slice(1)).filter((node) => node.tag === 'use');
    expect(visibleUses.map((node) => node.attrs.href)).toEqual([
      '#mono-paint-cu-shared',
      '#mono-paint-cu-shared',
    ]);
    expect(all.filter((node) => node.attrs.id === 'mono-paint-cu-shared')).toHaveLength(1);
  });

  it('retains physical strokes and namespaces nested use chains independently', () => {
    const source = compilePass(
      pass(`
      <defs>
        <path id="stroke" d="M1 2 L3 4" fill="none" stroke="#d87822" stroke-width="0.127" stroke-linecap="round" stroke-opacity="0.5"/>
        <g id="repeat" transform="rotate(90)"><use href="#stroke"/></g>
      </defs>
      <use href="#repeat" transform="translate(7 8)"/>`),
      'cu',
    );
    const first = compileMaterialPass(source, 'first');
    const second = compileMaterialPass(source, 'second');
    const ids = [
      ...idsAndReferences(source.nodes),
      ...idsAndReferences(first.nodes),
      ...idsAndReferences(second.nodes),
    ];
    expect(new Set(ids).size).toBe(ids.length);
    const nodes = descendants(first.nodes);
    const stroke = nodes.find((node) => node.attrs.id === 'first-paint-cu-stroke')!;
    expect(stroke.attrs).toMatchObject({
      d: 'M1 2 L3 4',
      fill: 'none',
      stroke: '#000000',
      strokeWidth: 0.127,
      strokeLinecap: 'round',
    });
    expect(stroke.attrs.strokeOpacity).toBeUndefined();
    expect(stroke.attrs.vectorEffect).toBeUndefined();
    const repeat = nodes.find((node) => node.attrs.id === 'first-paint-cu-repeat')!;
    expect(repeat.attrs.transform).toBe('rotate(90)');
    expect(repeat.children[0].attrs.href).toBe('#first-paint-cu-stroke');
  });

  it('rejects recursive material uses instead of recursing indefinitely', () => {
    const source = compilePass(
      pass('<defs><g id="cycle"><use href="#cycle"/></g></defs><use href="#cycle"/>'),
      'cu',
    );
    expect(() => compileMaterialPass(source, 'mono')).toThrow(/Cyclic material/);
    expect(() =>
      compileMaterialPass(compilePass(pass(nativeArtwork), 'cu'), 'unsafe namespace'),
    ).toThrow(/namespace/);
  });
});

describe('scene import safety and invalid geometry', () => {
  it.each([
    ['script', '<script>alert(1)</script>'],
    [
      'foreign HTML',
      '<foreignObject><div xmlns="http://www.w3.org/1999/xhtml">active</div></foreignObject>',
    ],
    ['embedded image', '<image href="data:image/svg+xml;base64,AAAA"/>'],
    ['animation', '<animate attributeName="href" to="https://example.com/shape.svg"/>'],
    ['attribute animation', '<set attributeName="fill" to="red"/>'],
    ['CSS', '<style>path { fill: red }</style>'],
    ['link', '<a href="https://example.com"><path d="M0 0 L1 1"/></a>'],
    ['event handler', '<path d="M0 0 L1 1" onload="alert(1)"/>'],
    ['inline style', '<path d="M0 0 L1 1" style="fill: url(https://example.com/p.svg)"/>'],
    ['external aperture', '<use href="https://example.com/a.svg#a0"/>'],
    ['external xlink aperture', '<use xlink:href="https://example.com/a.svg#a0"/>'],
    ['relative external aperture', '<use href="other.svg#a0"/>'],
    ['script URL', '<use href="javascript:alert(1)"/>'],
    ['data URL', '<use href="data:image/svg+xml,anything"/>'],
    ['external mask', '<path mask="url(https://example.com/a.svg#m0)"/>'],
    ['external clip', '<path clip-path="url(https://example.com/a.svg#clip)"/>'],
    ['external paint', '<path fill="url(https://example.com/a.svg#paint)"/>'],
    ['unknown ID', '<use href="#missing"/>'],
    ['unknown mask ID', '<path mask="url(#missing)"/>'],
    ['duplicate IDs', '<path id="a0"/><path id="a0"/>'],
    ['invalid ID', '<path id="two words"/>'],
    ['nonfinite coordinate', '<circle cx="Infinity" cy="0" r="1"/>'],
  ])('rejects %s', (_label, body) => {
    expect(() => compilePass(pass(body), 'cu')).toThrow();
  });

  it.each([
    'matrix(1 0 0)',
    'matrix(1 0 0 1 1e309 0)',
    'matrix(1 0 0 1 1..2 0)',
    'translate(1 2 3)',
    'rotate(45 2)',
    'scale(1e309)',
    'rotate(1..2)',
    'matrix(e e e e e e)',
    'matrix(1 0 0 1 0 0) garbage',
  ])('rejects malformed or nonfinite transform %s', (transform) => {
    expect(() =>
      compilePass(pass(`<g transform="${transform}"><path d="M0 0 L1 1"/></g>`), 'cu'),
    ).toThrow();
  });

  it('rejects overflowed path coordinates instead of letting the browser drop geometry', () => {
    expect(() => compilePass(pass('<path d="M0 0 L1e309 1"/>'), 'cu')).toThrow();
  });

  it.each([
    'onload="alert(1)"',
    'style="background: url(https://example.com/track)"',
    'href="https://example.com/other.svg"',
  ])('rejects unsafe attributes on the document root: %s', (attribute) => {
    expect(() =>
      compilePass({ ...pass(''), svg: svg('<path d="M0 0 L1 1"/>', attribute) }, 'cu'),
    ).toThrow();
  });

  it('rejects document types and entity declarations before XML parsing', () => {
    const declaration = '<!DOCTYPE svg [<!ENTITY outside SYSTEM "https://example.com/file">]>';
    expect(() =>
      compilePass({ ...pass(''), svg: declaration + svg('<path d="M0 0 L1 1"/>') }, 'cu'),
    ).toThrow();
  });

  it.each([
    '<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0 L1 1"/></svg>',
    '<svg xmlns="http://www.w3.org/2000/svg"><g transform="scale(1 1)"><path/></g></svg>',
    '<svg xmlns="http://www.w3.org/2000/svg"><g transform="scale(1 -1)"/><g transform="scale(1 -1)"/></svg>',
    '<svg xmlns="http://www.w3.org/2000/svg"><g transform="scale(1 -1)"></svg>',
  ])('rejects malformed or unsupported coordinate frames', (document) => {
    expect(() => compilePass({ ...pass(''), svg: document }, 'cu')).toThrow();
  });
});
