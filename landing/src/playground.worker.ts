/// <reference lib="webworker" />

type RunRequest = {
  type: 'run'
  runId: number
  source: string
  moduleUrl: string
  wasmUrl: string
}

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

workerScope.onmessage = async (event: MessageEvent<RunRequest>) => {
  const request = event.data
  if (request.type !== 'run') return

  let engine: ZippEngine | undefined
  try {
    const zipp = await import(/* @vite-ignore */ request.moduleUrl) as ZippModule
    await zipp.default({ module_or_path: request.wasmUrl })
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
