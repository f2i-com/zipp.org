# ZIPP landing page

React + TypeScript, with Vite and a Cloudflare Worker. The landing page introduces
ZIPP, the Zippy Interpreter for Polyglot Programming: a portable polyglot runtime
that runs JavaScript and Python today. It embeds the full folder playground and
provides a dedicated `/playground/` route and a `/sandbox/playground/` route.
Existing Softn/Outerstead showcases, repository activity, dated native benchmarks
and the optional JavaScript story scratchpad remain available.

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
The page runs a Python-enabled Zipp WASM engine. Eager `torch` runs on the CPU;
experimental `torch.compile` runs GPU inference and dense training steps (SGD with
momentum, Adam or AdamW; relu, gelu, sigmoid, tanh, softmax and cross-entropy), and
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

The pair must be a LABELLED build of the release the site announces. The sync
script warns whenever `manifest.profile.version` differs from
`landing/package.json`, or `profile.source.sha` is null (an unlabelled local
build), and `--require-release` turns that warning into an error; `ci.yml`'s
landing job runs it that way. An unlabelled pair once served the public
`/playground` for a day with a class-method closure bug that the release it
claimed had already fixed. The simplest correct refresh is to unpack the
`zipp-wasm-<version>-web-python` release asset (verify its `SHA256SUMS`), copy
its `zipp_wasm.js` and `zipp_wasm_bg.wasm` into `crates/zipp-wasm/dist/all/`,
then run the command above.

The landing's iframe and dedicated route load the same public files. The old
JavaScript-only story scratchpad loads only when expanded. The native NCA research
project is maintained separately at https://github.com/f2i-com/neuralautomata.com.

## Sandbox playground

`/sandbox/playground/` (`public/sandbox/playground/`, static files like
`/playground/`) edits JavaScript and runs it in a sandbox made by the embed kit's
loader, `zipp.mjs`, with a switch for each permission (storage, files, network,
host functions) and each limit (steps, time), and shows the output and any
refusal or timeout. It needs a release that publishes single-file engines
(0.0.26 or later). Copy one in from the release files, then build:

```sh
node scripts/sync-sandbox.mjs <dir>   # zipp-<v>-javascript.wasm and zipp.mjs or the unpacked embed kit
node scripts/sync-sandbox.mjs --check # the files match public/sandbox/manifest.json
```

The script checks the module's own `zipp.meta` record and the kit's
`modules.json` and `SHA256SUMS`, copies `zipp.mjs` and the module into
`public/sandbox/`, and writes `manifest.json` (`version`, `file`, `sha256`, the
commit and the loader's hash); the page reads it to find the module. Without a
manifest the page says the sandbox is available once 0.0.26 is published. The
landing tests check that `public/sandbox/` holds a complete engine or none.

## Embed section and starter project

The home page's "Embed ZIPP on your site" section (`src/EmbedSection.tsx`, at
`/#embed`) shows the public way to embed ZIPP: an engine `.wasm` and `zipp.mjs`
from the release's embed kit. Its release, file names, sizes, example programs
and expected output live in `src/embed-data.mjs`, which the page, the starter
generator and the tests share. The starter project it offers,
`public/embed/zipp-starter-<v>.zip` (`index.html`, `main.mjs` and a README with
the download and serve commands; no engine or loader inside), is generated:

```sh
node scripts/build-embed-starter.mjs           # write the ZIP
node scripts/build-embed-starter.mjs --check   # fail if it differs from what the script makes
```

`tests/embed.test.mjs` checks the section, the ZIP's files and pinned URLs, and
runs the displayed example through `public/sandbox/zipp.mjs` and its engine.

## Recorded examples

`public/demos/` contains the actual local browser captures, still posters, an SVG
explaining Python on Zipp WASM, and `provenance.json`. The `/python/` and
`/webgpu/` documentation pages use them; the landing page does not, since its
embedded playground runs the real thing. Regenerate from the repository root:

```sh
python crates/zipp-wasm/playground/capture-demos.py
```

This needs Chrome, Python Playwright, ffmpeg on PATH, the Python-enabled WASM
build and working browser WebGL2 hardware. Python Life executes in Zipp WASM. They are not
performance measurements or claims that full PyTorch runs in browser WASM.

## Develop and check

`package.json` overrides Miniflare's `undici` dependency to 7.30.1 to fix
[GHSA-3wwx-pv8p-q78v](https://github.com/advisories/GHSA-3wwx-pv8p-q78v),
[GHSA-4qv8-2p9j-2685](https://github.com/advisories/GHSA-4qv8-2p9j-2685),
[GHSA-f72v-6w45-q472](https://github.com/advisories/GHSA-f72v-6w45-q472) and
[GHSA-xjj6-f33q-929c](https://github.com/advisories/GHSA-xjj6-f33q-929c).
Remove the override once the Cloudflare tooling resolves a patched version
without it; verify with `npm audit` after updating the lockfile.

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

The browser requests `/api/stats.php` (then `/api/stats`) on mount, every 15 minutes while visible, and
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

The benchmark section's figures come from `src/benchmark-data.json`, which
`python scripts/import-benchmarks.py` (run from the repository root as
`python landing/scripts/import-benchmarks.py`) writes from the 21 October 2026
captures and nothing else:

- JIT against JIT, the canonical PGO build of the 0.0.28 engine (`zipp 0.0.28`,
  clean source `84012a2d`): `../bench/real13_84012a2d_pgo_2026-10-21.json` and
  `../bench/hostile/head_clean_84012a2d_pgo_2026-10-21.json`
- interpreter against interpreter, the same binary with `ZIPP_NOJIT=1`:
  `../bench/comparison/interp_real13_84012a2d_2026-10-21.json` and `../bench/comparison/interp_hostile17_84012a2d_2026-10-21.json`
- Python against CPython, the published release binary: `../bench/python/cpython_da0fe6b1_2026-10-21.json`
  and `../bench/python/cpython_nojit_da0fe6b1_2026-10-21.json`
- the release build as downloaded: `../bench/comparison/release_real13_da0fe6b1_2026-10-21.json` and
  `../bench/comparison/pgo_ab_real13_da0fe6b1_84012a2d_2026-10-21.json`

The importer checks that the canonical pair is publishable and exact, and that
replaying the saved observations through `tools/bench.py` reproduces the
capture's own geomeans before it derives the Bun and Deno ones;
`--check` reports whether the committed data is current. The page states only
what the data says (counts and directions are computed), so a new capture
needs no hand edits there; the `/benchmarks/` page in `site/content.json`,
`docs/PERFORMANCE.md` and the root README quote the same figures. The previous
canonical pairs (`f1739644`, 18 October 2026, recorded as v0.0.26, and `fa8e3b86`,
15 October 2026, v0.0.24) stay in `../bench/` as prior series points.

The optional JavaScript story scratchpad's browser engine is v0.0.30, 4,880,200 raw bytes, SHA-256
`74a4417de548eb04f4d433ef42f01d3e8f6795d419460ec137c34bd471d056c5`.
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
