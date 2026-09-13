export function ProjectPlayground() {
  return (
    <section className="project-playground section-wrap" id="playground" aria-labelledby="project-playground-title">
      <div className="project-playground-heading">
        <div><p className="section-kicker">YOUR FILES. YOUR PROGRAM.</p><h2 id="project-playground-title">Python or JavaScript.<br />Bring the whole folder.</h2></div>
        <p>Load a local project, choose an entry file and run it on Zipp WASM.
          Edit code, browse data, watch the canvas, and select WebGL2 or WebGPU
          for supported Torch inference and Python compute graphs. Files stay in your browser.</p>
      </div>
      <div className="project-playground-links">
        <a className="button button-primary" href="/playground/" target="_blank" rel="noreferrer">Open full playground ↗</a>
        <a href="https://github.com/f2i-com/zipp.org/blob/main/crates/zipp-wasm/playground/README.md" target="_blank" rel="noreferrer">Examples &amp; API guide ↗</a>
        <span>Try Samples → Python: GPU compute → Run</span>
      </div>
      <iframe className="project-playground-frame" src="/playground/" title="Zipp folder playground: Python, JavaScript and browser GPU" loading="lazy" />
      <p className="project-playground-note">The Python-enabled engine runs inside a Worker with execution deadlines.
        Browser GPU support depends on your hardware and browser; the console reports the actual backend.
        Eager Torch supports CPU training. Experimental <code>torch.compile(model)</code> records supported GPU inference; results arrive through a callback.</p>
    </section>
  )
}
