// Publish the existing folder-based playground without maintaining a second app.
// Sources are copied on dev/build. Refresh the checked-in engine pair explicitly
// after building crates/zipp-wasm/dist/all: node scripts/sync-playground.mjs --refresh-engine
import { readFile, writeFile, mkdir, readdir, copyFile } from 'node:fs/promises'
import { createHash } from 'node:crypto'
import { fileURLToPath, pathToFileURL } from 'node:url'
import path from 'node:path'

const root = fileURLToPath(new URL('../../', import.meta.url))
const output = fileURLToPath(new URL('../public/', import.meta.url))
const hash = bytes => createHash('sha256').update(bytes).digest('hex')
async function copyTree(source, destination, filter = () => true) {
  await mkdir(destination, { recursive: true })
  for (const entry of await readdir(source, { withFileTypes: true })) {
    const from = path.join(source, entry.name), to = path.join(destination, entry.name)
    if (entry.isDirectory() && entry.name !== '__pycache__') await copyTree(from, to, filter)
    else if (entry.isFile() && filter(entry.name)) await copyFile(from, to)
  }
}
await mkdir(path.join(output, 'playground'), { recursive: true })
for (const name of ['index.html', 'playground.css', 'playground.js', 'engine.worker.js']) {
  let text = await readFile(path.join(root, 'crates/zipp-wasm/playground', name), 'utf8')
  if (name === 'engine.worker.js') text = text.replace('../dist/all/zipp_wasm.js', '../playground-runtime/zipp_wasm.js')
  if (name === 'index.html') {
    text = text.replace('<title>Zipp Playground</title>', '<title>Zipp Playground · Python, JavaScript &amp; GPU</title>')
      .replace('<strong>Zipp playground</strong>', '<a href="/" target="_top" class="playground-home">Zipp playground ↗</a>')
      .replace('Open a folder of .py or .js files (top level only)', 'Load a local project folder, including subfolders and data; files stay in your browser')
      .replace('</head>', '<meta http-equiv="Content-Security-Policy" content="default-src \'self\'; script-src \'self\' \'wasm-unsafe-eval\'; style-src \'self\'; img-src \'self\' data:; worker-src \'self\'; connect-src \'self\'; object-src \'none\'; base-uri \'none\'">\n</head>')
  }
  if (name === 'playground.css') text += '\n.playground-home{font-weight:700;color:inherit;text-decoration:none}\n'
  await writeFile(path.join(output, 'playground', name), text)
}
await copyTree(path.join(root, 'crates/zipp-wasm/gpu-lab/src'), path.join(output, 'gpu-lab/src'))
await mkdir(path.join(output, 'gpu-lab/wasm'), { recursive: true })
await copyFile(path.join(root, 'crates/zipp-wasm/gpu-lab/wasm/kernels.wasm'), path.join(output, 'gpu-lab/wasm/kernels.wasm'))
for (const folder of ['python/project', 'python/langtons_ant', 'python/gpu', 'python/torch_gpu', 'python/torch_training', 'js/project']) {
  await copyTree(path.join(root, 'examples', folder), path.join(output, 'examples', folder), name => /\.(py|js)$/.test(name))
}

const runtime = path.join(output, 'playground-runtime')
const names = ['zipp_wasm.js', 'zipp_wasm_bg.wasm']
if (process.argv.includes('--refresh-engine')) {
  await mkdir(runtime, { recursive: true })
  for (const name of names) await copyFile(path.join(root, 'crates/zipp-wasm/dist/all', name), path.join(runtime, name))
  const wasm = await import(pathToFileURL(path.join(runtime, names[0])))
  await wasm.default({ module_or_path: await readFile(path.join(runtime, names[1])) })
  const files = Object.fromEntries(await Promise.all(names.map(async name => [name, hash(await readFile(path.join(runtime, name)))])))
  await writeFile(path.join(runtime, 'manifest.json'), JSON.stringify({ profile: JSON.parse(wasm.zippProfile()), files }, null, 2) + '\n')
}
const manifest = JSON.parse(await readFile(path.join(runtime, 'manifest.json'), 'utf8'))
for (const name of names) {
  if (hash(await readFile(path.join(runtime, name))) !== manifest.files[name]) throw Error(`Playground engine pair mismatch: ${name}`)
}
// Move glue and WASM URLs together when either half changes, including CDN caches.
const build = hash(names.map(name => manifest.files[name]).join(':')).slice(0, 16)
const workerPath = path.join(output, 'playground/engine.worker.js')
const worker = (await readFile(workerPath, 'utf8'))
  .replace('../playground-runtime/zipp_wasm.js', `../playground-runtime/zipp_wasm.js?v=${build}`)
  .replace('const ready = init()', `const ready = init({ module_or_path: new URL("../playground-runtime/zipp_wasm_bg.wasm?v=${build}", import.meta.url) })`)
await writeFile(workerPath, worker)
console.log('Folder playground synchronized; checked-in Python WASM pair verified.')
