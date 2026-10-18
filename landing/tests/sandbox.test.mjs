import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { existsSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { checkSandbox, DEFAULT_OUT, syncSandbox } from '../scripts/sync-sandbox.mjs'

const sha256 = bytes => createHash('sha256').update(bytes).digest('hex')
const leb = n => {
  const out = []
  do {
    let byte = n & 0x7f
    n >>>= 7
    if (n) byte |= 0x80
    out.push(byte)
  } while (n)
  return Buffer.from(out)
}
// The smallest module a release could carry: the header and one custom
// section, `zipp.meta`, holding the record the loader reads.
function moduleWithMeta(meta) {
  const name = Buffer.from('zipp.meta')
  const body = meta === null ? Buffer.alloc(0) : Buffer.concat([leb(name.length), name, Buffer.from(JSON.stringify(meta))])
  const header = Buffer.from([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00])
  return meta === null ? header : Buffer.concat([header, Buffer.from([0x00]), leb(body.length), body])
}
const LOADER = 'export const ZIPP_LOADER_VERSION = "1.1.0";\nexport async function loadZipp(source) { return source; }\n'
const COMMIT = 'a'.repeat(40)

function release(version, { meta = { format: 1, version, commit: COMMIT, variant: 'javascript', languages: ['javascript'] }, modulesJson = true } = {}) {
  const dir = mkdtempSync(path.join(tmpdir(), 'zipp-sandbox-'))
  const kit = path.join(dir, `zipp-${version}-wasm-embed-kit`)
  mkdirSync(kit)
  const bytes = moduleWithMeta(meta)
  writeFileSync(path.join(dir, `zipp-${version}-javascript.wasm`), bytes)
  writeFileSync(path.join(dir, `zipp-${version}-javascript-lite.wasm`), moduleWithMeta({ ...meta, variant: 'lite' }))
  writeFileSync(path.join(kit, 'zipp.mjs'), LOADER)
  writeFileSync(path.join(kit, 'SHA256SUMS'), `${sha256(Buffer.from(LOADER))}  zipp.mjs\n`)
  if (modulesJson) {
    writeFileSync(path.join(kit, 'modules.json'), JSON.stringify({ version, modules: [{ file: `zipp-${version}-javascript.wasm`, sha256: modulesJson === 'wrong' ? '0'.repeat(64) : sha256(bytes) }] }))
  }
  return { dir, kit, bytes, cleanup: () => rmSync(dir, { recursive: true, force: true }) }
}

test('sync-sandbox copies the loader and the JavaScript engine and records them', async () => {
  const r = release('9.9.9')
  const out = path.join(r.dir, 'out')
  try {
    mkdirSync(out)
    writeFileSync(path.join(out, 'zipp-9.9.8-javascript.wasm'), 'an older release')
    const manifest = await syncSandbox(r.dir, out)
    assert.deepEqual(manifest, {
      version: '9.9.9',
      file: 'zipp-9.9.9-javascript.wasm',
      sha256: sha256(r.bytes),
      bytes: r.bytes.length,
      commit: COMMIT,
      loader: { file: 'zipp.mjs', sha256: sha256(Buffer.from(LOADER)), version: '1.1.0' },
    })
    assert.deepEqual(readdirSync(out).sort(), ['manifest.json', 'zipp-9.9.9-javascript.wasm', 'zipp.mjs'])
    assert.deepEqual(JSON.parse(readFileSync(path.join(out, 'manifest.json'), 'utf8')), manifest)
    assert.deepEqual(await checkSandbox(out), manifest)
    // The kit folder itself, with the modules beside it, works the same way.
    assert.deepEqual(await syncSandbox(r.kit, out), manifest)
    writeFileSync(path.join(out, 'zipp.mjs'), `${LOADER}// edited\n`)
    await assert.rejects(checkSandbox(out), /zipp\.mjs does not match/)
  } finally {
    r.cleanup()
  }
})

test('sync-sandbox refuses a module that is not the release engine it claims to be', async () => {
  const cases = [
    [release('9.9.9', { modulesJson: 'wrong' }), /does not match the kit's modules\.json/],
    [release('9.9.9', { meta: { format: 1, version: '9.9.8', commit: COMMIT, variant: 'javascript' } }), /records version 9\.9\.8/],
    [release('9.9.9', { meta: { format: 1, version: '9.9.9', commit: COMMIT, variant: 'lite' } }), /records the lite variant/],
    [release('9.9.9', { meta: null, modulesJson: false }), /no zipp\.meta record/],
  ]
  for (const [r, expected] of cases) {
    try {
      await assert.rejects(syncSandbox(r.dir, path.join(r.dir, 'out')), expected)
      assert.ok(!existsSync(path.join(r.dir, 'out', 'manifest.json')), 'nothing is published on a refusal')
    } finally {
      r.cleanup()
    }
  }
})

test('public/sandbox holds a complete engine or none at all', async () => {
  if (existsSync(path.join(DEFAULT_OUT, 'manifest.json'))) {
    await checkSandbox(DEFAULT_OUT)
  } else {
    const orphans = existsSync(DEFAULT_OUT) ? readdirSync(DEFAULT_OUT).filter(name => name === 'zipp.mjs' || name.endsWith('.wasm')) : []
    assert.deepEqual(orphans, [], 'engine files without a manifest')
  }
})

test('the sandbox playground page loads the engine through the loader under a strict policy', () => {
  const page = readFileSync(new URL('../public/sandbox/playground/index.html', import.meta.url), 'utf8')
  const csp = /<meta http-equiv="Content-Security-Policy" content="([^"]+)">/.exec(page)?.[1] ?? ''
  // The loader imports the module's bindings from a blob: URL and compiles
  // WebAssembly on the page; each sandbox's Worker is the loader itself.
  assert.match(csp, /script-src 'self' blob: 'wasm-unsafe-eval'/)
  assert.match(csp, /worker-src 'self'/)
  assert.match(csp, /connect-src 'self'/)
  assert.ok(!csp.includes('upgrade-insecure-requests'), 'local http previews must still load the page')
  assert.ok(!/<style[\s>]/.test(page) && !/<script(?![^>]*src=)/.test(page), 'no inline style or script')
  assert.ok(page.includes('<script type="module" src="/sandbox/playground/sandbox.js"></script>'))
  assert.ok(page.includes('<link rel="canonical" href="https://www.zipp.org/sandbox/playground/">'))
  assert.ok(page.includes('Available once ZIPP 0.0.26 is published.'))
  assert.equal((page.match(/<h1[\s>]/g) || []).length, 1)
  const script = readFileSync(new URL('../public/sandbox/playground/sandbox.js', import.meta.url), 'utf8')
  assert.ok(script.includes("`${ASSETS}manifest.json`") && script.includes('loader.loadZipp('), 'reads the manifest, then loads through zipp.mjs')
  assert.ok(!/^\s*import\s/m.test(script), 'no static import: a missing loader must not stop the page from explaining itself')
})
