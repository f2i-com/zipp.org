// Write a brotli and gzip body next to every compressible asset in dist/client.
//
// The engine is ~5.7 MB raw, 1.87 MB gzip, 1.26 MB brotli. Whether the origin
// hands out brotli is worth more than every build-level size change in the
// engine put together, and an origin that compresses on the fly does it at a
// lower quality than this does — so the bodies are built once, here.
//
//   npm run build   (runs this automatically)
//
// `.htaccess` in public/ is what actually serves them; it prefers .br, falls
// back to .gz, and restores the Content-Type the rewrite would otherwise lose.
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs'
import { join, extname, relative } from 'node:path'
import { brotliCompressSync, constants, gzipSync } from 'node:zlib'

const ROOT = 'dist/client'
const COMPRESSIBLE = new Set(['.wasm', '.js', '.css', '.html', '.json', '.svg', '.map', '.ts'])
// Below roughly a packet there is nothing to win and a second request to lose.
const MIN_BYTES = 1024

function* walk(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name)
    if (entry.isDirectory()) yield* walk(full)
    else yield full
  }
}

let rawTotal = 0
let brTotal = 0
const rows = []

for (const file of walk(ROOT)) {
  if (file.endsWith('.br') || file.endsWith('.gz')) continue
  if (!COMPRESSIBLE.has(extname(file))) continue
  const size = statSync(file).size
  if (size < MIN_BYTES) continue

  const body = readFileSync(file)
  const br = brotliCompressSync(body, {
    params: {
      [constants.BROTLI_PARAM_QUALITY]: 11,
      [constants.BROTLI_PARAM_SIZE_HINT]: body.length,
    },
  })
  const gz = gzipSync(body, { level: 9 })

  // A compressed body bigger than the original helps nobody; skip it so the
  // rewrite rules never find a .br worse than the file they came from.
  if (br.length < body.length) writeFileSync(file + '.br', br)
  if (gz.length < body.length) writeFileSync(file + '.gz', gz)

  rawTotal += body.length
  brTotal += Math.min(br.length, body.length)
  rows.push([relative(ROOT, file), body.length, br.length, gz.length])
}

rows.sort((a, b) => b[1] - a[1])
const n = (v) => v.toLocaleString().padStart(11)
console.log('precompress: ' + rows.length + ' files')
for (const [name, raw, br, gz] of rows) {
  console.log('  ' + name.padEnd(38) + n(raw) + '  br' + n(br) + '  gz' + n(gz))
}
console.log('  ' + 'TOTAL'.padEnd(38) + n(rawTotal) + '  br' + n(brTotal))
