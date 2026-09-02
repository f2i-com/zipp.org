import { useEffect, useState } from 'react'

/**
 * Live repository facts from `api/stats.php` (see `public/api/stats.php`).
 * Every field is optional: the page renders its built-in figures first and
 * upgrades to live values only when the endpoint answers with a well-formed
 * document, so a static host (no PHP) or an offline GitHub API changes
 * nothing visible.
 */
export type RepoStats = {
  generatedAt: string
  stale: boolean
  stars?: number
  forks?: number
  openIssues?: number
  pushedAt?: string
  releaseTag?: string
  releaseUrl?: string
  releasePublishedAt?: string
  version?: string
  commitCount?: number
  latestCommitSha?: string
  latestCommitMessage?: string
  latestCommitDate?: string
  latestCommitUrl?: string
  all30?: number
  all13?: number
  hostile17?: number
  nodeWins?: number
  test262Pct?: number
  test262Pass?: number
  test262Total?: number
  captureCommit?: string
  startupMs?: number
}

const num = (v: unknown): number | undefined =>
  typeof v === 'number' && Number.isFinite(v) ? v : undefined
const str = (v: unknown): string | undefined =>
  typeof v === 'string' && v.length > 0 ? v : undefined
const obj = (v: unknown): Record<string, unknown> =>
  typeof v === 'object' && v !== null ? (v as Record<string, unknown>) : {}

export function parseRepoStats(raw: unknown): RepoStats | null {
  const root = obj(raw)
  const generatedAt = str(root.generated_at)
  if (!generatedAt) return null
  const repo = obj(root.repo)
  const release = obj(root.release)
  const commits = obj(root.commits)
  const latest = obj(commits.latest)
  const readme = obj(root.readme)
  return {
    generatedAt,
    stale: root.stale === true,
    stars: num(repo.stars),
    forks: num(repo.forks),
    openIssues: num(repo.open_issues),
    pushedAt: str(repo.pushed_at),
    releaseTag: str(release.tag),
    releaseUrl: str(release.url),
    releasePublishedAt: str(release.published_at),
    version: str(root.version),
    commitCount: num(commits.count),
    latestCommitSha: str(latest.sha),
    latestCommitMessage: str(latest.message),
    latestCommitDate: str(latest.date),
    latestCommitUrl: str(latest.url),
    all30: num(readme.all30),
    all13: num(readme.all13),
    hostile17: num(readme.hostile17),
    nodeWins: num(readme.node_wins),
    test262Pct: num(readme.test262_pct),
    test262Pass: num(readme.test262_pass),
    test262Total: num(readme.test262_total),
    captureCommit: str(readme.capture_commit),
    startupMs: num(readme.startup_ms),
  }
}

export function useRepoStats(): RepoStats | null {
  const [stats, setStats] = useState<RepoStats | null>(null)
  useEffect(() => {
    const controller = new AbortController()
    const url = new URL(`${import.meta.env.BASE_URL}api/stats.php`, document.baseURI)
    fetch(url.href, { signal: controller.signal, headers: { Accept: 'application/json' } })
      .then((response) => (response.ok ? response.json() : null))
      .then((json) => {
        const parsed = parseRepoStats(json)
        if (parsed) setStats(parsed)
      })
      .catch(() => {
        // The built-in figures stay. Nothing to report to the reader.
      })
    return () => controller.abort()
  }, [])
  return stats
}

/** "3 days ago" style relative time for a timestamp; empty when unparseable. */
export function relativeTime(iso: string | undefined, now = Date.now()): string {
  if (!iso) return ''
  const then = Date.parse(iso)
  if (!Number.isFinite(then)) return ''
  const seconds = Math.max(0, Math.round((now - then) / 1000))
  const units: [number, Intl.RelativeTimeFormatUnit][] = [
    [60, 'second'],
    [60, 'minute'],
    [24, 'hour'],
    [7, 'day'],
    [4.35, 'week'],
    [12, 'month'],
    [Number.POSITIVE_INFINITY, 'year'],
  ]
  let value = seconds
  for (const [size, unit] of units) {
    if (value < size) {
      return new Intl.RelativeTimeFormat('en', { numeric: 'auto' }).format(-Math.round(value), unit)
    }
    value /= size
  }
  return ''
}

export const formatCount = (n: number | undefined): string =>
  n === undefined ? '' : new Intl.NumberFormat('en').format(n)
