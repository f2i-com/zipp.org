import { useCallback, useEffect, useRef, useState } from 'react'
import snapshot from './repo-snapshot.json'
import { parseRepoStats, type RepoStats } from './repoData'
export { formatCount, relativeTime, formatDate, type RepoStats } from './repoData'

export type RepoStatus = 'snapshot' | 'synced' | 'stale' | 'offline'
export const bundledStats = parseRepoStats(snapshot)!
const REFRESH_MS = 15 * 60 * 1000

export function useRepoStats() {
  const [stats, setStats] = useState<RepoStats>(bundledStats)
  const [status, setStatus] = useState<RepoStatus>('snapshot')
  const [refreshing, setRefreshing] = useState(false)
  const active = useRef<AbortController | null>(null)
  const lastAttempt = useRef(0)

  const refresh = useCallback(async () => {
    if (active.current) return
    const controller = new AbortController()
    active.current = controller
    lastAttempt.current = Date.now()
    setRefreshing(true)
    const timeout = window.setTimeout(() => controller.abort(), 25_000)
    try {
      let parsed: RepoStats | null = null
      // The Worker handles /api/stats; existing PHP hosting keeps working too.
      for (const path of ['api/stats', 'api/stats.php']) {
        if (controller.signal.aborted) break
        try {
          const response = await fetch(new URL(`${import.meta.env.BASE_URL}${path}`, document.baseURI), { signal: controller.signal, headers: { Accept: 'application/json' }, cache: 'no-cache' })
          if (response.status === 503 || response.status === 429) break
          if (response.ok && response.headers.get('content-type')?.includes('application/json')) parsed = parseRepoStats(await response.json())
          if (parsed) break
        } catch { /* Try the legacy endpoint when available. */ }
      }
      if (active.current !== controller) return
      if (parsed) {
        setStats(parsed)
        const old = Date.now() - Date.parse(parsed.generatedAt) > REFRESH_MS
        setStatus(parsed.stale || old ? 'stale' : 'synced')
      } else setStatus('offline')
    } finally {
      window.clearTimeout(timeout)
      if (active.current === controller) { active.current = null; setRefreshing(false) }
    }
  }, [])

  useEffect(() => {
    void refresh()
    const onVisible = () => {
      if (document.visibilityState === 'visible' && Date.now() - lastAttempt.current >= REFRESH_MS) void refresh()
    }
    const interval = window.setInterval(onVisible, REFRESH_MS)
    document.addEventListener('visibilitychange', onVisible)
    window.addEventListener('focus', onVisible)
    return () => {
      window.clearInterval(interval)
      document.removeEventListener('visibilitychange', onVisible)
      window.removeEventListener('focus', onVisible)
      active.current?.abort()
      active.current = null
    }
  }, [refresh])
  return { stats, status, refreshing, refresh }
}
