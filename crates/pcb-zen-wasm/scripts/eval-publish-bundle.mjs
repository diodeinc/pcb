#!/usr/bin/env node

import { spawnSync } from 'node:child_process'
import { pathToFileURL } from 'node:url'
import { readFileSync, readdirSync, statSync } from 'node:fs'
import path from 'node:path'

const repoRoot = path.resolve(import.meta.dirname, '../../..')

const defaults = {
  wasmBundle: path.join(repoRoot, 'target/wasm-bundle'),
  pcbc: path.join(repoRoot, 'target/debug/pcbc'),
  pcbcArgs: [],
  inputs: '{}',
  mainFile: '',
  stdlib: '',
}

const excludedArtifacts = [
  'drc',
  'bom',
  'gerbers',
  'cpl',
  'assembly',
  'odb',
  'ipc2581',
  'step',
  'vrml',
]

function usage() {
  console.log(`Usage:
  node crates/pcb-zen-wasm/scripts/eval-publish-bundle.mjs --bundle <release.zip> [options]
  node crates/pcb-zen-wasm/scripts/eval-publish-bundle.mjs --publish <board.zen> [options]

Options:
  --wasm-bundle <dir>   wasm-pack output dir (default: target/wasm-bundle)
  --stdlib <tar.zst>    Stdlib artifact matching the evaluator/toolchain
  --bundle <zip>        Existing pcb publish release zip to evaluate
  --publish <board.zen> Run pcbc publish for a board, then evaluate newest release zip
  --pcbc <path>         Publish command for --publish (default: target/debug/pcbc)
  --pcbc-arg <arg>      Extra argument before publish; repeatable
  --build-wasm          Run ./bin/build-wasm-bundle.sh before evaluating
  --build-pcbc          Run cargo build -p pcbc before --publish
  --main-file <path>    Main .zen path inside bundle source root (default: metadata/autodetect)
  --inputs <json>       JSON inputs object (default: {})
  --json                Print the full EvaluationResult instead of only schematic
  -h, --help            Show this help

Examples:
  node crates/pcb-zen-wasm/scripts/eval-publish-bundle.mjs \
    --wasm-bundle target/wasm-bundle \
    --stdlib /path/to/stdlib.tar.zst \
    --bundle /path/to/Feign-44c930a.zip

  node crates/pcb-zen-wasm/scripts/eval-publish-bundle.mjs \
    --build-wasm --build-pcbc \
    --stdlib /path/to/stdlib.tar.zst \
    --publish /Users/akhilles/src/diodehub/demo/Feign/Feign.zen
`)
}

function parseArgs(argv) {
  const args = {
    ...defaults,
    bundle: undefined,
    publish: undefined,
    buildWasm: false,
    buildPcbc: false,
    json: false,
  }

  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i]
    switch (arg) {
      case '-h':
      case '--help':
        usage()
        process.exit(0)
      case '--build-wasm':
        args.buildWasm = true
        break
      case '--build-pcbc':
        args.buildPcbc = true
        break
      case '--json':
        args.json = true
        break
      case '--wasm-bundle':
        args.wasmBundle = requiredValue(argv, ++i, arg)
        break
      case '--stdlib':
        args.stdlib = requiredValue(argv, ++i, arg)
        break
      case '--bundle':
        args.bundle = requiredValue(argv, ++i, arg)
        break
      case '--publish':
        args.publish = requiredValue(argv, ++i, arg)
        break
      case '--pcbc':
        args.pcbc = requiredValue(argv, ++i, arg)
        break
      case '--pcbc-arg':
        args.pcbcArgs.push(requiredValue(argv, ++i, arg))
        break
      case '--main-file':
        args.mainFile = requiredValue(argv, ++i, arg)
        break
      case '--inputs':
        args.inputs = requiredValue(argv, ++i, arg)
        break
      default:
        throw new Error(`Unknown argument: ${arg}`)
    }
  }

  if ((args.bundle ? 1 : 0) + (args.publish ? 1 : 0) !== 1) {
    throw new Error('Specify exactly one of --bundle or --publish')
  }
  if (!args.stdlib) {
    throw new Error('Specify --stdlib <tar.zst>')
  }

  JSON.parse(args.inputs)
  return args
}

function requiredValue(argv, index, flag) {
  const value = argv[index]
  if (!value || value.startsWith('--')) {
    throw new Error(`${flag} requires a value`)
  }
  return value
}

function run(command, commandArgs, options = {}) {
  console.error(`$ ${[command, ...commandArgs].join(' ')}`)
  const result = spawnSync(command, commandArgs, {
    cwd: repoRoot,
    encoding: 'utf8',
    ...options,
  })
  if (result.stdout) {
    process.stderr.write(result.stdout)
  }
  if (result.stderr) {
    process.stderr.write(result.stderr)
  }
  if (result.error) {
    throw result.error
  }
  if (result.status !== 0) {
    throw new Error(`${command} exited with status ${result.status}`)
  }
}

function publishBundle(args) {
  if (args.buildPcbc) {
    run('cargo', ['build', '-p', 'pcbc'])
  }

  run(args.pcbc, [
    ...args.pcbcArgs,
    'publish',
    args.publish,
    ...excludedArtifacts.flatMap((artifact) => ['--exclude', artifact]),
  ])

  const after = newestReleaseZip(args.publish)
  if (!after) {
    throw new Error(`Could not find new release zip for ${args.publish}`)
  }
  return after
}

function newestReleaseZip(zenPath) {
  const workspaceRoot = findWorkspaceRoot(path.resolve(zenPath))
  if (!workspaceRoot) {
    return undefined
  }
  const releasesDir = path.join(workspaceRoot, '.pcb/releases')
  let entries
  try {
    entries = readdirSync(releasesDir)
  } catch {
    return undefined
  }

  return entries
    .filter((name) => name.endsWith('.zip'))
    .map((name) => path.join(releasesDir, name))
    .map((file) => ({ file, mtimeMs: statSync(file).mtimeMs }))
    .sort((a, b) => b.mtimeMs - a.mtimeMs)[0]?.file
}

function findWorkspaceRoot(start) {
  let dir = statSync(start).isDirectory() ? start : path.dirname(start)
  while (true) {
    try {
      statSync(path.join(dir, 'pcb.toml'))
      return dir
    } catch {}

    const parent = path.dirname(dir)
    if (parent === dir) {
      return undefined
    }
    dir = parent
  }
}

async function loadWasm(wasmBundle) {
  const jsPath = path.resolve(wasmBundle, 'pcb_zen_wasm.js')
  const wasmPath = path.resolve(wasmBundle, 'pcb_zen_wasm_bg.wasm')

  for (const file of [jsPath, wasmPath]) {
    try {
      statSync(file)
    } catch {
      throw new Error(`Missing ${file}. Run with --build-wasm or pass --wasm-bundle <dir>.`)
    }
  }

  const mod = await import(`${pathToFileURL(jsPath).href}?t=${Date.now()}`)
  await mod.default({ module_or_path: readFileSync(wasmPath) })
  return mod
}

try {
  const args = parseArgs(process.argv.slice(2))

  if (args.buildWasm) {
    run('./bin/build-wasm-bundle.sh', [])
  }

  const bundlePath = args.bundle ? path.resolve(args.bundle) : publishBundle(args)
  const wasm = await loadWasm(args.wasmBundle)
  const bundleBytes = readFileSync(bundlePath)
  const stdlibBytes = readFileSync(path.resolve(args.stdlib))
  const result = wasm.evaluate(bundleBytes, stdlibBytes, args.mainFile, args.inputs)

  if (!result.success) {
    console.error(JSON.stringify(result.diagnostics, null, 2))
    process.exitCode = 1
  }

  console.log(JSON.stringify(args.json ? result : result.schematic, null, 2))
} catch (error) {
  console.error(error instanceof Error ? error.message : error)
  process.exit(1)
}
