export type RepoRelease = { tag: string; name: string; url: string; publishedAt?: string }
export type RepoStats = {
  generatedAt: string
  stale: boolean
  cached: boolean
  branch: string
  sourceCommit?: string
  stars?: number
  forks?: number
  openIssues?: number
  license?: string
  pushedAt?: string
  releaseTag?: string
  releaseUrl?: string
  releasePublishedAt?: string
  releases: RepoRelease[]
  version?: string
  latestCommitSha?: string
  latestCommitMessage?: string
  latestCommitDate?: string
  latestCommitUrl?: string
  test262Pct?: number
  test262Pass?: number
  test262Total?: number
}

const obj = (v: unknown): Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v) ? v as Record<string, unknown> : {}
const str = (v: unknown): string | undefined => typeof v === 'string' && v.length > 0 ? v.slice(0, 500) : undefined
const count = (v: unknown): number | undefined => typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? v : undefined
const date = (v: unknown): string | undefined => str(v) && Number.isFinite(Date.parse(v as string)) ? v as string : undefined

export function repositoryUrl(value: unknown): string | undefined {
  try {
    const url = new URL(String(value))
    if (url.protocol === 'https:' && url.hostname === 'github.com' && !url.port && !url.username && !url.password && url.pathname.startsWith('/f2i-com/zipp.org/')) return url.href
  } catch { /* Untrusted API data is never used as a navigation target. */ }
  return undefined
}

export function parseRepoStats(raw: unknown): RepoStats | null {
  const root = obj(raw), source = obj(root.source)
  const generatedAt = date(root.generated_at)
  if (!generatedAt || source.repo !== 'f2i-com/zipp.org') return null
  const repo = obj(root.repo), release = obj(root.release), latest = obj(obj(root.commits).latest), readme = obj(root.readme)
  const releases = (Array.isArray(root.releases) ? root.releases : root.release ? [root.release] : []).slice(0, 3).flatMap(rawRelease => {
    const item = obj(rawRelease), tag = str(item.tag), url = repositoryUrl(item.url)
    return tag && url ? [{ tag, name: str(item.name) ?? tag, url, publishedAt: date(item.published_at) }] : []
  })
  const pct = typeof readme.test262_pct === 'number' && readme.test262_pct >= 0 && readme.test262_pct <= 100 ? readme.test262_pct : undefined
  return {
    generatedAt, stale: root.stale === true, cached: root.cached === true,
    branch: str(source.branch) ?? 'main', sourceCommit: str(source.commit),
    stars: count(repo.stars), forks: count(repo.forks), openIssues: count(repo.open_issues), license: str(repo.license),
    pushedAt: date(repo.pushed_at), version: str(root.version),
    releaseTag: str(release.tag), releaseUrl: repositoryUrl(release.url), releasePublishedAt: date(release.published_at), releases,
    latestCommitSha: str(latest.sha), latestCommitMessage: str(latest.message), latestCommitDate: date(latest.date), latestCommitUrl: repositoryUrl(latest.url),
    test262Pct: pct, test262Pass: count(readme.test262_pass), test262Total: count(readme.test262_total),
  }
}

export function relativeTime(iso: string | undefined, now = Date.now()): string {
  if (!iso) return ''
  const then = Date.parse(iso)
  if (!Number.isFinite(then)) return ''
  let value = Math.max(0, Math.round((now - then) / 1000))
  const units: [number, Intl.RelativeTimeFormatUnit][] = [[60, 'second'], [60, 'minute'], [24, 'hour'], [7, 'day'], [4.35, 'week'], [12, 'month'], [Infinity, 'year']]
  for (const [size, unit] of units) {
    if (value < size) return new Intl.RelativeTimeFormat('en', { numeric: 'auto' }).format(-Math.round(value), unit)
    value /= size
  }
  return ''
}

export const formatCount = (n: number | undefined): string => n === undefined ? '—' : new Intl.NumberFormat('en').format(n)
export const formatDate = (iso: string | undefined): string => iso && Number.isFinite(Date.parse(iso)) ? new Intl.DateTimeFormat('en', { day: 'numeric', month: 'short', year: 'numeric', timeZone: 'UTC' }).format(new Date(iso)) : 'Date unavailable'
