import { useEffect, useState } from 'react'

const clips = [
  { id: 'python-life', tag: 'PYTHON → ZIPP WASM → WEBGL2', title: 'A living grid, driven by Python.', description: 'Python runs in Zipp’s WebAssembly VM. The displayed Life rules use ordinary Torch operations, compiled for WebGL2 by a separate playground driver.', alt: 'Recorded portable Torch Life source running in the Zipp playground with a changing green Game of Life grid and a verified WebGL2 backend.' },
]

export function RecordedDemos() {
  const [playing, setPlaying] = useState(false)
  useEffect(() => {
    const preference = matchMedia('(prefers-reduced-motion: reduce)')
    const update = () => setPlaying(!preference.matches)
    update(); preference.addEventListener('change', update)
    return () => preference.removeEventListener('change', update)
  }, [])
  return (
    <section className="recorded-demos section-wrap" id="recorded-demos" aria-labelledby="recorded-title">
      <div className="recorded-heading"><div><p className="section-kicker">REAL CODE. RECORDED RUNS.</p><h2 id="recorded-title">See what’s happening.</h2></div>
        <button className="button button-secondary" type="button" aria-pressed={playing} onClick={() => setPlaying(!playing)}>{playing ? 'Pause animations' : 'Play animations'}</button></div>
      <p className="recorded-intro">Python drives a glider gun, oscillators and growing patterns in one shared Life grid. The browser GPU computes every generation.</p>
      <img className="execution-diagram" src="/demos/python-wasm-flow.svg" alt="Python source compiles to Zipp bytecode, executes in the WASM VM, and sends GPU graphs to a JavaScript WebGL2 or WebGPU host." width="1400" height="410" loading="lazy" />
      <div className="recorded-grid">{clips.map(clip => <article key={clip.id} className={`recorded-card recorded-${clip.id}`}>
        <div className="recorded-card-heading"><span>{clip.tag}</span><h3>{clip.title}</h3><p>{clip.description}</p></div>
        <a href={`/demos/${clip.id}.gif`} target="_blank" rel="noreferrer" aria-label={`Open the full recording: ${clip.title}`}>
          <img src={`/demos/${clip.id}.${playing ? 'gif' : 'png'}`} alt={clip.alt} width="1400" height={clip.id === 'python-life' ? 960 : 1200} loading="lazy" />
        </a>
      </article>)}</div>
      <div className="recorded-actions"><a className="button button-primary" href="/playground/">Try Python in the playground ↗</a></div>
      <p className="recording-note">Recorded from Python running on Zipp WASM with an RTX 5090 WebGL2 backend.
        The GIF demonstrates behavior, not GPU benchmarks. <a href="/demos/provenance.json">Capture details</a></p>
    </section>
  )
}
