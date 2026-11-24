import { useState } from 'react'

const layers = [
  { name: 'JS, TS, Python, PHP & Rust', label: '01 / YOUR SOURCE', symbol: '{ }', copy: 'Write JavaScript, TypeScript/TSX, Python 3.13, PHP 8.x, or Rust. Every frontend compiles directly into ZIPP’s own register VM bytecode — no external interpreter or transpiler.' },
  { name: 'ZIPP VM + WASM', label: '02 / THE ENGINE', symbol: 'ϟ', copy: 'Native CLI runs an x86-64/ARM64 tiered JIT; browsers run the pure-safe-Rust WebAssembly engine inside isolated Workers with step, heap, and timeout ceilings.' },
  { name: 'Host Bridges & Containers', label: '03 / GPU, SQL & SANDBOX', symbol: '</>', copy: 'Composable addAppBridge plugins expose WebGPU/WebGL2 compute, XDB SQLite 3, and closed Shadow DOM containers (like SoftN and FormLogic) while keeping the audited 51-function WASM surface closed.' },
]

export function SandboxStack() {
  const [active, setActive] = useState(1)

  return (
    <aside className="stack-lab" aria-label="Explore how ZIPP runs five languages on one VM">
      <div className="lab-heading"><span className="lab-label">ONE ENGINE. FIVE LANGUAGES.</span><span aria-hidden="true">✳</span></div>
      <div className="stack-caption"><span>JS, Py, PHP, Rust.</span><span>One ZIPP VM.</span></div>
      <p className="stack-hint">Choose a layer to see where the work happens.</p>
      <div className="stack-layers" aria-label="Choose a runtime layer">
        {layers.map((layer, index) => (
          <button type="button" key={layer.name} className={`stack-layer stack-layer-${index}`} aria-pressed={active === index} aria-controls="stack-explanation" onClick={() => setActive(index)}>
            <span className="layer-symbol" aria-hidden="true">{layer.symbol}</span>
            <span><small>{layer.label}</small><strong>{layer.name}</strong></span>
            <span className="layer-select" aria-hidden="true">{active === index ? '−' : '+'}</span>
          </button>
        ))}
      </div>
      <div className="stack-explanation" id="stack-explanation" aria-live="polite"><span className="lab-label">PEEK INSIDE / 0{active + 1}</span><p>{layers[active].copy}</p></div>
      <a className="lab-link" href="#playground">Give the real engine a spin <span aria-hidden="true">↗</span></a>
    </aside>
  )
}

const projects = [
  {
    name: 'SoftN',
    domain: 'softn.com',
    eyebrow: 'THE CONTAINER RUNTIME',
    title: 'Docker-like containers for web apps.',
    description: 'SoftN packages a full-stack web app — any frontend (HTML/JS, React, Vue), an optional sandboxed backend (PHP 8, JS/TS, Rust, or Python), a SQLite 3 database, and explicit permission rules — into one portable .softn container running inside ZIPP WASM Workers and a closed Shadow DOM.',
    tag: 'Powered by ZIPP WASM',
    mark: '{ softn }',
  },
  {
    name: 'FormLogic',
    domain: 'formlogic.com',
    eyebrow: 'THE CONNECTED PLATFORM',
    title: 'Marketplace apps, sandboxed by ZIPP.',
    description: 'FormLogic pairs visual forms, charts, and shared SQLite databases with full-stack SoftN apps and private source-hidden Marketplace sandboxes. Guest UI and backend code run inside ZIPP WebAssembly Workers in the browser and ZIPP safe-sandbox on the server — with zero app code changes.',
    tag: 'Built with SoftN + ZIPP',
    mark: 'formlogic ↗',
  },
]

const containerStack = [
  {
    name: 'FormLogic',
    role: 'Marketplace & workspace',
    detail: 'FormLogic hosts full-stack SoftN business apps alongside Forms, Charts, and Flows. Every Marketplace demo runs in an isolated per-visitor sandbox with its own ephemeral SQLite database and strict source protection.',
  },
  {
    name: 'SoftN Container',
    role: 'Closed Shadow DOM + Workers',
    detail: 'SoftN’s <softn-app> container sanitizes HTML/CSS into a closed ShadowRoot so guest code never touches the host window, cookies, or page DOM. Standard fetch("/api/...") calls cross a gated transport bridge instead of hitting the host network.',
  },
  {
    name: 'ZIPP WASM VMs',
    role: 'Frontend + Backend Workers',
    detail: 'Two isolated ZIPP v0.0.40 Workers separate UI from backend: frontend JS/TS runs in zipp-javascript-lite (or full JS/TS), while PHP 8, Rust, JS/TS, or Python API routes run in a dedicated Backend Worker under strict instruction, heap, and timeout ceilings.',
  },
  {
    name: 'XDB SQLite 3',
    role: 'Composable addAppBridge',
    detail: 'Standard PHP PDO("sqlite:app.db") or SQL queries route over ZIPP’s composable addAppBridge("sql", ...) plugin into in-browser XDB SQLite 3 WASM (or FormLogic’s multi-user WAL SQLite on the server) — sharing the VM’s step meter without widening the audited 51-function WASM import surface.',
  },
]

const containerPillars = [
  {
    label: '01 / ZERO HOST DOM ACCESS',
    title: 'Closed Shadow DOM + Worker split',
    copy: 'Untrusted UI scripts run off the main thread inside a ZIPP Worker and emit surgical DOM patches into a closed ShadowRoot. Guest CSS and scripts cannot read host cookies, localStorage, or outer elements.',
  },
  {
    label: '02 / FULL-STACK IN TWO WORKERS',
    title: 'Sandboxed PHP, Rust, JS or Python backend',
    copy: 'A second ZIPP Backend Worker runs standard PHP 8 (PhpApp SAPI / Slim 4), Rust Axum, JS/TS, or Python handlers against virtual cookies, a virtual filesystem, and XDB SQLite 3 — with zero server setup.',
  },
  {
    label: '03 / DEFAULT-DENY CAPABILITIES',
    title: 'Explicit permissions & portable deploy',
    copy: 'Network hosts, storage, camera, and files stay denied unless declared in permission.json and approved by the user. The exact same .softn container deploys multi-user on FormLogic inside ZIPP safe-sandbox.',
  },
]

export function ProjectShowcase() {
  const [active, setActive] = useState(0)
  const [demoOpen, setDemoOpen] = useState(false)
  const [demoApp, setDemoApp] = useState<'pottery' | 'brewdesk'>('pottery')

  const launchDemo = (app: 'pottery' | 'brewdesk') => {
    setDemoApp(app)
    setDemoOpen(true)
  }

  return (
    <section className="projects-section section-wrap" id="use-cases" aria-labelledby="projects-title">
      <div className="projects-heading">
        <p className="section-kicker">SMALL ENGINE. REAL CONTAINERS.</p>
        <h2 id="projects-title">Full-stack apps,<br />sandboxed by <em>ZIPP.</em></h2>
        <p>SoftN turns ZIPP WebAssembly into portable web app containers. FormLogic runs a whole marketplace of business apps on top of them.</p>
      </div>

      <div className="project-cards">
        {projects.map((project, index) => (
          <article className={`project-card project-card-${index}`} key={project.name}>
            <div className="project-card-top"><span>{project.eyebrow}</span><span className="project-number">0{index + 1}</span></div>
            <a className="project-wordmark" href={`https://${project.domain}`} target="_blank" rel="noreferrer" aria-label={`Visit ${project.name}`}>{project.mark}</a>
            <span className="project-tag">{project.tag}</span>
            <h3>{project.title}</h3><p>{project.description}</p>
            <a className="project-link" href={`https://${project.domain}`} target="_blank" rel="noreferrer">Explore {project.domain}<span aria-hidden="true">↗</span></a>
          </article>
        ))}
      </div>

      <div className="ecosystem" id="containers">
        <div className="ecosystem-intro">
          <span className="lab-label">HOW SOFTN CONTAINERS SANDBOX CODE</span>
          <p>One portable <code>.softn</code> archive.<br />Zero host trust.</p>
        </div>
        <div className="ecosystem-interaction">
          <div className="ecosystem-path" aria-label="Explore the FormLogic, SoftN and ZIPP container stack">
            {containerStack.map((step, index) => (
              <div className="ecosystem-step-wrap" key={step.name}>
                <button type="button" aria-pressed={active === index} aria-controls="ecosystem-detail" onClick={() => setActive(index)}>
                  {step.name}
                  <small>{step.role}</small>
                </button>
                {index < containerStack.length - 1 && <span aria-hidden="true">→</span>}
              </div>
            ))}
          </div>
          <p id="ecosystem-detail" className="ecosystem-detail" aria-live="polite">{containerStack[active].detail}</p>
        </div>
      </div>

      <div className="container-pillars" aria-label="Container isolation architecture">
        {containerPillars.map((pillar) => (
          <article key={pillar.label}>
            <span className="lab-label">{pillar.label}</span>
            <h3>{pillar.title}</h3>
            <p>{pillar.copy}</p>
          </article>
        ))}
      </div>

      <div className="container-demo-box" id="formlogic-demo" aria-labelledby="container-demo-title">
        <div className="container-demo-head">
          <div>
            <p className="section-kicker">LIVE FORMLOGIC × SOFTN CONTAINER DEMO</p>
            <h3 id="container-demo-title">Try a real FormLogic marketplace app inside ZIPP WASM.</h3>
            <p>
              Launch <strong>Kiln &amp; Wheel Pottery Studio</strong> (9.9 KB: HTML/JS frontend + PHP 8 PDO backend + SQLite 3) or{' '}
              <strong>BrewDesk Café Ops</strong> (242 KB: Slim 4 + PHP 8 + SQLite 3 FormLogic Marketplace pack).
              Every booking, ticket, and SQL query runs completely in your browser inside isolated ZIPP v0.0.40 WebAssembly Workers — no external server.
            </p>
          </div>
          <div className="container-demo-actions">
            <a className="button button-secondary" href={`/container-demo/?app=${demoApp}`} target="_blank" rel="noreferrer">
              Open full screen ↗
            </a>
            <a className="text-link" href="https://formlogic.com/#live-demo" target="_blank" rel="noreferrer">
              Explore marketplace apps on formlogic.com ↗
            </a>
          </div>
        </div>

        {demoOpen ? (
          <iframe
            className="container-demo-frame"
            src={`/container-demo/?app=${demoApp}`}
            title="Live FormLogic SoftN container running on ZIPP v0.0.40 WebAssembly"
          />
        ) : (
          <div className="container-demo-standby">
            <div className="container-arch-flow" aria-label="Container data flow">
              <div>
                <small>FRONTEND WORKER</small>
                <strong>HTML / JS UI → Closed Shadow DOM</strong>
                <span>ZIPP v0.0.40 JS-Lite · no host window</span>
              </div>
              <span aria-hidden="true">→</span>
              <div>
                <small>BACKEND WORKER</small>
                <strong>fetch("/api/*") → PHP 8 SAPI</strong>
                <span>ZIPP v0.0.40 PHP · virtual VFS &amp; cookies</span>
              </div>
              <span aria-hidden="true">→</span>
              <div>
                <small>SQL BRIDGE</small>
                <strong>PDO("sqlite:app.db") → XDB SQLite 3</strong>
                <span>addAppBridge("sql") · ephemeral DB</span>
              </div>
            </div>
            <p className="container-standby-note">
              Loads on demand (~2.6 MB Brotli for ZIPP JS-Lite + ZIPP PHP + XDB SQLite 3 WASM). Choose a real FormLogic SoftN container to boot it live in your browser:
            </p>
            <div className="container-standby-buttons">
              <button type="button" className="button button-primary" onClick={() => launchDemo('pottery')}>
                ▶ Launch Kiln &amp; Wheel Pottery Studio (9.9 KB)
              </button>
              <button type="button" className="button button-secondary" onClick={() => launchDemo('brewdesk')}>
                ▶ Launch BrewDesk Café Ops (Slim 4 Marketplace Pack)
              </button>
            </div>
          </div>
        )}
      </div>
    </section>
  )
}
