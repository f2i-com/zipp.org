// A fixed public repository: this endpoint never accepts upstream URLs from clients.
export const REPOSITORY = 'f2i-com/zipp.org'
export const REFRESH_SECONDS = 15 * 60
export const MAX_STALE_SECONDS = 24 * 60 * 60
const API = `https://api.github.com/repos/${REPOSITORY}`

export function readReadmeFacts(text) {
  const match = text.match(/\*\*([\d.]+)% of test262\*\*:\s*([\d,]+)\s*\/\s*([\d,]+)/)
  if (!match) return {}
  const [, percent, passed, total] = match
  const pass = Number(passed.replaceAll(',', ''))
  const count = Number(total.replaceAll(',', ''))
  return pass <= count && count > 0 && Number(percent) <= 100
    ? { test262_pct: Number(percent), test262_pass: pass, test262_total: count }
    : {}
}

export async function fetchRepository(fetcher = fetch, token) {
  const headers = { Accept: 'application/vnd.github+json', 'User-Agent': 'zipp-landing', 'X-GitHub-Api-Version': '2022-11-28' }
  if (token) headers.Authorization = `Bearer ${token}`
  const get = async (url, raw = false, allowMissing = false) => {
    const response = await fetcher(url, { headers: raw ? { 'User-Agent': 'zipp-landing' } : headers, signal: AbortSignal.timeout(8000) })
    if (allowMissing && response.status === 404) return null
    if (!response.ok) throw new Error(`GitHub returned ${response.status}`)
    return raw ? response.text() : response.json()
  }
  const [repo, releases, latestRelease] = await Promise.all([get(API), get(`${API}/releases?per_page=10`), get(`${API}/releases/latest`, false, true)])
  if (!repo.default_branch || !Array.isArray(releases)) throw new Error('Invalid repository response')
  const commits = await get(`${API}/commits?sha=${encodeURIComponent(repo.default_branch)}&per_page=1`)
  const latest = commits[0]
  if (!/^[a-f0-9]{40}$/.test(latest?.sha)) throw new Error('Invalid commit response')
  // Read both files at the same immutable revision, rather than racing a push.
  const raw = `https://raw.githubusercontent.com/${REPOSITORY}/${latest.sha}`
  const [readme, cargo] = await Promise.all([get(`${raw}/README.md`, true), get(`${raw}/Cargo.toml`, true)])
  const toRelease = release => ({
    tag: release.tag_name, name: release.name || release.tag_name,
    url: release.html_url, published_at: release.published_at,
  })
  // GitHub's latest-release selection can differ from creation-order listings.
  const stable = [...(latestRelease ? [latestRelease] : []), ...releases]
    .filter((release, index, all) => !release.draft && !release.prerelease && all.findIndex(item => item.tag_name === release.tag_name) === index)
    .slice(0, 3).map(toRelease)
  return {
    generated_at: new Date().toISOString(), stale: false,
    source: { repo: REPOSITORY, branch: repo.default_branch, commit: latest.sha },
    repo: { stars: repo.stargazers_count, forks: repo.forks_count, open_issues: repo.open_issues_count, pushed_at: repo.pushed_at, license: repo.license?.spdx_id },
    release: latestRelease ? toRelease(latestRelease) : null, releases: stable,
    commits: { latest: { sha: latest.sha, message: latest.commit.message.split('\n')[0], date: latest.commit.committer.date, url: latest.html_url } },
    version: cargo.match(/\[workspace\.package\][\s\S]*?^version\s*=\s*"([^"]+)"/m)?.[1],
    readme: readReadmeFacts(readme),
  }
}

const json = (body, status = 200) => new Response(JSON.stringify(body), {
  status, headers: { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' },
})

export function createStatsHandler({ fetcher = fetch, now = Date.now } = {}) {
  let memory = null
  let pending = null
  let retryAfter = 0
  return async function repositoryStats(request, env = {}, cache) {
    if (request.method !== 'GET' && request.method !== 'HEAD') return new Response(null, { status: 405, headers: { Allow: 'GET, HEAD' } })
    const key = new Request(new URL('/api/repository-cache-v1', request.url))
    let cached = memory
    if (!cached && cache) {
      try { cached = await (await cache.match(key))?.json() } catch { /* Cache failure must not prevent a refresh. */ }
    }
    const age = cached ? (now() - Date.parse(cached.generated_at)) / 1000 : Infinity
    if (age >= 0 && age < REFRESH_SECONDS) return json({ ...cached, cached: true })
    try {
      if (now() < retryAfter) throw new Error('Refresh backoff')
      if (!pending) pending = fetchRepository(fetcher, env.ZIPP_GITHUB_TOKEN)
        .then(async fresh => {
          memory = fresh
          if (cache) {
            try { await cache.put(key, new Response(JSON.stringify(fresh), { headers: { 'Cache-Control': `public, max-age=${MAX_STALE_SECONDS}` } })) } catch { /* Memory cache remains available. */ }
          }
          return fresh
        })
        .catch(error => { retryAfter = now() + 60_000; throw error })
        .finally(() => { pending = null })
      return json(await pending)
    } catch {
      if (age >= 0 && age < MAX_STALE_SECONDS) return json({ ...cached, stale: true, cached: true })
      return json({ error: 'Repository updates are temporarily unavailable.' }, 503)
    }
  }
}
