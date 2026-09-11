import { formatCount, formatDate, relativeTime, type RepoStats, type RepoStatus } from './repoStats'

export function RepositoryActivity({ stats, status, refreshing, refresh }: { stats: RepoStats; status: RepoStatus; refreshing: boolean; refresh: () => void }) {
  const statusText = { snapshot: 'Saved repository snapshot', synced: 'Connected to GitHub', stale: 'Showing cached GitHub data', offline: 'Offline · showing last known data' }[status]
  return (
    <section className="repository-section section-wrap" id="updates" aria-labelledby="updates-title">
      <div className="repository-heading">
        <div><p className="section-kicker">OUT IN THE OPEN</p><h2 id="updates-title">Fresh from<br /><em>the workbench.</em></h2></div>
        <div className="repository-sync"><span className={`sync-status sync-${status}`} role="status">{refreshing ? 'Checking GitHub…' : statusText}</span><p>Fetched {formatDate(stats.generatedAt)} · {relativeTime(stats.generatedAt)}</p><button type="button" onClick={refresh} disabled={refreshing}>{refreshing ? 'Checking…' : '↻ Check for updates'}</button></div>
      </div>
      <div className="repository-grid">
        <article className="latest-release-card">
          <div className="release-card-label"><span>LATEST STABLE RELEASE</span><span aria-hidden="true">↗</span></div>
          <a href={stats.releaseUrl ?? 'https://github.com/f2i-com/zipp.org/releases'} target="_blank" rel="noreferrer" className="release-version">{stats.releaseTag ?? 'Releases'}</a>
          <p>Native speed. Browser possibilities.</p>
          <p className="release-date">{stats.releasePublishedAt ? `Released ${formatDate(stats.releasePublishedAt)}` : 'Browse release details on GitHub'}</p>
          <a className="release-download" href={stats.releaseUrl ?? 'https://github.com/f2i-com/zipp.org/releases'} target="_blank" rel="noreferrer">Release notes & downloads <span aria-hidden="true">↗</span></a>
        </article>
        <div className="repository-details">
          <div className="repository-counters">
            <a href="https://github.com/f2i-com/zipp.org/stargazers" target="_blank" rel="noreferrer"><strong>{formatCount(stats.stars)}</strong><span>GitHub stars</span></a>
            <a href="https://github.com/f2i-com/zipp.org/forks" target="_blank" rel="noreferrer"><strong>{formatCount(stats.forks)}</strong><span>Forks</span></a>
            <a href="https://github.com/f2i-com/zipp.org/issues" target="_blank" rel="noreferrer"><strong>{formatCount(stats.openIssues)}</strong><span>Open issues + PRs</span></a>
          </div>
          <article className="latest-commit"><p className="lab-label">LATEST ON {stats.branch.toUpperCase()}</p><a href={stats.latestCommitUrl ?? 'https://github.com/f2i-com/zipp.org/commits/main'} target="_blank" rel="noreferrer">{stats.latestCommitMessage ?? 'Explore the latest changes'} <span aria-hidden="true">↗</span></a><p><code>{stats.latestCommitSha?.slice(0, 8)}</code> {formatDate(stats.latestCommitDate)} <span>· Source v{stats.version ?? '—'}</span></p></article>
          <div className="release-history"><span className="lab-label">RELEASE TRAIL</span><div>{stats.releases.map(release => <a key={release.tag} href={release.url} target="_blank" rel="noreferrer">{release.tag}<small>{formatDate(release.publishedAt)}</small></a>)}</div></div>
        </div>
      </div>
      <p className="repository-footnote">Repository facts refresh every 15 minutes while this page is open. Releases link straight to GitHub. Benchmark results below retain their measured version and capture date.</p>
    </section>
  )
}
