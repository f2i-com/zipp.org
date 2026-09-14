// Client-side search and filtering for /journal/.
//
// The page already contains every entry as real HTML, so without JavaScript a
// reader (or a crawler) still sees the whole journal. This script only hides the
// entries that do not match. Full-text search uses the prebuilt
// /journal/index.json so long entries can be matched on their body text too.
;(function () {
  'use strict'
  var form = document.querySelector('.journal-search')
  var input = document.getElementById('journal-query')
  var status = document.getElementById('journal-status')
  var empty = document.getElementById('journal-empty')
  var list = document.getElementById('journal-entries')
  if (!form || !input || !list) return

  var entries = Array.prototype.slice.call(list.querySelectorAll('.entry'))
  var years = Array.prototype.slice.call(list.querySelectorAll('.year'))
  var tagButtons = Array.prototype.slice.call(form.querySelectorAll('.tag-filters .tag'))
  var total = entries.length
  var activeTag = ''
  var bodies = null
  var loading = null

  function normalize(text) {
    return String(text || '')
      .toLowerCase()
      .normalize('NFKD')
      .replace(/[̀-ͯ]/g, '')
  }

  function terms(query) {
    return normalize(query)
      .split(/\s+/)
      .filter(function (term) {
        return term.length > 0
      })
  }

  function loadBodies() {
    if (bodies || loading) return loading
    loading = fetch('/journal/index.json', { credentials: 'same-origin' })
      .then(function (response) {
        return response.ok ? response.json() : []
      })
      .then(function (index) {
        bodies = {}
        index.forEach(function (item) {
          bodies[item.slug] = normalize(item.text)
        })
      })
      .catch(function () {
        bodies = {}
      })
    return loading
  }

  function haystack(entry) {
    if (!entry.__hay) {
      var title = entry.querySelector('h2, h1')
      var summary = entry.querySelector('.entry-summary')
      entry.__hay = normalize(
        (title ? title.textContent : '') + ' ' + (summary ? summary.textContent : '') + ' ' + (entry.getAttribute('data-tags') || '') + ' ' + entry.getAttribute('data-date'),
      )
      var link = entry.querySelector('h2 a, h1 a')
      var match = link && /\/journal\/([^/]+)\//.exec(link.getAttribute('href') || '')
      entry.__slug = match ? match[1] : ''
    }
    return entry.__hay + ' ' + (bodies && bodies[entry.__slug] ? bodies[entry.__slug] : '')
  }

  function apply() {
    var query = input.value.trim()
    var words = terms(query)
    var shown = 0
    entries.forEach(function (entry) {
      var text = haystack(entry)
      var tags = ' ' + (entry.getAttribute('data-tags') || '') + ' '
      var visible = (!activeTag || tags.indexOf(' ' + activeTag + ' ') !== -1) &&
        words.every(function (word) {
          return text.indexOf(word) !== -1
        })
      entry.hidden = !visible
      if (visible) shown++
    })
    years.forEach(function (heading) {
      var year = heading.id.replace('year-', '')
      heading.hidden = !entries.some(function (entry) {
        return !entry.hidden && entry.getAttribute('data-date').slice(0, 4) === year
      })
    })
    tagButtons.forEach(function (button) {
      button.setAttribute('aria-pressed', button.getAttribute('data-tag') === activeTag ? 'true' : 'false')
    })
    if (empty) empty.hidden = shown > 0
    if (status) {
      var parts = []
      if (query) parts.push('matching “' + query + '”')
      if (activeTag) parts.push('tagged ' + activeTag)
      status.textContent = parts.length ? shown + ' of ' + total + ' entries ' + parts.join(' and ') + '.' : 'Showing all ' + total + ' entries.'
    }
    var url = new URL(window.location.href)
    if (query) url.searchParams.set('q', query)
    else url.searchParams.delete('q')
    if (activeTag) url.searchParams.set('tag', activeTag)
    else url.searchParams.delete('tag')
    url.hash = ''
    window.history.replaceState(null, '', url.pathname + url.search)
  }

  function search() {
    if (!bodies) {
      apply()
      loadBodies().then(apply)
    } else {
      apply()
    }
  }

  input.addEventListener('input', search)
  form.addEventListener('submit', function (event) {
    event.preventDefault()
    search()
  })
  tagButtons.forEach(function (button) {
    button.addEventListener('click', function () {
      var tag = button.getAttribute('data-tag') || ''
      activeTag = activeTag === tag ? '' : tag
      search()
    })
  })
  list.addEventListener('click', function (event) {
    var target = event.target
    if (target && target.classList && target.classList.contains('tag')) {
      event.preventDefault()
      activeTag = target.getAttribute('data-tag') || ''
      search()
      form.scrollIntoView({ behavior: 'smooth', block: 'start' })
    }
  })

  var initial = new URL(window.location.href)
  var q = initial.searchParams.get('q')
  var tag = initial.searchParams.get('tag')
  var hashTag = /^#tag-(.+)$/.exec(initial.hash)
  if (q) input.value = q
  if (tag) activeTag = tag
  else if (hashTag) activeTag = decodeURIComponent(hashTag[1])
  if (q || activeTag) search()
})()
