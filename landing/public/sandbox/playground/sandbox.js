// The sandbox playground at /sandbox/playground/: edit JavaScript and run it
// in a sandbox made by a ZIPP release's loader, zipp.mjs, with a switch for
// each permission and limit.
//
// Plain static files with no build step, like /playground/: the loader has to
// be served as a file of its own, because each sandbox starts its Worker from
// the loader's own URL. scripts/sync-sandbox.mjs copies the loader and the
// JavaScript engine module into /sandbox/ and writes /sandbox/manifest.json,
// which names the module. Until a release with those files is published there
// is no manifest, and the page says so instead of failing.

const ASSETS = '/sandbox/'
const MAX_STEPS = 2_000_000_000 // the engine's maxInstructionBudgetSteps
const DEFAULT_STEPS = 50_000_000
const DEFAULT_TIMEOUT_MS = 5000
const MIN_TIMEOUT_MS = 100
const MAX_TIMEOUT_MS = 60_000
const ORIGIN = location.origin

const $ = id => document.getElementById(id)

// The host functions the "Host functions" switch grants. They run here, in
// the page; the program reaches them only through host.call.
const HOST_FUNCTIONS = {
  'page.time': () => new Date().toISOString(),
  'dice.roll': sides => {
    const n = Math.max(2, Math.min(1000, Math.trunc(Number(sides)) || 6))
    return 1 + (crypto.getRandomValues(new Uint32Array(1))[0] % n)
  },
}
const HOST_SOURCE = `{
    "page.time": () => new Date().toISOString(),
    "dice.roll": sides => 1 + Math.floor(Math.random() * sides),
  }`

const EXAMPLES = [
  {
    id: 'hello',
    title: 'What a program can reach',
    hint: 'The default: it computes and prints, and everything else starts switched off. Switch Storage or Files on and run it again.',
    code: `// A fresh ZIPP sandbox runs this in its own Worker. It can compute and
// print; everything outside the program starts switched off.
const primes = [];
for (let n = 2; primes.length < 10; n++) {
  if (primes.every(p => n % p !== 0)) primes.push(n);
}
console.log("first ten primes:", primes.join(" "));
console.log("document:", typeof document, "| process:", typeof process,
  "| fetch:", typeof fetch);

const probe = (what, read) => {
  try { console.log(what + ": " + read()); }
  catch (error) { console.log(what + ": refused (" + error.message + ")"); }
};
probe("localStorage theme", () => localStorage.getItem("theme"));
probe("notes.txt", () => host.callSync("fs.read", "notes.txt"));
`,
  },
  {
    id: 'storage',
    title: 'localStorage',
    hint: 'Run it with Storage off, then on. Each run is a new sandbox, so its storage starts from the data the page seeds it with.',
    code: `// localStorage exists only when the page grants storage, and then it is
// private to this sandbox and kept in memory.
try {
  const visits = Number(localStorage.getItem("visits") || 0) + 1;
  localStorage.setItem("visits", String(visits));
  console.log("stored visits =", visits);
  console.log("theme from the page:", localStorage.getItem("theme"));
} catch (error) {
  console.log("localStorage refused:", error.message);
}
`,
  },
  {
    id: 'files',
    title: 'Files',
    hint: 'Switch Files on to read the map the page provides; switch Writable on as well to let the program write.',
    code: `// The page hands the sandbox a map of paths to text. Paths stay inside
// the map: there is no way out of it.
const call = (kind, ...args) => {
  try { return JSON.stringify(host.callSync(kind, ...args)); }
  catch (error) { return "refused (" + error.message + ")"; }
};
console.log("list:", call("fs.list"));
console.log("read notes.txt:", call("fs.read", "notes.txt"));
console.log("read ../secret:", call("fs.read", "../secret"));
console.log("write result.txt:", call("fs.write", "result.txt", "written by the sandbox"));
`,
  },
  {
    id: 'network',
    title: 'fetch',
    hint: 'Without Network there is no fetch at all. With it, fetch reaches only the allowed list, which starts with this site’s /sandbox/ folder.',
    code: () => `// fetch exists only when the page grants the network, and then it reaches
// only the URLs on the allowed list.
async function main() {
  if (typeof fetch !== "function") {
    console.log("no fetch: the network is not permitted");
    return;
  }
  for (const url of ["${ORIGIN}/sandbox/manifest.json", "https://example.com/"]) {
    try {
      const response = await fetch(url);
      const text = await response.text();
      console.log(url, "-> HTTP " + response.status + ", " + text.length + " characters");
    } catch (error) {
      console.log(url, "-> " + error.message);
    }
  }
}
main();
`,
  },
  {
    id: 'host',
    title: 'Host functions',
    hint: 'Switch Host functions on: the page answers page.time and dice.roll. A kind the page did not grant is always denied.',
    code: `// host.call asks the page to run one of the functions it granted; the
// reply arrives in a callback as { ok, value } or { ok: false, error }.
for (const [kind, args] of [["page.time", []], ["dice.roll", [6]], ["page.cookies", []]]) {
  host.call(kind, args, reply => {
    console.log(kind + ":", reply.ok ? JSON.stringify(reply.value) : "refused (" + reply.error.code + ": " + reply.error.message + ")");
  });
}
`,
  },
  {
    id: 'functions',
    title: 'Calling its functions',
    hint: 'After the program runs, the page calls greet("visitor") from the “Then call” fields. The program’s state lives on between calls.',
    call: { name: 'greet', args: '["visitor"]' },
    code: `// After run(), the page can call the program's top-level functions with
// sandbox.call(name, args); the program keeps its state between calls.
let greeted = 0;
function greet(name) {
  greeted++;
  console.log("greeting", name);
  return "Hello, " + name + "! (call " + greeted + ")";
}
console.log("defined greet()");
`,
  },
  {
    id: 'loop',
    title: 'A loop that never ends',
    hint: 'This example raises Steps to 2,000,000,000 and sets a 2-second time limit, so the time limit ends it. Set Steps back to 50,000,000 to see the step budget stop it first.',
    limits: { steps: MAX_STEPS, timeoutMs: 2000 },
    code: `// A loop that never ends. With steps raised to their maximum, the time
// limit stops it: the page terminates the sandbox's Worker. With the
// default 50,000,000 steps, the engine's step budget stops it first.
let spins = 0;
for (;;) {
  spins++;
}
`,
  },
]

const VERDICTS = {
  ok: 'Finished',
  guest: 'The program threw an exception',
  source: 'The program did not compile',
  resource: 'A limit of the engine stopped it',
  timeout: 'The time limit stopped it',
  host: 'A host request failed',
  denied: 'Refused',
  usage: 'Refused',
  closed: 'Stopped',
}
const EXPLAIN = {
  guest: 'It raised an error it did not catch. What it printed before that is above.',
  source: 'The engine could not parse the program, so nothing ran.',
  resource: 'The program reached one of the sandbox’s limits (its step budget, its heap or its output), and the engine stopped it. The sandbox is finished.',
  timeout: 'Only ending the Worker can stop a program that never yields; the next run gets a new sandbox.',
  host: 'A request the program made to the page could not be completed.',
  denied: 'The sandbox refused something the program asked for.',
  usage: 'The sandbox refused these settings or this use of it.',
  closed: 'You stopped the run; the sandbox’s Worker was terminated.',
}

let zipp = null
let ZippSandboxError = null
let current = null // the sandbox of the run in progress
let running = false

// --- Settings -----------------------------------------------------------------

function parseJsonObject(text, what, values) {
  let value
  try {
    value = JSON.parse(text.trim() || '{}')
  } catch (error) {
    throw new Error(`${what} is not valid JSON: ${error.message}`)
  }
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new Error(`${what} must be a JSON object`)
  for (const [key, item] of Object.entries(value)) {
    if (typeof item !== values) throw new Error(`${what}: "${key}" must be a ${values}`)
  }
  return value
}

function readInteger(id, what, min, max) {
  const raw = $(id).value.trim()
  const value = Number(raw)
  if (!raw || !Number.isInteger(value) || value < min || value > max) {
    throw new Error(`${what} must be a whole number from ${min.toLocaleString('en')} to ${max.toLocaleString('en')}`)
  }
  return value
}

// The createSandbox options the switches describe, exactly as they are passed.
function readOptions() {
  const options = {
    language: 'javascript',
    limits: {
      instructions: readInteger('limit-steps', 'Steps', 1, MAX_STEPS),
      timeoutMs: readInteger('limit-time', 'The time limit', MIN_TIMEOUT_MS, MAX_TIMEOUT_MS),
    },
  }
  const permissions = {}
  if ($('perm-storage').checked) {
    const initial = parseJsonObject($('storage-initial').value, 'The storage data', 'string')
    permissions.storage = Object.keys(initial).length ? { initial } : true
  }
  if ($('perm-files').checked) {
    const files = parseJsonObject($('files-map').value, 'The files', 'string')
    permissions.files = $('files-writable').checked || 'files' in files ? { files, writable: $('files-writable').checked } : files
  }
  if ($('perm-network').checked) {
    const allow = $('network-allow').value.split(/\r?\n/).map(line => line.trim()).filter(Boolean)
    if (!allow.length) throw new Error('The network list is empty: add a URL, or switch Network off')
    permissions.network = allow
  }
  if (Object.keys(permissions).length) options.permissions = permissions
  if ($('perm-host').checked) options.host = HOST_FUNCTIONS
  return options
}

function readCall() {
  const name = $('call-name').value.trim()
  if (!name) return null
  if (!/^[A-Za-z_$][\w$]*$/.test(name)) throw new Error(`"${name}" is not a function name`)
  let args = []
  const raw = $('call-args').value.trim()
  if (raw) {
    try {
      args = JSON.parse(raw)
    } catch (error) {
      throw new Error(`The call's arguments are not valid JSON: ${error.message}`)
    }
    if (!Array.isArray(args)) args = [args]
  }
  return { name, args }
}

// --- The code shown under "The same sandbox in your page" ---------------------

const IDENTIFIER = /^[A-Za-z_$][\w$]*$/
function literal(value, indent = '') {
  if (typeof value === 'number') return Number.isInteger(value) && Math.abs(value) >= 10000 ? value.toLocaleString('en').replace(/,/g, '_') : String(value)
  if (typeof value === 'string' || typeof value === 'boolean' || value === null) return JSON.stringify(value)
  if (Array.isArray(value)) return `[${value.map(item => literal(item, indent)).join(', ')}]`
  const inner = `${indent}  `
  const entries = Object.entries(value).map(([key, item]) => `${inner}${IDENTIFIER.test(key) ? key : JSON.stringify(key)}: ${literal(item, inner)},`)
  return entries.length ? `{\n${entries.join('\n')}\n${indent}}` : '{}'
}

function renderCode() {
  let options
  try {
    options = readOptions()
  } catch (error) {
    $('options-code').textContent = `// ${error.message}`
    return
  }
  const shown = { ...options }
  delete shown.host
  delete shown.language
  let body = literal(shown)
  if (options.host) body = body.replace(/\n}$/, `\n  host: ${HOST_SOURCE},\n}`)
  const version = zipp?.meta?.version ?? '<version>'
  let call = ''
  try {
    const c = readCall()
    if (c) call = `\nconst { value } = await sandbox.call(${JSON.stringify(c.name)}, ${JSON.stringify(c.args)});`
  } catch {
    // Shown in the output when it runs.
  }
  $('options-code').textContent = `import { loadZipp } from "./zipp.mjs";

const zipp = await loadZipp("./zipp-${version}-javascript.wasm");
const sandbox = await zipp.createSandbox(${body});
try {
  const { output } = await sandbox.run(source);${call}
} catch (error) {
  // A ZippSandboxError: error.category, error.message, error.output
} finally {
  await sandbox.close();
}`
}

// --- Examples -------------------------------------------------------------------

function exampleCode(example) {
  return typeof example.code === 'function' ? example.code() : example.code
}

function selectExample(id) {
  const example = EXAMPLES.find(e => e.id === id) ?? EXAMPLES[0]
  $('example').value = example.id
  $('source').value = exampleCode(example)
  $('example-hint').textContent = example.hint
  $('limit-steps').value = String(example.limits?.steps ?? DEFAULT_STEPS)
  $('limit-time').value = String(example.limits?.timeoutMs ?? DEFAULT_TIMEOUT_MS)
  $('call-name').value = example.call?.name ?? ''
  $('call-args').value = example.call?.args ?? ''
  renderCode()
}

// --- Output -----------------------------------------------------------------------

function setRunState(state, text) {
  const el = $('run-state')
  el.dataset.state = state
  el.textContent = text
}

function clearOutput() {
  $('output').textContent = ''
  $('verdict').hidden = true
  $('snapshot-box').hidden = true
}

function print(lines, kind) {
  const out = $('output')
  for (const line of lines) {
    const row = document.createElement('span')
    row.className = kind ? `line line-${kind}` : 'line'
    row.textContent = `${line}\n`
    out.append(row)
  }
}

function showVerdict(category, message) {
  const box = $('verdict')
  box.hidden = false
  box.dataset.category = category
  $('verdict-title').textContent = category === 'ok' ? message : `${VERDICTS[category] ?? 'Stopped'} (${category})`
  // The loader's own messages start "zipp: " and end without a full stop.
  const said = String(message).replace(/^zipp: /, '').trim()
  const sentence = /[.!?)]$/.test(said) ? said : `${said}.`
  $('verdict-text').textContent = category === 'ok' ? '' : `${sentence}${EXPLAIN[category] ? ` ${EXPLAIN[category]}` : ''}`
  $('verdict-text').hidden = category === 'ok'
}

async function showSnapshot(sandbox, options) {
  if (!options.permissions?.storage && !options.permissions?.files) return
  try {
    const snapshot = await sandbox.snapshot()
    const shown = {}
    if (options.permissions.storage) shown.storage = snapshot.storage
    if (options.permissions.files) shown.files = snapshot.files
    $('snapshot').textContent = JSON.stringify(shown, null, 2)
    $('snapshot-box').hidden = false
  } catch {
    // A finished sandbox has nothing to read back.
  }
}

const ms = value => `${Math.round(value).toLocaleString('en')} ms`

// --- Running ----------------------------------------------------------------------

function setRunning(on) {
  running = on
  $('run').disabled = on || !zipp
  $('stop').disabled = !on
  $('example').disabled = on
  $('reset').disabled = on
}

async function run() {
  if (!zipp || running) return
  let options
  let call
  try {
    options = readOptions()
    call = readCall()
  } catch (error) {
    clearOutput()
    setRunState('error', 'Check the settings')
    showVerdict('usage', error.message)
    return
  }
  const source = $('source').value
  clearOutput()
  setRunning(true)
  setRunState('running', 'Starting a sandbox…')
  const started = performance.now()
  let sandbox = null
  try {
    sandbox = await zipp.createSandbox(options)
    current = sandbox
    const ready = performance.now()
    setRunState('running', 'Running…')
    const { output } = await sandbox.run(source)
    print(output)
    if (call) {
      const result = await sandbox.call(call.name, call.args)
      print(result.output)
      print([`${call.name}(${call.args.map(arg => JSON.stringify(arg)).join(', ')}) returned ${result.value === undefined ? 'undefined' : JSON.stringify(result.value)}`], 'returned')
    }
    const finished = performance.now()
    if (!output.length && !call) print(['(the program printed nothing)'], 'note')
    setRunState('ok', `Finished in ${ms(finished - ready)}`)
    showVerdict('ok', `Finished in ${ms(finished - ready)}; the sandbox's Worker started in ${ms(ready - started)}.`)
    await showSnapshot(sandbox, options)
  } catch (error) {
    if (ZippSandboxError && error instanceof ZippSandboxError) {
      print(error.output ?? [])
      const category = error.category || 'host'
      setRunState(category === 'closed' ? 'stopped' : 'error', `${VERDICTS[category] ?? 'Stopped'} after ${ms(performance.now() - started)}`)
      showVerdict(category, error.message)
      if (!error.terminal && sandbox && !sandbox.closed) await showSnapshot(sandbox, options)
    } else {
      // createSandbox refuses an option it does not accept with a TypeError.
      setRunState('error', 'Refused')
      showVerdict('usage', String(error?.message ?? error))
    }
  } finally {
    current = null
    if (sandbox) await sandbox.close().catch(() => {})
    setRunning(false)
  }
}

async function stop() {
  if (current) await current.close()
}

// --- Loading the engine ---------------------------------------------------------------

function engineStatus(state, text) {
  const el = $('engine-status')
  el.dataset.state = state
  el.textContent = text
}

function validManifest(m) {
  return m && typeof m === 'object'
    && /^\d+\.\d+\.\d+$/.test(m.version ?? '')
    && m.file === `zipp-${m.version}-javascript.wasm`
    && /^[0-9a-f]{64}$/.test(m.sha256 ?? '')
}

async function readManifest() {
  try {
    const response = await fetch(`${ASSETS}manifest.json`, { cache: 'no-cache' })
    if (!response.ok) return null
    // A dev server may answer a missing file with an HTML page and a 200.
    const manifest = JSON.parse(await response.text())
    return validManifest(manifest) ? manifest : null
  } catch {
    return null
  }
}

async function boot() {
  const select = $('example')
  for (const example of EXAMPLES) select.append(new Option(example.title, example.id))
  $('network-allow').value = `${ORIGIN}/sandbox/`
  $('storage-initial').value = '{ "theme": "dark" }'
  $('files-map').value = '{ "notes.txt": "hello from the page" }'
  selectExample(EXAMPLES[0].id)

  select.addEventListener('change', () => selectExample(select.value))
  $('reset').addEventListener('click', () => selectExample(select.value))
  $('run').addEventListener('click', run)
  $('stop').addEventListener('click', stop)
  $('source').addEventListener('keydown', event => {
    if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
      event.preventDefault()
      run()
    }
  })
  for (const el of document.querySelectorAll('.settings-pane input, .settings-pane textarea, #call-name, #call-args')) {
    el.addEventListener('input', renderCode)
    el.addEventListener('change', renderCode)
  }

  const manifest = await readManifest()
  if (!manifest) {
    engineStatus('unavailable', 'The sandbox engine is not on this site yet.')
    $('unavailable').hidden = false
    setRunState('idle', 'Not available yet')
    return
  }
  engineStatus('loading', `Loading ZIPP ${manifest.version}…`)
  try {
    const started = performance.now()
    const loaderKey = (manifest.loader?.sha256 ?? manifest.sha256).slice(0, 16)
    const loader = await import(`${ASSETS}zipp.mjs?v=${loaderKey}`)
    ZippSandboxError = loader.ZippSandboxError
    zipp = await loader.loadZipp(`${ASSETS}${manifest.file}?v=${manifest.sha256.slice(0, 16)}`, { expectVersion: manifest.version })
    const commit = zipp.meta.commit ? ` at ${zipp.meta.commit.slice(0, 8)}` : ''
    engineStatus('ready', `ZIPP ${zipp.meta.version} (${zipp.meta.variant}${commit}), loader ${loader.ZIPP_LOADER_VERSION}, loaded in ${ms(performance.now() - started)}. Each run gets a new sandbox in its own Worker.`)
    setRunning(false)
    setRunState('idle', 'Ready')
    renderCode()
  } catch (error) {
    engineStatus('failed', `ZIPP ${manifest.version} could not be loaded here: ${error?.message ?? error}`)
    setRunState('idle', 'Not available')
  }
}

boot()
