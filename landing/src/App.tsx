import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react'

const GITHUB_URL = 'https://github.com/f2i-com/zipp.org'
const DOCS_URL = `${GITHUB_URL}/blob/main/DOC.md#embedding`
const BENCHMARK_URL = `${GITHUB_URL}/blob/main/bench/real13_21288c1_pgo_2026-08-30.json`
const HOSTILE_BENCHMARK_URL = `${GITHUB_URL}/blob/main/bench/hostile/head_clean_21288c1_pgo_2026-08-30.json`

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

type BenchmarkRow = {
  id: string
  name: string
  group: BenchmarkGroup
  times: Record<Engine, number>
  nodeRatio: number
}

const benchmarkRows: BenchmarkRow[] = [
  { id: 'async-promise-chain', name: 'Async / promises', group: 'headline', times: { node: 328.786, bun: 363.605, deno: 354.774, zipp: 403.520 }, nodeRatio: 1.231865623 },
  { id: 'class-prototype-hot', name: 'Class / prototype', group: 'headline', times: { node: 295.190, bun: 332.877, deno: 326.413, zipp: 224.388 }, nodeRatio: 0.763521730 },
  { id: 'json-large', name: 'JSON', group: 'headline', times: { node: 258.640, bun: 189.975, deno: 308.320, zipp: 268.906 }, nodeRatio: 1.050831813 },
  { id: 'map-set-heavy', name: 'Map / Set', group: 'headline', times: { node: 687.650, bun: 784.031, deno: 1155.782, zipp: 578.761 }, nodeRatio: 0.851300473 },
  { id: 'markdown-render', name: 'Markdown render', group: 'headline', times: { node: 269.243, bun: 207.020, deno: 310.900, zipp: 232.357 }, nodeRatio: 0.869559492 },
  { id: 'parse-large-js', name: 'Parse JavaScript', group: 'headline', times: { node: 270.157, bun: 227.320, deno: 286.377, zipp: 239.810 }, nodeRatio: 0.890355724 },
  { id: 'polymorphic-objects', name: 'Polymorphic objects', group: 'headline', times: { node: 325.768, bun: 329.732, deno: 334.570, zipp: 306.949 }, nodeRatio: 0.939821866 },
  { id: 'regex-log-scan', name: 'RegExp log scan', group: 'headline', times: { node: 464.885, bun: 555.160, deno: 458.206, zipp: 476.285 }, nodeRatio: 1.017445258 },
  { id: 'sparse-array', name: 'Sparse array', group: 'headline', times: { node: 79.779, bun: 101.962, deno: 125.291, zipp: 81.723 }, nodeRatio: 1.050387229 },
  { id: 'typedarray-math', name: 'TypedArray math', group: 'headline', times: { node: 198.452, bun: 909.489, deno: 167.531, zipp: 133.305 }, nodeRatio: 0.673612550 },
  { id: 'polymorphic-objects-v2', name: 'Polymorphic objects v2', group: 'diagnostic', times: { node: 80.647, bun: 87.478, deno: 128.792, zipp: 23.823 }, nodeRatio: 0.294970167 },
  { id: 'property-ic-shapes', name: 'Property IC shapes', group: 'diagnostic', times: { node: 259.169, bun: 158.105, deno: 311.346, zipp: 10.526 }, nodeRatio: 0.040193527 },
  { id: 'sparse-array-v2', name: 'Sparse array v2', group: 'diagnostic', times: { node: 168.055, bun: 365.806, deno: 182.312, zipp: 100.959 }, nodeRatio: 0.601139082 },
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

function App() {
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'error'>('idle')
  const [menuOpen, setMenuOpen] = useState(false)
  const [benchmarkFilter, setBenchmarkFilter] = useState<BenchmarkFilter>('all')
  const resetTimer = useRef<number | undefined>(undefined)

  const visibleBenchmarks = useMemo(
    () => benchmarkRows.filter((row) => benchmarkFilter === 'all' || row.group === benchmarkFilter),
    [benchmarkFilter],
  )

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
              <span>Canonical PGO · 30/30 exact outputs</span>
              <strong>0.768× Node across all 30 measured rows</strong>
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
              <ExternalLink className="button button-primary" href={GITHUB_URL}>Explore on GitHub</ExternalLink>
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

        <section className="proof-band" aria-label="Measured Zipp results">
          <div className="section-wrap proof-grid">
            <div className="proof-lead">
              <span className="metric-index">01</span>
              <strong>17 / 30</strong>
              <p>point-estimate wins vs Node</p>
            </div>
            <div>
              <span className="metric-index">02</span>
              <strong>0.768×</strong>
              <p>Node time · equal-row all 30</p>
            </div>
            <div>
              <span className="metric-index">03</span>
              <strong>7.9 ms</strong>
              <p>median process launch</p>
            </div>
            <div>
              <span className="metric-index">04</span>
              <strong>30 / 30</strong>
              <p>exact-output parity</p>
            </div>
          </div>
        </section>

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
              <p className="section-kicker">Measured performance</p>
              <h2>Fast where it counts. Honest where work remains.</h2>
            </div>
            <div className="benchmark-statement">
              <strong>17<span>/30</span></strong>
              <p>Node point wins · every gap visible</p>
            </div>
          </div>

          <div className="benchmark-summary">
            <article className="headline-result">
              <div>
                <span>Equal-row all-30 headline</span>
                <strong>0.7679×</strong>
                <p>Zipp / Node paired geomean · lower is better</p>
              </div>
              <div className="confidence-pill">95% CI&nbsp; 0.7642–0.7712</div>
            </article>

            <article className="ratio-card">
              <span>All-30 paired geomeans</span>
              <div className="ratio-row"><b>vs Node</b><span><i className="bar-geomean-node" /></span><strong>0.7679×</strong></div>
              <div className="ratio-row"><b>vs Bun</b><span><i className="bar-geomean-bun" /></span><strong>0.6154×</strong></div>
              <div className="ratio-row"><b>vs Deno</b><span><i className="bar-geomean-deno" /></span><strong>0.4918×</strong></div>
              <small>95% CIs: Node 0.7642–0.7712 · Bun 0.6125–0.6199 · Deno 0.4885–0.4958. Normal 13 + hostile 17; equal weight per row.</small>
            </article>

            <article className="ratio-card suite-card">
              <span>Suite geomeans vs Node</span>
              <div className="ratio-row"><b>Normal 13</b><span><i className="bar-suite-normal" /></span><strong>0.6419×</strong></div>
              <div className="ratio-row"><b>Hostile 17</b><span><i className="bar-suite-hostile" /></span><strong>0.8807×</strong></div>
              <small>95% CIs: normal 0.6378–0.6457 · hostile 0.8745–0.8863. Node point wins: 9/13 + 8/17.</small>
            </article>
          </div>

          <div className="scoreboard">
            <div className="scoreboard-toolbar">
              <div>
                <p>Canonical normal 13 · cold wall time <span>milliseconds · lower is better</span></p>
              </div>
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
            </div>

            <div className="benchmark-table-wrap">
              <table className="benchmark-table">
                <caption>Canonical cold wall-time medians for Zipp, Node, Bun, and Deno</caption>
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
                          <small>{row.group === 'headline' ? 'Headline' : 'Diagnostic'}</small>
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
            <p>
              Windows x86-64, high-performance power mode. Cold wall time includes process launch;
              15 paired repetitions with deterministically shuffled engine and benchmark order;
              10,000 paired-bootstrap samples; exact-byte outputs. Node 24.12.0, Bun 1.3.14,
              Deno 2.6.10, Zipp 0.0.1 at clean PGO source <code>21288c1</code>; binary SHA-256
              <code>c2ddb9e6…6a3cb5</code>. Median startup: Zipp 7.9 ms, Node 29.6 ms, Bun 44.9 ms,
              Deno 82.1 ms. The all-30 result gives equal weight to all normal and hostile rows;
              its bootstrap intervals are descriptive. Zipp has 17/30 Node point wins and 30/30
              exact outputs. Ratios above one remain point gaps even when an interval crosses one.
              Normal gaps: async-promise-chain 1.232×, json-large 1.051×, regex-log-scan 1.017×,
              sparse-array 1.050×. Hostile gaps: calls-closures 1.307×, shapes-stable 1.493×,
              shapes-megamorphic 1.484×, allocation-survival 1.772×, async-lived 1.082×,
              reactish-reconcile 1.646×, warm-router 1.674×, bytecode-vm 1.014×, and npm-nanoid
              1.0004×. These workloads are evidence, not a claim of universal runtime superiority.
            </p>
            <div className="methodology-links">
              <ExternalLink className="text-link" href={BENCHMARK_URL}>Normal capture</ExternalLink>
              <ExternalLink className="text-link" href={HOSTILE_BENCHMARK_URL}>Hostile capture</ExternalLink>
            </div>
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
