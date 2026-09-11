# ZIPP landing page

React + TypeScript, with Vite and a Cloudflare Worker. The landing page includes
an executable ZIPP WASM playground, Softn and Outerstead project showcases,
repository activity, and dated native benchmark evidence.

## Develop and check

```sh
npm ci
npm run dev
npm run test
npm run build
```

The dependency-free tests use Node's TypeScript stripping (Node 22.18+ or 24
recommended). They cover GitHub response validation, caching, concurrent
refreshes, stale/offline behavior, and execution of the actual story sample in
the checked-in WASM engine. Build output is `dist/client` and `dist/server`.

## Automatic repository updates

The browser requests `/api/stats` on mount, every 15 minutes while visible, and
when returning to a tab whose data is due for refresh. Readers can also use
“Check for updates.” Both `/api/stats` and `/api/stats.php` are routed to the
Cloudflare Worker before static assets. Existing PHP hosting is supported by
`public/api/stats.php` as a fallback.

The server fetches the fixed public repository `f2i-com/zipp.org`: stable
releases, stars, forks, open issues plus PRs, license, the default branch's
latest commit, workspace version, and README-reported test262 coverage. Source
files are fetched at the same immutable commit. Prereleases and drafts are
excluded. Live stats do not alter benchmark numbers or the bundled WASM engine.

Worker responses are cached for 15 minutes with a 24-hour stale limit. An
in-flight refresh is shared within an isolate, and failures back off for one
minute. An upstream failure returns the previous snapshot with its original
fetch timestamp and a stale flag, or HTTP 503 if no usable cache remains.
The client keeps its last good data and explicitly labels saved/offline states.
The PHP endpoint similarly serves a coherent cached snapshot on partial failure.

No token is required. For higher GitHub API limits, optionally set
`ZIPP_GITHUB_TOKEN` only in the hosting server environment (or an ignored local
`.dev.vars` file). It is never exposed to the browser or sent to raw-file URLs.
The HTTP response is not browser-cached, so stale labels cannot be hidden by a
browser cache. The upstream server cache still protects GitHub's rate limit.

`src/repo-snapshot.json` supplies an offline fallback. It is visibly identified
as a saved snapshot until the endpoint succeeds. Refresh it when desired:

```sh
npm run refresh:repo
```

A purely static host without the Worker or PHP can display this snapshot, but
cannot fetch live repository facts. Normal builds work without network access.

## Evidence and engine provenance

Canonical native CLI numbers remain attached to the reviewed 2 September 2026
captures at `8229b3fc`:

- `../bench/real13_8229b3fc_pgo_2026-09-02.json`
- `../bench/hostile/head_clean_8229b3fc_pgo_2026-09-02.json`

The executable's own recorded build identity is v0.0.12. Aggregate descriptive
intervals are reported by the README pinned at repository commit
`e6e0f65dd402f1bf75b9675d7acd904cb239c5a3`. This README calls the same capture
v0.0.13; the page uses the artifact's recorded v0.0.12 identity. Benchmark
headlines, confidence intervals, row counts, and the table refer to the same
capture, rather than silently mixing new README headlines with old table rows.

The checked-in browser engine is v0.0.15, 5,119,419 raw bytes, SHA-256
`7c40f488d8b69209eea9e56dea2b0be13b3da0fd3810f3d8ec1d44c3d95972e4`.
The file pair is fingerprinted together in Vite configuration to avoid loading
mismatched glue and WASM files. Each run uses a disposable Worker, a 6-second
host deadline, and the module's instruction and heap limits.

The adventure demonstrates classes, getters, closures, Math.imul and a seeded
PRNG, Map, Set, object/array operations, branching, and JSON serialization.
Each adventure run receives one uint32 seed from the browser's Web Crypto API;
the Worker injects that numeric value as `STORY_SEED`, without exposing host
APIs to guest code. ZIPP's own Math.random starts deterministically in a fresh
VM, so host-provided entropy is intentional. A literal seed replays a story;
outside the playground the example defaults to seed 42.
It is an original standalone demonstration, not Outerstead's game code.

The local development HTML permits Vite's inline setup and CSS, and does not
upgrade HTTP localhost requests to HTTPS. The production CSP remains strict.
