import { useEffect, useId, useRef, useState } from 'react'
import {
  EMBED_CSP, EMBED_RELEASE, ENGINES, KIT, PYTHON_BASE, RELEASE_TAG_URL, STARTER, TORCH_PACKAGE,
  capabilityBlurb, capabilityExample, capabilityOutput, capabilityTitle, defaultExample, defaultOutput,
  engineById, formatMegabytes, releaseAssetUrl, type EmbedEngine,
} from './embed-data.mjs'

type CopyState = 'idle' | 'copied' | 'selected'

/**
 * The public way to put ZIPP in a page: one engine `.wasm` plus `zipp.mjs` from
 * the release's embed kit. Static on purpose: it shows code, files and the
 * expected output, and downloads nothing until a link is followed (a live
 * runner here would fetch megabytes for every visitor, and the home page's
 * policy does not admit the loader's blob: import).
 */
export function EmbedSection() {
  const [engineId, setEngineId] = useState<EmbedEngine['id']>(ENGINES[0].id)
  const [copyState, setCopyState] = useState<CopyState>('idle')
  const codeRef = useRef<HTMLElement | null>(null)
  const resetTimer = useRef<number | undefined>(undefined)
  const groupName = `embed-engine-${useId().replace(/[^a-zA-Z0-9_-]/g, '')}`
  const engine = engineById(engineId)
  const python = engine.language === 'python'
  const code = defaultExample(engine)

  useEffect(() => () => window.clearTimeout(resetTimer.current), [])

  const settle = (state: CopyState) => {
    setCopyState(state)
    window.clearTimeout(resetTimer.current)
    resetTimer.current = window.setTimeout(() => setCopyState('idle'), 4000)
  }

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code)
      settle('copied')
    } catch {
      // No clipboard access (an insecure page, or a browser that refuses):
      // select the code so the reader can copy it themselves.
      const node = codeRef.current
      const selection = window.getSelection()
      if (node && selection) {
        const range = document.createRange()
        range.selectNodeContents(node)
        selection.removeAllRanges()
        selection.addRange(range)
      }
      settle('selected')
    }
  }

  const choose = (id: EmbedEngine['id']) => {
    setEngineId(id)
    setCopyState('idle')
  }

  const copyLabel = { idle: 'Copy', copied: 'Copied', selected: 'Selected' }[copyState]
  const copyMessage = {
    idle: '',
    copied: `The ${engine.label} example is copied to the clipboard.`,
    selected: 'The browser did not allow copying. The example is selected: press Ctrl+C, or Command+C on a Mac, to copy it.',
  }[copyState]

  return (
    <section className="embed-section section-wrap" id="embed" aria-labelledby="embed-title">
      <div className="section-heading split-heading">
        <div>
          <p className="section-kicker">Embed ZIPP on your site</p>
          <h2 id="embed-title">Two files. A sandbox that permits nothing.</h2>
        </div>
        <p>
          Put ZIPP in your own page with an engine <code>.wasm</code> from the v{EMBED_RELEASE} release
          and <code>zipp.mjs</code>, the loader from its embed kit. No build step, no package manager:
          serve the files and import the loader. The playgrounds on this page load the engine’s
          internal two-file packaging; this is the public way in.
        </p>
      </div>

      <div className="embed-layout">
        <div className="embed-steps">
          <fieldset className="embed-step embed-chooser">
            <legend><span className="embed-step-number" aria-hidden="true">1</span>Choose an engine</legend>
            {ENGINES.map((option) => (
              <label className="engine-option" key={option.id}>
                <input
                  type="radio"
                  name={groupName}
                  value={option.id}
                  checked={option.id === engineId}
                  onChange={() => choose(option.id)}
                />
                <span className="engine-option-text">
                  <strong>{option.label}</strong>
                  <span>{option.summary}</span>
                  <small>{formatMegabytes(option.bytes)} · {formatMegabytes(option.brBytes)} Brotli</small>
                </span>
              </label>
            ))}
            {python && (
              <p className="embed-note">
                Torch is built into this engine. To skip it until a program needs it, take{' '}
                <a href={releaseAssetUrl(PYTHON_BASE.file)}>{PYTHON_BASE.file}</a> ({formatMegabytes(PYTHON_BASE.bytes)})
                and add the{' '}
                <a href={releaseAssetUrl(TORCH_PACKAGE.file)}>torch package</a> ({formatMegabytes(TORCH_PACKAGE.bytes)}) on
                demand: <code>{'loadZipp({ engine, packages: [torch] })'}</code>.
              </p>
            )}
          </fieldset>

          <div className="embed-step">
            <h3><span className="embed-step-number" aria-hidden="true">2</span>Download the two files</h3>
            <ul className="embed-downloads">
              <li>
                <a href={releaseAssetUrl(engine.file)}><code>{engine.file}</code></a>
                <span>The {engine.label} engine · {formatMegabytes(engine.bytes)}</span>
              </li>
              <li>
                <a href={releaseAssetUrl(KIT.file)}><code>{KIT.file}</code></a>
                <span>Take <code>{KIT.loader}</code> out of it: the one loader file every engine needs · {Math.round(KIT.bytes / 1000)} KB</span>
              </li>
            </ul>
            <p className="embed-note">
              Smaller: <a href={releaseAssetUrl(`${engine.file}.br`)}><code>{engine.file}.br</code></a>{' '}
              ({formatMegabytes(engine.brBytes)}) is the same engine, about a quarter of the size. Serve it
              with <code>Content-Encoding: br</code> and the browser decodes it; <code>zipp.mjs</code> 1.2.0
              also loads a <code>.wasm.br</code> URL itself, decompressing it where it can or falling back to
              the plain <code>.wasm</code> beside it.
            </p>
          </div>

          <div className="embed-step embed-starter">
            <h3>Or start from a project</h3>
            <p>
              <code>index.html</code>, <code>main.mjs</code> (the example, with loading and error messages) and
              a README with the exact download and serve commands. The engine and loader are not inside: it
              stays a few kilobytes.
            </p>
            <a className="button button-secondary" href={STARTER.path} download>
              Download the starter project <span className="embed-file-size">.zip · 4 KB</span>
            </a>
          </div>
        </div>

        <div className="embed-code">
          <div className="embed-window">
            <div className="embed-window-header">
              <span className="terminal-dots" aria-hidden="true"><i /><i /><i /></span>
              <h3 id="embed-code-title"><span className="embed-step-number" aria-hidden="true">3</span>main.mjs</h3>
              <button type="button" className="embed-copy" onClick={copy}>
                {copyLabel}{copyState === 'idle' && <span className="sr-only"> the main.mjs example</span>}
              </button>
            </div>
            {copyState === 'selected' && (
              <p className="embed-copy-hint" aria-hidden="true">The browser blocked copying. The code is selected: press Ctrl+C, or ⌘C on a Mac.</p>
            )}
            <pre tabIndex={0} aria-label={`Default-deny example for the ${engine.label} engine`}><code ref={codeRef}>{code}</code></pre>
            <div className="embed-output">
              <p>Expected console output</p>
              <pre tabIndex={0} aria-label="Expected console output"><code>{defaultOutput(engine).join('\n')}</code></pre>
            </div>
          </div>
          <div className="sr-only" role="status" aria-live="polite">{copyMessage}</div>

          <details className="embed-more">
            <summary>{capabilityTitle(engine)}</summary>
            <p>{capabilityBlurb(engine)}</p>
            <pre tabIndex={0} aria-label={`Example with one permission for the ${engine.label} engine`}><code>{capabilityExample(engine)}</code></pre>
            <p className="embed-more-output">Prints <code>{capabilityOutput(engine).join(' · ')}</code>.</p>
          </details>

          <div className="embed-step embed-serve">
            <h3><span className="embed-step-number" aria-hidden="true">4</span>Serve it over HTTP</h3>
            <p>
              Browsers will not run it from a <code>file://</code> address. In the folder, run{' '}
              <code>python3 -m http.server 8000</code> or <code>npx serve .</code> and open the address it
              prints. A page that sends a Content-Security-Policy must allow:
            </p>
            <pre className="embed-csp" tabIndex={0} aria-label="Content-Security-Policy directives ZIPP needs"><code>{EMBED_CSP}</code></pre>
            <p className="embed-links">
              <a className="text-link" href="/wasm/#from-0-0-26">Permissions and limits</a>
              <a className="text-link" href="/sandbox/playground/">Try each permission in the sandbox playground</a>
              <a className="text-link" href={RELEASE_TAG_URL} target="_blank" rel="noreferrer">All v{EMBED_RELEASE} downloads</a>
            </p>
          </div>
        </div>
      </div>
    </section>
  )
}
