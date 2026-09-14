import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
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

test('internal links resolve to a route or a known public file', () => {
  const known = new Set([...routes, '/', '/playground/', '/journal/feed.xml', '/journal/index.json', '/sitemap.xml', '/robots.txt', '/zipp-bolt.svg', '/zipp-og-card.png'])
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
})

test('sitemap lists every route once with a lastmod, and robots points at it', () => {
  const sitemap = files.get('sitemap.xml')
  for (const route of [...routes, '/', '/playground/']) {
    const occurrences = sitemap.split(`<loc>${content.site.origin}${route}</loc>`).length - 1
    assert.equal(occurrences, 1, `${route} appears once in the sitemap`)
  }
  assert.equal((sitemap.match(/<url>/g) || []).length, routes.length + 2)
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

test('the home page keeps the site identity and links to the documentation without JavaScript', () => {
  const home = readFileSync(new URL('../index.html', import.meta.url), 'utf8')
  assert.ok(home.includes('<title>Zipp: Rust JavaScript &amp; Python Engine</title>'))
  assert.ok(new RegExp(`<link rel="canonical" href="${content.site.origin}/" ?/?>`).test(home))
  assert.ok(home.includes('"SoftwareApplication"') && home.includes('"WebSite"'))
  for (const item of content.site.nav) assert.ok(home.includes(`href="${item.href}"`), `home links to ${item.href}`)
  assert.ok(home.includes('<link rel="alternate" type="application/rss+xml"'))
  assert.ok(!/<style[\s>]/.test(home), 'home has no inline style')
})
