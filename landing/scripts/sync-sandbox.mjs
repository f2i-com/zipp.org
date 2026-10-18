// Publish the engine behind the sandbox playground (/sandbox/playground/):
// copy a ZIPP release's loader, zipp.mjs, and its JavaScript engine module,
// zipp-<version>-javascript.wasm, into public/sandbox/, and record them in
// public/sandbox/manifest.json, which the page reads to find the module.
//
//   node scripts/sync-sandbox.mjs <dir> [--out <dir>]
//   node scripts/sync-sandbox.mjs --check [--out <dir>]
//
// <dir> holds the release files: the module, and either zipp.mjs or the
// unpacked zipp-<version>-wasm-embed-kit/ folder. <dir> may also be the kit
// folder itself with the modules beside it, as the kit's README lays them out.
// Modules from 0.0.26 on carry a `zipp.meta` record; the module's record must
// name its file's version and the `javascript` variant, and when the kit's
// modules.json and SHA256SUMS are there, both files must match them.
//
// --check verifies that the files in public/sandbox/ are the ones its
// manifest names (the landing tests run it whenever a manifest exists).
//
// The page stays usable without these files: it says the sandbox is
// available once a release that has them is published.
import { createHash } from 'node:crypto'
import { copyFile, mkdir, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

export const DEFAULT_OUT = fileURLToPath(new URL('../public/sandbox/', import.meta.url))
const MODULE = /^zipp-(\d+\.\d+\.\d+)-javascript\.wasm$/
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex')
const exists = async file => { try { return (await stat(file)).isFile() } catch { return false } }
const isDir = async dir => { try { return (await stat(dir)).isDirectory() } catch { return false } }

async function findModule(dirs) {
  for (const dir of dirs) {
    if (!(await isDir(dir))) continue
    const found = (await readdir(dir)).filter(name => MODULE.test(name)).sort()
    if (found.length > 1) throw new Error(`${dir} holds ${found.length} JavaScript engine modules (${found.join(', ')}); keep one release's files together`)
    if (found.length === 1) return { file: path.join(dir, found[0]), name: found[0], version: MODULE.exec(found[0])[1] }
  }
  throw new Error(`no zipp-<version>-javascript.wasm in ${dirs.join(' or ')}`)
}

// The module's own record of what it is: its `zipp.meta` custom section.
async function readMeta(bytes) {
  const module = await WebAssembly.compile(bytes)
  const sections = WebAssembly.Module.customSections(module, 'zipp.meta')
  if (sections.length !== 1) throw new Error('the module has no zipp.meta record: use a module from a ZIPP release that ships single-file engines (0.0.26 or later)')
  return JSON.parse(new TextDecoder().decode(sections[0]))
}

// SHA256SUMS lines are "<hex>  <path>"; paths may carry the kit's folder name.
function sumsFor(text, name) {
  for (const line of text.split(/\r?\n/)) {
    const match = /^([0-9a-f]{64})\s+\*?(.+)$/.exec(line.trim())
    if (match && (match[2] === name || match[2].endsWith(`/${name}`))) return match[1]
  }
  return null
}

export async function syncSandbox(source, out = DEFAULT_OUT) {
  const dir = path.resolve(source)
  const module = await findModule([dir, path.dirname(dir)])
  const { version } = module
  const kitDirs = [dir, path.join(dir, `zipp-${version}-wasm-embed-kit`)]
  let kit = null
  for (const candidate of kitDirs) if (await exists(path.join(candidate, 'zipp.mjs'))) { kit = candidate; break }
  if (!kit) throw new Error(`no zipp.mjs in ${kitDirs.join(' or ')}: unpack zipp-${version}-wasm-embed-kit.zip there`)
  const loaderFile = path.join(kit, 'zipp.mjs')

  const moduleBytes = await readFile(module.file)
  const loaderBytes = await readFile(loaderFile)
  const moduleSha = sha256(moduleBytes)
  const loaderSha = sha256(loaderBytes)

  const meta = await readMeta(moduleBytes)
  if (meta.version !== version) throw new Error(`${module.name} records version ${meta.version}, not ${version}`)
  if (meta.variant !== 'javascript') throw new Error(`${module.name} records the ${meta.variant} variant, not javascript`)

  const loaderText = loaderBytes.toString('utf8')
  const loaderVersion = /export const ZIPP_LOADER_VERSION = "([^"]+)"/.exec(loaderText)?.[1]
  if (!loaderVersion || !/export async function loadZipp\b/.test(loaderText)) throw new Error(`${loaderFile} is not the ZIPP loader`)

  // The kit's own records, when present, must agree with the files.
  if (await exists(path.join(kit, 'modules.json'))) {
    const listed = JSON.parse(await readFile(path.join(kit, 'modules.json'), 'utf8'))
    if (listed.version && listed.version !== version) throw new Error(`the kit is ${listed.version}, the module ${version}`)
    const entry = listed.modules?.find(m => m.file === module.name)
    if (entry && entry.sha256 !== moduleSha) throw new Error(`${module.name} does not match the kit's modules.json`)
  }
  if (await exists(path.join(kit, 'SHA256SUMS'))) {
    const sums = await readFile(path.join(kit, 'SHA256SUMS'), 'utf8')
    const expected = sumsFor(sums, 'zipp.mjs')
    if (expected && expected !== loaderSha) throw new Error("zipp.mjs does not match the kit's SHA256SUMS")
  }

  await mkdir(out, { recursive: true })
  for (const name of await readdir(out)) {
    if (MODULE.test(name) && name !== module.name) await rm(path.join(out, name))
  }
  await copyFile(module.file, path.join(out, module.name))
  await copyFile(loaderFile, path.join(out, 'zipp.mjs'))
  const manifest = {
    version,
    file: module.name,
    sha256: moduleSha,
    bytes: moduleBytes.length,
    commit: meta.commit ?? null,
    loader: { file: 'zipp.mjs', sha256: loaderSha, version: loaderVersion },
  }
  await writeFile(path.join(out, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n')
  return manifest
}

export async function checkSandbox(out = DEFAULT_OUT) {
  const manifest = JSON.parse(await readFile(path.join(out, 'manifest.json'), 'utf8'))
  if (!/^\d+\.\d+\.\d+$/.test(manifest.version ?? '')) throw new Error('manifest.json has no version')
  if (manifest.file !== `zipp-${manifest.version}-javascript.wasm`) throw new Error(`manifest.json names ${manifest.file} for version ${manifest.version}`)
  const moduleBytes = await readFile(path.join(out, manifest.file))
  if (sha256(moduleBytes) !== manifest.sha256) throw new Error(`${manifest.file} does not match manifest.json`)
  if (sha256(await readFile(path.join(out, 'zipp.mjs'))) !== manifest.loader?.sha256) throw new Error('zipp.mjs does not match manifest.json')
  const meta = await readMeta(moduleBytes)
  if (meta.version !== manifest.version || meta.variant !== 'javascript') throw new Error(`${manifest.file} records ${meta.variant} ${meta.version}`)
  for (const name of await readdir(out)) {
    if (MODULE.test(name) && name !== manifest.file) throw new Error(`${name} is not the module manifest.json names`)
  }
  return manifest
}

async function main(argv) {
  const args = [...argv]
  let out = DEFAULT_OUT
  const outAt = args.indexOf('--out')
  if (outAt !== -1) {
    out = path.resolve(args[outAt + 1] ?? '')
    args.splice(outAt, 2)
  }
  if (args[0] === '--check') {
    const manifest = await checkSandbox(out)
    console.log(`Sandbox engine ${manifest.file} and zipp.mjs ${manifest.loader.version} match ${path.join(out, 'manifest.json')}.`)
    return
  }
  if (args.length !== 1) {
    console.error('usage: node scripts/sync-sandbox.mjs <dir-with-release-files> [--out <dir>]\n       node scripts/sync-sandbox.mjs --check [--out <dir>]')
    process.exitCode = 2
    return
  }
  const manifest = await syncSandbox(args[0], out)
  console.log(`Sandbox playground engine: ${manifest.file} (${manifest.bytes.toLocaleString('en')} bytes, ${manifest.commit ?? 'no commit'}) and zipp.mjs ${manifest.loader.version}, copied to ${out}.`)
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main(process.argv.slice(2)).catch(error => {
    console.error(`sync-sandbox: ${error.message}`)
    process.exitCode = 1
  })
}
