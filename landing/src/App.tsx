import { useEffect, useRef, useState } from 'react'

const GITHUB_URL = 'https://github.com/f2i-com/zipp.org'

const installCommands = `git clone https://github.com/f2i-com/zipp.org.git zipp
cd zipp
cargo build --release
./target/release/zipp js examples/hello.js`

const pipeline = [
  { step: '01', title: 'Source', detail: 'Modern JavaScript' },
  { step: '02', title: 'Front end', detail: 'Lexer + parser' },
  { step: '03', title: 'Bytecode', detail: 'Register-based' },
  { step: '04', title: 'Runtime', detail: 'VM + OSR JIT' },
]

const features = [
  {
    number: '01',
    title: 'Own the whole pipeline',
    copy: 'A hand-written lexer and parser flow into Zipp bytecode, an explicit-frame register VM, and a native JIT. No third-party parser hiding in the middle.',
    tag: 'Clean-sheet core',
  },
  {
    number: '02',
    title: 'Correctness, measured',
    copy: 'The interpreter and JIT produce a byte-identical failure set across 95,942 required test262 executions — an intentional guard against tier drift.',
    tag: '99.994% test262',
  },
  {
    number: '03',
    title: 'Built to stay running',
    copy: 'Embed a persistent VM, compile once, then call functions and exchange structured data. The wasm-bindgen layer brings that model to browser hosts.',
    tag: 'Native + WASM',
  },
]

function GithubLink({ className, children }: { className?: string; children: React.ReactNode }) {
  return (
    <a className={className} href={GITHUB_URL} target="_blank" rel="noreferrer">
      {children}
      <span aria-hidden="true">↗</span>
    </a>
  )
}

function App() {
  const [copied, setCopied] = useState(false)
  const resetTimer = useRef<number | undefined>(undefined)

  useEffect(() => {
    return () => window.clearTimeout(resetTimer.current)
  }, [])

  const copyInstall = async () => {
    try {
      await navigator.clipboard.writeText(installCommands)
      setCopied(true)
      window.clearTimeout(resetTimer.current)
      resetTimer.current = window.setTimeout(() => setCopied(false), 1800)
    } catch {
      setCopied(false)
    }
  }

  return (
    <div className="site-shell">
      <a className="skip-link" href="#main-content">
        Skip to content
      </a>

      <header className="site-header">
        <a className="brand" href="#top" aria-label="Zipp home">
          <span className="brand-mark" aria-hidden="true">z</span>
          <span className="brand-name">zipp</span>
        </a>

        <nav className="nav-links" aria-label="Primary navigation">
          <a href="#engine">Engine</a>
          <a href="#numbers">Numbers</a>
          <a href="#quickstart">Quickstart</a>
        </nav>

        <GithubLink className="header-cta">GitHub</GithubLink>
      </header>

      <main id="main-content">
        <section className="hero section-wrap" id="top">
          <div className="hero-glow hero-glow-one" aria-hidden="true" />
          <div className="hero-glow hero-glow-two" aria-hidden="true" />

          <div className="hero-copy">
            <div className="eyebrow">
              <span className="status-dot" aria-hidden="true" />
              JavaScript engine · Rust core
            </div>
            <h1>
              JavaScript,
              <span>built from first principles.</span>
            </h1>
            <p className="hero-intro">
              Zipp is a clean-sheet engine with its own front end, a register VM,
              per-call-site inline caches, and a native x86-64 OSR JIT.
            </p>
            <div className="hero-actions">
              <GithubLink className="button button-primary">Explore the code</GithubLink>
              <a className="button button-secondary" href="#quickstart">
                Get started <span aria-hidden="true">↓</span>
              </a>
            </div>
            <p className="hero-note">
              Native JIT on x86-64 <span aria-hidden="true">·</span> Interpreter on
              aarch64 and wasm32
            </p>
          </div>

          <div className="engine-card" aria-label="Zipp engine pipeline overview">
            <div className="engine-card-header">
              <div className="window-dots" aria-hidden="true">
                <span />
                <span />
                <span />
              </div>
              <span>engine.trace</span>
              <span className="engine-ready">ready</span>
            </div>
            <div className="code-preview" aria-hidden="true">
              <div><span className="line-number">1</span><span className="code-keyword">function</span> sum(values) {'{'}</div>
              <div><span className="line-number">2</span>&nbsp;&nbsp;<span className="code-keyword">let</span> total = <span className="code-number">0</span></div>
              <div><span className="line-number">3</span>&nbsp;&nbsp;<span className="code-keyword">for</span> (<span className="code-keyword">const</span> value <span className="code-keyword">of</span> values) {'{'}</div>
              <div><span className="line-number">4</span>&nbsp;&nbsp;&nbsp;&nbsp;total += value</div>
              <div><span className="line-number">5</span>&nbsp;&nbsp;{'}'}</div>
              <div><span className="line-number">6</span>&nbsp;&nbsp;<span className="code-keyword">return</span> total</div>
              <div><span className="line-number">7</span>{'}'}</div>
            </div>
            <div className="trace-list">
              <div>
                <span className="trace-check" aria-hidden="true">✓</span>
                <span>Parse</span>
                <span>own front end</span>
              </div>
              <div>
                <span className="trace-check" aria-hidden="true">✓</span>
                <span>Compile</span>
                <span>register bytecode</span>
              </div>
              <div>
                <span className="trace-spark" aria-hidden="true">↯</span>
                <span>Execute</span>
                <span>OSR JIT</span>
              </div>
            </div>
            <div className="engine-card-footer">
              <span>zipp js app.js</span>
              <span className="terminal-cursor" aria-hidden="true" />
            </div>
          </div>

          <div className="proof-strip" id="numbers">
            <div className="proof-item">
              <strong>99.994%</strong>
              <span>test262 conformance</span>
            </div>
            <div className="proof-item">
              <strong>~3.4×</strong>
              <span>faster measured startup</span>
            </div>
            <div className="proof-item">
              <strong>1</strong>
              <span>self-contained binary</span>
            </div>
            <div className="proof-item proof-item-last">
              <strong>0</strong>
              <span>runtime files</span>
            </div>
          </div>
          <p className="measurement-note">
            Current repository measurements: 95,936 / 95,942 required test262
            executions; 9.8 ms vs 33.0 ms startup against Node 24.12.0.
          </p>
        </section>

        <section className="feature-section section-wrap" id="engine">
          <div className="section-heading">
            <div>
              <p className="section-kicker">Engineered end to end</p>
              <h2>Small surface. Serious machinery.</h2>
            </div>
            <p>
              Zipp owns every stage of execution, making performance work visible
              and correctness testable from source text to native code.
            </p>
          </div>

          <div className="feature-grid">
            {features.map((feature) => (
              <article className="feature-card" key={feature.number}>
                <div className="feature-card-top">
                  <span className="feature-number">{feature.number}</span>
                  <span className="feature-tag">{feature.tag}</span>
                </div>
                <h3>{feature.title}</h3>
                <p>{feature.copy}</p>
              </article>
            ))}
          </div>
        </section>

        <section className="pipeline-section section-wrap" aria-labelledby="pipeline-title">
          <div className="pipeline-copy">
            <p className="section-kicker">No black boxes</p>
            <h2 id="pipeline-title">From source to speed.</h2>
            <p>
              Straightforward architecture keeps the hot path legible. Loops begin
              in the interpreter and can move into the native JIT when they become hot.
            </p>
            <div className="portability-row">
              <span>x86-64 JIT</span>
              <span>aarch64</span>
              <span>wasm32</span>
            </div>
          </div>

          <ol className="pipeline" aria-label="Zipp execution pipeline">
            {pipeline.map((item, index) => (
              <li key={item.step}>
                <div className="pipeline-index">{item.step}</div>
                <div>
                  <strong>{item.title}</strong>
                  <span>{item.detail}</span>
                </div>
                {index < pipeline.length - 1 && <span className="pipeline-arrow" aria-hidden="true">→</span>}
              </li>
            ))}
          </ol>
        </section>

        <section className="quickstart-section section-wrap" id="quickstart">
          <div className="quickstart-copy">
            <p className="section-kicker">Build from source</p>
            <h2>Up and running in four lines.</h2>
            <p>
              Stable Rust is the only requirement. Release builds produce a single
              executable you can place on your PATH.
            </p>
            <div className="requirement-list" aria-label="Build details">
              <span><b>01</b> Clone the repository</span>
              <span><b>02</b> Build the release binary</span>
              <span><b>03</b> Run JavaScript</span>
            </div>
          </div>

          <div className="terminal-block">
            <div className="terminal-header">
              <span>Terminal</span>
              <button type="button" onClick={copyInstall} aria-live="polite">
                {copied ? 'Copied!' : 'Copy'}
              </button>
            </div>
            <pre><code>{installCommands}</code></pre>
            <div className="terminal-output">
              <span aria-hidden="true">→</span> hello, world
            </div>
          </div>
        </section>

        <section className="closing-cta section-wrap">
          <div>
            <p className="section-kicker">Open source · Apache-2.0</p>
            <h2>See how the engine moves.</h2>
          </div>
          <GithubLink className="button button-light">View Zipp on GitHub</GithubLink>
        </section>
      </main>

      <footer className="site-footer section-wrap">
        <a className="brand brand-footer" href="#top" aria-label="Back to top">
          <span className="brand-mark" aria-hidden="true">z</span>
          <span className="brand-name">zipp</span>
        </a>
        <p>A clean-sheet JavaScript engine written in Rust.</p>
        <GithubLink>GitHub</GithubLink>
      </footer>
    </div>
  )
}

export default App
