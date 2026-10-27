import { test } from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, readFileSync } from 'node:fs'
import { renderSite, validateContent, siteRoutes, inline, escapeHtml, sortedEntries } from '../site/render.mjs'

const content = JSON.parse(readFileSync(new URL('../site/content.json', import.meta.url), 'utf8'))
const assets = { css: '/assets/site-test.css', journalJs: '/assets/journal-test.js' }
const files = renderSite(content, assets)
const routes = siteRoutes(content)
const html = path => files.get(path.endsWith('/') ? `${path.slice(1)}index.html` : path.slice(1))

test('content.json passes validation and every page has unique SEO metadata', () => {
  assert.deepEqual(validateContent(content), [])
  const titles = new Set(), descriptions = new Set()
  for (const route of routes) {
    const page = html(route)
    assert.ok(page, `${route} rendered`)
    const title = /<title>([^<]+)<\/title>/.exec(page)?.[1]
    const description = /<meta name="description" content="([^"]+)">/.exec(page)?.[1]
    assert.ok(title && description, `${route} has a title and a description`)
    assert.ok(!titles.has(title), `${route} title is unique`)
    assert.ok(!descriptions.has(description), `${route} description is unique`)
    titles.add(title); descriptions.add(description)
    assert.equal((page.match(/<h1[\s>]/g) || []).length, 1, `${route} has exactly one h1`)
    assert.ok(page.includes(`<link rel="canonical" href="${content.site.origin}${route}">`), `${route} canonical`)
    assert.ok(page.includes('application/ld+json'), `${route} structured data`)
    assert.ok(page.includes('"BreadcrumbList"'), `${route} breadcrumbs`)
    assert.ok(page.includes('Content-Security-Policy'), `${route} CSP`)
    assert.ok(!/<style[\s>]/.test(page) && !/<script(?![^>]*application\/ld\+json)(?![^>]*src=)/.test(page), `${route} has no inline style or script`)
  }
})

// Interactive pages served from public/ rather than rendered from content.json.
const publicPages = ['/playground/', '/sandbox/playground/']

test('internal links resolve to a route or a known public file', () => {
  const known = new Set([...routes, '/', ...publicPages, '/journal/feed.xml', '/journal/index.json', '/sitemap.xml', '/robots.txt', '/zipp-bolt.svg', '/zipp-og-card.png'])
  const publicFiles = ['/demos/python-wasm-flow.svg', '/demos/python-life.png', '/demos/python-life.gif']
  for (const file of publicFiles) known.add(file)
  const broken = []
  for (const route of routes) {
    for (const match of html(route).matchAll(/<a [^>]*href="(\/[^"#?]*)/g)) {
      const target = match[1]
      if (!known.has(target) && !known.has(`${target}/`)) broken.push(`${route} -> ${target}`)
    }
  }
  assert.deepEqual(broken, [])
})

test('inline markup escapes HTML and renders bold, code and links', () => {
  assert.equal(escapeHtml('<a href="x">&'), '&lt;a href=&quot;x&quot;&gt;&amp;')
  assert.equal(inline('**bold** and `code` and [link](/x/) and <b>'), '<strong>bold</strong> and <code>code</code> and <a href="/x/">link</a> and &lt;b&gt;')
  assert.equal(inline('[out](https://example.com/)'), '<a href="https://example.com/" rel="noopener">out</a>')
  assert.equal(inline('[jump](#section)'), '<a href="#section">jump</a>')
  assert.equal(inline('[same](https://www.zipp.org/path)'), '<a href="https://www.zipp.org/path">same</a>')
  assert.equal(inline('[lookalike](https://www.zipp.org.evil/path)'), '<a href="https://www.zipp.org.evil/path" rel="noopener">lookalike</a>')
  for (const href of ['javascript:alert%281%29', 'data:text/html,hello', '//example.com/', '/\\example.com/']) {
    assert.equal(inline(`[blocked](${href})`), 'blocked')
  }
})

test('content links and images cannot use active URL schemes', () => {
  const altered = () => structuredClone(content)
  const cases = [
    copy => { copy.site.nav[0].href = 'javascript:alert%281%29' },
    copy => { copy.site.footer.links[0].href = '//example.com/' },
    copy => { copy.pages.find(page => page.related?.length).related[0].href = 'data:text/html,hello' },
    copy => { copy.pages.flatMap(page => page.sections.flatMap(section => section.blocks)).find(block => block.type === 'cards').items[0].href = 'javascript:1' },
    copy => { copy.pages.flatMap(page => page.sections.flatMap(section => section.blocks)).find(block => block.type === 'figure').src = 'data:image/svg+xml,hello' },
  ]
  for (const change of cases) {
    const copy = altered()
    change(copy)
    assert.throws(() => renderSite(copy, assets), /Unsafe site (link|image)/)
  }
  const copy = altered()
  copy.site.origin = 'javascript:alert'
  assert.ok(validateContent(copy).includes('site.origin must be an HTTPS origin'))

  const pageTraversal = altered()
  pageTraversal.pages[0].slug = '../outside'
  assert.ok(validateContent(pageTraversal).some(error => error.includes('invalid page slug')))
  const journalTraversal = altered()
  journalTraversal.journal.entries[0].slug = '../../outside'
  assert.ok(validateContent(journalTraversal).some(error => error.includes('invalid journal slug')))
})

test('sitemap lists every route once with a lastmod, and robots points at it', () => {
  const sitemap = files.get('sitemap.xml')
  for (const route of [...routes, '/', ...publicPages]) {
    const occurrences = sitemap.split(`<loc>${content.site.origin}${route}</loc>`).length - 1
    assert.equal(occurrences, 1, `${route} appears once in the sitemap`)
  }
  assert.equal((sitemap.match(/<url>/g) || []).length, routes.length + 1 + publicPages.length)
  assert.ok(/<lastmod>\d{4}-\d{2}-\d{2}<\/lastmod>/.test(sitemap))
  assert.ok(!sitemap.includes('404.html'))
  assert.equal(files.get('robots.txt'), `User-agent: *\nAllow: /\nDisallow: /api/\n\nSitemap: ${content.site.origin}/sitemap.xml\n`)
  assert.ok(files.get('404.html').includes('<meta name="robots" content="noindex">'))
})

test('the journal feed is well-formed RSS with one item per entry, newest first', () => {
  const feed = files.get('journal/feed.xml')
  const entries = sortedEntries(content.journal.entries)
  assert.ok(feed.startsWith('<?xml version="1.0" encoding="UTF-8"?>'))
  assert.equal((feed.match(/<item>/g) || []).length, entries.length)
  const links = [...feed.matchAll(/<guid isPermaLink="true">([^<]+)<\/guid>/g)].map(m => m[1])
  assert.deepEqual(links, entries.map(entry => `${content.site.origin}/journal/${entry.slug}/`))
  const dates = [...feed.matchAll(/<pubDate>([^<]+)<\/pubDate>/g)].map(m => Date.parse(m[1]))
  for (let i = 1; i < dates.length; i++) assert.ok(dates[i - 1] >= dates[i], 'feed is newest first')
  assert.ok(feed.includes('<content:encoded><![CDATA['))
  assert.ok(!feed.includes(']]>]]>'))
  const index = JSON.parse(files.get('journal/index.json'))
  assert.equal(index.length, entries.length)
  assert.ok(index.every(item => item.slug && item.url && item.date && item.title && item.text.length > 0))
  assert.ok(!index.some(item => /<[a-z]/.test(item.text)), 'search index text is plain')
})

test('journal pages carry the search form, tag filters and prev/next navigation', () => {
  const index = html('/journal/')
  assert.ok(index.includes('id="journal-query"'))
  assert.ok(index.includes(`<script src="${assets.journalJs}" defer></script>`))
  assert.ok(index.includes('<link rel="alternate" type="application/rss+xml"'))
  const entries = sortedEntries(content.journal.entries)
  assert.ok(index.includes(`href="/journal/${entries[0].slug}/"`))
  const middle = html(`/journal/${entries[1].slug}/`)
  assert.ok(middle.includes(`href="/journal/${entries[0].slug}/" rel="next"`))
  assert.ok(middle.includes(`href="/journal/${entries[2].slug}/" rel="prev"`))
  assert.ok(middle.includes('"@type":"BlogPosting"'))
})

test('every journal entry and page is dated within the repository history', () => {
  const first = '2026-05-29'
  const today = new Date().toISOString().slice(0, 10)
  for (const entry of content.journal.entries) assert.ok(entry.date >= first && entry.date <= today, `${entry.slug} dated ${entry.date}`)
  for (const page of content.pages) {
    assert.ok(page.datePublished >= first && page.datePublished <= today, `${page.slug} published ${page.datePublished}`)
    assert.ok(page.dateModified >= page.datePublished, `${page.slug} modified after publish`)
  }
})

test('release install snippets run the Linux CLI the way releases package it', () => {
  // From 0.0.26 a release publishes the Linux CLI as the binary itself,
  // zipp-<version>-linux-<arch> (tools/release/package-native.sh), so a
  // snippet that downloads it must make it executable before running it.
  // 0.0.25 and earlier shipped <bundle>.tar.gz holding <bundle>/zipp, so a
  // snippet that extracts one runs ./<bundle>/zipp, and no ./zipp exists.
  const packager = readFileSync(new URL('../../tools/release/package-native.sh', import.meta.url), 'utf8')
  assert.ok(packager.includes('asset="$out/zipp-$version-linux-$arch"'))
  assert.ok(packager.includes('install -m 0755 "$binary" "$asset"'))
  const snippets = []
  const walk = value => {
    if (typeof value === 'string') { if (/tar x\w*f zipp-|zipp-[\w.$-]*linux-/.test(value)) snippets.push(value) }
    else if (value && typeof value === 'object') Object.values(value).forEach(walk)
  }
  walk(content)
  assert.ok(snippets.length > 0)
  for (const code of snippets) {
    const archives = [...code.matchAll(/tar x\w*f (zipp-[\w.-]+?)\.tar\.gz/g)]
    for (const [, bundle] of archives) {
      assert.ok(code.includes(`./${bundle}/zipp `), `${bundle}: run the extracted ./${bundle}/zipp`)
      assert.ok(!/(^|\n)\.\/zipp /.test(code), `${bundle}: no ./zipp exists after extraction`)
    }
    if (archives.length === 0 && /(^|\n)\.\/zipp /.test(code)) {
      assert.ok(/chmod \+x zipp/.test(code), 'a downloaded Linux binary is made executable before it runs')
    }
  }
})

test('the home page\'s story blurb states the journey page\'s own figures', () => {
  // A dated snapshot on the home page, kept in step with the page it describes.
  const app = readFileSync(new URL('../src/App.tsx', import.meta.url), 'utf8')
  const blurb = /href: '\/journey\/'[^\n]*text: '([^']+)'/.exec(app)?.[1] ?? ''
  const [, commits, days] = /^([\d,]+) commits in (\d+) days, to (\d+ \w+ \d{4})/.exec(blurb) ?? []
  assert.ok(commits && days, `the blurb names a commit count, a day count and a date: ${blurb}`)
  const journey = content.pages.find(page => page.slug === 'journey')
  assert.ok(journey.lede.includes(`${commits} commits over ${days} days`), 'the journey page states the same numbers')
  assert.ok(journey.title.includes(`${days} Days`))
  const acts = /told in (\w+) acts/.exec(blurb)?.[1]
  assert.ok(journey.lede.includes(`in ${acts} acts`), `the journey page is told in ${acts} acts`)
})

test('every benchmark capture the page cites is in the repository, and the data names this release', () => {
  // src/benchmark-data.json is written by scripts/import-benchmarks.py from these
  // artifacts; BenchmarkSection.tsx links each of them on GitHub main.
  const data = JSON.parse(readFileSync(new URL('../src/benchmark-data.json', import.meta.url), 'utf8'))
  const artifacts = [data.jit.artifacts, data.interp.artifacts, data.python.artifacts, data.release.artifacts].flatMap(Object.values)
  assert.equal(artifacts.length, 8)
  for (const file of artifacts) assert.ok(existsSync(new URL(`../../${file}`, import.meta.url)), `${file} is missing`)
  // The captures measure one released engine, which may trail the release the
  // site serves (a fix release reuses its predecessor's measurements).
  const site = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'))
  const versions = [data.jit.zipp.version, data.python.zipp.version, data.release.zipp.version]
  assert.equal(new Set(versions).size, 1, `one engine version: ${versions}`)
  const parts = v => v.replace(/^zipp /, '').split('.').map(Number)
  const [measured, served] = [parts(versions[0]), parts(site.zippEngineRelease)]
  assert.ok(measured.every((n, i) => Number.isInteger(n)) && measured.length === 3, versions[0])
  assert.ok(measured[0] < served[0] || (measured[0] === served[0] && (measured[1] < served[1] || (measured[1] === served[1] && measured[2] <= served[2]))),
    `the measured engine ${versions[0]} is not newer than the served release ${site.zippEngineRelease}`)
  assert.equal(data.jit.nodeWins, [...data.jit.normal.rows, ...data.jit.hostile.rows].filter(row => row.ratio < 1).length)
})

test('the home page release tab is the served release, and its figures are the release notes\' own', () => {
  // src/release-comparison.json is not a stamped capture: it restates the
  // comparison in docs/releases/<version>.md, so the two must agree.
  const rel = JSON.parse(readFileSync(new URL('../src/release-comparison.json', import.meta.url), 'utf8'))
  const site = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'))
  assert.equal(rel.version, site.zippEngineRelease, 'the release tab names the release the site serves')
  const notes = readFileSync(new URL(`../../${rel.notes}`, import.meta.url), 'utf8')
  assert.equal(rel.notes, `docs/releases/${rel.version}.md`)
  assert.ok(notes.includes(`Bellard's QuickJS ${rel.quickjs}`), 'the notes name the same QuickJS')
  // The notes' bundle table: seconds to two places from 400 ms, whole ms below.
  const cell = msValue => (msValue >= 400 ? `${(msValue / 1000).toFixed(2)} s` : `${Math.round(msValue)} ms`)
  for (const bundle of rel.bundles) {
    const { previous, current } = bundle.large
    assert.ok(notes.includes(`| ${cell(previous)} | ${cell(current)} |`), `${bundle.id}: ${cell(previous)} -> ${cell(current)} in the notes`)
  }
  const p = rel.programs
  for (const ratio of [p.interpVsPrevious, p.jitVsPrevious, p.interpVsQuickjs, p.previousInterpVsQuickjs]) {
    assert.ok(notes.includes(`${ratio.toFixed(3)}x`), `${ratio} in the notes`)
  }
  assert.ok(notes.includes(`faster on ${p.interpFasterThanQuickjs} of the ${p.count}`), 'the QuickJS win count')
})

test('repository links on the site point at files and headings that exist on main', () => {
  // Links to github.com/f2i-com/zipp.org/blob/main/<path>[#anchor] (and tree/main) in the
  // home page's source and the documentation pages; the README and docs moved on 17
  // October 2026, and links into them broke silently before this check.
  const root = new URL('../../', import.meta.url)
  const sources = ['src/App.tsx', 'src/ProjectPlayground.tsx', 'src/EmbedSection.tsx', 'src/RepositoryActivity.tsx', 'src/Experience.tsx', 'site/content.json', 'index.html']
    .map(file => readFileSync(new URL(`../${file}`, import.meta.url), 'utf8').replaceAll('${GITHUB_URL}', 'https://github.com/f2i-com/zipp.org'))
  const slug = heading => heading.trim().toLowerCase().replace(/[^\w\- ]+/g, '').replace(/ /g, '-')
  const broken = []
  for (const text of sources) {
    for (const [, kind, file, anchor] of text.matchAll(/https:\/\/github\.com\/f2i-com\/zipp\.org\/(blob|tree)\/main\/([^\s"'`)#?]+)(?:#([\w-]+))?/g)) {
      const target = new URL(file, root)
      if (!existsSync(target)) { broken.push(`${kind}/main/${file}`); continue }
      if (anchor && file.endsWith('.md')) {
        const headings = [...readFileSync(target, 'utf8').matchAll(/^#+\s+(.+)$/gm)].map(m => slug(m[1]))
        if (!headings.includes(anchor)) broken.push(`${file}#${anchor}`)
      }
    }
  }
  assert.deepEqual([...new Set(broken)], [])
})

test('the home page keeps the site identity and links to the documentation without JavaScript', () => {
  const home = readFileSync(new URL('../index.html', import.meta.url), 'utf8')
  assert.ok(home.includes('<title>ZIPP: Portable Polyglot Runtime for JavaScript and Python</title>'))
  assert.ok(home.includes('Zippy Interpreter for Polyglot Programming'), 'home spells out the name')
  assert.ok(new RegExp(`<link rel="canonical" href="${content.site.origin}/" ?/?>`).test(home))
  assert.ok(home.includes('"SoftwareApplication"') && home.includes('"WebSite"'))
  for (const item of content.site.nav) assert.ok(home.includes(`href="${item.href}"`), `home links to ${item.href}`)
  assert.ok(home.includes('<link rel="alternate" type="application/rss+xml"'))
  assert.ok(!/<style[\s>]/.test(home), 'home has no inline style')
})
