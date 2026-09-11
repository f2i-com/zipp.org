import { createStatsHandler } from './repository.js'

const repositoryStats = createStatsHandler()

export default {
  async fetch(request, env) {
    const path = new URL(request.url).pathname
    if (path === '/api/stats' || path === '/api/stats.php') {
      return repositoryStats(request, env, caches.default)
    }
    return new Response('Not found', {
      status: 404,
      headers: { 'content-type': 'text/plain; charset=utf-8' },
    })
  },
}
