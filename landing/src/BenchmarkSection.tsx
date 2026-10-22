import { useMemo, useState, type ReactNode } from 'react'
import data from './benchmark-data.json'

/*
 * The measured-performance section. Every figure comes from
 * benchmark-data.json, which scripts/import-benchmarks.py writes from the
 * capture artifacts under bench/; the prose below states only what those
 * numbers say (counts and directions are computed, not typed).
 */

const GITHUB_URL = 'https://github.com/f2i-com/zipp.org'
const blob = (path: string) => `${GITHUB_URL}/blob/main/${path}`
const PERFORMANCE_URL = blob('docs/PERFORMANCE.md')
const ROADMAP_URL = blob('docs/PERF_ROADMAP.md')

type Mode = 'jit' | 'interp' | 'python'
type Suite = 'normal' | 'hostile'
type Filter = 'all' | 'headline' | 'diagnostic'
type PyGroup = 'all' | 'micro' | 'macro'
type Interval = number[] | null | undefined

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
}

export function BenchmarkSection() {
  const [mode, setMode] = useState<Mode>('jit')
  const [suite, setSuite] = useState<Suite>('normal')
  const [filter, setFilter] = useState<Filter>('all')
  const [pyGroup, setPyGroup] = useState<PyGroup>('all')

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

      <p className="benchmark-capture-note">
        Measured {longDate(jit.capturedAt)} on native Windows x86-64 · ZIPP {benchmarkFacts.version}, canonical PGO build (source <code>{jit.zipp.source}</code>)
        {' '}· Node {jit.engines.node} · Bun {jit.engines.bun} · Deno {jit.engines.deno}
        {' '}· QuickJS {interp.normal.engines.quickjs} · QuickJS-NG {interp.normal.engines['quickjs-ng']} · CPython {python.cpython}. Native CLI results, not the browser build.
      </p>

      <Tabs
        label="Choose a comparison"
        className="mode-tabs"
        value={mode}
        onChange={setMode}
        options={[['jit', 'JIT vs JIT'], ['interp', 'Interpreter vs interpreter'], ['python', 'Python vs CPython']] as const}
      />

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
              {mode === 'jit' && <>{suite === 'normal' ? 'Normal 13' : 'Hostile 17'} · JIT vs JIT · cold wall time</>}
              {mode === 'interp' && <>{suite === 'normal' ? 'Normal 13' : 'Hostile 17'} · interpreter vs interpreter · cold wall time</>}
              {mode === 'python' && <>Python programs · in-process work time</>}
              <span>milliseconds · lower is better</span>
            </p>
          </div>
          <div className="scoreboard-controls">
            {mode !== 'python' ? (
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
        </div>
      </div>

      <div className="methodology-note">
        <span className="methodology-mark">i</span>
        <div>
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
                <strong>The download is a plain release build.</strong> The published <code>zipp-{versionOf(release.zipp.version)}-windows-x86_64-portable.exe</code> runs
                the normal 13 rows at {x(release.normalVsNode.all_measured.value)} Node ({range(release.normalVsNode.all_measured.ci)}); the PGO build takes{' '}
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
                The same PGO build with <code>ZIPP_NOJIT=1 ZIPP_NO_RX_JIT=1</code> (no native code; the captures before 22 October left its regex JIT on), Bellard’s QuickJS 2026-06-04, QuickJS-NG 0.17.0, Node{' '}
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
                The published {versionOf(python.zipp.version)} binary (<code>{python.zipp.sha256.slice(0, 8)}…{python.zipp.sha256.slice(-6)}</code>, the plain release build: the PGO
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
        </div>
        <div className="methodology-links">
          {mode === 'jit' && <><Link href={blob(jit.artifacts.normal)}>Normal capture</Link><Link href={blob(jit.artifacts.hostile)}>Hostile capture</Link><Link href={blob(release.artifacts.release)}>Release build</Link></>}
          {mode === 'interp' && <><Link href={blob(interp.artifacts.normal)}>Normal capture</Link><Link href={blob(interp.artifacts.hostile)}>Hostile capture</Link></>}
          {mode === 'python' && <><Link href={blob(python.artifacts.jit)}>With JIT</Link><Link href={blob(python.artifacts.interp)}>Without JIT</Link></>}
          <Link href={PERFORMANCE_URL}>Method & intervals</Link>
          <Link href={ROADMAP_URL}>What is next</Link>
        </div>
      </div>
    </section>
  )
}
