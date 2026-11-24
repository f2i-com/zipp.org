import { useMemo, useState, type ReactNode } from 'react'
import data from './benchmark-data.json'
import releaseData from './release-comparison.json'

/*
 * The measured-performance section. Every figure comes from
 * benchmark-data.json, which scripts/import-benchmarks.py writes from the
 * capture artifacts under bench/; the prose below states only what those
 * numbers say (counts and directions are computed, not typed). The one
 * exception is the release tab: release-comparison.json holds the latest
 * release notes' comparison of the published binaries, labelled as such.
 */

const GITHUB_URL = 'https://github.com/f2i-com/zipp.org'
const blob = (path: string) => `${GITHUB_URL}/blob/main/${path}`
const PERFORMANCE_URL = blob('docs/PERFORMANCE.md')
const ROADMAP_URL = blob('docs/PERF_ROADMAP.md')

type Mode = 'jit' | 'interp' | 'python' | 'v040' | 'release'
type Suite = 'normal' | 'hostile'
type Die = 'large' | 'small'
type Filter = 'all' | 'headline' | 'diagnostic'
type PyGroup = 'all' | 'micro' | 'macro'
type V040View = 'apps' | 'suites'
type Interval = number[] | null | undefined

// Verified v0.0.40 real-world application and extra-suite measurements from docs/releases/0.0.40.md
// (Windows x86-64, non-PGO fat-LTO v0.0.40 vs published PGO v0.0.39, Node.js v24.19.0 & CPython 3.13;
// byte-identical output across all runs).
const V040_APP_ROWS = [
  { id: 'linkedom', name: 'linkedom', detail: 'DOM parsing & querySelectorAll', ref: 'Node.js v24.19.0', refWarm: '1.31 ms', refWall: '336–337 ms', v039: '9.56 ms', v040Warm: '6.14–6.21 ms', v040Wall: '467–473 ms', speedup: '1.55× faster', vsRef: '4.69–4.74× warm · 1.38–1.41× wall' },
  { id: 'defuddle', name: 'defuddle', detail: 'Article extraction over HTML DOM', ref: 'Node.js v24.19.0', refWarm: '26.05–26.73 ms', refWall: '597–604 ms', v039: '160.32 ms', v040Warm: '139.76–146.77 ms', v040Wall: '1595–1650 ms', speedup: '1.09–1.15× faster', vsRef: '5.36–5.49× warm · 2.64–2.76× wall' },
  { id: 'webforai', name: 'webforai', detail: 'DOM-to-Markdown scoring pipeline', ref: 'Node.js v24.19.0', refWarm: '17.57–18.30 ms', refWall: '521–522 ms', v039: '107.57 ms', v040Warm: '102.18–107.16 ms', v040Wall: '1277–1351 ms', speedup: '1.00–1.05× faster', vsRef: '5.58–6.10× warm · 2.45–2.59× wall' },
  { id: 'html2text', name: 'html2text', detail: 'Unmodified Python HTML → Markdown', ref: 'CPython 3.13', refWarm: '5.60–5.67 ms', refWall: '198–206 ms', v039: '41.15 ms', v040Warm: '14.31–15.39 ms', v040Wall: '282–289 ms', speedup: '2.67–2.88× faster', vsRef: '2.55–2.72× warm · 1.37–1.46× wall' },
  { id: 'bs4_css', name: 'bs4_css', detail: 'BeautifulSoup4 + SoupSieve selectors', ref: 'CPython 3.13', refWarm: '5.74–5.89 ms', refWall: '282–300 ms', v039: '65.73 ms', v040Warm: '36.12–38.44 ms', v040Wall: '482–502 ms', speedup: '1.71–1.82× faster', vsRef: '6.13–6.70× warm · 1.68–1.71× wall' },
  { id: 'markdownify', name: 'markdownify', detail: 'BeautifulSoup4 HTML → Markdown', ref: 'CPython 3.13', refWarm: '7.87–8.10 ms', refWall: '227–239 ms', v039: '104.80 ms', v040Warm: '54.03–56.54 ms', v040Wall: '645–671 ms', speedup: '1.85–1.94× faster', vsRef: '6.67–7.19× warm · 2.81–2.84× wall' },
] as const

const V040_SUITE_ROWS = [
  { id: 'py-bench', name: 'Python tools/python_bench.py', detail: '44 programs vs CPython 3.13 (36 comparable)', prev: '0.79× geomean', curr: '0.62× geomean (~1.27× faster)', note: 'micro 0.60×, macro 0.75×, 26/36 wins over CPython; try_raise_catch 2.4× → 0.29×, instance_new 2.6× → 1.37×, sort_heapq 2.3× → 1.27×.' },
  { id: 'js-regex', name: 'JS bench/regex', detail: '16 regex patterns vs Node.js v24.19.0', prev: '2.44× geomean', curr: '1.90× geomean (~1.28× faster)', note: 'literal-utf16 4.0× → 0.82× (now beating Node), lookbehind 1.41× faster.' },
  { id: 'js-hostile', name: 'JS bench/hostile (4 slowest)', detail: 'Stress tests vs Node.js v24.19.0', prev: '4 lagged Node', curr: '3 of 4 now beat Node', note: 'reactish-reconcile 1.21× → 0.97×, shapes-megamorphic 1.04× → 0.85×, calls-closures 1.12× → 0.93×, allocation-survival 1.35× → 1.24×.' },
  { id: 'php-rust', name: 'PHP & Rust runtime O(N²) fixes', detail: 'array_pop, cursor loops, VecDeque/BTreeMap', prev: 'O(N²) / O(N² log N)', curr: 'O(1) / O(N) (8.6×–180× faster)', note: 'PHP array_pop ~65× faster (614 → 9.5 ms), PHP cursor loop ~180× faster (2266 → 12.5 ms), Rust collections 8.6× faster (481 → 56 ms).' },
] as const

const jit = data.jit
const interp = data.interp
const python = data.python
const release = data.release
// "zipp 0.0.28" -> "0.0.28": each capture names the engine it measured.
const versionOf = (stamp: string) => stamp.replace(/^zipp /, '')

const x =(value: number, digits = 3) => `${value.toFixed(digits)}×`
const ms = (value: number | undefined | null) =>
  value === undefined || value === null ? '—' : value >= 1000 ? value.toLocaleString('en-US', { maximumFractionDigits: 0 }) : value.toFixed(value >= 100 ? 0 : 1)
const range = (ci: Interval) => (ci && ci.length === 2 ? `${ci[0].toFixed(3)}–${ci[1].toFixed(3)}` : 'no interval')
const longDate = (iso: string) =>
  new Date(`${iso}T00:00:00Z`).toLocaleDateString('en-GB', { day: 'numeric', month: 'long', year: 'numeric', timeZone: 'UTC' })
const below = (rows: { ratio: number }[]) => rows.filter((row) => row.ratio < 1).length
const geomean = (values: number[]) => Math.exp(values.reduce((sum, value) => sum + Math.log(value), 0) / values.length)

// The release tab: the published binaries of the latest release, its
// predecessor and QuickJS, per bundled application and per L3 die.
const rel = releaseData
type Timing = { previous: number; current: number; warm: number; pgo: number; quickjs: number | null }
const relRows = (die: Die) => rel.bundles.map((bundle) => ({ ...bundle, t: bundle[die] as Timing }))
const relVsQuickjs = (die: Die, engine: 'current' | 'pgo') =>
  geomean(relRows(die).filter((row) => row.t.quickjs !== null).map((row) => row.t[engine] / row.t.quickjs!))
const relVsPrevious = geomean(relRows('large').map((row) => row.t.current / row.t.previous))
const relFaster = relRows('large').filter((row) => row.t.current < row.t.previous).length

/** The home page's proof band reads these: the JIT capture's own headline facts. */
export const benchmarkFacts = {
  nodeWins: jit.nodeWins,
  all30: jit.all30.node,
  startupMs: jit.startupMs.zipp,
  capturedAt: jit.capturedAt,
  version: jit.zipp.version.replace(/^zipp /, ''),
  engines: jit.engines,
}

const ENGINE_LABEL: Record<string, string> = {
  node: 'Node', bun: 'Bun', deno: 'Deno',
  'zipp-interp': 'ZIPP interpreter', quickjs: 'QuickJS', 'quickjs-ng': 'QuickJS-NG',
  'node-jitless': 'Node jitless', 'deno-jitless': 'Deno jitless', 'bun-nojit': 'Bun no-JIT',
}
const INTERP_COMPETITORS = ['quickjs', 'quickjs-ng', 'node-jitless', 'deno-jitless', 'bun-nojit'] as const

function Arrow() {
  return (
    <svg viewBox="0 0 16 16" aria-hidden="true">
      <path d="M4 12 12 4M6 4h6v6" />
    </svg>
  )
}

function Link({ href, children }: { href: string; children: ReactNode }) {
  return <a className="text-link" href={href} target="_blank" rel="noreferrer">{children}<Arrow /></a>
}

/** Bars scaled to the largest ratio shown, with 1× (parity) never off the end. */
function RatioRows({ rows }: { rows: { label: string; value: number }[] }) {
  const scale = Math.max(1, ...rows.map((row) => row.value))
  return (
    <>
      {rows.map((row) => (
        <div className="ratio-row" key={row.label}>
          <b>{row.label}</b>
          <span><i className={row.value > 1 ? 'ratio-bar-gap' : undefined} style={{ width: `${(row.value / scale) * 100}%` }} /></span>
          <strong>{x(row.value)}</strong>
        </div>
      ))}
    </>
  )
}

function Tabs<T extends string>({ label, value, options, onChange, className }: {
  label: string; value: T; options: readonly (readonly [T, string])[]; onChange: (value: T) => void; className?: string
}) {
  return (
    <div className={`filter-tabs${className ? ` ${className}` : ''}`} role="group" aria-label={label}>
      {options.map(([key, text]) => (
        <button key={key} type="button" className={value === key ? 'active' : ''} aria-pressed={value === key} onClick={() => onChange(key)}>
          {text}
        </button>
      ))}
    </div>
  )
}

function RowBars({ zipp, other }: { zipp: number; other: number }) {
  const max = Math.max(zipp, other)
  return (
    <div className="row-bars">
      <span className="row-bar row-bar-zipp" style={{ width: `${(zipp / max) * 100}%` }} />
      <span className="row-bar row-bar-node" style={{ width: `${(other / max) * 100}%` }} />
    </div>
  )
}

function RatioCell({ ratio, label, versus }: { ratio: number; label: string; versus: string }) {
  return (
    <td className="lead-cell" data-label={label}>
      <strong className={ratio < 1 ? 'ratio-win' : 'ratio-gap'}>{x(ratio)}</strong>
      <span>{ratio < 1 ? `faster than ${versus}` : `slower than ${versus}`}</span>
    </td>
  )
}

const READING: Record<Mode, { title: string; copy: string }[]> = {
  v040: [
    { title: 'Latest v0.0.40 measurements', copy: 'Captured on native Windows x86-64 comparing a non-PGO fat-LTO v0.0.40 build against published PGO v0.0.39, Node.js v24.19.0, Bun, Deno, and CPython 3.13. Release PGO binaries widen these gains further.' },
    { title: 'Official suites & real-world apps', copy: 'ZIPP v0.0.40 wins 29/30 canonical JS rows vs Node (0.583× geomean, 11.1 ms startup), jumps to 0.626× CPython across Python benchmarks (26/36 wins), and speeds up real-world Python and DOM apps 1.55×–2.88× over v0.0.39.' },
    { title: 'Honest warm-app comparisons', copy: 'Steady-state warm DOM & Python HTML apps still trail V8 TurboFan and CPython’s C-extension parsers on warm loops — both the v0.0.39 → v0.0.40 gains and the remaining warm/wall gaps against Node.js and CPython are shown side by side.' },
  ],
  jit: [
    { title: 'JIT against JIT', copy: 'Every engine runs as it ships: ZIPP with its native x86-64 JIT, Node and Deno with V8, Bun with JavaScriptCore. The ratio is ZIPP’s time divided by Node’s, so 0.72× means ZIPP needed 72% of Node’s time; above 1× is a gap still owed.' },
    { title: 'Cold time, exact output', copy: `Each row is a fresh process, ${jit.reps} counterbalanced repetitions per engine, and it only counts when all four print byte-identical output. Launch is included: ZIPP starts in ${jit.startupMs.zipp.toFixed(1)} ms against Node’s ${jit.startupMs.node.toFixed(1)} ms.` },
    { title: 'One number, then the table', copy: 'The all-30 figure weights every normal and hostile row equally. It summarises; the rows are the evidence, and the slower ones are listed under the table.' },
  ],
  interp: [
    { title: 'Interpreter against interpreter', copy: 'The same ZIPP build with ZIPP_NOJIT=1 and ZIPP_NO_RX_JIT=1 emits no native code: its JavaScript JIT and its regex JIT are both off. It runs against Bellard’s QuickJS and QuickJS-NG, which have no JIT, and against Node, Deno and Bun with their JITs switched off (V8’s Ignition, JavaScriptCore’s LLInt).' },
    { title: 'The ratio is ZIPP over the other', copy: `Lower is faster. Each row is a fresh process, ${interp.normal.reps} counterbalanced repetitions, every output checked against Node’s. The headline is against Bellard’s QuickJS, the interpreter ZIPP is most often compared with.` },
    { title: 'Why it matters', copy: 'The WebAssembly build, the safe-sandbox profile and arm64 hosts without the JIT all run this interpreter, so these rows are what most embedders get.' },
  ],
  python: [
    { title: 'ZIPP Python against CPython', copy: `Python 3 compiled to ZIPP’s register bytecode, run by the published ${versionOf(python.zipp.version)} binary, against CPython ${python.cpython}. Standard CPython is an interpreter, so ZIPP is shown both with its JIT and with ZIPP_NOJIT=1.` },
    { title: 'Work time, and the whole process', copy: 'Each program times its own workload in-process (the ratio in the table), and the harness records wall time too. Wall time adds process start and compile, where ZIPP’s lighter start counts in its favour.' },
    { title: 'Same answers', copy: `Every run’s output must equal CPython’s. ${python.zippOnlyPrograms} more programs (the torch ones) have no CPython counterpart and are left out of every figure here.` },
  ],
  release: [
    { title: `What ${rel.version} changed`, copy: `The published ${rel.version} and ${rel.previous} Windows binaries, as downloaded, on bundled applications: one script per library, a fixed workload, the bundle’s compile included. They are the release builds as published (profile-guided since 0.0.31, with the release’s features and its own training run), not the canonical profile-guided build the other tabs measure.` },
    { title: 'Two dies, one processor', copy: `The Ryzen 9 9950X3D has one die with ${rel.dies.large} and one with ${rel.dies.small}. Large programs feel the difference, so every figure is given for both: runs are pinned to one core of the chosen die.` },
    { title: 'A release comparison', copy: 'Measured for the release notes with scratch scripts on an otherwise idle machine, not one of the stamped, provenance-checked captures behind the other tabs. The notes carry the full method, the memory figures and the slower rows.' },
  ],
}

export function BenchmarkSection() {
  const [mode, setMode] = useState<Mode>('jit')
  const [v040View, setV040View] = useState<V040View>('apps')
  const [suite, setSuite] = useState<Suite>('normal')
  const [filter, setFilter] = useState<Filter>('all')
  const [pyGroup, setPyGroup] = useState<PyGroup>('all')
  const [die, setDie] = useState<Die>('large')

  const jitRows = suite === 'normal' ? jit.normal.rows.filter((row) => filter === 'all' || row.group === filter) : jit.hostile.rows
  const interpSuite = suite === 'normal' ? interp.normal : interp.hostile
  const pyRows = python.rows.filter((row) => pyGroup === 'all' || row.group === pyGroup)

  const jitGaps = useMemo(() => [...jit.normal.rows, ...jit.hostile.rows].filter((row) => row.ratio >= 1).sort((a, b) => b.ratio - a.ratio), [])
  const interpGaps = useMemo(() => [...interp.normal.rows, ...interp.hostile.rows]
    .filter((row) => (row.vs as Record<string, number | undefined>).quickjs !== undefined && (row.vs as Record<string, number>).quickjs >= 1)
    .sort((a, b) => (b.vs as Record<string, number>).quickjs - (a.vs as Record<string, number>).quickjs), [])
  const pyGaps = useMemo(() => python.rows.filter((row) => row.ratio >= 1).sort((a, b) => b.ratio - a.ratio), [])

  const normalWins = below(jit.normal.rows)
  const hostileWins = below(jit.hostile.rows)
  const qjs = interp.all30.quickjs
  const pyWorkJit = python.geomean.work.jit
  const pyWorkInterp = python.geomean.work.interp
  const pyWallJit = python.geomean.wall.jit
  const pyFaster = below(python.rows)
  const releaseRows = relRows(die)
  const releaseGaps = releaseRows.filter((row) => row.t.quickjs !== null && row.t.current >= row.t.quickjs)

  return (
    <section className="benchmark-section section-wrap" id="benchmarks">
      <div className="benchmark-heading">
        <div>
          <p className="section-kicker">Measured native performance</p>
          <h2>Fast where it counts. Honest where work remains.</h2>
        </div>
        <div className="benchmark-statement">
          <strong>{jit.nodeWins}<span>/30</span></strong>
          <p>rows faster than Node · every gap visible</p>
        </div>
      </div>

      {mode === 'v040' ? (
        <p className="benchmark-capture-note">
          Measured November 2026 on native Windows x86-64 · ZIPP 0.0.40 (non-PGO fat-LTO) vs published PGO ZIPP 0.0.39 · Node.js v24.19.0 · Bun · Deno · CPython 3.13 · byte-identical output across all runs. From{' '}
          <a href={blob('docs/releases/0.0.40.md')} target="_blank" rel="noreferrer">docs/releases/0.0.40.md</a>. Native CLI results, not the browser build.
        </p>
      ) : mode === 'release' ? (
        <p className="benchmark-capture-note">
          Measured {longDate(rel.measuredAt)} on native Windows x86-64 · the published ZIPP {rel.version} and {rel.previous} Windows binaries, as published
          {' '}· Bellard’s QuickJS {rel.quickjs}. A release comparison from the {rel.version} notes, not a stamped capture. Native CLI results, not the browser build.
        </p>
      ) : (
        <p className="benchmark-capture-note">
          Measured {longDate(jit.capturedAt)} on native Windows x86-64 · ZIPP {benchmarkFacts.version}, canonical PGO build (source <code>{jit.zipp.source}</code>)
          {' '}· Node {jit.engines.node} · Bun {jit.engines.bun} · Deno {jit.engines.deno}
          {' '}· QuickJS {interp.normal.engines.quickjs} · QuickJS-NG {interp.normal.engines['quickjs-ng']} · CPython {python.cpython}. Native CLI results, not the browser build.
        </p>
      )}

      <Tabs
        label="Choose a comparison"
        className="mode-tabs"
        value={mode}
        onChange={setMode}
        options={[['jit', 'JIT vs JIT'], ['interp', 'Interpreter vs interpreter'], ['python', 'Python vs CPython'], ['v040', 'Real-world apps'], ['release', `${rel.version} bundles`]] as const}
      />

      {mode === 'v040' && (
        <div className="benchmark-summary">
          <article className="headline-result">
            <div>
              <span>v0.0.40 · unmodified real-world apps</span>
              <strong>1.55–2.88×</strong>
              <p>faster than v0.0.39 across DOM &amp; Python HTML extraction · byte-identical output</p>
            </div>
            <div className="confidence-pill">linkedom · defuddle · webforai · html2text · bs4_css · markdownify</div>
          </article>
          <article className="ratio-card">
            <span>v0.0.40 vs v0.0.39 (real-world apps)</span>
            <RatioRows rows={[
              { label: 'html2text', value: 0.36 },
              { label: 'markdownify', value: 0.53 },
              { label: 'bs4_css', value: 0.57 },
              { label: 'linkedom', value: 0.65 },
            ]} />
            <small>Lower is faster: html2text 2.67–2.88×, markdownify 1.85–1.94×, bs4_css 1.71–1.82×, linkedom 1.55× faster than v0.0.39.</small>
          </article>
          <article className="ratio-card suite-card">
            <span>Extra suites in v0.0.40</span>
            <RatioRows rows={[
              { label: 'Python / CPython', value: pyWorkJit.all },
              { label: 'JS bench/regex', value: 1.90 },
              { label: 'All 30 JIT / Node', value: jit.all30.node },
            ]} />
            <small>JS regex cut from 2.44× to 1.90× Node (literal-utf16 4.0× → 0.82×); PHP &amp; Rust O(N²) paths sped up 8.6×–180×.</small>
          </article>
        </div>
      )}

      {mode === 'jit' && (
        <div className="benchmark-summary">
          <article className="headline-result">
            <div>
              <span>JIT vs JIT · all 30 rows</span>
              <strong>{x(jit.all30.node)}</strong>
              <p>ZIPP / Node equal-row geomean · lower is faster</p>
            </div>
            <div className="confidence-pill">faster than Node on {jit.nodeWins} of 30 rows</div>
          </article>
          <article className="ratio-card">
            <span>All 30 rows, ZIPP / engine</span>
            <RatioRows rows={[{ label: 'vs Node', value: jit.all30.node }, { label: 'vs Bun', value: jit.all30.bun }, { label: 'vs Deno', value: jit.all30.deno }]} />
            <small>Equal-row geometric means of the 13 normal and 17 hostile rows. Every engine with its production JIT.</small>
          </article>
          <article className="ratio-card suite-card">
            <span>Suite geomeans vs Node</span>
            <RatioRows rows={[{ label: 'Normal 13', value: jit.normal.geomean.node.all_measured.value }, { label: 'Hostile 17', value: jit.hostile.geomean.node.value }]} />
            <small>
              95% intervals: normal {range(jit.normal.geomean.node.all_measured.ci)} · hostile {range(jit.hostile.geomean.node.ci)}.
              Faster than Node on {normalWins}/13 normal and {hostileWins}/17 hostile rows.
            </small>
          </article>
        </div>
      )}

      {mode === 'interp' && qjs && (
        <div className="benchmark-summary">
          <article className="headline-result">
            <div>
              <span>Interpreter vs interpreter · all 30 rows</span>
              <strong>{x(qjs.value)}</strong>
              <p>ZIPP interpreter / Bellard’s QuickJS · lower is faster</p>
            </div>
            <div className="confidence-pill">faster than QuickJS on {qjs.wins} of {qjs.of} rows</div>
          </article>
          <article className="ratio-card dense-card">
            <span>All 30 rows, ZIPP interpreter / engine</span>
            <RatioRows rows={INTERP_COMPETITORS.filter((engine) => interp.all30[engine]).map((engine) => ({ label: ENGINE_LABEL[engine].replace(' jitless', ' no-JIT'), value: interp.all30[engine]!.value }))} />
            <small>QuickJS and QuickJS-NG have no JIT; Node, Deno and Bun run with theirs switched off.</small>
          </article>
          <article className="ratio-card suite-card">
            <span>Suite geomeans vs QuickJS</span>
            <RatioRows rows={[{ label: 'Normal 13', value: interp.normal.geomean.quickjs.value! }, { label: 'Hostile 17', value: interp.hostile.geomean.quickjs.value! }]} />
            <small>
              95% intervals: normal {range(interp.normal.geomean.quickjs.ci)} · hostile {range(interp.hostile.geomean.quickjs.ci)}.
              Against QuickJS-NG: {x(interp.all30['quickjs-ng']!.value)} over all 30.
            </small>
          </article>
        </div>
      )}

      {mode === 'python' && (
        <div className="benchmark-summary">
          <article className="headline-result">
            <div>
              <span>Python · {python.rows.length} programs</span>
              <strong>{x(pyWorkJit.all)}</strong>
              <p>ZIPP Python / CPython {python.cpython} · in-process work time</p>
            </div>
            <div className="confidence-pill">faster than CPython on {pyFaster} of {python.rows.length} programs</div>
          </article>
          <article className="ratio-card">
            <span>Work time, ZIPP / CPython</span>
            <RatioRows rows={[
              { label: 'JIT micro', value: pyWorkJit.micro }, { label: 'JIT macro', value: pyWorkJit.macro },
              { label: 'No JIT', value: pyWorkInterp.all },
            ]} />
            <small>Micro: {python.rows.filter((row) => row.group === 'micro').length} focused kernels. Macro: {python.rows.filter((row) => row.group === 'macro').length} application-shaped programs. No JIT: ZIPP_NOJIT=1, all programs.</small>
          </article>
          <article className="ratio-card suite-card">
            <span>Whole process, ZIPP / CPython</span>
            <RatioRows rows={[{ label: 'With JIT', value: pyWallJit.all }, { label: 'No JIT', value: python.geomean.wall.interp.all }]} />
            <small>Wall time per run, including process start and compilation; the table’s ratios are the work time alone.</small>
          </article>
        </div>
      )}

      {mode === 'release' && (
        <div className="benchmark-summary">
          <article className="headline-result">
            <div>
              <span>{rel.version} release · {rel.bundles.length} bundled apps</span>
              <strong>{x(relVsPrevious)}</strong>
              <p>ZIPP {rel.version} / {rel.previous} geomean, {rel.dies.large} die · lower is faster</p>
            </div>
            <div className="confidence-pill">faster than {rel.previous} on {relFaster} of {rel.bundles.length} apps</div>
          </article>
          <article className="ratio-card">
            <span>Bundled apps, ZIPP / QuickJS</span>
            <RatioRows rows={[
              { label: `${rel.version}, ${rel.dies.large}`, value: relVsQuickjs('large', 'current') },
              { label: `${rel.version}, ${rel.dies.small}`, value: relVsQuickjs('small', 'current') },
              { label: `PGO build, ${rel.dies.large}`, value: relVsQuickjs('large', 'pgo') },
            ]} />
            <small>Geomeans over the {rel.bundles.filter((bundle) => bundle.large.quickjs !== null).length} apps QuickJS runs, cache off. The canonical profile-guided build of the same engine is not shipped.</small>
          </article>
          <article className="ratio-card suite-card">
            <span>The {rel.programs.count} benchmark programs</span>
            <RatioRows rows={[
              { label: `Interpreter / ${rel.previous}`, value: rel.programs.interpVsPrevious },
              { label: `JIT / ${rel.previous}`, value: rel.programs.jitVsPrevious },
              { label: 'Interpreter / QuickJS', value: rel.programs.interpVsQuickjs },
            ]} />
            <small>
              The interpreter is faster than QuickJS on {rel.programs.interpFasterThanQuickjs} of {rel.programs.count} ({rel.previous}: {rel.programs.previousInterpFasterThanQuickjs},
              {' '}{x(rel.programs.previousInterpVsQuickjs)}) and faster than {rel.previous} on {rel.programs.interpFasterThanPrevious}.
            </small>
          </article>
        </div>
      )}

      <div className="reading-guide" aria-label="How to read these numbers">
        {READING[mode].map((item, index) => (
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
              {mode === 'v040' && v040View === 'apps' && <>ZIPP v0.0.40 · real-world JS &amp; Python apps · warm median &amp; 8-run wall clock</>}
              {mode === 'v040' && v040View === 'suites' && <>ZIPP v0.0.40 · Python, Regex, Hostile &amp; PHP/Rust runtime gains</>}
              {mode === 'jit' && <>{suite === 'normal' ? 'Normal 13' : 'Hostile 17'} · JIT vs JIT · cold wall time</>}
              {mode === 'interp' && <>{suite === 'normal' ? 'Normal 13' : 'Hostile 17'} · interpreter vs interpreter · cold wall time</>}
              {mode === 'python' && <>Python programs · in-process work time</>}
              {mode === 'release' && <>Bundled apps · {rel.version} against {rel.previous} and QuickJS · {rel.dies[die]} die, cold</>}
              <span>{mode === 'v040' && v040View === 'suites' ? 'v0.0.39 → v0.0.40 summary' : 'milliseconds · lower is better'}</span>
            </p>
          </div>
          <div className="scoreboard-controls">
            {mode === 'v040' ? (
              <Tabs label="Choose v0.0.40 benchmark view" value={v040View} onChange={setV040View}
                options={[['apps', 'Real-World Apps (6)'], ['suites', 'Regex, PHP & Rust']] as const} />
            ) : mode === 'release' ? (
              <Tabs label="Choose a die" value={die} onChange={setDie}
                options={[['large', `${rel.dies.large} die`], ['small', `${rel.dies.small} die`]] as const} />
            ) : mode !== 'python' ? (
              <Tabs label="Choose a benchmark suite" className="suite-tabs" value={suite} onChange={setSuite}
                options={[['normal', 'Normal suite'], ['hostile', 'Hostile suite']] as const} />
            ) : (
              <Tabs label="Choose Python programs" value={pyGroup} onChange={setPyGroup}
                options={[['all', `All ${python.rows.length}`], ['micro', 'Micro'], ['macro', 'Macro']] as const} />
            )}
            {mode === 'jit' && suite === 'normal' && (
              <Tabs label="Filter benchmark rows" value={filter} onChange={setFilter}
                options={[['all', 'All 13'], ['headline', 'Headline 10'], ['diagnostic', 'Diagnostics 3']] as const} />
            )}
          </div>
        </div>

        <div className="benchmark-table-wrap">
          {mode === 'v040' && v040View === 'apps' && (
            <table className="benchmark-table">
              <caption>Real-world JS and Python applications: v0.0.40 vs v0.0.39, Node.js v24.19.0 and CPython 3.13</caption>
              <thead>
                <tr>
                  <th scope="col">Application</th>
                  <th scope="col">Reference</th>
                  <th scope="col">ZIPP 0.0.39</th>
                  <th scope="col" className="zipp-column">ZIPP <span>0.0.40 warm / wall(8×)</span></th>
                  <th scope="col">vs 0.0.39</th>
                  <th scope="col">vs Reference</th>
                </tr>
              </thead>
              <tbody>
                {V040_APP_ROWS.map((row) => (
                  <tr key={row.id}>
                    <th scope="row"><span>{row.name}</span><small>{row.detail}</small></th>
                    <td data-label="Reference"><strong>{row.ref}</strong><br /><small>{row.refWarm} · {row.refWall}</small></td>
                    <td data-label="ZIPP 0.0.39">{row.v039}</td>
                    <td className="zipp-time" data-label="ZIPP 0.0.40"><strong>{row.v040Warm}</strong><br /><small>wall: {row.v040Wall}</small></td>
                    <td className="lead-cell" data-label="vs 0.0.39"><strong className="ratio-win">{row.speedup}</strong></td>
                    <td data-label="vs Reference"><small>{row.vsRef}</small></td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          {mode === 'v040' && v040View === 'suites' && (
            <table className="benchmark-table">
              <caption>ZIPP v0.0.40 performance across Python, Regex, Hostile JS, and PHP/Rust suites</caption>
              <thead>
                <tr>
                  <th scope="col">Suite</th>
                  <th scope="col">v0.0.39 (PGO)</th>
                  <th scope="col" className="zipp-column">ZIPP <span>0.0.40</span></th>
                  <th scope="col">Measured highlights</th>
                </tr>
              </thead>
              <tbody>
                {V040_SUITE_ROWS.map((row) => (
                  <tr key={row.id}>
                    <th scope="row"><span>{row.name}</span><small>{row.detail}</small></th>
                    <td data-label="v0.0.39">{row.prev}</td>
                    <td className="zipp-time" data-label="ZIPP 0.0.40"><strong>{row.curr}</strong></td>
                    <td data-label="Highlights">{row.note}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {mode === 'jit' && (
            <table className="benchmark-table">
              <caption>JIT against JIT: cold wall-time medians for ZIPP, Node, Bun and Deno</caption>
              <thead>
                <tr>
                  <th scope="col">Workload</th>
                  <th scope="col" className="zipp-column">ZIPP <span>JIT</span></th>
                  <th scope="col">Node</th>
                  <th scope="col">Bun</th>
                  <th scope="col">Deno</th>
                  <th scope="col" className="bar-column">ZIPP vs Node <span>relative time</span></th>
                  <th scope="col">ZIPP / Node</th>
                </tr>
              </thead>
              <tbody>
                {jitRows.map((row) => (
                  <tr key={row.id}>
                    <th scope="row"><span>{row.name}</span><small>{suite === 'normal' ? (row.group === 'headline' ? 'Headline' : 'Diagnostic') : row.group}</small></th>
                    <td className="zipp-time" data-label="ZIPP"><strong>{ms(row.times.zipp)}</strong><span className="sr-only"> milliseconds</span></td>
                    <td data-label="Node">{ms(row.times.node)}</td>
                    <td data-label="Bun">{ms(row.times.bun)}</td>
                    <td data-label="Deno">{ms(row.times.deno)}</td>
                    <td className="bar-cell" data-label="ZIPP vs Node" aria-hidden="true"><RowBars zipp={row.times.zipp} other={row.times.node} /></td>
                    <RatioCell ratio={row.ratio} label="ZIPP divided by Node" versus="Node" />
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          {mode === 'interp' && (
            <table className="benchmark-table interp-table">
              <caption>Interpreter against interpreter: cold wall-time medians</caption>
              <thead>
                <tr>
                  <th scope="col">Workload</th>
                  <th scope="col" className="zipp-column">ZIPP <span>no JIT</span></th>
                  {INTERP_COMPETITORS.map((engine) => <th scope="col" key={engine}>{ENGINE_LABEL[engine]}</th>)}
                  <th scope="col">ZIPP / QuickJS</th>
                </tr>
              </thead>
              <tbody>
                {interpSuite.rows.map((row) => {
                  const times = row.times as Record<string, number | undefined>
                  const vs = row.vs as Record<string, number | undefined>
                  return (
                    <tr key={row.id}>
                      <th scope="row"><span>{row.name}</span></th>
                      <td className="zipp-time" data-label="ZIPP interpreter"><strong>{ms(times['zipp-interp'])}</strong><span className="sr-only"> milliseconds</span></td>
                      {INTERP_COMPETITORS.map((engine) => <td key={engine} data-label={ENGINE_LABEL[engine]}>{ms(times[engine])}</td>)}
                      {vs.quickjs !== undefined ? <RatioCell ratio={vs.quickjs} label="ZIPP divided by QuickJS" versus="QuickJS" /> : <td data-label="ZIPP / QuickJS">—</td>}
                    </tr>
                  )
                })}
              </tbody>
            </table>
          )}

          {mode === 'python' && (
            <table className="benchmark-table python-table">
              <caption>ZIPP Python against CPython: in-process work-time medians</caption>
              <thead>
                <tr>
                  <th scope="col">Program</th>
                  <th scope="col" className="zipp-column">ZIPP <span>JIT</span></th>
                  <th scope="col">ZIPP <span>no JIT</span></th>
                  <th scope="col">CPython {python.cpython}</th>
                  <th scope="col" className="bar-column">ZIPP vs CPython <span>relative time</span></th>
                  <th scope="col">ZIPP / CPython</th>
                  <th scope="col">No JIT / CPython</th>
                </tr>
              </thead>
              <tbody>
                {pyRows.map((row) => (
                  <tr key={row.id}>
                    <th scope="row"><span>{row.name}</span><small>{row.group}</small></th>
                    <td className="zipp-time" data-label="ZIPP"><strong>{ms(row.workMs.zipp)}</strong><span className="sr-only"> milliseconds</span></td>
                    <td data-label="ZIPP no JIT">{ms(row.workMs.zippInterp)}</td>
                    <td data-label="CPython">{ms(row.workMs.cpython)}</td>
                    <td className="bar-cell" data-label="ZIPP vs CPython" aria-hidden="true"><RowBars zipp={row.workMs.zipp} other={row.workMs.cpython} /></td>
                    <RatioCell ratio={row.ratio} label="ZIPP divided by CPython" versus="CPython" />
                    <td data-label="No JIT / CPython">{x(row.ratioInterp, 2)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}

          {mode === 'release' && (
            <table className="benchmark-table release-table">
              <caption>ZIPP {rel.version} against {rel.previous} and QuickJS: bundled-app wall-time medians, {rel.dies[die]} die</caption>
              <thead>
                <tr>
                  <th scope="col">Bundled app</th>
                  <th scope="col" className="zipp-column">ZIPP <span>{rel.version}</span></th>
                  <th scope="col">ZIPP {rel.previous}</th>
                  <th scope="col">{rel.version} <span>cache warm</span></th>
                  <th scope="col">QuickJS</th>
                  <th scope="col" className="bar-column">{rel.version} vs QuickJS <span>relative time</span></th>
                  <th scope="col">{rel.version} / QuickJS</th>
                  <th scope="col">{rel.version} / {rel.previous}</th>
                </tr>
              </thead>
              <tbody>
                {releaseRows.map((row) => (
                  <tr key={row.id}>
                    <th scope="row"><span>{row.name}</span><small>{row.detail}</small></th>
                    <td className="zipp-time" data-label={`ZIPP ${rel.version}`}><strong>{ms(row.t.current)}</strong><span className="sr-only"> milliseconds</span></td>
                    <td data-label={`ZIPP ${rel.previous}`}>{ms(row.t.previous)}</td>
                    <td data-label="Cache warm">{ms(row.t.warm)}</td>
                    <td data-label="QuickJS">{row.t.quickjs === null ? 'fails' : ms(row.t.quickjs)}</td>
                    <td className="bar-cell" data-label={`${rel.version} vs QuickJS`} aria-hidden="true">{row.t.quickjs !== null && <RowBars zipp={row.t.current} other={row.t.quickjs} />}</td>
                    {row.t.quickjs !== null ? <RatioCell ratio={row.t.current / row.t.quickjs} label={`ZIPP ${rel.version} divided by QuickJS`} versus="QuickJS" /> : <td data-label={`${rel.version} / QuickJS`}>—</td>}
                    <td data-label={`${rel.version} / ${rel.previous}`}>{x(row.t.current / row.t.previous, 2)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </div>

      <div className="methodology-note">
        <span className="methodology-mark">i</span>
        <div>
          {mode === 'v040' && (
            <>
              <p>
                Measured on Windows x86-64 with byte-identical output against Node.js v24.19.0 and CPython 3.13,
                comparing a local non-PGO fat-LTO <code>v0.0.40</code> build against the published PGO <code>v0.0.39</code> binary.
                Startup in <code>tools/bench.py</code>: ZIPP 10.2 ms vs Node.js 34.3 ms, Bun 52.7 ms, Deno 87.6 ms.
                Also includes readable JS function labels in Linux <code>ZIPP_PERF_MAP=1</code> output contributed by{' '}
                <a href="https://github.com/codeandsolder" target="_blank" rel="noreferrer">@codeandsolder</a> (PR #110).
              </p>
              <p className="gap-list">
                <strong>Where gaps remain:</strong> warm steady-state DOM extraction (<code>linkedom</code>, <code>defuddle</code>, <code>webforai</code>)
                still trails V8 TurboFan by 4.7–6.1× warm (1.4–2.8× 8-run wall), and warm BeautifulSoup/<code>html2text</code> loops trail CPython’s C-extension parsers by 2.6–7.2× warm (1.4–2.8× wall).
                Use the Canonical JIT tab for the 15-rep provenance-stamped PGO capture.
              </p>
            </>
          )}
          {mode === 'jit' && (
            <>
              <p>
                Native Windows x86-64 CLI, high-performance power mode, {jit.reps} paired repetitions in a deterministically
                shuffled order, {jit.bootstrapSamples.toLocaleString('en-US')} bootstrap samples, exact-byte output. ZIPP is the
                provenance-stamped profile-guided (PGO) build of the {versionOf(jit.zipp.version)} engine, SHA-256 <code>{jit.zipp.sha256.slice(0, 8)}…{jit.zipp.sha256.slice(-6)}</code>,
                the project’s canonical benchmark build. Medians include process launch: ZIPP {jit.startupMs.zipp.toFixed(1)} ms,
                Node {jit.startupMs.node.toFixed(1)} ms, Bun {jit.startupMs.bun.toFixed(1)} ms, Deno {jit.startupMs.deno.toFixed(1)} ms.
              </p>
              <p>
                <strong>The download is the release build:</strong> profile-guided since 0.0.31, but with the release’s features and its own training run. The published <code>zipp-{versionOf(release.zipp.version)}-windows-x86_64-portable.exe</code> runs
                the normal 13 rows at {x(release.normalVsNode.all_measured.value)} Node ({range(release.normalVsNode.all_measured.ci)}); the canonical PGO build takes{' '}
                {release.pgoOverRelease.all_measured ? x(release.pgoOverRelease.all_measured.value) : '—'} of its time on the same rows. That compares the two
                builds, not profile guidance alone: the PGO build also leaves out the GPU and Python features and has its own CPU target and linker.
              </p>
              <p className="gap-list">
                <strong>Rows still behind Node ({jitGaps.length}):</strong>{' '}
                {jitGaps.length === 0 ? 'none.' : jitGaps.map((row, index) => <span key={row.id}>{row.id} {x(row.ratio)}{index < jitGaps.length - 1 ? ', ' : '.'}</span>)}
              </p>
            </>
          )}
          {mode === 'interp' && (
            <>
              <p>
                The same PGO build with <code>ZIPP_NOJIT=1 ZIPP_NO_RX_JIT=1</code> (no native code; the captures before 22 October left its regex JIT on), Bellard’s QuickJS {interp.normal.engines.quickjs}, QuickJS-NG {interp.normal.engines['quickjs-ng']}, Node{' '}
                <code>--jitless</code>, Deno <code>--v8-flags=--jitless</code> and Bun with <code>BUN_JSC_useJIT=0</code>. Identical staged sources for every
                engine, every output checked against Node’s, {interp.normal.reps} counterbalanced repetitions, cold medians including launch.
                Over all 30 rows ZIPP’s interpreter measures {interp.all30['node-jitless'] ? x(interp.all30['node-jitless'].value) : '—'} Node’s
                Ignition and {interp.all30['bun-nojit'] ? x(interp.all30['bun-nojit'].value) : '—'} Bun’s LLInt.
              </p>
              <p className="gap-list">
                <strong>Rows where QuickJS is faster ({interpGaps.length}):</strong>{' '}
                {interpGaps.length === 0 ? 'none.' : interpGaps.map((row, index) => <span key={row.id}>{row.id} {x((row.vs as Record<string, number>).quickjs)}{index < interpGaps.length - 1 ? ', ' : '.'}</span>)}
              </p>
            </>
          )}
          {mode === 'python' && (
            <>
              <p>
                The published {versionOf(python.zipp.version)} binary (<code>{python.zipp.sha256.slice(0, 8)}…{python.zipp.sha256.slice(-6)}</code>, the release build: the canonical PGO
                build has no Python) against CPython {python.cpython}, a fresh process per engine per repetition, interleaved. Ratios are medians of
                the in-process work time; geometric means: micro {x(pyWorkJit.micro)}, macro {x(pyWorkJit.macro)}, all {x(pyWorkJit.all)} with the JIT,
                {' '}{x(pyWorkInterp.all)} without it. Whole-process wall time: {x(pyWallJit.all)} with the JIT.
              </p>
              <p className="gap-list">
                <strong>Programs where CPython is faster ({pyGaps.length}):</strong>{' '}
                {pyGaps.length === 0 ? 'none.' : pyGaps.map((row, index) => <span key={row.id}>{row.name} {x(row.ratio, 2)}{index < pyGaps.length - 1 ? ', ' : '.'}</span>)}
              </p>
            </>
          )}
          {mode === 'release' && (
            <>
              <p>
                {rel.method} Peak working set, cache off: {rel.memory.map((row, index) => (
                  <span key={row.id}>{row.name} {Math.round(row.previous)} → {Math.round(row.current)} MiB (QuickJS {Math.round(row.quickjs)}){index < rel.memory.length - 1 ? ', ' : '.'}</span>
                ))}
              </p>
              <p className="gap-list">
                <strong>Apps where QuickJS is faster on the {rel.dies[die]} die ({releaseGaps.length}):</strong>{' '}
                {releaseGaps.length === 0 ? 'none.' : releaseGaps.map((row, index) => <span key={row.id}>{row.name} {x(row.t.current / row.t.quickjs!)}{index < releaseGaps.length - 1 ? ', ' : '.'}</span>)}
              </p>
            </>
          )}
        </div>
        <div className="methodology-links">
          {mode === 'v040' && <Link href={blob('docs/releases/0.0.40.md')}>v0.0.40 release notes</Link>}
          {mode === 'jit' && <><Link href={blob(jit.artifacts.normal)}>Normal capture</Link><Link href={blob(jit.artifacts.hostile)}>Hostile capture</Link><Link href={blob(release.artifacts.release)}>Release build</Link></>}
          {mode === 'interp' && <><Link href={blob(interp.artifacts.normal)}>Normal capture</Link><Link href={blob(interp.artifacts.hostile)}>Hostile capture</Link></>}
          {mode === 'python' && <><Link href={blob(python.artifacts.jit)}>With JIT</Link><Link href={blob(python.artifacts.interp)}>Without JIT</Link></>}
          {mode === 'release' && <Link href={`${blob(rel.notes)}#performance`}>{rel.version} release notes</Link>}
          <Link href={PERFORMANCE_URL}>Method & intervals</Link>
          <Link href={ROADMAP_URL}>What is next</Link>
        </div>
      </div>
    </section>
  )
}
