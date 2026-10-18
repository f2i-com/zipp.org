// What the home page's "Embed ZIPP on your site" section shows, and what the
// downloadable starter project is generated from (scripts/build-embed-starter.mjs).
// One module, plain JavaScript, so the page, the generator and the Node tests
// all read the same file names, programs and expected output: the test that
// runs the displayed example through public/sandbox/zipp.mjs checks the very
// strings the page prints.
//
// The release named here is the one the embed section links to; package.json's
// zippEngineRelease must match it (tests/embed.test.mjs).

export const EMBED_RELEASE = '0.0.27'
export const REPOSITORY_URL = 'https://github.com/f2i-com/zipp.org'
export const RELEASE_TAG_URL = `${REPOSITORY_URL}/releases/tag/v${EMBED_RELEASE}`
export const releaseAssetUrl = (name) => `${REPOSITORY_URL}/releases/download/v${EMBED_RELEASE}/${name}`

/** The embed kit, and the one file in it a page needs. */
export const KIT = {
  file: `zipp-${EMBED_RELEASE}-wasm-embed-kit.zip`,
  folder: `zipp-${EMBED_RELEASE}-wasm-embed-kit`,
  bytes: 212_266,
  loader: 'zipp.mjs',
}

/** The starter project served from public/ (no engine or loader inside). */
export const STARTER = {
  folder: `zipp-starter-${EMBED_RELEASE}`,
  path: `/embed/zipp-starter-${EMBED_RELEASE}.zip`,
}

// Release asset sizes in bytes, from the v0.0.27 release: each plain module and
// its Brotli-compressed `.wasm.br` copy.
const asset = (variant, bytes, brBytes) => ({ file: `zipp-${EMBED_RELEASE}-${variant}.wasm`, bytes, brBytes })

export const PYTHON_BASE = asset('javascript-python-base', 6_413_933, 1_590_974)
export const TORCH_PACKAGE = asset('torch-package', 2_162_843, 403_260)

export const ENGINES = [
  {
    id: 'javascript',
    label: 'JavaScript',
    summary: 'The full JavaScript engine, with Intl and Temporal.',
    language: 'javascript',
    ...asset('javascript', 4_681_257, 1_199_466),
  },
  {
    id: 'lite',
    label: 'JavaScript Lite',
    summary: 'JavaScript without Intl and Temporal, built for size.',
    language: 'javascript',
    ...asset('javascript-lite', 2_652_720, 724_650),
  },
  {
    id: 'javascript-python',
    label: 'JavaScript + Python',
    summary: 'Both languages, with torch built in.',
    language: 'python',
    ...asset('javascript-python', 8_452_811, 1_966_220),
  },
]

export const DEFAULT_ENGINE = ENGINES[0]
export const engineById = (id) => ENGINES.find((engine) => engine.id === id) ?? DEFAULT_ENGINE

/** "4.67 MB": decimal megabytes, as the release page states sizes. */
export const formatMegabytes = (bytes) => `${(bytes / 1_000_000).toFixed(2)} MB`

// The program each example hands the sandbox, and what it prints. A
// JavaScript engine runs JavaScript; the Python engine is shown running Python
// because that is what it adds (it runs JavaScript too).
const PROGRAMS = {
  javascript: {
    source: [
      '  console.log("6 * 7 =", 6 * 7);',
      '  console.log("fetch is", typeof fetch);',
    ],
    output: ['6 * 7 = 42', 'fetch is undefined'],
  },
  python: {
    source: [
      'print("6 * 7 =", 6 * 7)',
      'print("sum of 1..10 =", sum(range(1, 11)))',
    ],
    output: ['6 * 7 = 42', 'sum of 1..10 = 55'],
  },
}

/** The program text the default example runs, exactly as the template literal holds it. */
export const defaultProgram = (engine) => `\n${PROGRAMS[engine.language].source.join('\n')}\n`
/** The lines the default example prints. */
export const defaultOutput = (engine) => [...PROGRAMS[engine.language].output]

/** The sandbox options of the default example: nothing permitted. */
export const defaultOptions = (engine) => (engine.language === 'python' ? { language: 'python' } : undefined)

/** The default-deny example as the page shows it: load, create, run, close. */
export function defaultExample(engine) {
  const python = engine.language === 'python'
  return [
    'import { loadZipp } from "./zipp.mjs";',
    '',
    `const zipp = await loadZipp("./${engine.file}");`,
    '',
    python
      ? '// Nothing is permitted, and this sandbox runs Python.'
      : '// Nothing is permitted: no storage, files, network or host functions.',
    '// It runs in its own Worker, stopped after 30 s or 50 million steps.',
    python ? 'const sandbox = await zipp.createSandbox({ language: "python" });' : 'const sandbox = await zipp.createSandbox();',
    '',
    'const { output } = await sandbox.run(`',
    ...PROGRAMS[engine.language].source,
    '`);',
    'console.log(output.join("\\n"));',
    'await sandbox.close();',
    '',
  ].join('\n')
}

// One permission, granted: private storage for a JavaScript program, or files
// for a Python one, whose programs reach no storage, network or host functions
// in this release; their files are the modules they import.
const CAPABILITY = {
  javascript: {
    title: 'Grant one permission: storage',
    blurb: 'Without the permission, localStorage throws. With it, the program gets a private, in-memory localStorage, and the page can read it back.',
    options: { permissions: { storage: { initial: { visits: '1' } } } },
    optionsSource: [
      '  // One permission: a private, in-memory localStorage.',
      '  permissions: { storage: { initial: { visits: "1" } } },',
    ],
    source: [
      '  const visits = Number(localStorage.getItem("visits")) + 1;',
      '  localStorage.setItem("visits", String(visits));',
      '  console.log("visit", visits);',
    ],
    after: ['console.log(JSON.stringify((await sandbox.snapshot()).storage));'],
    output: ['visit 2', '{"visits":"2"}'],
    storage: { visits: '2' },
  },
  python: {
    title: 'Grant one permission: files to import',
    blurb: 'A Python program reaches no storage, network or host functions in this release. The files you grant are its importable modules.',
    options: { language: 'python', permissions: { files: { 'helpers.py': 'def double(x):\n    return 2 * x\n' } } },
    optionsSource: [
      '  language: "python",',
      '  // One permission: files, which a Python program imports as modules.',
      '  permissions: { files: { "helpers.py": "def double(x):\\n    return 2 * x\\n" } },',
    ],
    source: [
      'import helpers',
      'print("double(21) =", helpers.double(21))',
    ],
    after: [],
    output: ['double(21) = 42'],
    storage: null,
  },
}

export const capabilityTitle = (engine) => CAPABILITY[engine.language].title
export const capabilityBlurb = (engine) => CAPABILITY[engine.language].blurb
export const capabilityOptions = (engine) => structuredClone(CAPABILITY[engine.language].options)
export const capabilityProgram = (engine) => `\n${CAPABILITY[engine.language].source.join('\n')}\n`
/** What the capability example prints: the program's lines, then the page's own. */
export const capabilityOutput = (engine) => [...CAPABILITY[engine.language].output]
export const capabilityStorage = (engine) => CAPABILITY[engine.language].storage

/** The example with one capability granted, as the page shows it. */
export function capabilityExample(engine) {
  const spec = CAPABILITY[engine.language]
  return [
    'import { loadZipp } from "./zipp.mjs";',
    '',
    `const zipp = await loadZipp("./${engine.file}");`,
    'const sandbox = await zipp.createSandbox({',
    ...spec.optionsSource,
    '});',
    '',
    'const { output } = await sandbox.run(`',
    ...spec.source,
    '`);',
    'console.log(output.join("\\n"));',
    ...spec.after,
    'await sandbox.close();',
    '',
  ].join('\n')
}

/** The Content-Security-Policy directives a page that embeds ZIPP needs. */
export const EMBED_CSP = "script-src 'self' blob: 'wasm-unsafe-eval'; worker-src 'self' blob:"
