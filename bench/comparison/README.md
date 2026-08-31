# QuickJS-NG and Boa comparison

This is a **diagnostic ecosystem comparison**, separate from Zipp's canonical
Node/Bun/Deno publication series.  It answers two narrower questions:

1. How do Zipp's default native engine and interpreter-only mode compare with
   pinned QuickJS-NG and Boa interpreter builds on identical work?
2. How large are the projects' distributable WebAssembly modules under their
   published interfaces?

Do not splice these numbers into the canonical engine geomean.  The engines do
not expose identical language, locale, host, or embedding surfaces, and the
Wasm packages in particular are not interface-equivalent.

## Pinned native builds

### Zipp

- Release: `v0.0.3`
- Commit: `d71168a9fba3c4b97a05aaacbf14cf046dc65d38`
- Build: clean detached worktree, locked graph, Rust/Cargo 1.92.0, repository
  `release` profile (optimization level 3, fat LTO, one codegen unit, aborting
  panics), default features, no extra Rust flags

```powershell
git worktree add --detach `
  target\comparison\sources\zipp-d71168a `
  d71168a9fba3c4b97a05aaacbf14cf046dc65d38
$comparisonRoot = (Resolve-Path target\comparison).Path
$env:CARGO_TARGET_DIR = $comparisonRoot + '\build\zipp-native-d71168a'
Remove-Item Env:RUSTFLAGS,Env:CARGO_ENCODED_RUSTFLAGS,Env:RUSTC_WRAPPER,`
  Env:CARGO_BUILD_RUSTFLAGS,Env:CARGO_PROFILE_RELEASE_LTO,`
  Env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS,Env:CARGO_PROFILE_RELEASE_OPT_LEVEL `
  -ErrorAction SilentlyContinue
Push-Location target\comparison\sources\zipp-d71168a
cargo build --locked --release -p zipp-cli
Pop-Location
```

The executable's embedded `--version --json` provenance reports the full
commit, `dirty:false`, `profile:"release"`, `opt_level:"3"`, default features,
an empty rustflags source, and no PGO profile.

### QuickJS-NG

- Upstream: [quickjs-ng/quickjs](https://github.com/quickjs-ng/quickjs)
- Release: [`v0.16.2`](https://github.com/quickjs-ng/quickjs/releases/tag/v0.16.2)
  (2026-08-20)
- Tag commit: [`1ab8676f4b6d6d669baeb5f21790fb9734636a20`](https://github.com/quickjs-ng/quickjs/tree/1ab8676f4b6d6d669baeb5f21790fb9734636a20)
- Native artifact: official `qjs-windows-x86_64.exe`, 2,149,746 bytes,
  SHA-256 `7b27412de844403545bd151fbe49191b4d5b91a9e15b5db7c863fea54639a82b`

The release workflow builds the x86-64 CLI on `windows-latest` under MSYS2
UCRT64, in CMake's release mode, and statically links mimalloc.  Measuring that
published artifact follows QuickJS-NG's own shipping policy without inventing a
local compiler configuration.  The release artifact does not stamp the exact
rolling MSYS2 compiler package revision, which is a provenance limit of this
otherwise first-party binary.

```powershell
$qjsNg = 'target\comparison\bin\quickjs-ng-v0.16.2\qjs-windows-x86_64.exe'
Invoke-WebRequest `
  'https://github.com/quickjs-ng/quickjs/releases/download/v0.16.2/qjs-windows-x86_64.exe' `
  -OutFile $qjsNg
```

QuickJS-NG is a fork of the original Bellard QuickJS lineage.  The Bellard
`2026-06-04` engine was checked during setup, but it is not the requested
project and is therefore not a benchmark headline or Wasm table row.

### Boa

- Upstream: [boa-dev/boa](https://github.com/boa-dev/boa)
- Release: `0.22.0` (upstream tag
  [`v0.22`](https://github.com/boa-dev/boa/releases/tag/v0.22), 2026-08-28)
- Tag commit: [`337a3668a0dc86dd401ea20906e782249a64a228`](https://github.com/boa-dev/boa/tree/337a3668a0dc86dd401ea20906e782249a64a228)
- Build: locked upstream workspace, Rust/Cargo 1.92.0, official `release`
  profile (fat LTO, one codegen unit, stripped symbols)

```powershell
git clone https://github.com/boa-dev/boa.git target\comparison\sources\boa
git -C target\comparison\sources\boa checkout --detach 337a3668a0dc86dd401ea20906e782249a64a228
$env:CARGO_TARGET_DIR = (Resolve-Path target\comparison).Path + '\build\boa-native'
cargo build --locked --release -p boa_cli `
  --manifest-path target\comparison\sources\boa\Cargo.toml
```

The runner measures Boa both with its default CLI configuration and with the
documented `--optimize` flag.  Reporting only whichever one happens to win a
row would create selection bias, so both remain in the artifact.

The exact native payloads supplied to the runner are:

| Engine binary | Bytes | SHA-256 |
|---|---:|---|
| Zipp v0.0.3 release-default | 7,913,984 | `dc5c89bac1b6af84d619da4da54b79443024fa99e3100bd6ce1c52ff90956daf` |
| QuickJS-NG v0.16.2 official x86-64 | 2,149,746 | `7b27412de844403545bd151fbe49191b4d5b91a9e15b5db7c863fea54639a82b` |
| Boa v0.22.0 local official-profile build | 35,422,720 | `1b41611e2c0bafcb5c73736f04896e3d82bf1903a455755b2e1f6876e1629d24` |

Executable size is provenance, not an apples-to-apples product-size score:
the CLIs bundle different standard libraries, locale data, allocators, and host
facilities.

## Native corpus and protocol

`run_native.py` supports the complete frozen `real13` suite, but Boa v0.22.0
takes tens of seconds on several of those JIT-sized inputs.  The default
`micro5` suite therefore derives bounded interpreter-sized inputs from five
already tracked project fixtures:

| Case | Existing source | Deterministic staging change |
|---|---|---|
| recursive calls | `bench/long/fib.js` | `fib(38)` → `fib(32)` |
| arithmetic dispatch | `bench/long/loop.js` | 500,000,000 → 10,000,000 iterations |
| array HOF/closures | `bench/long/array.js` | 5,000,000 → 500,000 elements |
| object properties | `bench/long/object.js` | 40,000,000 → 2,000,000 iterations |
| comparator sort | `bench/long/sort.js` | 2,000,000 → 200,000 elements |

The runner refuses the transformation if an expected source token count has
changed.  It records the original and staged SHA-256 hashes.  Every staged
program receives the same terminal `;void 0;`: Boa's CLI otherwise prints a
non-undefined script completion (notably a final Promise), while the other CLIs
do not.  This suppresses only that CLI presentation difference.  Console stdout
must then match Node byte-for-byte after canonicalizing only CRLF to LF because
the official Windows QuickJS-NG CLI uses CRLF; raw bytes and hashes are retained,
and a lone carriage return is rejected.

Each observation launches a fresh process.  An empty launch of the same engine
is paired immediately before each full launch.  Results include:

- **cold** wall time: process startup, parsing, and execution;
- **startup-adjusted** time: full launch minus its paired empty launch, useful
  for interpreter throughput when both values remain positive; and
- raw and CRLF-canonical stdout hashes, raw stdout, exit status, timeout status,
  binary/input hashes, every raw timing, and the deterministic schedule.

Twelve repetitions put each of the six engines in every order position twice.
The 95% intervals are a descriptive percentile bootstrap.  Each bootstrap draw
uses one shared repetition-index sample across all five cases, preserving the
within-run pairing and common machine noise.  A ratio below `1.0` means Zipp was
faster.

Run only after all builds and other CPU-heavy work have stopped:

```powershell
Get-Process cargo,rustc,cl,lld-link,wasm-opt -ErrorAction SilentlyContinue

python bench\comparison\run_native.py `
  --zipp target\comparison\build\zipp-native-d71168a\release\zipp.exe `
  --quickjs-ng target\comparison\bin\quickjs-ng-v0.16.2\qjs-windows-x86_64.exe `
  --boa target\comparison\build\boa-native\release\boa.exe `
  --suite micro5 `
  --reps 12 `
  --bootstrap-samples 10000 `
  --zipp-revision d71168a9fba3c4b97a05aaacbf14cf046dc65d38 `
  --quickjs-ng-revision 1ab8676f4b6d6d669baeb5f21790fb9734636a20 `
  --boa-revision 337a3668a0dc86dd401ea20906e782249a64a228 `
  --json target\comparison\results\native-micro5.json
```

The captured Zipp binary is the clean v0.0.3 release-default build at
`d71168a9fba3c4b97a05aaacbf14cf046dc65d38`.  This deliberately avoids giving
only Zipp a PGO build while QuickJS-NG and Boa use their ordinary release build
policies.  The artifact records Zipp's complete `--version --json` response,
not just the supplied revision label.  It also hashes and re-probes every
binary before and after measurement, so a concurrent rebuild fails the run.

The result belongs under ignored `target/comparison/results/`.  Promote only a
short reviewed summary here; do not commit a machine-specific raw transcript.

## Native results

The final run completed all 360 measured observations (12 repetitions × five
cases × six engines) on an AMD Ryzen 9 9950X3D under Windows 11, with 30/30
validation executions passing and no timeout, non-zero exit, or output drift.
Every engine occupied every schedule position exactly twice.  The following
ratios are **Zipp time / comparison-engine time** over the five case medians;
lower is better for Zipp.  Intervals are the descriptive 95% bootstrap
intervals defined above, not a claim about other machines or larger programs.

Cold fresh-process wall time is the headline because it is directly observed:

| Zipp mode | Compared with | Geomean ratio (95% interval) | Point-estimate reading |
|---|---|---:|---|
| default JIT | QuickJS-NG | `0.1566` (`0.1491`–`0.1609`) | Zipp JIT was 6.39× faster |
| default JIT | Boa | `0.0496` (`0.0469`–`0.0509`) | Zipp JIT was 20.17× faster |
| default JIT | Boa `--optimize` | `0.0492` (`0.0467`–`0.0507`) | Zipp JIT was 20.31× faster |
| interpreter only | QuickJS-NG | `1.0220` (`1.0058`–`1.0413`) | Zipp took 2.2% longer |
| interpreter only | Boa | `0.3235` (`0.3171`–`0.3290`) | Zipp interpreter was 3.09× faster |
| interpreter only | Boa `--optimize` | `0.3213` (`0.3147`–`0.3283`) | Zipp interpreter was 3.11× faster |
| interpreter only | Zipp default JIT | `6.5266` (`6.3597`–`6.8933`) | Zipp JIT was 6.53× faster than its interpreter |

Subtracting each engine's immediately preceding empty launch emphasizes engine
work but magnifies noise when execution is short.  Five of 360 individual
subtractions were non-positive; affected bootstrap draws were discarded, while
all 10,000 draws remained valid for every interpreter-versus-ecosystem row:

| Zipp mode | Compared with | Startup-adjusted ratio (95% interval) | Point-estimate reading |
|---|---|---:|---|
| default JIT | QuickJS-NG | `0.0606` (`0.0561`–`0.0629`) | Zipp JIT was 16.51× faster |
| default JIT | Boa | `0.0181` (`0.0167`–`0.0189`) | Zipp JIT was 55.12× faster |
| default JIT | Boa `--optimize` | `0.0180` (`0.0167`–`0.0189`) | Zipp JIT was 55.43× faster |
| interpreter only | QuickJS-NG | `1.0032` (`0.9852`–`1.0211`) | effectively tied; Zipp took 0.3% longer at the point estimate |
| interpreter only | Boa | `0.3004` (`0.2951`–`0.3057`) | Zipp interpreter was 3.33× faster |
| interpreter only | Boa `--optimize` | `0.2987` (`0.2928`–`0.3064`) | Zipp interpreter was 3.35× faster |
| interpreter only | Zipp default JIT | `16.5595` (`15.9273`–`18.1096`) | Zipp JIT was 16.56× faster than its interpreter |

The QuickJS-NG comparison varies by workload: in adjusted time Zipp's
interpreter was faster on array HOF/closures and sorting, and slower on
recursion, the arithmetic loop, and object properties.  The aggregate therefore
describes this bounded corpus rather than a universal engine ranking.

## WebAssembly artifact scope

The size comparison counts the final `.wasm` module only, excluding generated
JavaScript/TypeScript glue.  `gzip-9` and Brotli quality 11 are computed over
the exact raw module with Node v24.12.0, zlib `1.3.1-470d3a2`, and Brotli 1.1.0.
These settings make compression reproducible, but the rows are still **not
equivalent products**:

```powershell
node bench\comparison\measure_wasm.mjs `
  path\to\zipp_wasm_bg.wasm `
  path\to\boa_wasm_bg.wasm `
  path\to\qjs-wasi.wasm `
  path\to\qjs-wasi-reactor.wasm
```

The table also excludes the host runtime: QuickJS-NG needs a WASI
implementation, while Zipp and Boa need their generated JavaScript imports.
That makes it a module-payload comparison, not total application download size.

- Zipp exports a persistent, sandbox-profile `Engine` with a substantial host
  bridge and an interpreter optimized for speed (`opt-level=3`).
- Boa's official [`@boa-dev/boa_wasm`](https://www.npmjs.com/package/@boa-dev/boa_wasm)
  exposes a stateless `evaluate` call that creates a new default context and
  includes its default Annex B, experimental, Float16, bundled Intl, Temporal,
  and precise-sum features.
- QuickJS-NG's official `qjs-wasi.wasm` is a WASI command-line executable built
  from the same v0.16.2 tag with WASI SDK 29.  It is the closest official Wasm
  counterpart to the native `qjs` CLI.  The release also ships a slightly
  smaller reactor module, but that has a different lifecycle/export surface.

### Why there is no cross-project Wasm speed table

A runtime comparison was assessed but rejected because the official interfaces
cannot execute the unchanged corpus with the same observable result or aligned
lifecycle:

- QuickJS-NG's executable enters through WASI `_start`, reads a file via its CLI,
  creates its runtime/context internally, and writes through WASI stdout.
- Zipp's web package exposes a persistent `Engine`, with console output captured
  by its host bridge and returned separately.
- Boa's official binding exposes `evaluate(source)`: its Rust function creates a
  fresh `Context::default()` for every call and returns the displayed completion
  value.  It does not install the console host used by these fixtures.

Precompiling each `WebAssembly.Module` and pairing full execution with an empty
instance would remove V8 compile cost, but it would not remove those differences
in instance startup, context creation, file I/O, output plumbing, or returned
value semantics.  Making output validation pass would require engine-specific
source wrappers or custom embeddings, so the resulting ranking would measure
the adapters as much as the engines.  This report therefore compares native
speed and official Wasm module payloads, but deliberately does not publish a
cross-project Wasm runtime number.

| Module | Provenance | Raw bytes | gzip-9 | Brotli-11 |
|---|---|---:|---:|---:|
| Zipp Wasm | commit `d71168a`, installed release CGU4, stripped | 5,566,206 | 1,845,822 | 1,244,186 |
| Boa Wasm | official `@boa-dev/boa_wasm` 0.22.0 | 21,296,176 | 7,737,026 | 5,484,164 |
| QuickJS-NG WASI CLI | official v0.16.2 `qjs-wasi.wasm` | 1,566,956 | 548,057 | 434,855 |
| QuickJS-NG WASI reactor | official v0.16.2 `qjs-wasi-reactor.wasm` | 1,528,293 | 527,761 | 417,087 |

The final Zipp row uses Rust 1.92.0 and wasm-bindgen 0.2.126.  It was built with
the locked release graph and `profile.release.codegen-units=4`, then processed
with `wasm-bindgen --target web --remove-name-section
--remove-producers-section`.  Against the pre-change stripped web artifact,
the installed CGU4 module removes 102,327 raw bytes (1.805%), 25,163 gzip bytes
(1.345%), and 17,474 Brotli bytes (1.385%).  In the quiet counterbalanced Wasm
run, its observed equal-row steady-time geomean was `0.99053×` baseline (0.947%
faster) across 11 workloads.  No aggregate confidence interval was computed,
so this is an observed point, not a proved speedup.  The memory/import audits
passed: 41 audited function imports and an unshared 32-bit memory capped at
4,096 pages (256 MiB).  Release validation also passed 115/115 host-contract
checks, the external Worker deadline/recovery check, and 23/23 syntax/depth
checks.

The isolated like-for-like profile screen reported 5,568,906 raw and 1,245,797
Brotli bytes for CGU4.  The installed primary-worktree rebuild is 2,700 raw and
1,611 Brotli bytes smaller.  This is rebuild layout variance, not another source
optimization: the screen used a detached worktree plus Cargo's CLI `--config`
override, while the installed artifact used the checked-in profile and the
CI-equivalent warning/linker `RUSTFLAGS`.  The profile README keeps the isolated
number so its candidate rows remain comparable; this ecosystem table uses the
exact tracked module a user downloads.

Exact ecosystem artifact hashes used for the table:

- Zipp module SHA-256:
  `71df836700a2431021ef6aa3793da77b86226b6b43ead76e70169ec0b9ed00fd`
- Boa module SHA-256:
  `03a3e4c1c0e71514cb28d2158ea52566dbbfbefe16fee795480a751e9b6b5f31`
- QuickJS-NG WASI CLI SHA-256:
  `d2939e98c808e8b9f4164cd0d7b0398cbc0121ddf52862bcd92157d923e461cc`
- QuickJS-NG WASI reactor SHA-256:
  `fc638ef0bad35edb860ca93fe5c0ea288a6ad137888b34afa8ca2c2513727cf0`
