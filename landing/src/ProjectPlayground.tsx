import { useEffect, useRef, useState } from 'react'

const PLAYGROUND_URL = '/playground/'

/**
 * The folder playground, framed from /playground/. That page starts its engine
 * Worker as soon as it loads, and the Worker fetches the JavaScript + Python
 * engine (8.4 MB, about 2 MB compressed), so the frame is created on intent
 * rather than on page load: when the reader points at, touches or tabs into
 * this section, keeps it in view for a moment, follows a #playground link, or
 * presses the button. `loading="lazy"` was not enough: Chromium's lazy-frame
 * distance is large enough to load it for a visitor who never scrolls.
 */
export function ProjectPlayground() {
  const holderRef = useRef<HTMLDivElement | null>(null)
  const frameRef = useRef<HTMLIFrameElement | null>(null)
  const [wanted, setWanted] = useState(false)
  const [focusFrame, setFocusFrame] = useState(false)
  const want = () => setWanted(true)

  useEffect(() => {
    if (wanted) return
    const fromHash = () => window.location.hash === '#playground'
    if (fromHash()) {
      setWanted(true)
      return
    }
    const onHash = () => { if (fromHash()) setWanted(true) }
    window.addEventListener('hashchange', onHash)
    // Scrolled to and kept in view: a third of the frame for 600 ms.
    let dwell: number | undefined
    const observer = typeof IntersectionObserver === 'undefined' ? null : new IntersectionObserver((entries) => {
      window.clearTimeout(dwell)
      if (entries.some((entry) => entry.isIntersecting)) dwell = window.setTimeout(() => setWanted(true), 600)
    }, { threshold: 0.33 })
    if (holderRef.current) observer?.observe(holderRef.current)
    return () => {
      window.removeEventListener('hashchange', onHash)
      observer?.disconnect()
      window.clearTimeout(dwell)
    }
  }, [wanted])

  // The button that asked for the frame disappears with it; hand focus on.
  useEffect(() => {
    if (wanted && focusFrame) frameRef.current?.focus()
  }, [wanted, focusFrame])

  return (
    <section className="project-playground section-wrap" id="playground" aria-labelledby="project-playground-title"
      onPointerEnter={want} onTouchStart={want}
      onFocus={(event) => {
        // Focus that lands on the standby button (Shift+Tab from below) moves into the frame.
        if (holderRef.current?.contains(event.target)) setFocusFrame(true)
        want()
      }}>
      <div className="project-playground-heading">
        <div><p className="section-kicker">YOUR FILES. YOUR PROGRAM.</p><h2 id="project-playground-title">Python or JavaScript.<br />Bring the whole folder.</h2></div>
        <p>Load a local project, choose an entry file and run it on Zipp WASM.
          Edit code, browse data, watch the canvas, and select WebGL2 or WebGPU
          for supported Torch inference, SGD training and Python compute graphs. Files stay in your browser.</p>
      </div>
      <div className="project-playground-links">
        <a className="button button-primary" href={PLAYGROUND_URL} target="_blank" rel="noreferrer">Open full playground ↗</a>
        <a href="https://github.com/f2i-com/zipp.org/blob/main/crates/zipp-wasm/playground/README.md" target="_blank" rel="noreferrer">Examples &amp; API guide ↗</a>
        <span>Try Samples → Python: Game of Life → Run</span>
      </div>
      {wanted ? (
        <iframe ref={frameRef} className="project-playground-frame" src={PLAYGROUND_URL} title="Zipp folder playground: Python, JavaScript and browser GPU" />
      ) : (
        <div ref={holderRef} className="project-playground-frame project-playground-standby">
          <p><strong>The playground loads when you get here.</strong> It runs the JavaScript + Python engine,
            an 8.4 MB download (about 2 MB compressed), so it waits until you scroll to it, point at it or ask.</p>
          <button type="button" className="button button-secondary" onClick={() => { setFocusFrame(true); want() }}>Load the playground here</button>
        </div>
      )}
      <p className="project-playground-note">The Python-enabled engine runs inside a Worker with execution deadlines.
        Browser GPU support depends on your hardware and browser; the console reports the actual backend.
        Eager Torch supports CPU training. Experimental <code>torch.compile</code> supports GPU inference and dense training steps with SGD, momentum, Adam or AdamW; results arrive through a callback. Try Samples → Python: Torch ML training (GPU) to watch the loss fall.</p>
    </section>
  )
}
