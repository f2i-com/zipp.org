// Static documentation site renderer.
//
// Every word of the documentation pages lives in site/content.json. This module
// turns that JSON into complete, crawlable HTML documents (plus sitemap.xml,
// robots.txt, the journal RSS feed and its search index) at build time, so the
// text is in the served HTML before any JavaScript runs. Nothing here touches the
// network or the filesystem: the Vite plugin and the tests both call renderSite()
// with an already-parsed content object.

const HTML_ESCAPES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }
export const escapeHtml = value => String(value).replace(/[&<>"']/g, char => HTML_ESCAPES[char])
const escapeAttr = escapeHtml

// Inline markup allowed inside JSON text fields, applied after HTML escaping so
// authors can never inject raw markup by accident:
//   **bold**   `code`   [label](href)   _emphasis_
export function inline(text) {
  let html = escapeHtml(text)
  html = html.replace(/`([^`]+)`/g, (_, code) => `<code>${code}</code>`)
  html = html.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
  html = html.replace(/(^|[\s(])_([^_\n]+)_(?=[\s.,;:!?)]|$)/g, '$1<em>$2</em>')
  html = html.replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_, label, href) => {
    const external = /^https?:\/\//.test(href) && !href.startsWith('https://www.zipp.org')
    const rel = external ? ' rel="noopener"' : ''
    return `<a href="${escapeAttr(href)}"${rel}>${label}</a>`
  })
  return html
}

export const slugify = text =>
  String(text)
    .toLowerCase()
    .replace(/[`*_[\]()]/g, '')
    .replace(/&[a-z]+;/g, '')
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')

const isoDate = value => {
  if (!/^\d{4}-\d{2}-\d{2}$/.test(value)) throw new Error(`Expected an ISO date (YYYY-MM-DD), got ${JSON.stringify(value)}`)
  return value
}
const MONTHS = ['January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December']
export const humanDate = value => {
  const [year, month, day] = isoDate(value).split('-').map(Number)
  return `${day} ${MONTHS[month - 1]} ${year}`
}
const rfc822 = value => new Date(`${isoDate(value)}T00:00:00Z`).toUTCString()

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

const renderers = {
  p: block => `<p>${inline(block.text)}</p>`,
  lead: block => `<p class="lead">${inline(block.text)}</p>`,
  h3: block => `<h3 id="${escapeAttr(block.id || slugify(block.text))}">${inline(block.text)}</h3>`,
  ul: block => `<ul>${block.items.map(item => `<li>${inline(item)}</li>`).join('')}</ul>`,
  ol: block => `<ol>${block.items.map(item => `<li>${inline(item)}</li>`).join('')}</ol>`,
  code: block =>
    `<figure class="code">${block.caption ? `<figcaption>${inline(block.caption)}</figcaption>` : ''}<pre><code${
      block.lang ? ` class="language-${escapeAttr(block.lang)}"` : ''
    }>${escapeHtml(block.code)}</code></pre></figure>`,
  callout: block =>
    `<aside class="callout${block.tone ? ` callout-${escapeAttr(block.tone)}` : ''}">${
      block.title ? `<strong class="callout-title">${inline(block.title)}</strong>` : ''
    }<p>${inline(block.text)}</p></aside>`,
  table: block =>
    `<div class="table-scroll"><table>${block.caption ? `<caption>${inline(block.caption)}</caption>` : ''}<thead><tr>${block.head
      .map(cell => `<th scope="col">${inline(cell)}</th>`)
      .join('')}</tr></thead><tbody>${block.rows
      .map(row => `<tr>${row.map((cell, index) => (index === 0 ? `<th scope="row">${inline(cell)}</th>` : `<td>${inline(cell)}</td>`)).join('')}</tr>`)
      .join('')}</tbody></table></div>`,
  stats: block =>
    `<dl class="stats">${block.items
      .map(item => `<div class="stat"><dt>${inline(item.label)}</dt><dd>${inline(item.value)}</dd>${item.note ? `<p class="stat-note">${inline(item.note)}</p>` : ''}</div>`)
      .join('')}</dl>`,
  figure: block =>
    `<figure class="media"><img src="${escapeAttr(block.src)}" alt="${escapeAttr(block.alt)}" loading="lazy" decoding="async"${
      block.width ? ` width="${escapeAttr(block.width)}"` : ''
    }${block.height ? ` height="${escapeAttr(block.height)}"` : ''}>${block.caption ? `<figcaption>${inline(block.caption)}</figcaption>` : ''}</figure>`,
  timeline: block =>
    `<ol class="timeline">${block.items
      .map(
        item =>
          `<li class="timeline-item"><time datetime="${escapeAttr(isoDate(item.date))}">${humanDate(item.date)}</time><div><strong>${inline(item.title)}</strong>${
            item.text ? `<p>${inline(item.text)}</p>` : ''
          }</div></li>`,
      )
      .join('')}</ol>`,
  faq: block =>
    `<div class="faq">${block.items
      .map(item => `<details><summary><h3>${inline(item.q)}</h3></summary><p>${inline(item.a)}</p></details>`)
      .join('')}</div>`,
  cards: block =>
    `<div class="cards">${block.items
      .map(
        item =>
          `<a class="card" href="${escapeAttr(item.href)}"><strong>${inline(item.title)}</strong><p>${inline(item.text)}</p></a>`,
      )
      .join('')}</div>`,
  steps: block =>
    `<ol class="steps">${block.items.map(item => `<li><strong>${inline(item.title)}</strong><p>${inline(item.text)}</p></li>`).join('')}</ol>`,
}

export function renderBlock(block) {
  const renderer = renderers[block.type]
  if (!renderer) throw new Error(`Unknown block type ${JSON.stringify(block.type)}`)
  return renderer(block)
}

const renderSections = sections =>
  sections
    .map(section => {
      const id = section.id || slugify(section.heading)
      return `<section id="${escapeAttr(id)}"><h2>${inline(section.heading)}</h2>${section.blocks.map(renderBlock).join('\n')}</section>`
    })
    .join('\n')

const renderToc = sections =>
  sections.length < 3
    ? ''
    : `<nav class="toc" aria-label="On this page"><strong>On this page</strong><ol>${sections
        .map(section => `<li><a href="#${escapeAttr(section.id || slugify(section.heading))}">${inline(section.heading)}</a></li>`)
        .join('')}</ol></nav>`

// ---------------------------------------------------------------------------
// Document shell
// ---------------------------------------------------------------------------

const CSP =
  "default-src 'self'; base-uri 'none'; object-src 'none'; form-action 'self'; img-src 'self' data:; script-src 'self' https://static.cloudflareinsights.com; style-src 'self'; connect-src 'self' https://cloudflareinsights.com; upgrade-insecure-requests"

function jsonLd(objects) {
  // JSON-LD is data, not executable script, so the strict CSP does not block it.
  // Escape "<" so a closing tag inside a string can never end the element early.
  return objects
    .map(object => `<script type="application/ld+json">${JSON.stringify(object).replace(/</g, '\\u003c')}</script>`)
    .join('\n')
}

function breadcrumbs(site, trail) {
  const items = [{ label: site.name, href: '/' }, ...trail]
  const html = `<nav class="breadcrumbs" aria-label="Breadcrumb"><ol>${items
    .map((item, index) =>
      index === items.length - 1
        ? `<li aria-current="page">${inline(item.label)}</li>`
        : `<li><a href="${escapeAttr(item.href)}">${inline(item.label)}</a></li>`,
    )
    .join('')}</ol></nav>`
  const data = {
    '@context': 'https://schema.org',
    '@type': 'BreadcrumbList',
    itemListElement: items.map((item, index) => ({
      '@type': 'ListItem',
      position: index + 1,
      name: item.label,
      item: site.origin + item.href,
    })),
  }
  return { html, data }
}

function shell({ site, assets, route, title, description, canonicalPath, body, structuredData, ogType = 'website', keywords, dateModified, robots }) {
  const canonical = site.origin + canonicalPath
  const image = site.origin + site.image
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="strict-origin-when-cross-origin">
<meta http-equiv="Content-Security-Policy" content="${CSP}">
<title>${escapeHtml(title)}</title>
<meta name="description" content="${escapeAttr(description)}">
${robots ? `<meta name="robots" content="${escapeAttr(robots)}">
` : ''}${keywords?.length ? `<meta name="keywords" content="${escapeAttr(keywords.join(', '))}">\n` : ''}<link rel="canonical" href="${escapeAttr(canonical)}">
<link rel="icon" href="/zipp-bolt.svg" type="image/svg+xml">
<link rel="stylesheet" href="${escapeAttr(assets.css)}">
<link rel="alternate" type="application/rss+xml" title="${escapeAttr(site.name)} engineering journal" href="${escapeAttr(site.origin + '/journal/feed.xml')}">
<meta name="theme-color" content="#101016">
<meta name="color-scheme" content="dark">
<meta property="og:site_name" content="${escapeAttr(site.name)}">
<meta property="og:type" content="${ogType}">
<meta property="og:url" content="${escapeAttr(canonical)}">
<meta property="og:title" content="${escapeAttr(title)}">
<meta property="og:description" content="${escapeAttr(description)}">
<meta property="og:image" content="${escapeAttr(image)}">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta property="og:image:alt" content="${escapeAttr(site.imageAlt)}">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:title" content="${escapeAttr(title)}">
<meta name="twitter:description" content="${escapeAttr(description)}">
<meta name="twitter:image" content="${escapeAttr(image)}">
${dateModified ? `<meta property="article:modified_time" content="${escapeAttr(dateModified)}">\n` : ''}${jsonLd(structuredData)}
</head>
<body class="docs${route ? ` route-${escapeAttr(route)}` : ''}">
<a class="skip" href="#main">Skip to content</a>
<header class="site-header">
  <a class="brand" href="/"><img src="/zipp-bolt.svg" alt="" width="28" height="28"><span>${escapeHtml(site.name)}</span></a>
  <nav class="site-nav" aria-label="Documentation">${site.nav
    .map(item => `<a href="${escapeAttr(item.href)}"${item.href === canonicalPath ? ' aria-current="page"' : ''}>${escapeHtml(item.label)}</a>`)
    .join('')}</nav>
</header>
${body}
<footer class="site-footer">
  <p>${inline(site.footer.text)}</p>
  <nav aria-label="Footer">${site.footer.links.map(item => `<a href="${escapeAttr(item.href)}">${escapeHtml(item.label)}</a>`).join('')}</nav>
</footer>
</body>
</html>
`
}

// ---------------------------------------------------------------------------
// Page types
// ---------------------------------------------------------------------------

function articleMeta(page) {
  const modified = page.dateModified && page.dateModified !== page.datePublished
  return `<p class="page-meta"><span>Published <time datetime="${escapeAttr(page.datePublished)}">${humanDate(page.datePublished)}</time></span>${
    modified ? `<span>Updated <time datetime="${escapeAttr(page.dateModified)}">${humanDate(page.dateModified)}</time></span>` : ''
  }${page.readingMinutes ? `<span>${page.readingMinutes} min read</span>` : ''}</p>`
}

// Meta description for a journal entry: an explicit `description`, or the
// summary clipped at a word boundary to search-snippet length.
export const entryDescription = entry => {
  if (entry.description) return entry.description
  if (entry.summary.length <= 160) return entry.summary
  const clipped = entry.summary.slice(0, 157)
  return `${clipped.slice(0, clipped.lastIndexOf(' '))}…`
}

const wordCount = html => html.replace(/<[^>]+>/g, ' ').split(/\s+/).filter(Boolean).length

function relatedLinks(items) {
  if (!items?.length) return ''
  return `<nav class="related" aria-label="Related pages"><h2>Keep reading</h2><ul>${items
    .map(item => `<li><a href="${escapeAttr(item.href)}">${inline(item.label)}</a>${item.text ? ` <span>${inline(item.text)}</span>` : ''}</li>`)
    .join('')}</ul></nav>`
}

export function renderDocPage(site, assets, page, options = {}) {
  const path = options.path || `/${page.slug}/`
  const trail = [...(options.trail || []), { label: page.breadcrumb || page.h1, href: path }]
  const crumbs = breadcrumbs(site, trail)
  const sectionsHtml = renderSections(page.sections)
  const readingMinutes = Math.max(1, Math.round(wordCount(sectionsHtml) / 220))
  const body = `<main id="main" class="page">
${crumbs.html}
<article class="doc">
<header class="page-header">
<h1>${inline(page.h1)}</h1>
<p class="lede">${inline(page.lede)}</p>
${articleMeta({ ...page, readingMinutes })}
</header>
${renderToc(page.sections)}
${sectionsHtml}
${relatedLinks(page.related)}
</article>
</main>`
  const type = options.schemaType || page.schemaType || 'TechArticle'
  const structured = [
    {
      '@context': 'https://schema.org',
      '@type': type,
      headline: page.h1,
      name: page.title,
      description: page.description,
      url: site.origin + path,
      inLanguage: 'en',
      datePublished: page.datePublished,
      dateModified: page.dateModified || page.datePublished,
      keywords: page.keywords?.join(', '),
      author: { '@type': 'Organization', name: site.organization.name, url: site.organization.url },
      publisher: { '@type': 'Organization', name: site.organization.name, url: site.organization.url, logo: { '@type': 'ImageObject', url: site.origin + '/zipp-bolt.svg' } },
      image: site.origin + site.image,
      isPartOf: { '@type': 'WebSite', name: site.name, url: site.origin + '/' },
      about: { '@type': 'SoftwareApplication', name: site.software.name, url: site.origin + '/' },
    },
    ...(page.extraStructuredData || []),
    crumbs.data,
  ]
  return shell({
    site,
    assets,
    route: page.slug,
    title: page.title,
    description: page.description,
    canonicalPath: path,
    body,
    structuredData: structured,
    ogType: 'article',
    keywords: page.keywords,
    dateModified: page.dateModified || page.datePublished,
  })
}

function journalEntryCard(entry, { full = false } = {}) {
  const href = `/journal/${entry.slug}/`
  return `<article class="entry" data-tags="${escapeAttr((entry.tags || []).join(' '))}" data-date="${escapeAttr(entry.date)}">
<header><time datetime="${escapeAttr(entry.date)}">${humanDate(entry.date)}</time><h2><a href="${escapeAttr(href)}">${inline(entry.title)}</a></h2></header>
<p class="entry-summary">${inline(entry.summary)}</p>
${full ? entry.blocks.map(renderBlock).join('\n') : ''}
<footer>${(entry.tags || []).map(tag => `<a class="tag" href="/journal/#tag-${escapeAttr(tag)}" data-tag="${escapeAttr(tag)}">${escapeHtml(tag)}</a>`).join('')}${
    entry.commits?.length
      ? `<span class="commits">Commits: ${entry.commits
          .map(hash => `<a href="https://github.com/f2i-com/zipp.org/commit/${escapeAttr(hash)}" rel="noopener"><code>${escapeHtml(hash)}</code></a>`)
          .join(', ')}</span>`
      : ''
  }</footer>
</article>`
}

export function renderJournalIndex(site, assets, journal, entries) {
  const path = '/journal/'
  const crumbs = breadcrumbs(site, [{ label: journal.breadcrumb, href: path }])
  const tags = [...new Set(entries.flatMap(entry => entry.tags || []))].sort()
  const years = [...new Set(entries.map(entry => entry.date.slice(0, 4)))].sort()
  const body = `<main id="main" class="page journal">
${crumbs.html}
<header class="page-header">
<h1>${inline(journal.h1)}</h1>
<p class="lede">${inline(journal.lede)}</p>
<p class="page-meta"><span>${entries.length} entries</span><span>${humanDate(entries[entries.length - 1].date)} to ${humanDate(entries[0].date)}</span><span><a href="/journal/feed.xml">RSS feed</a></span></p>
</header>
${journal.intro.map(renderBlock).join('\n')}
<form class="journal-search" role="search" action="/journal/" method="get">
<label for="journal-query">Search the journal</label>
<div class="search-row"><input id="journal-query" name="q" type="search" placeholder="${escapeAttr(journal.searchPlaceholder)}" autocomplete="off"><button type="submit">Search</button></div>
<p class="search-status" id="journal-status" aria-live="polite">Showing all ${entries.length} entries.</p>
<div class="tag-filters" aria-label="Filter by topic">${tags.map(tag => `<button type="button" class="tag" data-tag="${escapeAttr(tag)}">${escapeHtml(tag)}</button>`).join('')}</div>
<div class="year-filters" aria-label="Jump to year">${years.map(year => `<a href="#year-${year}">${year}</a>`).join('')}</div>
</form>
<div class="entries" id="journal-entries">
${entries
  .map((entry, index) => {
    const year = entry.date.slice(0, 4)
    const heading = index === 0 || entries[index - 1].date.slice(0, 4) !== year ? `<h2 class="year" id="year-${year}">${year}</h2>` : ''
    return heading + journalEntryCard(entry)
  })
  .join('\n')}
</div>
<p class="search-empty" id="journal-empty" hidden>No entries match that search. Try a different word, or clear the filters.</p>
</main>
<script src="${escapeAttr(assets.journalJs)}" defer></script>`
  const structured = [
    {
      '@context': 'https://schema.org',
      '@type': 'Blog',
      name: journal.title,
      description: journal.description,
      url: site.origin + path,
      inLanguage: 'en',
      publisher: { '@type': 'Organization', name: site.organization.name, url: site.organization.url },
      blogPost: entries.slice(0, 20).map(entry => ({
        '@type': 'BlogPosting',
        headline: entry.title,
        datePublished: entry.date,
        url: `${site.origin}/journal/${entry.slug}/`,
      })),
    },
    crumbs.data,
  ]
  return shell({ site, assets, route: 'journal', title: journal.title, description: journal.description, canonicalPath: path, body, structuredData: structured, keywords: journal.keywords })
}

export function renderJournalEntry(site, assets, journal, entries, index) {
  const entry = entries[index]
  const path = `/journal/${entry.slug}/`
  const crumbs = breadcrumbs(site, [
    { label: journal.breadcrumb, href: '/journal/' },
    { label: entry.title, href: path },
  ])
  const newer = entries[index - 1]
  const older = entries[index + 1]
  const body = `<main id="main" class="page journal-entry">
${crumbs.html}
${journalEntryCard(entry, { full: true }).replace('<h2><a href', '<h1><a href').replace('</a></h2>', '</a></h1>')}
<nav class="pager" aria-label="Journal navigation">${
    older ? `<a class="older" href="/journal/${escapeAttr(older.slug)}/" rel="prev">← ${inline(older.title)}</a>` : '<span></span>'
  }<a href="/journal/">All entries</a>${newer ? `<a class="newer" href="/journal/${escapeAttr(newer.slug)}/" rel="next">${inline(newer.title)} →</a>` : '<span></span>'}</nav>
${relatedLinks(entry.related)}
</main>`
  const title = `${entry.title} · ${journal.shortTitle}`
  const structured = [
    {
      '@context': 'https://schema.org',
      '@type': 'BlogPosting',
      headline: entry.title,
      description: entry.summary,
      url: site.origin + path,
      inLanguage: 'en',
      datePublished: entry.date,
      dateModified: entry.dateModified || entry.date,
      keywords: (entry.tags || []).join(', '),
      author: { '@type': 'Organization', name: site.organization.name, url: site.organization.url },
      publisher: { '@type': 'Organization', name: site.organization.name, url: site.organization.url, logo: { '@type': 'ImageObject', url: site.origin + '/zipp-bolt.svg' } },
      image: site.origin + site.image,
      isPartOf: { '@type': 'Blog', name: journal.title, url: site.origin + '/journal/' },
    },
    crumbs.data,
  ]
  return shell({
    site,
    assets,
    route: 'journal-entry',
    title,
    description: entryDescription(entry),
    canonicalPath: path,
    body,
    structuredData: structured,
    ogType: 'article',
    keywords: entry.tags,
    dateModified: entry.dateModified || entry.date,
  })
}

export function renderFeed(site, journal, entries) {
  const items = entries
    .map(entry => {
      const url = `${site.origin}/journal/${entry.slug}/`
      const html = `<p>${inline(entry.summary)}</p>\n${entry.blocks.map(renderBlock).join('\n')}`
      return `<item>
<title>${escapeHtml(entry.title)}</title>
<link>${escapeHtml(url)}</link>
<guid isPermaLink="true">${escapeHtml(url)}</guid>
<pubDate>${rfc822(entry.date)}</pubDate>
${(entry.tags || []).map(tag => `<category>${escapeHtml(tag)}</category>`).join('\n')}
<description>${escapeHtml(entry.summary)}</description>
<content:encoded><![CDATA[${html.replace(/]]>/g, ']]]]><![CDATA[>')}]]></content:encoded>
</item>`
    })
    .join('\n')
  return `<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:content="http://purl.org/rss/1.0/modules/content/">
<channel>
<title>${escapeHtml(journal.title)}</title>
<link>${escapeHtml(site.origin + '/journal/')}</link>
<atom:link href="${escapeHtml(site.origin + '/journal/feed.xml')}" rel="self" type="application/rss+xml"/>
<description>${escapeHtml(journal.description)}</description>
<language>en</language>
<lastBuildDate>${rfc822(entries[0].date)}</lastBuildDate>
<image><url>${escapeHtml(site.origin + '/zipp-og-card.png')}</url><title>${escapeHtml(journal.title)}</title><link>${escapeHtml(site.origin + '/journal/')}</link></image>
${items}
</channel>
</rss>
`
}

// A compact JSON index so the search box (and anyone else) can query the journal
// without scraping HTML.
export const renderSearchIndex = entries =>
  JSON.stringify(
    entries.map(entry => ({
      slug: entry.slug,
      url: `/journal/${entry.slug}/`,
      date: entry.date,
      title: entry.title,
      summary: entry.summary,
      tags: entry.tags || [],
      text: entry.blocks
        .map(renderBlock)
        .join(' ')
        .replace(/<[^>]+>/g, ' ')
        .replace(/\s+/g, ' ')
        .trim(),
    })),
  )

export const renderSitemap = (site, urls) =>
  `<?xml version="1.0" encoding="UTF-8"?>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
${urls
  .map(
    ({ path, lastmod, priority }) =>
      `<url><loc>${escapeHtml(site.origin + path)}</loc>${lastmod ? `<lastmod>${escapeHtml(lastmod)}</lastmod>` : ''}${
        priority ? `<priority>${priority}</priority>` : ''
      }</url>`,
  )
  .join('\n')}
</urlset>
`

export function renderNotFound(site, assets) {
  const page = site.notFound
  const body = `<main id="main" class="page not-found">
<article class="doc">
<header class="page-header">
<h1>${inline(page.h1)}</h1>
<p class="lede">${inline(page.lede)}</p>
</header>
${page.blocks.map(renderBlock).join('\n')}
</article>
</main>`
  return shell({ site, assets, route: 'not-found', title: page.title, description: page.description, canonicalPath: '/404.html', body, structuredData: [], robots: 'noindex' })
}

export const renderRobots = site => `User-agent: *
Allow: /
Disallow: /api/

Sitemap: ${site.origin}/sitemap.xml
`

// ---------------------------------------------------------------------------
// Validation and whole-site rendering
// ---------------------------------------------------------------------------

export function validateContent(content) {
  const errors = []
  const seenTitles = new Map()
  const seenPaths = new Set()
  const register = (path, title, description) => {
    if (seenPaths.has(path)) errors.push(`Duplicate route ${path}`)
    seenPaths.add(path)
    if (!title || title.length > 70) errors.push(`${path}: title must be 1-70 characters (got ${title?.length ?? 0})`)
    if (!description || description.length < 50 || description.length > 170) errors.push(`${path}: description must be 50-170 characters (got ${description?.length ?? 0})`)
    if (seenTitles.has(title)) errors.push(`${path}: title duplicates ${seenTitles.get(title)}`)
    seenTitles.set(title, path)
  }
  for (const page of content.pages) {
    const path = `/${page.slug}/`
    register(path, page.title, page.description)
    if (!page.h1) errors.push(`${path}: missing h1`)
    if (!page.lede) errors.push(`${path}: missing lede`)
    for (const key of ['datePublished', 'dateModified']) {
      if (!/^\d{4}-\d{2}-\d{2}$/.test(page[key] || '')) errors.push(`${path}: ${key} must be YYYY-MM-DD`)
    }
    if (!page.sections?.length) errors.push(`${path}: needs at least one section`)
    const ids = new Set()
    for (const section of page.sections || []) {
      const id = section.id || slugify(section.heading)
      if (ids.has(id)) errors.push(`${path}: duplicate section id ${id}`)
      ids.add(id)
      for (const block of section.blocks || []) if (!renderers[block.type]) errors.push(`${path}: unknown block type ${block.type}`)
    }
  }
  const slugs = new Set()
  for (const entry of content.journal.entries) {
    const path = `/journal/${entry.slug}/`
    if (slugs.has(entry.slug)) errors.push(`Duplicate journal slug ${entry.slug}`)
    slugs.add(entry.slug)
    register(path, entry.title, entryDescription(entry))
    if (!entry.summary || entry.summary.length < 50) errors.push(`${path}: summary must be at least 50 characters`)
    if (!/^\d{4}-\d{2}-\d{2}$/.test(entry.date || '')) errors.push(`${path}: date must be YYYY-MM-DD`)
    if (!entry.blocks?.length) errors.push(`${path}: needs at least one block`)
    for (const block of entry.blocks || []) if (!renderers[block.type]) errors.push(`${path}: unknown block type ${block.type}`)
  }
  register('/journal/', content.journal.title, content.journal.description)
  return errors
}

export const sortedEntries = entries => [...entries].sort((a, b) => (a.date < b.date ? 1 : a.date > b.date ? -1 : a.slug.localeCompare(b.slug)))

// Returns Map<outputPath, text>. Paths are relative to the client output root,
// e.g. "javascript-engine/index.html", "sitemap.xml".
export function renderSite(content, assets) {
  const errors = validateContent(content)
  if (errors.length) throw new Error(`site/content.json is invalid:\n- ${errors.join('\n- ')}`)
  const { site, journal } = content
  const entries = sortedEntries(journal.entries)
  const files = new Map()
  const urls = [{ path: '/', lastmod: site.homeDateModified, priority: '1.0' }, { path: '/playground/', lastmod: site.homeDateModified, priority: '0.6' }]

  for (const page of content.pages) {
    const path = `/${page.slug}/`
    files.set(`${page.slug}/index.html`, renderDocPage(site, assets, page, { trail: page.parent ? [page.parent] : [] }))
    urls.push({ path, lastmod: page.dateModified || page.datePublished, priority: page.priority || '0.8' })
  }
  files.set('journal/index.html', renderJournalIndex(site, assets, journal, entries))
  urls.push({ path: '/journal/', lastmod: entries[0].date, priority: '0.8' })
  entries.forEach((entry, index) => {
    files.set(`journal/${entry.slug}/index.html`, renderJournalEntry(site, assets, journal, entries, index))
    urls.push({ path: `/journal/${entry.slug}/`, lastmod: entry.dateModified || entry.date, priority: '0.5' })
  })
  files.set('journal/feed.xml', renderFeed(site, journal, entries))
  files.set('journal/index.json', renderSearchIndex(entries))
  files.set('404.html', renderNotFound(site, assets))
  files.set('sitemap.xml', renderSitemap(site, urls))
  files.set('robots.txt', renderRobots(site))
  return files
}

// Every route the site serves, for the dev middleware and the tests.
export const siteRoutes = content => [
  ...content.pages.map(page => `/${page.slug}/`),
  '/journal/',
  ...content.journal.entries.map(entry => `/journal/${entry.slug}/`),
]
