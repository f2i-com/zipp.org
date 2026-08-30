import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig(({ command }) => ({
  base: './',
  plugins: [
    react(),
    command === 'serve' && {
      name: 'zipp-dev-csp',
      transformIndexHtml(html) {
        // Vite injects imported CSS into a style element during development.
        // Keep the production document strict while allowing the dev server to
        // render the same page engineers will build.
        return html
          .replace("script-src 'self';", "script-src 'self' 'unsafe-inline';")
          .replace("style-src 'self';", "style-src 'self' 'unsafe-inline';")
      },
    },
  ],
}))
