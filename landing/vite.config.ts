import { cloudflare } from '@cloudflare/vite-plugin'
import { sites } from '@openai/sites-vite-plugin'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig(({ command }) => ({
  base: './',
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
