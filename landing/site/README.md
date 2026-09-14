# Documentation pages

Everything under `/javascript-engine/`, `/python/`, `/wasm/`, `/webgpu/`, `/test262/`,
`/benchmarks/`, `/architecture/`, `/embedding/`, `/sandbox/`, `/comparisons/`,
`/releases/`, `/journey/`, `/articles/…` and `/journal/…` is rendered from **one
file, `content.json`**. Edit the JSON, and the next `npm run dev` reload or
`npm run build` regenerates the pages. No text lives in HTML.

The renderer (`render.mjs`) produces complete static HTML at build time, so every
page has its text in the served document before any script runs, plus a unique
`<title>`, meta description, canonical URL, Open Graph tags, breadcrumbs and JSON-LD.
The same build writes `sitemap.xml`, `robots.txt`, `404.html`, the journal's RSS
feed (`/journal/feed.xml`) and its search index (`/journal/index.json`).

## Files

| File | Purpose |
| --- | --- |
| `content.json` | All site copy: `site` (name, canonical origin, nav, footer, 404), `pages`, `journal` |
| `render.mjs` | JSON → HTML/XML. Pure; used by the Vite plugin and by `tests/site.test.mjs` |
| `vite-plugin.ts` | Emits the pages into the client bundle on build; serves them on request in dev |
| `site.css` | The stylesheet, fingerprinted on build. System fonts only (the CSP allows no external assets) |
| `journal-search.js` | Client-side search and tag filtering for `/journal/`; the page works without it |

## Editing content

**Inline markup** inside any `text`, `title`, `summary`, table cell or list item:
`**bold**`, `` `code` ``, `[label](href)`, `_emphasis_`. Raw HTML is escaped, never
rendered.

**Pages** (`pages[]`): `slug` (the URL path; nested slugs such as
`articles/foo` are allowed, with `parent` for the breadcrumb), `title` (≤ 70
characters), `description` (50–170 characters), `h1`, `lede`, `keywords`,
`datePublished`, `dateModified` (both `YYYY-MM-DD`), optional `priority`,
`schemaType` and `extraStructuredData`, then `sections[]` of `{heading, blocks}` and
optional `related[]` links.

**Block types**: `p`, `lead`, `h3`, `ul`, `ol`, `code` (`lang`, `caption`, `code`),
`callout` (`tone`: `note` | `warning`, `title`, `text`), `table` (`caption`, `head`,
`rows`), `stats` (`items[]` of `value`, `label`, `note`), `figure` (`src`, `alt`,
`caption`), `timeline` (`items[]` of `date`, `title`, `text`), `faq` (`items[]` of
`q`, `a`), `cards` (`items[]` of `title`, `href`, `text`) and `steps`.

**Journal entries** (`journal.entries[]`): `slug`, `date`, `title`, `summary`,
`tags`, `commits` (short hashes, linked to GitHub), `blocks`, optional `related`.
Entries are sorted newest first automatically; each gets `/journal/<slug>/`, an RSS
item, and a row in the search index.

Validation runs on every build and in `npm test`: duplicate routes or titles,
missing dates, out-of-range descriptions, unknown block types and broken internal
links all fail.

## Canonical domain

`site.origin` is `https://www.zipp.org`. Every canonical URL, sitemap entry and
feed link uses it. The apex domain should 301 to `www` (a Cloudflare redirect
rule; it cannot be expressed in this repository).
