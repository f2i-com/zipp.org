/// <reference lib="webworker" />

type WarmRequest = {
  type: 'warm'
  moduleUrl: string
  wasmUrl: string
}

type RunRequest = {
  type: 'run'
  runId: number
  source: string
  moduleUrl: string
  wasmUrl: string
}

type Request = WarmRequest | RunRequest

type ZippEngine = {
  initScript(source: string): unknown
  takeOutput(): unknown
  dispose(): void
}

type ZippModule = {
  default(input?: { module_or_path: string | URL }): Promise<unknown>
  Engine: new () => ZippEngine
}

const workerScope: DedicatedWorkerGlobalScope = self as unknown as DedicatedWorkerGlobalScope

// V8 compiles WebAssembly functions lazily, with Liftoff, the first time each one
// is actually called. On this module that is worth ~40 ms — measured by moving it
// with --no-wasm-lazy-compilation, which shifts the same cost from the first run
// into instantiation. Because a Worker is discarded after every run, every run
// used to be a first run and paid it, which is why a script whose real cost is
// ~1.2 ms reported ~68 ms.
//
// Running representative JavaScript once before the user's script forces those
// functions to be compiled ahead of time. It is deliberately not the user's code:
// the point is only to walk the parser, compiler, dispatch loop and the array,
// string, closure and console paths a typical snippet reaches.
const WARMUP = `
  const rows = [{ id: "w", n: 2 }, { id: "x", n: 4 }];
  const s = rows.filter((r) => r.n > 1).map((r) => r.id + ":" + r.n).join(" | ");
  const t = rows.reduce((a, r) => a + r.n, 0);
  for (let i = 0; i < 64; i++) { s.indexOf("x"); }
  console.log(s, t);
`

let modulePromise: Promise<ZippModule> | null = null

// Load and instantiate once per Worker, whether that was triggered by a warm-up
// or by a run that arrived before warming finished.
function loadModule(moduleUrl: string, wasmUrl: string): Promise<ZippModule> {
  if (!modulePromise) {
    modulePromise = (async () => {
      const zipp = (await import(/* @vite-ignore */ moduleUrl)) as ZippModule
      await zipp.default({ module_or_path: wasmUrl })
      return zipp
    })()
  }
  return modulePromise
}

function warmUp(zipp: ZippModule) {
  let engine: ZippEngine | undefined
  try {
    engine = new zipp.Engine()
    engine.initScript(WARMUP)
    engine.takeOutput()
  } catch {
    // A failed warm-up must never fail the run it was meant to speed up.
  } finally {
    engine?.dispose()
  }
}

workerScope.onmessage = async (event: MessageEvent<Request>) => {
  const request = event.data

  if (request.type === 'warm') {
    try {
      const zipp = await loadModule(request.moduleUrl, request.wasmUrl)
      warmUp(zipp)
      workerScope.postMessage({ type: 'warmed' })
    } catch (error) {
      workerScope.postMessage({
        type: 'warm-failed',
        message: error instanceof Error ? error.message : String(error),
      })
    }
    return
  }

  if (request.type !== 'run') return

  let engine: ZippEngine | undefined
  try {
    const zipp = await loadModule(request.moduleUrl, request.wasmUrl)
    workerScope.postMessage({ type: 'started', runId: request.runId })

    const started = performance.now()
    engine = new zipp.Engine()
    engine.initScript(request.source)
    const captured = engine.takeOutput()
    const output = Array.isArray(captured) ? captured.map((line) => String(line)) : []

    workerScope.postMessage({
      type: 'result',
      runId: request.runId,
      output,
      elapsedMs: performance.now() - started,
    })
  } catch (error) {
    workerScope.postMessage({
      type: 'error',
      runId: request.runId,
      message: error instanceof Error ? error.message : String(error),
    })
  } finally {
    engine?.dispose()
  }
}

export {}
