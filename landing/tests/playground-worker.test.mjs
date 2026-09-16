// The /playground engine Worker (public/playground/engine.worker.js, synced
// from crates/zipp-wasm/playground) driven in Node with the checked-in Python
// engine pair and the gpu-lab runtime. A GPU readback yields to the event loop
// on WebGPU, so here the CPU backend's read() waits on a timer to stand in for
// mapAsync.
//
// Pressing Run while the previous program's graph was still in flight used to
// hand the new program's first graph to the busy shared runtime: it failed
// with BUSY, and a program that treats a failed frame as fatal (the Game of
// Life sample) stopped. The worker now waits for the retiring generation.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

const realFetch = globalThis.fetch
globalThis.fetch = async (input, init) => {
  const url = new URL(typeof input === 'string' ? input : input.url ?? String(input))
  if (url.protocol !== 'file:') return realFetch(input, init)
  url.search = ''
  // The compiled-kernel download is slow enough to switch backends during it.
  if (url.pathname.endsWith('kernels.wasm')) await new Promise(resolve => setTimeout(resolve, 150))
  const type = url.pathname.endsWith('.wasm') ? 'application/wasm' : 'text/javascript'
  return new Response(await readFile(fileURLToPath(url)), { headers: { 'content-type': type } })
}
const posted = []
globalThis.self = globalThis
globalThis.postMessage = message => { posted.push(message) }

const { CPUBackend } = await import('../public/gpu-lab/src/backends/cpu.mjs')
const read = CPUBackend.prototype.read
CPUBackend.prototype.read = async function (handle) {
  await new Promise(resolve => setTimeout(resolve, 150))
  return read.call(this, handle)
}
await import('../public/playground/engine.worker.js')

const tick = ms => new Promise(resolve => setTimeout(resolve, ms))
async function send(message) {
  await globalThis.onmessage({ data: message })
  const reply = posted.findLast(m => m.id === message.id)
  assert.ok(reply, `a reply to ${message.type}`)
  return reply
}
const PROGRAM = [
  'from zipp_gpu import Graph',
  'g = Graph()',
  'a = g.tensor([1, 2, 3, 4])',
  'def show(result):',
  '    print("ok", result["outputs"]["result"]["data"])',
  'def failed(error):',
  '    print("failed", error.code, str(error))',
  'g.submit(show, failed, result=(a * 2).relu())',
  'print("submitted")',
  '',
].join('\n')
const run = (id, gpuBackend = 'cpu-js') => ({ id, type: 'run', language: 'python', files: { 'main.py': PROGRAM }, entry: 'main.py', argv: [], budget: 2e9, gpuBackend })
const printed = () => posted.flatMap(m => (m.console || []).map(line => line.text))

test('a re-run waits for the previous engine\'s in-flight GPU graph', async () => {
  assert.equal((await send({ id: 1, type: 'hello' })).type, 'hello')
  assert.equal((await send(run(2))).type, 'ran')
  await tick(20)
  assert.equal((await send(run(3))).type, 'ran')
  for (let i = 0; i < 100 && !printed().some(text => text.startsWith('ok')); i++) await tick(20)
  assert.deepEqual(printed().filter(text => !/^submitted$/.test(text)), ['ok [2.0, 4.0, 6.0, 8.0]'])
  assert.equal(printed().filter(text => text === 'submitted').length, 2)
  await send({ id: 4, type: 'stop' })
})

test('a backend chosen while another is still being created is the one used', async () => {
  posted.length = 0
  assert.equal((await send(run(5, 'wasm'))).type, 'ran')
  await tick(20)
  assert.equal((await send(run(6, 'cpu-js'))).type, 'ran')
  for (let i = 0; i < 100 && !printed().some(text => text.startsWith('ok')); i++) await tick(20)
  assert.deepEqual(printed().filter(text => !/^submitted$/.test(text)), ['ok [2.0, 4.0, 6.0, 8.0]'])
  const backends = posted.filter(m => m.gpu).map(m => m.gpu.backend)
  assert.deepEqual(backends, ['cpu-js'], 'the stale wasm runtime is discarded, never installed')
  await send({ id: 7, type: 'stop' })
})
