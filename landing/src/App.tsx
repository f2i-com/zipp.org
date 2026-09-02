import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react'

const GITHUB_URL = 'https://github.com/f2i-com/zipp.org'
const DOCS_URL = `${GITHUB_URL}/blob/main/DOC.md#embedding`
const BENCHMARK_URL = `${GITHUB_URL}/blob/main/bench/real13_c28781cf_pgo_2026-09-02.json`
const HOSTILE_BENCHMARK_URL = `${GITHUB_URL}/blob/main/bench/hostile/head_clean_c28781cf_pgo_2026-09-02.json`
const ROADMAP_URL = `${GITHUB_URL}/blob/main/PERF_ROADMAP.md`
const RELEASE_URL = `${GITHUB_URL}/releases/tag/v0.0.12`

const playgroundExample = `const orders = [
  { id: "A-104", total: 48 },
  { id: "B-208", total: 73 },
  { id: "C-512", total: 29 },
];

const summary = orders
  .filter((order) => order.total >= 40)
  .map((order) => order.id + ": $" + order.total)
  .join(" | ");

console.log("priority orders", summary);
console.log("total", orders.reduce((sum, order) => sum + order.total, 0));`

const PLAYGROUND_BOOT_TIMEOUT_MS = 15_000
const PLAYGROUND_RUN_TIMEOUT_MS = 2_500

const installCommands = `git clone https://github.com/f2i-com/zipp.org.git zipp
cd zipp
cargo build --release
./target/release/zipp js examples/hello.js`

const sandboxCode = `let mut script = compile_script(user_code)?;

// Build zipp-vm with features = ["instrument"]
script.set_limits(5_000_000, Some(abort));
script.set_heap_limit(32 * 1024 * 1024);
script.set_host_call(Box::new(allowed_calls));

script.run_init()?;
script.call_slot(on_event, &[event])?;`

type Engine = 'node' | 'bun' | 'deno' | 'zipp'
type BenchmarkGroup = 'headline' | 'diagnostic'
type BenchmarkFilter = 'all' | BenchmarkGroup
type Suite = 'normal' | 'hostile'

type BenchmarkRow = {
  id: string
  name: string
  /** Normal rows: headline or diagnostic. Hostile rows: the corpus category. */
  group: string
  times: Record<Engine, number>
  nodeRatio: number
}

// Canonical clean PGO capture at engine commit c28781cf (2026-09-02): cold wall
// time medians in milliseconds over 15 counterbalanced repetitions, exact
// output on every row. Zipp / Node is the paired median ratio; below 1 is a win.
const benchmarkRows: BenchmarkRow[] = [
  { id: 'async-promise-chain', name: 'Async / promises', group: 'headline', times: { node: 344.321, bun: 372.640, deno: 365.741, zipp: 369.366 }, nodeRatio: 1.074364005 },
  { id: 'class-prototype-hot', name: 'Class / prototype', group: 'headline', times: { node: 302.386, bun: 339.084, deno: 331.811, zipp: 230.240 }, nodeRatio: 0.761261483 },
  { id: 'json-large', name: 'JSON', group: 'headline', times: { node: 270.749, bun: 199.802, deno: 327.023, zipp: 277.751 }, nodeRatio: 1.022337801 },
  { id: 'map-set-heavy', name: 'Map / Set', group: 'headline', times: { node: 695.556, bun: 821.199, deno: 1246.528, zipp: 624.996 }, nodeRatio: 0.890409164 },
  { id: 'markdown-render', name: 'Markdown render', group: 'headline', times: { node: 271.002, bun: 213.136, deno: 317.642, zipp: 214.108 }, nodeRatio: 0.785775701 },
  { id: 'parse-large-js', name: 'Parse JavaScript', group: 'headline', times: { node: 275.531, bun: 231.822, deno: 294.052, zipp: 239.741 }, nodeRatio: 0.874717990 },
  { id: 'polymorphic-objects', name: 'Polymorphic objects', group: 'headline', times: { node: 335.470, bun: 337.432, deno: 343.557, zipp: 309.192 }, nodeRatio: 0.920789325 },
  { id: 'regex-log-scan', name: 'RegExp log scan', group: 'headline', times: { node: 477.108, bun: 573.414, deno: 461.284, zipp: 456.106 }, nodeRatio: 0.953465853 },
  { id: 'sparse-array', name: 'Sparse array', group: 'headline', times: { node: 81.134, bun: 101.361, deno: 129.746, zipp: 75.030 }, nodeRatio: 0.924017154 },
  { id: 'typedarray-math', name: 'TypedArray math', group: 'headline', times: { node: 204.063, bun: 935.527, deno: 171.668, zipp: 147.258 }, nodeRatio: 0.718713786 },
  { id: 'polymorphic-objects-v2', name: 'Polymorphic objects v2', group: 'diagnostic', times: { node: 85.234, bun: 88.551, deno: 135.266, zipp: 24.991 }, nodeRatio: 0.295598405 },
  { id: 'property-ic-shapes', name: 'Property IC shapes', group: 'diagnostic', times: { node: 265.963, bun: 159.224, deno: 316.274, zipp: 10.278 }, nodeRatio: 0.038544299 },
  { id: 'sparse-array-v2', name: 'Sparse array v2', group: 'diagnostic', times: { node: 172.310, bun: 375.362, deno: 188.775, zipp: 102.126 }, nodeRatio: 0.591381962 },
]

// The 17-case hostile corpus from the same capture: closures, mixed locals,
// shape churn, GC survival, async lifetimes, modules, a React-shaped kernel, a
// warm router, a bytecode VM and vendored NanoID.
const hostileRows: BenchmarkRow[] = [
  { id: 'calls-baseline', name: 'Calls baseline', group: 'scope', times: { node: 35.438, bun: 48.138, deno: 88.908, zipp: 16.832 }, nodeRatio: 0.479671118 },
  { id: 'calls-closures', name: 'Closure calls', group: 'scope', times: { node: 43.014, bun: 54.772, deno: 93.447, zipp: 45.877 }, nodeRatio: 1.099988047 },
  { id: 'shapes-stable', name: 'Stable shapes', group: 'objects', times: { node: 40.083, bun: 60.690, deno: 92.940, zipp: 48.101 }, nodeRatio: 1.199181037 },
  { id: 'shapes-megamorphic', name: 'Megamorphic shapes', group: 'objects', times: { node: 47.882, bun: 68.022, deno: 96.022, zipp: 58.374 }, nodeRatio: 1.226424684 },
  { id: 'types-stable', name: 'Stable types', group: 'types', times: { node: 36.829, bun: 49.687, deno: 90.369, zipp: 18.509 }, nodeRatio: 0.516750981 },
  { id: 'types-churn', name: 'Type churn', group: 'types', times: { node: 45.011, bun: 61.786, deno: 99.637, zipp: 33.581 }, nodeRatio: 0.747415271 },
  { id: 'branch-control', name: 'Branch control', group: 'errors', times: { node: 39.286, bun: 51.767, deno: 90.098, zipp: 31.900 }, nodeRatio: 0.797849099 },
  { id: 'throw-catch', name: 'Throw / catch', group: 'errors', times: { node: 314.566, bun: 93.826, deno: 106.533, zipp: 155.147 }, nodeRatio: 0.496536981 },
  { id: 'allocation-ephemeral', name: 'Ephemeral allocation', group: 'allocation', times: { node: 37.099, bun: 70.767, deno: 91.069, zipp: 12.882 }, nodeRatio: 0.345696654 },
  { id: 'allocation-survival', name: 'Allocation survival', group: 'allocation', times: { node: 59.055, bun: 75.793, deno: 113.320, zipp: 88.570 }, nodeRatio: 1.483715204 },
  { id: 'async-burst', name: 'Async burst', group: 'async', times: { node: 54.811, bun: 54.611, deno: 107.089, zipp: 33.274 }, nodeRatio: 0.607391801 },
  { id: 'async-lived', name: 'Long-lived async', group: 'async', times: { node: 41.538, bun: 67.951, deno: 93.018, zipp: 42.489 }, nodeRatio: 1.065282414 },
  { id: 'reactish-reconcile', name: 'React-shaped reconcile', group: 'applications', times: { node: 45.225, bun: 67.760, deno: 99.424, zipp: 69.868 }, nodeRatio: 1.573522475 },
  { id: 'warm-router', name: 'Warm router', group: 'server', times: { node: 46.045, bun: 70.003, deno: 101.505, zipp: 70.674 }, nodeRatio: 1.563177836 },
  { id: 'bytecode-vm', name: 'Bytecode VM', group: 'endurance', times: { node: 44.249, bun: 56.427, deno: 95.007, zipp: 43.612 }, nodeRatio: 0.994103840 },
  { id: 'module-hot-graph', name: 'Hot module graph', group: 'modules', times: { node: 41.952, bun: 51.150, deno: 94.788, zipp: 16.286 }, nodeRatio: 0.403273005 },
  { id: 'npm-nanoid', name: 'npm nanoid', group: 'npm', times: { node: 86.019, bun: 98.589, deno: 127.279, zipp: 82.525 }, nodeRatio: 0.966236392 },
]

const nodeWins = (rows: BenchmarkRow[]) => rows.filter((row) => row.nodeRatio < 1).length
const nodeGaps = (rows: BenchmarkRow[]) =>
  rows.filter((row) => row.nodeRatio >= 1).sort((a, b) => b.nodeRatio - a.nodeRatio)

const readingGuide = [
  {
    title: 'The ratio is Zipp divided by Node',
    copy: 'Each row runs Node, Bun, Deno and Zipp in a shuffled order, 15 times each, and pairs the medians. 0.72× means Zipp finished in 72% of Node’s time; anything above 1× is a gap we still owe.',
  },
  {
    title: 'Cold time, exact output',
    copy: 'Every number includes process launch, and a row only counts when all four engines print byte-identical output. Zipp’s 7.7 ms launch is real, but the ratios are about the work, not the start.',
  },
  {
    title: 'One number for the whole picture',
    copy: 'The all-30 figure gives every normal and hostile row equal weight and reports a descriptive bootstrap interval. It is a summary, not a proof of universal speed — the table is the evidence.',
  },
]

const releaseNotes = [
  ['v0.0.12', 'Array element stores run inline in compiled loops; the `x | 0` idiom fuses into its add. sparse-array 1.01× → 0.92×, closure calls 1.18× → 1.10×.'],
  ['v0.0.11', 'Booleans and receivers get their own registers, so tokenizer-shaped loops stay on the integer tier. parse-large-js 1.24× → 0.88×.'],
  ['v0.0.10', 'Hardened limits sized for applications: megabyte-scale strings and buffers, a 512 MB heap budget.'],
]

const useCases = [
  {
    number: '01',
    eyebrow: 'User-authored code',
    title: 'Sandbox-oriented scripts',
    copy: 'Run customer rules inside a VM built for bounded execution. Add instruction budgets, a host-driven abort flag, and an approximate heap ceiling when you enable instrumentation.',
    tags: ['Step budgets', 'Abort signal', 'Heap indicator'],
  },
  {
    number: '02',
    eyebrow: 'Extensibility',
    title: 'Plugin runtimes',
    copy: 'Keep one VM alive, discover stable global slots, and call plugin functions without recompiling. Scripts only reach the host capabilities you deliberately install.',
    tags: ['Persistent state', 'Slot calls', 'Host bridge'],
  },
  {
    number: '03',
    eyebrow: 'Product logic',
    title: 'Rules and workflows',
    copy: 'Move scoring, transforms, policy, and workflow steps out of release cycles. JavaScript stays familiar while your Rust host owns data, side effects, and lifecycle.',
    tags: ['Rules', 'Transforms', 'Automation'],
  },
  {
    number: '04',
    eyebrow: 'Everywhere else',
    title: 'CLI and browser hosts',
    copy: 'Use the fast-starting native binary for trusted jobs, or bring a persistent interpreter to browser hosts through the wasm-bindgen package.',
    tags: ['Native CLI', 'WASM', 'Browser events'],
  },
]

const controls = [
  {
    number: '01',
    title: 'Capabilities start closed',
    copy: 'The host-call bridge is inert until your embedder installs it. Embedded scripts get no ambient Node, Bun, Deno, filesystem, or network API.',
  },
  {
    number: '02',
    title: 'Meter and interrupt',
    copy: 'Optional instrumentation charges matching bytecode units, polls a host abort flag, and reports work used. On x86-64 it stays consistent across interpreter and JIT execution.',
  },
  {
    number: '03',
    title: 'Keep state, not recompiles',
    copy: 'Compile once, run initialization, exchange structured data, call stable function slots, and pump microtasks for the life of the host.',
  },
]

const pipeline = [
  ['01', 'Source', 'Modern JavaScript'],
  ['02', 'Front end', 'Own lexer + parser'],
  ['03', 'Bytecode', 'Register-based VM'],
  ['04', 'Hot loops', 'x86-64 OSR JIT'],
]

function ArrowIcon() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path d="M4 12 12 4M6 4h6v6" />
    </svg>
  )
}

function Brand() {
  const gradientId = `zipp-bolt-${useId().replace(/[^a-zA-Z0-9_-]/g, '')}`

  return (
    <span className="brand-lockup">
      <svg className="brand-symbol" viewBox="0 0 142 208" aria-hidden="true">
        <defs>
          <linearGradient id={gradientId} x1="0" y1="0" x2="1" y2="1">
            <stop offset="0" stopColor="#c4b5fd" />
            <stop offset="0.5" stopColor="#7c3aed" />
            <stop offset="1" stopColor="#22d3ee" />
          </linearGradient>
        </defs>
        <path d="M92 0 0 122h58l-25 86L142 69h-62z" fill={`url(#${gradientId})`} />
      </svg>
      <span>Zipp</span>
    </span>
  )
}

function ExternalLink({ className, href, children }: { className?: string; href: string; children: ReactNode }) {
  return (
    <a className={className} href={href} target="_blank" rel="noreferrer">
      {children}
      <ArrowIcon />
    </a>
  )
}

type PlaygroundStatus = 'idle' | 'loading' | 'running' | 'success' | 'error' | 'timeout'

type PlaygroundWorkerMessage =
  | { type: 'started'; runId: number }
  | { type: 'result'; runId: number; output: string[]; elapsedMs: number }
  | { type: 'error'; runId: number; message: string }

function Playground() {
  const [source, setSource] = useState(playgroundExample)
  const [output, setOutput] = useState('Run the sample to see console output from Zipp WASM.')
  const [status, setStatus] = useState<PlaygroundStatus>('idle')
  const [elapsedMs, setElapsedMs] = useState<number | null>(null)
  const workerRef = useRef<Worker | null>(null)
  const timerRef = useRef<number | undefined>(undefined)
  const runIdRef = useRef(0)
  // A Worker that has already loaded the module and executed some JavaScript, so
  // the WebAssembly functions a run needs are compiled before the click rather
  // than during it. It is handed to the next run and immediately replaced — a run
  // still gets a Worker of its own, so terminating one on a deadline still
  // discards everything that run touched.
  const spareRef = useRef<Worker | null>(null)
  const spareReadyRef = useRef(false)

  const packageBase = () => new URL(`${import.meta.env.BASE_URL}wasm/`, document.baseURI)

  // The glue and the .wasm are hash-matched halves of one artifact; a cache that
  // serves a new .wasm beside an older glue fails instantiation with "function
  // import requires a callable". Neither filename is fingerprinted (they are
  // copied verbatim out of public/), so the build id is what keeps the pair
  // together — it changes whenever either file does.
  const wasmUrls = () => {
    const base = packageBase()
    const v = `?v=${__ZIPP_WASM_BUILD__}`
    return {
      moduleUrl: new URL(`zipp_wasm.js${v}`, base).href,
      wasmUrl: new URL(`zipp_wasm_bg.wasm${v}`, base).href,
    }
  }

  const stopWorker = () => {
    window.clearTimeout(timerRef.current)
    timerRef.current = undefined
    workerRef.current?.terminate()
    workerRef.current = null
  }

  const discardSpare = () => {
    spareRef.current?.terminate()
    spareRef.current = null
    spareReadyRef.current = false
  }

  // V8 compiles this module's WebAssembly lazily, on first call of each function,
  // which measured ~40 ms against ~1.2 ms for the sample's actual work. Paying it
  // on an idle Worker ahead of time is the whole difference.
  const prewarm = () => {
    if (spareRef.current) return
    const worker = new Worker(new URL('./playground.worker.ts', import.meta.url), { type: 'module' })
    spareRef.current = worker
    spareReadyRef.current = false
    worker.onmessage = (event: MessageEvent<{ type?: string }>) => {
      if (event.data?.type === 'warmed') spareReadyRef.current = true
      // A warm-up that fails is not an error the reader should see: the run path
      // loads the module itself and will surface anything real.
      else if (event.data?.type === 'warm-failed') discardSpare()
    }
    worker.onerror = () => discardSpare()
    worker.postMessage({ type: 'warm', ...wasmUrls() })
  }

  // Warm on intent rather than on mount, so a visitor who never touches the
  // playground is not made to download and compile 5.7 MB to scroll past it.
  useEffect(() => {
    const idle = window.requestIdleCallback?.bind(window)
    const handle = idle ? idle(() => prewarm(), { timeout: 4000 }) : undefined
    return () => {
      if (handle !== undefined) window.cancelIdleCallback?.(handle)
    }
  }, [])

  useEffect(() => () => {
    stopWorker()
    discardSpare()
  }, [])

  const armTimeout = (runId: number, delay: number, phase: 'boot' | 'run') => {
    window.clearTimeout(timerRef.current)
    timerRef.current = window.setTimeout(() => {
      if (runId !== runIdRef.current) return
      stopWorker()
      setStatus(phase === 'boot' ? 'error' : 'timeout')
      setElapsedMs(null)
      setOutput(phase === 'boot'
        ? 'Zipp WASM did not finish loading. Check the connection and try again.'
        : `Execution stopped after ${(PLAYGROUND_RUN_TIMEOUT_MS / 1000).toFixed(1)} seconds. The Worker was discarded.`)
    }, delay)
  }

  const runSource = () => {
    stopWorker()
    const runId = ++runIdRef.current

    // Take the pre-warmed Worker if there is one, then start warming its
    // replacement straight away so a second Run is as quick as the first.
    const warmed = spareReadyRef.current
    const worker = spareRef.current ?? new Worker(new URL('./playground.worker.ts', import.meta.url), { type: 'module' })
    spareRef.current = null
    spareReadyRef.current = false

    workerRef.current = worker
    setStatus('loading')
    setElapsedMs(null)
    setOutput(warmed ? 'Running in an isolated Worker…' : 'Loading the browser-safe Zipp runtime…')
    armTimeout(runId, warmed ? PLAYGROUND_RUN_TIMEOUT_MS : PLAYGROUND_BOOT_TIMEOUT_MS, warmed ? 'run' : 'boot')

    worker.onmessage = (event: MessageEvent<PlaygroundWorkerMessage>) => {
      const message = event.data
      if (message.runId !== runIdRef.current) return

      if (message.type === 'started') {
        setStatus('running')
        setOutput('Running in an isolated Worker…')
        armTimeout(runId, PLAYGROUND_RUN_TIMEOUT_MS, 'run')
        return
      }

      stopWorker()
      if (message.type === 'result') {
        setStatus('success')
        setElapsedMs(message.elapsedMs)
        setOutput(message.output.length > 0 ? message.output.join('\n') : '(script completed with no console output)')
      } else {
        setStatus('error')
        setElapsedMs(null)
        setOutput(message.message)
      }
    }

    worker.onerror = (event) => {
      if (runId !== runIdRef.current) return
      stopWorker()
      setStatus('error')
      setElapsedMs(null)
      setOutput(event.message || 'The Zipp Worker could not start.')
    }

    worker.postMessage({ type: 'run', runId, source, ...wasmUrls() })

    prewarm()
  }

  const resetSource = () => {
    ++runIdRef.current
    stopWorker()
    setSource(playgroundExample)
    setStatus('idle')
    setElapsedMs(null)
    setOutput('Run the sample to see console output from Zipp WASM.')
  }

  const statusLabel = {
    idle: 'ready',
    loading: 'loading WASM',
    running: 'running',
    success: elapsedMs === null ? 'complete' : `complete · ${elapsedMs.toFixed(1)} ms`,
    error: 'error',
    timeout: 'stopped',
  }[status]

  return (
    <section className="playground-section section-wrap" id="playground">
      <div className="playground-heading">
        <div>
          <p className="section-kicker">Live browser runtime</p>
          <h2>Try JavaScript in Zipp.</h2>
        </div>
        <p>
          This editor runs the interpreter-only WASM build in a disposable Worker. It has no
          ambient network, filesystem, Node, or browser authority, and a hung run is terminated
          from the responsive page outside the guest runtime.
        </p>
      </div>

      <div className="playground-shell">
        <div className="playground-pane playground-editor-pane">
          <div className="playground-toolbar">
            <div>
              <span className="terminal-dots" aria-hidden="true"><i /><i /><i /></span>
              <span>playground.js</span>
            </div>
            <span className={`playground-status status-${status}`}><i />{statusLabel}</span>
          </div>
          <label className="sr-only" htmlFor="playground-source">JavaScript source</label>
          <textarea
            id="playground-source"
            value={source}
            spellCheck={false}
            onChange={(event) => setSource(event.target.value)}
            onKeyDown={(event) => {
              if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') {
                event.preventDefault()
                runSource()
              }
            }}
          />
          <div className="playground-actions">
            <button className="button playground-run" type="button" onClick={runSource} disabled={status === 'loading' || status === 'running'}>
              {status === 'loading' ? 'Loading…' : status === 'running' ? 'Running…' : 'Run with Zipp'}
              <span aria-hidden="true">Ctrl/⌘ + Enter</span>
            </button>
            <button className="playground-reset" type="button" onClick={resetSource}>Reset sample</button>
          </div>
        </div>

        <div className="playground-pane playground-output-pane">
          <div className="playground-toolbar">
            <div><span className="output-mark" aria-hidden="true">›_</span><span>Console output</span></div>
            <span>WASM · safe-sandbox</span>
          </div>
          <pre aria-live="polite" aria-label="Zipp console output"><code>{output}</code></pre>
          <div className="playground-boundary">
            <span><i />50m instruction lifetime cap</span>
            <span><i />128 MiB VM heap ceiling</span>
            <span><i />2.5 s host deadline</span>
          </div>
        </div>
      </div>
    </section>
  )
}

function App() {
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'error'>('idle')
  const [menuOpen, setMenuOpen] = useState(false)
  const [benchmarkFilter, setBenchmarkFilter] = useState<BenchmarkFilter>('all')
  const [suite, setSuite] = useState<Suite>('normal')
  const resetTimer = useRef<number | undefined>(undefined)

  const visibleBenchmarks = useMemo(
    () =>
      suite === 'hostile'
        ? hostileRows
        : benchmarkRows.filter((row) => benchmarkFilter === 'all' || row.group === benchmarkFilter),
    [benchmarkFilter, suite],
  )
  const gaps = useMemo(() => [...nodeGaps(benchmarkRows), ...nodeGaps(hostileRows)], [])

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setMenuOpen(false)
    }

    window.addEventListener('keydown', handleKeyDown)
    return () => {
      window.removeEventListener('keydown', handleKeyDown)
      window.clearTimeout(resetTimer.current)
    }
  }, [])

  const copyInstall = async () => {
    try {
      await navigator.clipboard.writeText(installCommands)
      setCopyState('copied')
    } catch {
      setCopyState('error')
    }
    window.clearTimeout(resetTimer.current)
    resetTimer.current = window.setTimeout(() => setCopyState('idle'), 2200)
  }

  const closeMenu = () => setMenuOpen(false)

  return (
    <div className="site-shell">
      <a className="skip-link" href="#main-content">Skip to content</a>

      <header className="site-header">
        <a className="brand" href="#top" aria-label="Zipp home" onClick={closeMenu}>
          <Brand />
        </a>

        <button
          className="menu-button"
          type="button"
          aria-label="Toggle navigation"
          aria-expanded={menuOpen}
          aria-controls="primary-navigation"
          onClick={() => setMenuOpen((open) => !open)}
        >
          <span />
          <span />
        </button>

        <nav className={`nav-links ${menuOpen ? 'nav-open' : ''}`} id="primary-navigation" aria-label="Primary navigation">
          <a href="#playground" onClick={closeMenu}>Playground</a>
          <a href="#use-cases" onClick={closeMenu}>Use cases</a>
          <a href="#controls" onClick={closeMenu}>Controls</a>
          <a href="#benchmarks" onClick={closeMenu}>Benchmarks</a>
          <a href="#architecture" onClick={closeMenu}>Engine</a>
        </nav>

        <ExternalLink className="header-cta" href={GITHUB_URL}>GitHub</ExternalLink>
      </header>

      <main id="main-content">
        <section className="hero section-wrap" id="top">
          <div className="hero-copy">
            <a className="result-pill" href="#benchmarks">
              <span>Native CLI · canonical PGO · 30/30 exact outputs</span>
              <strong>0.729× Node across all 30 measured rows · faster on 21</strong>
              <span aria-hidden="true">↓</span>
            </a>

            <h1>
              Fast JavaScript for the code <em>your users bring.</em>
            </h1>

            <p className="hero-intro">
              Zipp is a Rust-native, embeddable JavaScript engine for user-authored rules,
              plugin logic, workflow steps, and interactive scripts. Keep a VM alive,
              expose only the host capabilities you choose, and put hard limits around hostile jobs.
            </p>

            <div className="hero-actions">
              <a className="button button-primary" href="#playground">Try Zipp in browser <span aria-hidden="true">↓</span></a>
              <ExternalLink className="button button-secondary" href={GITHUB_URL}>Explore on GitHub</ExternalLink>
              <a className="button button-secondary" href="#benchmarks">See the numbers <span aria-hidden="true">↓</span></a>
            </div>

            <div className="hero-trust" aria-label="Zipp highlights">
              <span><i />99.997% test262</span>
              <span><i />Native + WASM</span>
              <span><i />Open source</span>
            </div>
          </div>

          <aside className="sandbox-card" aria-label="Illustrative capability-controlled Zipp session">
            <div className="sandbox-card-header">
              <span className="terminal-dots" aria-hidden="true"><i /><i /><i /></span>
              <span>execution / session-042</span>
              <span className="live-status"><i /> within budget</span>
            </div>

            <div className="boundary-map">
              <div className="boundary-node host-node">
                <span className="node-label">Trusted</span>
                <strong>Your product</strong>
                <small>Rust host</small>
              </div>
              <div className="bridge-line" aria-hidden="true">
                <span>capability bridge</span>
                <i />
              </div>
              <div className="boundary-node vm-node">
                <span className="node-label">Guest</span>
                <strong>Zipp VM</strong>
                <small>user-script.js</small>
              </div>
            </div>

            <div className="capability-panel">
              <div className="panel-label">
                <span>Exposed capabilities</span>
                <span>2 allowed</span>
              </div>
              <div className="capability-chips">
                <span><i />orders.read</span>
                <span><i />email.queue</span>
                <span className="capability-denied">filesystem ×</span>
                <span className="capability-denied">network ×</span>
              </div>
            </div>

            <div className="budget-panel">
              <div className="budget-heading">
                <span>Instruction budget</span>
                <strong>1.84m <small>/ 5m</small></strong>
              </div>
              <div className="budget-track"><span /></div>
              <div className="budget-meta">
                <span>approx. heap indicator <b>32 MB</b></span>
                <span>abort <b>armed</b></span>
              </div>
            </div>

            <div className="sandbox-log" aria-label="Illustrative session log">
              <div><span>09:42:08.114</span><b>compile</b><em>bytecode ready</em></div>
              <div><span>09:42:08.117</span><b>host.call</b><em>orders.read</em></div>
              <div><span>09:42:08.118</span><b>return</b><em className="log-success">score: 0.94</em></div>
            </div>

            <p className="sandbox-footnote"><span>↳</span> Illustrative session · no host access unless you install it.</p>
          </aside>
        </section>

        <section className="proof-band" aria-label="Measured native Zipp results">
          <div className="section-wrap proof-grid">
            <div className="proof-lead">
              <span className="metric-index">01</span>
              <strong>21 / 30</strong>
              <p>native rows faster than Node</p>
            </div>
            <div>
              <span className="metric-index">02</span>
              <strong>0.729×</strong>
              <p>native Zipp / Node · equal-row all 30</p>
            </div>
            <div>
              <span className="metric-index">03</span>
              <strong>7.7 ms</strong>
              <p>median native process launch</p>
            </div>
            <div>
              <span className="metric-index">04</span>
              <strong>30 / 30</strong>
              <p>exact-output parity</p>
            </div>
          </div>
        </section>

        <Playground />

        <section className="use-case-section section-wrap" id="use-cases">
          <div className="section-heading split-heading">
            <div>
              <p className="section-kicker">Built for the edge of trust</p>
              <h2>Put JavaScript where your users already think.</h2>
            </div>
            <p>
              Give product teams a familiar language without handing scripts your whole
              application. Zipp keeps the engine small, the host boundary explicit, and the
              hot path fast.
            </p>
          </div>

          <div className="use-case-grid">
            {useCases.map((useCase) => (
              <article className="use-case-card" key={useCase.number}>
                <div className="card-number">{useCase.number}</div>
                <p className="card-eyebrow">{useCase.eyebrow}</p>
                <h3>{useCase.title}</h3>
                <p>{useCase.copy}</p>
                <div className="tag-row">
                  {useCase.tags.map((tag) => <span key={tag}>{tag}</span>)}
                </div>
              </article>
            ))}
          </div>
        </section>

        <section className="controls-section" id="controls">
          <div className="section-wrap controls-layout">
            <div className="controls-copy">
              <p className="section-kicker">Capability-controlled embedding</p>
              <h2>A script boundary you can reason about.</h2>
              <p className="controls-intro">
                Embedded code cannot wander into your host by accident. Install the calls it
                may make, define the data that crosses, and choose how much work one session gets.
              </p>

              <div className="control-list">
                {controls.map((control) => (
                  <article key={control.number}>
                    <span>{control.number}</span>
                    <div>
                      <h3>{control.title}</h3>
                      <p>{control.copy}</p>
                    </div>
                  </article>
                ))}
              </div>

              <div className="security-note">
                <span aria-hidden="true">!</span>
                <p><strong>One layer, honestly described.</strong> Use the separately resolved, no-JIT <code>zipp-sandbox</code> runner for hostile native code, then add OS/process isolation when the threat model requires it.</p>
              </div>
            </div>

            <div className="code-window" aria-label="Rust embedding example">
              <div className="code-window-header">
                <span className="terminal-dots" aria-hidden="true"><i /><i /><i /></span>
                <span>src / sandbox.rs</span>
                <span>Rust</span>
              </div>
              <pre><code>{sandboxCode}</code></pre>
              <div className="code-window-status">
                <span><i /> host surface</span><strong>explicit</strong>
                <span><i /> VM state</span><strong>persistent</strong>
                <span><i /> execution meter</span><strong>enabled</strong>
              </div>
              <ExternalLink className="text-link" href={DOCS_URL}>Read the embedding guide</ExternalLink>
            </div>
          </div>
        </section>

        <section className="benchmark-section section-wrap" id="benchmarks">
          <div className="benchmark-heading">
            <div>
              <p className="section-kicker">Measured native performance</p>
              <h2>Fast where it counts. Honest where work remains.</h2>
            </div>
            <div className="benchmark-statement">
              <strong>{nodeWins(benchmarkRows) + nodeWins(hostileRows)}<span>/30</span></strong>
              <p>rows faster than Node · every gap visible</p>
            </div>
          </div>

          <div className="benchmark-summary">
            <article className="headline-result">
              <div>
                <span>Native equal-row all-30 headline</span>
                <strong>0.7288×</strong>
                <p>Native Zipp / Node paired geomean · lower is better</p>
              </div>
              <div className="confidence-pill">95% CI&nbsp; 0.7163–0.7359</div>
            </article>

            <article className="ratio-card">
              <span>Native all-30 paired geomeans</span>
              <div className="ratio-row"><b>vs Node</b><span><i className="bar-geomean-node" /></span><strong>0.7288×</strong></div>
              <div className="ratio-row"><b>vs Bun</b><span><i className="bar-geomean-bun" /></span><strong>0.6022×</strong></div>
              <div className="ratio-row"><b>vs Deno</b><span><i className="bar-geomean-deno" /></span><strong>0.4692×</strong></div>
              <small>95% CIs: Node 0.7163–0.7359 · Bun 0.5972–0.6073 · Deno 0.4639–0.4739. Normal 13 + hostile 17; equal weight per row.</small>
            </article>

            <article className="ratio-card suite-card">
              <span>Suite geomeans vs Node</span>
              <div className="ratio-row"><b>Normal 13</b><span><i className="bar-suite-normal" /></span><strong>0.6202×</strong></div>
              <div className="ratio-row"><b>Hostile 17</b><span><i className="bar-suite-hostile" /></span><strong>0.8244×</strong></div>
              <small>95% CIs: normal 0.6156–0.6240 · hostile 0.7995–0.8378. Rows faster than Node: {nodeWins(benchmarkRows)}/13 + {nodeWins(hostileRows)}/17.</small>
            </article>
          </div>

          <div className="reading-guide" aria-label="How to read the benchmark numbers">
            {readingGuide.map((item, index) => (
              <article key={item.title}>
                <span>0{index + 1}</span>
                <h3>{item.title}</h3>
                <p>{item.copy}</p>
              </article>
            ))}
          </div>

          <div className="scoreboard">
            <div className="scoreboard-toolbar">
              <div>
                <p>
                  {suite === 'normal' ? 'Canonical native normal 13' : 'Canonical native hostile 17'} · cold wall time
                  <span>milliseconds · lower is better</span>
                </p>
              </div>
              <div className="scoreboard-controls">
                <div className="filter-tabs suite-tabs" role="group" aria-label="Choose a benchmark suite">
                  {([
                    ['normal', 'Normal suite'],
                    ['hostile', 'Hostile suite'],
                  ] as const).map(([value, label]) => (
                    <button
                      key={value}
                      type="button"
                      className={suite === value ? 'active' : ''}
                      aria-pressed={suite === value}
                      onClick={() => setSuite(value)}
                    >
                      {label}
                    </button>
                  ))}
                </div>
                {suite === 'normal' && (
                  <div className="filter-tabs" role="group" aria-label="Filter benchmark rows">
                    {([
                      ['all', 'All 13'],
                      ['headline', 'Headline 10'],
                      ['diagnostic', 'Diagnostics 3'],
                    ] as const).map(([value, label]) => (
                      <button
                        key={value}
                        type="button"
                        className={benchmarkFilter === value ? 'active' : ''}
                        aria-pressed={benchmarkFilter === value}
                        onClick={() => setBenchmarkFilter(value)}
                      >
                        {label}
                      </button>
                    ))}
                  </div>
                )}
              </div>
            </div>

            <div className="benchmark-table-wrap">
              <table className="benchmark-table">
                <caption>Canonical native cold wall-time medians for Zipp, Node, Bun, and Deno</caption>
                <thead>
                  <tr>
                    <th scope="col">Workload</th>
                    <th scope="col" className="zipp-column">Zipp <span>focus</span></th>
                    <th scope="col">Node</th>
                    <th scope="col">Bun</th>
                    <th scope="col">Deno</th>
                    <th scope="col">Zipp / Node</th>
                  </tr>
                </thead>
                <tbody>
                  {visibleBenchmarks.map((row) => {
                    return (
                      <tr key={row.id}>
                        <th scope="row">
                          <span>{row.name}</span>
                          <small>{suite === 'normal' ? (row.group === 'headline' ? 'Headline' : 'Diagnostic') : row.group}</small>
                        </th>
                        <td className="zipp-time" data-label="Zipp"><strong>{row.times.zipp.toFixed(3)}</strong><span className="sr-only"> milliseconds</span></td>
                        <td data-label="Node">{row.times.node.toFixed(3)}</td>
                        <td data-label="Bun">{row.times.bun.toFixed(3)}</td>
                        <td data-label="Deno">{row.times.deno.toFixed(3)}</td>
                        <td className="lead-cell" data-label="Zipp divided by Node">
                          <strong className={row.nodeRatio < 1 ? 'ratio-win' : 'ratio-gap'}>{row.nodeRatio.toFixed(3)}×</strong>
                          <span>{row.nodeRatio < 1 ? 'faster than Node' : 'slower than Node'}</span>
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          </div>

          <div className="methodology-note">
            <span className="methodology-mark">i</span>
            <div>
              <p>
                Native Windows x86-64 CLI, high-performance power mode. Cold wall time includes process launch;
                15 paired repetitions with deterministically shuffled engine and benchmark order;
                10,000 paired-bootstrap samples; exact-byte outputs. Node 24.12.0, Bun 1.3.14,
                Deno 2.6.10, Zipp 0.0.11 at clean PGO source <code>c28781cf</code>; binary SHA-256
                <code>0b3cfcd0…b7fab6</code>. Median startup: Zipp 7.7 ms, Node 30.6 ms, Bun 44.0 ms,
                Deno 84.4 ms. The all-30 result gives equal weight to all normal and hostile rows;
                its bootstrap intervals are descriptive. Ratios above one remain point gaps even when an
                interval crosses one. These native workloads are evidence, not a claim of universal
                runtime superiority; they are not browser-WASM results.
              </p>
              <p className="gap-list">
                <strong>Rows still behind Node ({gaps.length}):</strong>{' '}
                {gaps.map((row, index) => (
                  <span key={row.id}>
                    {row.id} {row.nodeRatio.toFixed(3)}×{index < gaps.length - 1 ? ', ' : '.'}
                  </span>
                ))}
              </p>
            </div>
            <div className="methodology-links">
              <ExternalLink className="text-link" href={BENCHMARK_URL}>Normal capture</ExternalLink>
              <ExternalLink className="text-link" href={HOSTILE_BENCHMARK_URL}>Hostile capture</ExternalLink>
              <ExternalLink className="text-link" href={ROADMAP_URL}>What is next</ExternalLink>
            </div>
          </div>

          <div className="release-strip" aria-label="Recent releases">
            <div className="release-strip-heading">
              <p className="section-kicker">Recent releases</p>
              <ExternalLink className="text-link" href={RELEASE_URL}>Latest release</ExternalLink>
            </div>
            <ol>
              {releaseNotes.map(([version, note]) => (
                <li key={version}>
                  <strong>{version}</strong>
                  <p>{note}</p>
                </li>
              ))}
            </ol>
          </div>
        </section>

        <section className="architecture-section" id="architecture">
          <div className="section-wrap">
            <div className="section-heading split-heading">
              <div>
                <p className="section-kicker">Clean-sheet core</p>
                <h2>Own the path from source to native code.</h2>
              </div>
              <p>
                Zipp’s lexer, parser, bytecode compiler, register VM, collector, inline caches,
                and native JIT live in this repository. No parser or runtime is hiding in the middle.
              </p>
            </div>

            <ol className="pipeline" aria-label="Zipp execution pipeline">
              {pipeline.map(([number, title, detail], index) => (
                <li key={number}>
                  <span>{number}</span>
                  <div><strong>{title}</strong><small>{detail}</small></div>
                  {index < pipeline.length - 1 && <i aria-hidden="true">→</i>}
                </li>
              ))}
            </ol>

            <div className="runtime-grid">
              <article>
                <span className="runtime-platform">Native / x86-64</span>
                <h3>OSR JIT when code gets hot.</h3>
                <p>Start in the interpreter, compile hot loops into native code, and keep optional execution metering consistent across both tiers.</p>
                <div><span>Persistent VM</span><span>Native JIT</span><span>Host controls</span></div>
              </article>
              <article>
                <span className="runtime-platform">Native / aarch64</span>
                <h3>A guarded baseline for integer hot paths.</h3>
                <p>Bounded call-free integer functions and numeric loops can run natively, with exact-instruction fallback whenever a guard declines.</p>
                <div><span>Baseline JIT</span><span>Register VM</span><span>Exact fallback</span></div>
              </article>
              <article>
                <span className="runtime-platform">Browser / wasm32</span>
                <h3>Persistent scripts in the browser.</h3>
                <p>The wasm-bindgen Engine keeps state, calls functions, delivers events, and crosses structured data through a browser host.</p>
                <div><span>Interpreter</span><span>Structured values</span><span>Event bridge</span></div>
              </article>
            </div>
          </div>
        </section>

        <section className="quickstart-section section-wrap" id="quickstart">
          <div className="quickstart-copy">
            <p className="section-kicker">Run it locally</p>
            <h2>Four lines from source to JavaScript.</h2>
            <p>Build the native CLI with stable Rust, run trusted scripts and modules, then move to the embedding API when your host needs a live VM.</p>
            <div className="quickstart-links">
              <ExternalLink className="text-link" href={DOCS_URL}>Embedding docs</ExternalLink>
              <ExternalLink className="text-link" href={GITHUB_URL}>Browse the source</ExternalLink>
            </div>
          </div>

          <div className="terminal-block">
            <div className="terminal-header">
              <span className="terminal-dots" aria-hidden="true"><i /><i /><i /></span>
              <span>Terminal</span>
              <button type="button" onClick={copyInstall}>{copyState === 'copied' ? 'Copied' : copyState === 'error' ? 'Copy failed' : 'Copy'}</button>
            </div>
            <pre><code>{installCommands}</code></pre>
            <div className="terminal-output"><span>↳</span> hello, world</div>
            <div className="sr-only" role="status" aria-live="polite">
              {copyState === 'copied' ? 'Install commands copied to clipboard.' : copyState === 'error' ? 'Could not copy install commands.' : ''}
            </div>
          </div>
        </section>

        <section className="closing-cta section-wrap">
          <div>
            <p className="section-kicker">Fast. Explicit. Yours to embed.</p>
            <h2>Give users JavaScript.<br />Keep control of the runtime.</h2>
          </div>
          <div className="closing-actions">
            <ExternalLink className="button button-dark" href={GITHUB_URL}>Start with Zipp</ExternalLink>
            <ExternalLink className="closing-doc-link" href={DOCS_URL}>Read the docs</ExternalLink>
          </div>
        </section>
      </main>

      <footer className="site-footer section-wrap">
        <a className="brand" href="#top" aria-label="Back to top"><Brand /></a>
        <p>A clean-sheet JavaScript engine in Rust.</p>
        <div>
          <ExternalLink href={DOCS_URL}>Docs</ExternalLink>
          <ExternalLink href={BENCHMARK_URL}>Benchmarks</ExternalLink>
          <ExternalLink href={GITHUB_URL}>GitHub</ExternalLink>
        </div>
      </footer>
    </div>
  )
}

export default App
