import { useEffect, useState } from 'react'

const clips = [
  { id: 'python-life', tag: 'PYTHON → ZIPP WASM → WEBGL2', title: 'A living grid, driven by Python.', description: 'Python runs in Zipp’s WebAssembly VM. Each generation submits a Conway-Life graph to the browser’s WebGL2 backend, then draws the returned state.', alt: 'Recorded Python source running in the Zipp playground with a changing green Game of Life grid and a verified WebGL2 backend.' },
  { id: 'nca-memory', tag: 'NATIVE PYTORCH / CUDA', title: 'Write a memory. Follow a query.', description: 'Actual private memory changes as new associations are written. A query visits the ring’s cells, and the model checks its answer while shared weights remain frozen.', alt: 'Recorded native NCA memory replay with two selected RTX 5090 devices, changing memory heatmap and highlighted query cells.' },
  { id: 'nca-language', tag: 'NATIVE PYTORCH / CUDA', title: 'Watch the next byte emerge.', description: 'The separate causal language model generates template-style text while its recurrent cache changes. The supplied checkpoint has a 17-byte context.', alt: 'Recorded NCA byte-language generation with changing recurrent activations and generated text on CUDA.' },
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
      <p className="recorded-intro">Browser Python and native CUDA have different jobs. These captures show both execution paths on actual hardware.</p>
      <img className="execution-diagram" src="/demos/python-wasm-flow.svg" alt="Python source compiles to Zipp bytecode, executes in the WASM VM, and sends GPU graphs to a JavaScript WebGL2 or WebGPU host." width="1400" height="410" loading="lazy" />
      <div className="recorded-grid">{clips.map(clip => <article key={clip.id} className={`recorded-card recorded-${clip.id}`}>
        <div className="recorded-card-heading"><span>{clip.tag}</span><h3>{clip.title}</h3><p>{clip.description}</p></div>
        <a href={`/demos/${clip.id}.gif`} target="_blank" rel="noreferrer" aria-label={`Open the full recording: ${clip.title}`}>
          <img src={`/demos/${clip.id}.${playing ? 'gif' : 'png'}`} alt={clip.alt} width="1400" height={clip.id === 'python-life' ? 960 : 1200} loading="lazy" />
        </a>
      </article>)}</div>
      <div className="recorded-actions"><a className="button button-primary" href="/playground/">Try Python in the playground ↗</a>
        <a href="https://github.com/f2i-com/zipp.org/blob/main/README.md#start-the-local-gpu-lab" target="_blank" rel="noreferrer">Run the native NCA lab locally ↗</a></div>
      <p className="recording-note">Recorded locally with two RTX 5090s. Selecting both CUDA devices runs independent experiments; the browser uses one adapter to draw.
        These GIFs demonstrate behavior, not GPU benchmarks. <a href="/demos/provenance.json">Capture details</a></p>
    </section>
  )
}
