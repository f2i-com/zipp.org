# ZIPP landing page

React + TypeScript, with Vite and a Cloudflare Worker. The landing page introduces
Python, JavaScript and browser GPU compute. It embeds
the full folder playground, provides a dedicated `/playground/` route, and includes
a recorded Python Game of Life GIF. Existing Softn/Outerstead
showcases, repository activity, dated native benchmarks and the optional JavaScript
story scratchpad remain available.

## Documentation pages, sitemap and journal feed

`/javascript-engine/`, `/python/`, `/wasm/`, `/webgpu/`, `/test262/`, `/benchmarks/`,
`/architecture/`, `/embedding/`, `/sandbox/`, `/comparisons/`, `/releases/`,
`/journey/`, `/articles/…` and `/journal/…` are static HTML pages rendered at build
time from **one file, `site/content.json`**, by `site/render.mjs` through the
`staticSite()` Vite plugin. The same build writes `sitemap.xml`, `robots.txt`,
`404.html`, the journal RSS feed at `/journal/feed.xml` and the journal search
index. `npm run dev` serves the pages live from the JSON; `npm test` validates the
content (unique titles, description lengths, dates, internal links, feed shape).
See `site/README.md` for the block types and editing rules. The canonical origin
is `https://www.zipp.org`; the apex domain should redirect to it at the edge.

## Full Python / JavaScript project playground

`/playground` opens the same folder-based UI as `crates/zipp-wasm/playground`:
local folder/file loading, samples, source editor, entry selection, arguments,
canvas, console, autosave, run/stop, and WebGL2/WebGPU selection for Python graphs.
Files are read into a browser virtual filesystem, not sent to a server to execute.
The page runs a Python-enabled Zipp WASM engine. The `torch` subset still uses CPU;
browser GPU graphs use `zipp_gpu`. JavaScript guest GPU requests are not yet wired
into the stock playground, as described in the main README.

`scripts/sync-playground.mjs` copies the maintained browser sources, GPU kernels
and selected examples into `public/` before dev/build. The paired engine files
under `public/playground-runtime/` are committed and SHA-256 checked against their
manifest, so a clean checkout can build without Rust or downloading WASM.
To update that pair after rebuilding `crates/zipp-wasm/dist/all/`, run:

```sh
node scripts/sync-playground.mjs --refresh-engine
```

The landing's iframe and dedicated route load the same public files. The old
JavaScript-only story scratchpad loads only when expanded. The native NCA research
project is maintained separately at https://github.com/f2i-com/neuralautomata.com.

## Recorded examples

`public/demos/` contains the actual local browser captures, still posters, an SVG
explaining Python on Zipp WASM, and `provenance.json`. Both README and landing reuse
these same files. GIFs autoplay and loop continuously. The landing honors
reduced-motion settings and lets readers pause GIFs by switching to static posters.
Regenerate from the repository root:

```sh
python crates/zipp-wasm/playground/capture-demos.py
```

This needs Chrome, Python Playwright, ffmpeg on PATH, the Python-enabled WASM
build and working browser WebGL2 hardware. Python Life executes in Zipp WASM. They are not
performance measurements or claims that full PyTorch runs in browser WASM.

## Develop and check

```sh
npm ci
npm run dev
npm run test
npm run build
```

For browser integration checks, start `npm run preview` and run
`python scripts/smoke-browser.py http://127.0.0.1:4173` with Chrome and Python
Playwright installed. This checks the embedded and dedicated playground, real
Python/JavaScript execution, nested folder loading, media controls and mobile
layout. Set `REQUIRE_GPU=1` to also require hardware WebGL2 and WebGPU; the default
checks the portable WASM compute backend.

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

The optional JavaScript story scratchpad's browser engine is v0.0.15, 5,119,419 raw bytes, SHA-256
`7c40f488d8b69209eea9e56dea2b0be13b3da0fd3810f3d8ec1d44c3d95972e4`.
Its file pair is fingerprinted together in Vite configuration to avoid loading
mismatched glue and WASM files. Each run uses a disposable Worker, a 6-second
host deadline, and the module's instruction and heap limits.

The full Python project playground uses its separately checked
`public/playground-runtime/manifest.json` pair and a five-second per-request
deadline. Its actual profile appears in the playground status bar.

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
