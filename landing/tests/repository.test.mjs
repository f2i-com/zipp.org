import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { createStatsHandler, fetchRepository, readReadmeFacts, REFRESH_SECONDS, MAX_STALE_SECONDS } from '../worker/repository.js'
import { parseRepoStats, repositoryUrl } from '../src/repoData.ts'

const snapshot = JSON.parse(readFileSync(new URL('../src/repo-snapshot.json', import.meta.url)))
const sha = 'a'.repeat(40)
function upstream() {
  let calls = [], failed = false
  return {
    calls, fail: () => { failed = true },
    fetcher: async (url, options) => {
      calls.push({ url, options })
      if (failed) return new Response('', { status: 429 })
      let value
      if (url.endsWith('/releases/latest')) return Response.json({ draft: false, prerelease: false, tag_name: 'v1.2.3', html_url: 'https://github.com/f2i-com/zipp.org/releases/tag/v1.2.3', published_at: '2026-09-01T00:00:00Z' })
      if (url.endsWith('/README.md')) return new Response('**99.997% of test262**: 95,939 / 95,942 required executions.')
      if (url.endsWith('/Cargo.toml')) return new Response('[workspace.package]\nversion = "1.2.3"')
      if (url.includes('/releases?')) value = [
        { draft: false, prerelease: true, tag_name: 'v9-beta' },
        { draft: false, prerelease: false, tag_name: 'v1.2.3', html_url: 'https://github.com/f2i-com/zipp.org/releases/tag/v1.2.3', published_at: '2026-09-01T00:00:00Z' },
      ]
      else if (url.includes('/commits?')) value = [{ sha, html_url: `https://github.com/f2i-com/zipp.org/commit/${sha}`, commit: { message: 'New engine\nMore detail', committer: { date: '2026-09-01T00:00:00Z' } } }]
      else value = { default_branch: 'trunk', stargazers_count: 0, forks_count: 2, open_issues_count: 3, license: { spdx_id: 'Apache-2.0' } }
      return Response.json(value)
    },
  }
}

test('public metadata follows default branch, excludes prereleases and pins source files to one commit', async () => {
  const mock = upstream()
  const data = await fetchRepository(mock.fetcher, 'test-server-token')
  assert.equal(data.release.tag, 'v1.2.3')
  assert.equal(data.version, '1.2.3')
  assert.equal(data.source.branch, 'trunk')
  assert.equal(data.commits.latest.message, 'New engine')
  assert.equal(data.repo.stars, 0)
  assert.equal(mock.calls.length, 6)
  for (const call of mock.calls.filter(x => x.url.includes('raw.githubusercontent.com'))) {
    assert.ok(call.url.includes(`/${sha}/`))
    assert.equal(call.options.headers.Authorization, undefined)
  }
})

test('concurrent visitors share a refresh and fresh cache prevents repeat upstream calls', async () => {
  const mock = upstream(), handler = createStatsHandler({ fetcher: mock.fetcher })
  const request = new Request('https://zipp.test/api/stats')
  const results = await Promise.all([handler(request), handler(request), handler(request)])
  assert.ok(results.every(response => response.ok))
  assert.equal(mock.calls.length, 6)
  const cached = await (await handler(request)).json()
  assert.equal(cached.cached, true)
  assert.equal(mock.calls.length, 6)
})

test('failed refresh preserves original timestamp and stops serving an expired cache', async () => {
  const mock = upstream()
  let time = Date.now()
  const handler = createStatsHandler({ fetcher: mock.fetcher, now: () => time })
  const request = new Request('https://zipp.test/api/stats')
  const first = await (await handler(request)).json()
  time += (REFRESH_SECONDS + 10) * 1000
  mock.fail()
  const stale = await (await handler(request)).json()
  assert.equal(stale.stale, true)
  assert.equal(stale.generated_at, first.generated_at)
  const calls = mock.calls.length
  await handler(request)
  assert.equal(mock.calls.length, calls, 'rate-limit backoff prevents retry storms')
  time += (MAX_STALE_SECONDS + 10) * 1000
  assert.equal((await handler(request)).status, 503)
})

test('no cache produces explicit unavailability and unsupported methods cannot refresh', async () => {
  const mock = upstream(); mock.fail()
  const handler = createStatsHandler({ fetcher: mock.fetcher })
  assert.equal((await handler(new Request('https://zipp.test/api/stats', { method: 'POST' }))).status, 405)
  assert.equal(mock.calls.length, 0)
  assert.equal((await handler(new Request('https://zipp.test/api/stats'))).status, 503)
})

test('client rejects malformed data and unsafe links while retaining legitimate zero counts', () => {
  assert.ok(parseRepoStats(snapshot))
  assert.equal(parseRepoStats('<html>fallback</html>'), null)
  assert.equal(parseRepoStats({ ...snapshot, generated_at: 'invalid' }), null)
  assert.equal(parseRepoStats({ ...snapshot, source: { repo: 'other/repo' } }), null)
  const data = parseRepoStats({ ...snapshot, repo: { stars: 0, forks: -1 }, release: { tag: 'v1', url: 'javascript:alert(1)' } })
  assert.equal(data.stars, 0)
  assert.equal(data.forks, undefined)
  assert.equal(data.releaseUrl, undefined)
  for (const url of ['https://github.com.evil.test/f2i-com/zipp.org/releases', 'https://github.com/other/repo/releases', 'https://user:secret@github.com/f2i-com/zipp.org/releases']) assert.equal(repositoryUrl(url), undefined)
  assert.deepEqual(readReadmeFacts('**200% of test262**: 200 / 100'), {})
  assert.deepEqual(readReadmeFacts('A new README format'), {})
})
