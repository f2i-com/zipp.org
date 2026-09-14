// Vite plugin that publishes the documentation pages described by
// site/content.json as static HTML.
//
// - `vite build` emits every page (plus sitemap.xml, robots.txt, 404.html and the
//   journal feed/search index) into the client bundle, so Cloudflare serves them
//   as plain files with the text already in the DOM.
// - `vite dev` renders the same pages on request, re-reading content.json every
//   time so an edit to the JSON shows up on the next reload.
//
// The stylesheet and the journal search script are fingerprinted by content in
// production builds, mirroring how Vite treats the React bundle.
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import type { Plugin } from 'vite'
import { renderSite, siteRoutes, type SiteContent } from './render.mjs'

const CONTENT = 'site/content.json'
const STATIC = { css: 'site/site.css', journalJs: 'site/journal-search.js' } as const

const read = (path: string) => readFileSync(path, 'utf8')
const loadContent = () => JSON.parse(read(CONTENT)) as SiteContent
const fingerprint = (text: string) => createHash('sha256').update(text).digest('hex').slice(0, 10)

const TYPES: Record<string, string> = {
  html: 'text/html; charset=utf-8',
  xml: 'application/xml; charset=utf-8',
  json: 'application/json; charset=utf-8',
  txt: 'text/plain; charset=utf-8',
  css: 'text/css; charset=utf-8',
  js: 'text/javascript; charset=utf-8',
}
const contentType = (file: string) => TYPES[file.slice(file.lastIndexOf('.') + 1)] ?? 'application/octet-stream'

export function staticSite(): Plugin {
  return {
    name: 'zipp-static-site',

    // Fail fast: an invalid content.json should break `vite build`, not deploy
    // half a site.
    buildStart() {
      renderSite(loadContent(), { css: '', journalJs: '' })
    },

    generateBundle() {
      // The Cloudflare plugin builds a second (server) environment for the
      // Worker; the pages belong to the client bundle only.
      if (this.environment?.name !== 'client') return
      const css = read(STATIC.css)
      const js = read(STATIC.journalJs)
      const assets = { css: `/assets/site-${fingerprint(css)}.css`, journalJs: `/assets/journal-${fingerprint(js)}.js` }
      this.emitFile({ type: 'asset', fileName: assets.css.slice(1), source: css })
      this.emitFile({ type: 'asset', fileName: assets.journalJs.slice(1), source: js })
      for (const [fileName, source] of renderSite(loadContent(), assets)) {
        this.emitFile({ type: 'asset', fileName, source })
      }
    },

    configureServer(server) {
      const assets = { css: '/site/site.css', journalJs: '/site/journal-search.js' }
      server.middlewares.use((request, response, next) => {
        // No @types/node in this project (see vite-node-shims.d.ts); the URL is
        // the one request field the middleware reads.
        const url = (request as typeof request & { url?: string }).url ?? ''
        const [pathname, query] = url.split('?')
        const suffix = query ? `?${query}` : ''
        if (pathname === assets.css || pathname === assets.journalJs) {
          response.writeHead(200, { 'content-type': contentType(pathname), 'cache-control': 'no-store' })
          response.end(read(pathname.slice(1)))
          return
        }
        let content: SiteContent
        try {
          content = loadContent()
        } catch (error) {
          response.writeHead(500, { 'content-type': 'text/plain; charset=utf-8' })
          response.end(`site/content.json could not be loaded: ${(error as Error).message}`)
          return
        }
        const routes = new Set(siteRoutes(content))
        // Mirror Cloudflare's auto-trailing-slash handling so links behave the
        // same locally as in production.
        if (routes.has(`${pathname}/`)) {
          response.writeHead(302, { location: `${pathname}/${suffix}` })
          response.end()
          return
        }
        const file = pathname.endsWith('/') ? `${pathname.slice(1)}index.html` : pathname.slice(1)
        const files = renderSite(content, assets)
        const body = files.get(file)
        if (body === undefined) {
          next()
          return
        }
        response.writeHead(200, { 'content-type': contentType(file), 'cache-control': 'no-store' })
        response.end(body)
      })
    },
  }
}
