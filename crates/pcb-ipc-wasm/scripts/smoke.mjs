import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { constants, zstdCompressSync } from 'node:zlib';
import { Worker, isMainThread, parentPort, workerData } from 'node:worker_threads';

// Exercise the browser ES-module package without a DOM, filesystem imports in
// WASM, or shared memory. The same tests can load the generated Node package.
const repoRoot = resolve(import.meta.dirname, '../../..');
const packageDir = isMainThread ? resolve(process.argv[2] ?? `${repoRoot}/target/ipc-wasm-bundle`) : workerData.packageDir;
const api = await import(pathToFileURL(resolve(packageDir, 'pcb_ipc_wasm.js')));
if (typeof api.default === 'function') {
  await api.default({ module_or_path: await readFile(resolve(packageDir, 'pcb_ipc_wasm_bg.wasm')) });
}
const { IpcDocument, builtinPdks } = api;
const xml = await readFile(new URL('../tests/board.xml', import.meta.url), 'utf8');
const pdk = { name: 'process.toml', source: await readFile(new URL('../tests/pdk.toml', import.meta.url), 'utf8') };
const generatedAt = '2026-08-30T12:00:00Z';
const text = file => new TextDecoder().decode(file.data);
const hash = bytes => createHash('sha256').update(bytes).digest('hex');

if (!isMainThread) {
  const document = new IpcDocument(xml);
  try {
    const report = document.checkDfm({ pdk, generatedAt });
    parentPort.postMessage({ verdict: report.verdict, rules: report.rules });
  } finally {
    document.free();
  }
} else {
  const document = new IpcDocument(xml, { name: 'fixture.xml', validate: true });
  try {
    assert.equal(document.info().board_dimensions.width_mm, 30);
    assert.equal(document.info().layers.copper, 2);
    assert.ok(document.layers().includes('TOP'));
    const builtins = builtinPdks();
    assert.equal(builtins[0].name, 'standard');
    assert.ok(builtins[0].source.includes('[pdk]'));
    assert.equal(builtins.find(pdk => pdk.name === 'jlcpcb-1oz').profile, 'one-ounce');
    assert.equal(builtins.find(pdk => pdk.name === 'ipc').profile, '2b');
    const bom = JSON.parse(text(document.export({ format: 'bom' })[0]));
    assert.equal(bom[0].designator, 'J1');
    assert.equal(bom[0].characteristics.value, 'Connector');

    const svg = document.export({ format: 'svg', layer: 'TOP' })[0];
    assert.ok(svg.data instanceof Uint8Array);
    assert.equal(svg.mediaType, 'image/svg+xml');
    assert.match(text(svg), /<svg/);
    const png = document.export({ format: 'png', layer: 'TOP' })[0];
    assert.deepEqual([...png.data.slice(0, 8)], [137, 80, 78, 71, 13, 10, 26, 10]);
    const pngHeader = new DataView(png.data.buffer, png.data.byteOffset);
    assert.equal(Math.max(pngHeader.getUint32(16), pngHeader.getUint32(20)), 3200);
    assert.match(text(document.export({ format: 'dxf' })[0]), /ENTITIES/);
    assert.match(text(document.export({ format: 'cpl' })[0]), /J1,Connector,connector,5\.000000,5\.000000/);
    assert.match(text(document.export({ format: 'ict' })[0]), /Designator/);
    assert.match(text(document.export({ format: 'html' })[0]), /<html/);
    assert.equal(text(document.export({ format: 'ipc2581' })[0]), xml);
    const fabrication = new IpcDocument(text(document.export({ format: 'ipc2581', mode: 'fabrication' })[0]), { validate: true });
    assert.equal(fabrication.info().mode, 'FABRICATION');
    assert.equal(JSON.parse(text(fabrication.export({ format: 'bom' })[0])).length, 0);
    fabrication.free();

    const files = document.export({ format: 'gerber' });
    assert.ok(files.some(f => f.name.endsWith('.gtl')));
    assert.ok(files.some(f => f.name === 'PTH.drl' && text(f).includes('M48')));
    assert.ok(files.some(f => f.name.endsWith('.gm1')), 'plain-board package must include its outline');
    assert.deepEqual(files, document.export({ format: 'gerber', layoutTarget: 'board' }));
    const archive = document.export({ format: 'gerber', zip: true })[0];
    assert.equal(archive.mediaType, 'application/zip');
    assert.deepEqual([...archive.data.slice(0, 4)], [80, 75, 3, 4]);
    for (const file of files) assert.ok(Buffer.from(archive.data).includes(Buffer.from(file.name)));
    assert.deepEqual(archive.data, document.export({ format: 'gerber', zip: true })[0].data);

    const withDrawing = new IpcDocument(xml.replace('layerFunction="SIGNAL"', 'layerFunction="ASSEMBLY"'));
    try {
      const production = withDrawing.export({ format: 'gerber' });
      assert.ok(!production.some(f => f.name === 'TOP.gbr'));
      assert.deepEqual(production, withDrawing.export({ format: 'gerber', includeAuxiliaryLayers: false }));
      const auxiliary = withDrawing.export({ format: 'gerber', includeAuxiliaryLayers: true });
      assert.match(text(auxiliary.find(f => f.name === 'TOP.gbr')), /%TF.FileFunction,AssemblyDrawing,Top\*%/);
      for (const file of production.filter(f => f.name !== 'job.gbrjob')) {
        assert.deepEqual(auxiliary.find(f => f.name === file.name), file);
      }
      for (const packageFiles of [production, auxiliary]) {
        const job = JSON.parse(text(packageFiles.find(f => f.name === 'job.gbrjob')));
        assert.deepEqual(job.FilesAttributes.map(f => f.Path).sort(),
          packageFiles.filter(f => f.name !== 'job.gbrjob').map(f => f.name).sort());
        if (packageFiles === auxiliary) {
          assert.equal(job.FilesAttributes.find(f => f.Path === 'TOP.gbr').FileFunction, 'AssemblyDrawing,Top');
        }
      }
      const auxiliaryZip = withDrawing.export({ format: 'gerber', zip: true, includeAuxiliaryLayers: true })[0];
      assert.ok(Buffer.from(auxiliaryZip.data).includes(Buffer.from('TOP.gbr')));
    } finally {
      withDrawing.free();
    }

    const report = document.checkDfm({ pdk, generatedAt });
    assert.equal(report.verdict, 'fail');
    assert.equal(report.summary.errors, 3);
    assert.equal(report.input.path, 'fixture.xml');
    assert.equal(report.input.sha256, hash(xml));
    assert.equal(report.input.size_bytes, Buffer.byteLength(xml));
    assert.equal(report.pdk.sha256, hash(pdk.source));
    assert.equal(report.pdk.source, pdk.source);
    assert.equal(report.pdk.profile, 'test');
    assert.equal(report.pdk.profile_status, 'executable');
    assert.deepEqual(report.pdk.support.copper_layers, { exact: null, minimum: 2, maximum: 4 });
    assert.deepEqual(report.rules.filter(r => r.finding_count > 0).map(r => r.id).sort(), [
      'copper.minimum_feature_width', 'drilling.minimum_pth_hole_diameter', 'soldermask.minimum_web',
    ]);
    assert.equal(report.rules.find(r => r.id === 'copper.minimum_pth_annular_ring').checked, 2);
    assert.deepEqual(report, document.checkDfm({ pdk, generatedAt }));
    const defaultReport = document.checkDfm();
    assert.equal(defaultReport.pdk.path, 'builtin:standard');
    assert.ok(Date.parse(defaultReport.generated_at) > Date.parse('2026-01-01T00:00:00Z'));

    // The compressed input's identity is the original bytes, not decompressed
    // text. Test concatenated and skippable frames as well as ordinary XML.
    const split = Math.floor(xml.length / 2);
    const skippable = Buffer.from([0x50, 0x2a, 0x4d, 0x18, 3, 0, 0, 0, 1, 2, 3]);
    const checksumOptions = { params: { [constants.ZSTD_c_checksumFlag]: 1 } };
    const checksummed = zstdCompressSync(xml, checksumOptions);
    const firstFrame = zstdCompressSync(xml.slice(0, split), checksumOptions);
    const lastFrame = zstdCompressSync(xml.slice(split), checksumOptions);
    const withoutSize = zstdCompressSync(xml, { params: { [constants.ZSTD_c_contentSizeFlag]: 0 } });
    for (const bytes of [Buffer.from(xml), zstdCompressSync(xml), checksummed, withoutSize, Buffer.concat([
      skippable, firstFrame, skippable, lastFrame,
    ])]) {
      const decoded = IpcDocument.fromBytes(bytes, { name: 'input.xml.zst' });
      assert.equal(text(decoded.export({ format: 'ipc2581' })[0]), xml);
      assert.equal(decoded.checkDfm({ pdk, generatedAt }).input.sha256, hash(bytes));
      decoded.free();
    }
    const corruptChecksum = Buffer.from(checksummed);
    corruptChecksum[corruptChecksum.length - 1] ^= 1;
    assert.throws(() => IpcDocument.fromBytes(corruptChecksum), /checksum/i);
    const corruptLastFrame = Buffer.from(lastFrame);
    corruptLastFrame[corruptLastFrame.length - 1] ^= 1;
    assert.throws(() => IpcDocument.fromBytes(Buffer.concat([
      firstFrame, skippable, corruptLastFrame,
    ])), /checksum/i);
    // A short single-segment frame stores its content size in byte 5.
    // Alter only that field, leaving otherwise valid, decodable XML intact.
    const smallXml = '<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581"><Content roleRef="owner"><FunctionMode mode="BOM"/></Content></IPC-2581>';
    const wrongSize = zstdCompressSync(smallXml);
    assert.equal(wrongSize[4], 0x20);
    assert.equal(wrongSize[5], Buffer.byteLength(smallXml));
    wrongSize[5] += 1;
    assert.throws(() => IpcDocument.fromBytes(wrongSize), /content size/i);
    assert.throws(() => IpcDocument.fromBytes(Buffer.concat([
      zstdCompressSync(''), wrongSize,
    ])), /content size/i);
    const real = IpcDocument.fromBytes(await readFile(new URL('../../ipc2581/tests/data/DM0002-IPC-2518.xml.zst', import.meta.url)));
    assert.ok(real.layers().length > 2);
    assert.ok(JSON.parse(text(real.export({ format: 'bom' })[0])).length > 0);
    real.free();

    // Boundary errors must be catchable Error objects and leave the instance
    // usable. These inputs must not reach a panicking index or OS stub.
    const invalid = [
      () => new IpcDocument('<broken>'),
      () => new IpcDocument(xml, { validte: true }),
      () => IpcDocument.fromBytes(Uint8Array.of(255, 254)),
      () => IpcDocument.fromBytes(Uint8Array.of(0x28, 0xb5, 0x2f, 0xfd)),
      () => IpcDocument.fromBytes(Buffer.concat([zstdCompressSync(xml), Buffer.from('garbage')])),
      () => document.export({ format: 'svg', layer: 'missing' }),
      () => document.export({ format: 'bom', side: 'top' }),
      () => document.export({ format: 'html', zip: true }),
      () => document.export({ format: 'png', layer: 'TOP', maxDimension: 0 }),
      () => document.export({ format: 'svg', layer: 'TOP', layoutTarget: 'typo' }),
      () => document.export({ format: 'svg', layer: 'TOP', includeProfile: false }),
      () => document.export({ format: 'unsupported' }),
      () => document.checkDfm({ pdk: 'missing' }),
      () => document.checkDfm({ pdk: { source: 'bad TOML' } }),
      () => document.checkDfm({ pdk: { source: ' '.repeat(1024 * 1024 + 1) } }),
      () => document.checkDfm({ generatedAt: 'yesterday' }),
      () => document.checkDfm({ generatedAT: generatedAt }),
    ];
    for (const invoke of invalid) assert.throws(invoke, error => error instanceof Error, invoke.toString());
    const hugeRepeat = xml.replace('<StepRef name="board"/>', '<StepRef name="panel"/>')
      .replace('</CadData>', '<Step name="panel" type="PALLET"><StepRepeat stepRef="board" x="0" y="0" nx="4294967295" ny="1"/></Step></CadData>');
    const oversized = new IpcDocument(hugeRepeat);
    try {
      assert.throws(() => oversized.export({ format: 'gerber' }), /limit|exceed|too many/i);
    } finally { oversized.free(); }
    assert.equal(document.info().revision, 'C');

    const fromWorker = await new Promise((resolveResult, reject) => {
      const worker = new Worker(new URL(import.meta.url), { workerData: { packageDir } });
      worker.once('message', resolveResult);
      worker.once('error', reject);
      worker.once('exit', code => { if (code !== 0) reject(new Error(`worker exited ${code}`)); });
    });
    assert.equal(fromWorker.verdict, report.verdict);
    assert.deepEqual(fromWorker.rules, report.rules);
    console.log('IPC WASM: import, all exports, DFM, malformed input, and worker checks passed');
  } finally {
    document.free();
  }
}
