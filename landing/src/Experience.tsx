import { useState } from 'react'

const layers = [
  { name: 'Your JavaScript app', label: '01 / THE HOST', symbol: '{ }', copy: 'Your app runs in the browser’s JavaScript environment. It loads ZIPP WASM and chooses which capabilities to expose to guest scripts.' },
  { name: 'ZIPP WASM', label: '02 / THE ENGINE', symbol: 'ϟ', copy: 'ZIPP’s Rust JavaScript engine is compiled to WebAssembly. It interprets guest JavaScript inside its own VM, on top of your existing JavaScript environment.' },
  { name: 'Sandboxed JavaScript', label: '03 / THE GUEST', symbol: '</>', copy: 'User code runs inside ZIPP. It can use only the host capabilities you provide. Instruction limits and host-enforced deadlines help keep execution under control.' },
]

export function SandboxStack() {
  const [active, setActive] = useState(1)

  return (
    <aside className="stack-lab" aria-label="Explore JavaScript on top of JavaScript">
      <div className="lab-heading"><span className="lab-label">A LITTLE ENGINE. A WHOLE NEW LAYER.</span><span aria-hidden="true">✳</span></div>
      <div className="stack-caption"><span>JavaScript</span><span>on JavaScript.</span></div>
      <p className="stack-hint">Same language. A sandbox of its own.</p>
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
  { name: 'Softn', domain: 'softn.com', eyebrow: 'THE DIRECT CONNECTION', title: 'JavaScript, meet your sandbox.', description: 'Softn uses ZIPP WASM to execute sandboxed JavaScript on top of JavaScript. The host loads the WebAssembly engine, while guest code runs inside ZIPP’s own JavaScript VM.', tag: 'Powered by ZIPP WASM', mark: '{ softn }' },
  { name: 'Outerstead', domain: 'outerstead.com', eyebrow: 'THE NEXT LAYER OF PLAY', title: 'An engine behind the adventure.', description: 'Outerstead is a game built with Softn. Because Softn uses ZIPP WASM, ZIPP is part of the stack behind the game: Outerstead uses Softn, and Softn uses ZIPP.', tag: 'Built with Softn', mark: 'outerstead ↗' },
]

export function ProjectShowcase() {
  const [active, setActive] = useState(0)
  return (
    <section className="projects-section section-wrap" id="use-cases" aria-labelledby="projects-title">
      <div className="projects-heading"><p className="section-kicker">SMALL ENGINE. REAL PROJECTS.</p><h2 id="projects-title">Good things<br />have a little <em>ZIPP.</em></h2><p>From a JavaScript sandbox to a game built on top of it.<br />Here’s where the engine comes out to play.</p></div>
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
      <div className="ecosystem">
        <div className="ecosystem-intro"><span className="lab-label">FOLLOW THE CONNECTION</span><p>One stack.<br />Different possibilities.</p></div>
        <div className="ecosystem-interaction">
          <div className="ecosystem-path" aria-label="Explore the project connections">
            <button type="button" aria-pressed={active === 0} aria-controls="ecosystem-detail" onClick={() => setActive(0)}>Outerstead<small>The game</small></button>
            <span aria-label="uses">→</span>
            <button type="button" aria-pressed={active === 1} aria-controls="ecosystem-detail" onClick={() => setActive(1)}>Softn<small>The project</small></button>
            <span aria-label="uses">→</span>
            <button type="button" aria-pressed={active === 2} aria-controls="ecosystem-detail" onClick={() => setActive(2)}>ZIPP WASM<small>The engine</small></button>
          </div>
          <p id="ecosystem-detail" className="ecosystem-detail" aria-live="polite">{[
            'Outerstead uses Softn, bringing ZIPP WASM into the game’s underlying stack.',
            'Softn directly uses ZIPP WASM to run sandboxed JavaScript within a JavaScript host.',
            'ZIPP WASM is the WebAssembly build of ZIPP’s Rust engine. Guest scripts execute inside its VM.',
          ][active]}</p>
        </div>
      </div>
    </section>
  )
}
