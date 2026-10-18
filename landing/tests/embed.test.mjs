// The home page's "Embed ZIPP on your site" section and its starter project.
//
// The section's file names, programs and expected output live in
// src/embed-data.mjs, which the page, scripts/build-embed-starter.mjs and these
// tests share. The last test runs the displayed example, as displayed, through
// the public embedding path (public/sandbox/zipp.mjs and its one .wasm); the
// internal glue + .wasm pair the classic scratchpad loads is exercised by
// tests/adventure.test.mjs.
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'
import { assetName, kitName } from '../../crates/zipp-wasm/embed-kit/package.mjs'
import {
  DEFAULT_ENGINE, EMBED_CSP, EMBED_RELEASE, ENGINES, KIT, PYTHON_BASE, RELEASE_TAG_URL, STARTER, TORCH_PACKAGE,
  capabilityExample, capabilityOutput, defaultExample, defaultOutput, defaultProgram, releaseAssetUrl,
} from '../src/embed-data.mjs'
import { STARTER_ZIP, readZip, starterEntries } from '../scripts/build-embed-starter.mjs'

const landing = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const read = (file) => readFileSync(path.join(landing, file), 'utf8').replace(/\r\n/g, '\n')
const content = JSON.parse(read('site/content.json'))
const app = read('src/App.tsx')
const section = read('src/EmbedSection.tsx')

test('the embed section names the release the site serves, by its published file names', () => {
  const site = JSON.parse(read('package.json'))
  assert.equal(EMBED_RELEASE, site.zippEngineRelease, 'src/embed-data.mjs links the release package.json pins')
  const variants = { javascript: 'javascript', lite: 'lite', 'javascript-python': 'javascript-python' }
  for (const engine of ENGINES) assert.equal(engine.file, assetName(EMBED_RELEASE, variants[engine.id]))
  assert.equal(PYTHON_BASE.file, assetName(EMBED_RELEASE, 'javascript-python-base'))
  assert.equal(TORCH_PACKAGE.file, assetName(EMBED_RELEASE, 'torch-package'))
  assert.equal(KIT.folder, kitName(EMBED_RELEASE))
  assert.equal(KIT.file, `${kitName(EMBED_RELEASE)}.zip`)
  assert.equal(releaseAssetUrl(KIT.file), `https://github.com/f2i-com/zipp.org/releases/download/v${EMBED_RELEASE}/${KIT.file}`)
  assert.equal(RELEASE_TAG_URL, `https://github.com/f2i-com/zipp.org/releases/tag/v${EMBED_RELEASE}`)

  // The sizes the section prints are the release page's, plain and .wasm.br.
  const releases = content.pages.find((page) => page.slug === 'releases')
  const table = releases.sections[0].blocks.find((block) => block.type === 'table')
  const bytes = Object.fromEntries(table.rows.map(([name, size]) => [name.replace(/`/g, ''), Number(size.replace(/,/g, ''))]))
  for (const asset of [...ENGINES, PYTHON_BASE, TORCH_PACKAGE]) {
    assert.equal(bytes[asset.file], asset.bytes, `${asset.file} size`)
    assert.equal(bytes[`${asset.file}.br`], asset.brBytes, `${asset.file}.br size`)
  }
  assert.equal(bytes[KIT.file], KIT.bytes, `${KIT.file} size`)
})

test('every example loads, creates, runs and closes a sandbox on the chosen engine', () => {
  for (const engine of ENGINES) {
    for (const code of [defaultExample(engine), capabilityExample(engine)]) {
      assert.match(code, /^import \{ loadZipp \} from "\.\/zipp\.mjs";$/m)
      assert.ok(code.includes(`await loadZipp("./${engine.file}")`), `${engine.id}: loads its own file`)
      assert.match(code, /await zipp\.createSandbox\(/)
      assert.match(code, /await sandbox\.run\(`/)
      assert.match(code, /await sandbox\.close\(\);/)
      assert.ok(!/\.wasm\.br/.test(code), 'the copyable examples load the plain .wasm')
    }
    const deny = defaultExample(engine)
    assert.ok(!/permissions/.test(deny), `${engine.id}: the default example grants nothing`)
    if (engine.language === 'python') {
      assert.ok(deny.includes('createSandbox({ language: "python" })'))
      assert.ok(capabilityExample(engine).includes('language: "python"'))
    } else {
      assert.ok(deny.includes('createSandbox();'))
    }
    // Exactly one capability in the second example.
    const granted = capabilityExample(engine).match(/permissions: \{ (\w+):/g) ?? []
    assert.equal(granted.length, 1, `${engine.id}: one permission`)
  }
})

test('the home page carries the embed section after the playground, with its controls', () => {
  const order = ['<ProjectPlayground />', '<EmbedSection />', '<ProjectShowcase />'].map((tag) => app.indexOf(tag))
  assert.ok(order.every((at) => at > 0) && order[0] < order[1] && order[1] < order[2], 'Embed sits between the playground and the projects')
  assert.ok(app.includes('<a href="#embed" onClick={closeMenu}>Embed</a>'), 'the navigation links #embed')
  assert.ok(section.includes('id="embed"') && section.includes('aria-labelledby="embed-title"'))
  assert.ok(section.includes('type="radio"') && section.includes('<legend>'), 'the engine chooser is a radio group with a legend')
  assert.ok(/role="status" aria-live="polite">\{copyMessage\}/.test(section), 'copy feedback is announced')
  assert.ok(section.includes('navigator.clipboard.writeText(code)'))
  assert.ok(section.includes('href={releaseAssetUrl(engine.file)}') && section.includes('href={releaseAssetUrl(KIT.file)}'), 'links the chosen .wasm and the kit')
  assert.ok(section.includes('href={releaseAssetUrl(`${engine.file}.br`)}'), 'offers the .wasm.br beside it')
  assert.ok(section.includes('href={STARTER.path} download'), 'offers the starter project')
  assert.ok(section.includes('{EMBED_CSP}') && section.includes('python3 -m http.server 8000') && section.includes('npx serve .'))
  assert.ok(section.includes('<details className="embed-more">'), 'the one-capability example is collapsed')
  // Scrollable code is reachable by keyboard and named.
  for (const pre of section.match(/<pre[^>]*>/g)) assert.ok(pre.includes('tabIndex={0}') && pre.includes('aria-label='), pre)
})

test('the starter ZIP holds index.html, main.mjs and a README, and nothing from the release', () => {
  assert.equal(STARTER_ZIP, path.join(landing, 'public', 'embed', `zipp-starter-${EMBED_RELEASE}.zip`))
  assert.equal(STARTER.path, `/embed/zipp-starter-${EMBED_RELEASE}.zip`)
  const entries = readZip(readFileSync(STARTER_ZIP))
  const expected = starterEntries()
  assert.deepEqual(entries.map((e) => e.name), [`${STARTER.folder}/README.md`, `${STARTER.folder}/index.html`, `${STARTER.folder}/main.mjs`])
  assert.deepEqual(entries.map((e) => e.name), expected.map((e) => e.name))
  for (const [i, entry] of entries.entries()) {
    assert.ok(entry.data.equals(expected[i].data), `${entry.name} is what scripts/build-embed-starter.mjs makes: rerun it`)
    assert.ok(!entry.data.includes('\r'), `${entry.name} has LF line endings`)
  }
  const size = statSync(STARTER_ZIP).size
  assert.ok(size < 8_000, `the starter stays small (${size} bytes)`)
  assert.ok(section.includes(`.zip · ${Math.round(size / 1000)} KB`), 'the section states the starter\'s size')

  const file = (name) => entries.find((e) => e.name === `${STARTER.folder}/${name}`).data.toString('utf8')
  const readme = file('README.md'), main = file('main.mjs'), index = file('index.html')
  for (const command of [
    `curl -fLO ${releaseAssetUrl(KIT.file)}`,
    `unzip -j ${KIT.file} ${KIT.folder}/zipp.mjs`,
    `curl.exe -fLO ${releaseAssetUrl(KIT.file)}`,
    `tar -xf ${KIT.file} --strip-components=1 ${KIT.folder}/zipp.mjs`,
    `curl -fLO ${releaseAssetUrl(DEFAULT_ENGINE.file)}`,
    `curl.exe -fLO ${releaseAssetUrl(DEFAULT_ENGINE.file)}`,
    'python3 -m http.server 8000',
    'npx serve .',
    EMBED_CSP,
    defaultOutput(DEFAULT_ENGINE).join('\n'),
  ]) assert.ok(readme.includes(command), `README: ${command}`)
  // Every download is pinned to the release, never to "latest".
  for (const [url] of readme.matchAll(/https:\/\/github\.com\/[^\s)`]+/g)) {
    assert.match(url, new RegExp(`/(releases/download|releases/tag)/v${EMBED_RELEASE.replace(/\./g, '\\.')}(/|$)`), url)
  }
  assert.ok(!/latest/.test(readme))

  assert.ok(main.includes(`const ENGINE = "./${DEFAULT_ENGINE.file}";`))
  assert.ok(main.includes('await import("./zipp.mjs")') && !/^\s*import\s/m.test(main), 'zipp.mjs is imported dynamically, so a missing loader is reported')
  assert.ok(main.includes('location.protocol === "file:"'))
  assert.ok(main.includes('await zipp.createSandbox();') && main.includes('await sandbox.close();'))
  assert.ok(main.includes(`sandbox.run(\`${defaultProgram(DEFAULT_ENGINE)}\`)`), 'main.mjs runs the program the section shows')
  assert.ok(main.includes('show("error"') && main.includes('show("loading"'), 'loading and error states')

  assert.ok(index.includes(`content="default-src 'self'; ${EMBED_CSP}"`))
  assert.ok(index.includes('<script type="module" src="./main.mjs"></script>'))
  assert.ok(!/<style[\s>]/.test(index) && !/<script(?![^>]*src=)/.test(index), 'no inline script or style')
  assert.ok(index.includes('id="status"') && index.includes('id="output"'))
})

test('the displayed examples print the displayed output through the public zipp.mjs path', { timeout: 120_000 }, (t) => {
  const served = path.join(landing, 'public', 'sandbox')
  if (!existsSync(path.join(served, 'manifest.json'))) {
    t.skip('public/sandbox holds no engine (see sync-sandbox.mjs)')
    return
  }
  const manifest = JSON.parse(readFileSync(path.join(served, 'manifest.json'), 'utf8'))
  assert.equal(manifest.file, DEFAULT_ENGINE.file, 'the sandbox playground serves the engine the section defaults to')
  // A page's folder: the loader, the engine and the example, as the section lays it out.
  const dir = mkdtempSync(path.join(tmpdir(), 'zipp-embed-'))
  try {
    copyFileSync(path.join(served, 'zipp.mjs'), path.join(dir, 'zipp.mjs'))
    copyFileSync(path.join(served, manifest.file), path.join(dir, manifest.file))
    for (const [name, code, expected] of [
      ['default.mjs', defaultExample(DEFAULT_ENGINE), defaultOutput(DEFAULT_ENGINE)],
      ['capability.mjs', capabilityExample(DEFAULT_ENGINE), capabilityOutput(DEFAULT_ENGINE)],
    ]) {
      writeFileSync(path.join(dir, name), code)
      const printed = execFileSync(process.execPath, [name], { cwd: dir, encoding: 'utf8', timeout: 60_000 })
      assert.deepEqual(printed.replace(/\r\n/g, '\n').trimEnd().split('\n'), expected, name)
    }
  } finally {
    rmSync(dir, { recursive: true, force: true })
  }
})
