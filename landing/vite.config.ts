import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { cloudflare } from '@cloudflare/vite-plugin'
import { sites } from '@openai/sites-vite-plugin'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// The wasm-bindgen glue and the .wasm are one artifact in two files: the import
// names carry a per-build hash, so a new .wasm loaded against an older glue
// fails instantiation outright with "function import requires a callable".
//
// They live in public/, which Vite copies verbatim, so neither filename is
// fingerprinted — and the two are cached very differently in production (the
// .js came back with Cache-Control: max-age=14400 and a CDN HIT while the .wasm
// had no Cache-Control at all). That combination guarantees a window after every
// deploy where the new .wasm is paired with the previous glue.
//
// Stamping both URLs with a hash of their contents makes the pair move together:
// change either file and both URLs change, so no cache can serve a mismatched
// half.
function wasmBuildId() {
  // Vite runs the config with the project root as the working directory.
  const dir = 'public/wasm/'
  const hash = createHash('sha256')
  for (const name of ['zipp_wasm.js', 'zipp_wasm_bg.wasm']) {
    hash.update(readFileSync(dir + name))
  }
  return hash.digest('hex').slice(0, 12)
}

export default defineConfig(({ command }) => ({
  base: './',
  define: {
    __ZIPP_WASM_BUILD__: JSON.stringify(wasmBuildId()),
  },
  plugins: [
    react(),
    sites(),
    cloudflare({ viteEnvironment: { name: 'server' } }),
    command === 'serve' && {
      name: 'zipp-dev-csp',
      transformIndexHtml(html) {
        // Vite injects imported CSS into a style element during development.
        // Keep the production document strict while allowing the dev server to
        // render the same page engineers will build.
        return html
          .replace(
            "script-src 'self' 'wasm-unsafe-eval';",
            "script-src 'self' 'wasm-unsafe-eval' 'unsafe-inline';",
          )
          .replace("style-src 'self';", "style-src 'self' 'unsafe-inline';")
      },
    },
  ],
}))
