import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { runInNewContext } from 'node:vm'
import { randomInt } from 'node:crypto'
import { injectStorySeed } from '../src/storySeed.ts'
import init, { Engine } from '../public/wasm/zipp_wasm.js'

await init({ module_or_path: readFileSync(new URL('../public/wasm/zipp_wasm_bg.wasm', import.meta.url)) })
// Extract the authored template literal so tests execute precisely the editor sample.
const sourceFile = readFileSync(new URL('../src/App.tsx', import.meta.url), 'utf8')
const template = sourceFile.match(/id: 'adventure'[\s\S]*?source: (`[\s\S]*?`) \}/)[1]
const sample = runInNewContext(template, {}, { timeout: 1000 })
const execute = source => {
  const engine = new Engine()
  try { engine.initScript(source); return engine.takeOutput().join('\n') }
  finally { engine.dispose() }
}
const seeded = seed => injectStorySeed(sample, seed)

test('the actual WASM engine runs a random story with a save-game summary', () => {
  const output = execute(seeded(randomInt(0x100000000)))
  assert.match(output, /THE LITTLE CHRONICLES \/ seed \d+/)
  assert.match(output, /Chapter 1:/)
  assert.match(output, /EPILOGUE/)
  const save = JSON.parse(output.split('SAVE GAME ')[1])
  assert.ok(save.chapters > 0 && save.chapters <= 5)
  assert.ok(save.spirit >= 0 && save.spirit <= 10)
  assert.ok(save.uniquePlaces <= save.chapters)
})

test('a seed reproduces the same story and different seeds produce different stories', () => {
  const first = execute(seeded(42))
  assert.equal(first, execute(seeded(42)))
  assert.notEqual(first, execute(seeded(12345)))
  const replay = sample.replace('typeof STORY_SEED === "number" ? STORY_SEED : 42', '42')
  assert.equal(first, execute(injectStorySeed(replay, 9999)))
})

test('the host bridge accepts only a bounded numeric seed and supports standalone replay', () => {
  for (const seed of ['1; throw 2', NaN, Infinity, -1, 2 ** 32]) assert.equal(injectStorySeed(sample, seed), sample)
  assert.match(execute(sample), /seed 42/)
})

test('encounters exercise both outcomes and exhaustion ends longer journeys', () => {
  const output = Array.from({ length: 12 }, (_, seed) => execute(seeded(seed).replace('const CHAPTERS = 5', 'const CHAPTERS = 20'))).join('\n')
  assert.match(output, /A keepsake joins the collection/)
  assert.match(output, /Spirit remaining/)
  assert.match(output, /The quest can wait/)
})
