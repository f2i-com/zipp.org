# QuickJS-NG and Boa comparison

This is a **diagnostic ecosystem comparison**, separate from Zipp's canonical
Node/Bun/Deno publication series.  It answers two narrower questions:

1. How do Zipp's default native engine and interpreter-only mode compare with
   pinned QuickJS-NG and Boa interpreter builds on identical work?
2. How do the published WebAssembly interfaces behave on identical work, and
   how large are their distributable modules?

Do not splice these numbers into the canonical engine geomean.  The engines do
not expose identical language, locale, host, or embedding surfaces, and the
Wasm packages in particular are not interface-equivalent.

The historical v0.0.5 snapshot was measured from clean source commit
`7cb72106c9591613b170ba057d3c07e1cee01379`. Its raw result files are:

- `target/comparison/results/native-real13-v005-qjsng-clean-6.json`
- `target/comparison/results/native-micro5-v005-clean-6.json`
- `target/comparison/results/wasm-v005-clean-6.json`

All three use only six samples per case or phase. Their intervals and ratios
are useful diagnostics for this machine and corpus, not evidence of a universal
engine ranking.

The v0.0.6 work adds a direct attempt over the exact frozen normal 13 and hostile
17 inventory:

- `target/comparison/results/wasm-suites-v006-c77829269703-final-6.json`

It is a complete capture attempt, but explicitly not a publishable comparison:
the production interfaces and limits leave only five common timed rows. The
older specialization experiments remain available for mechanism attribution:

- speed kernels: `target/comparison/results/wasm-kernels-final-48.json`
- pre-kernel baseline: `target/comparison/results/wasm-meter-countdown-final-48.json`
- exact lanes disabled: `target/comparison/results/wasm-v006-integrity-noexact-48.json`

These results answer different questions. Never use the specialization-sensitive
five-workload result as a replacement for exact-suite coverage.

## Pinned native builds

### Zipp

- Release: `v0.0.5`
- Commit: `7cb72106c9591613b170ba057d3c07e1cee01379`
- Build: clean detached worktree, locked graph, Rust/Cargo 1.92.0, repository
  `release` profile (optimization level 3, fat LTO, one codegen unit, aborting
  panics), default features, no extra Rust flags

```powershell
git worktree add --detach `
  target\comparison\sources\zipp-v005-7cb72106c959 `
  7cb72106c9591613b170ba057d3c07e1cee01379
$comparisonRoot = (Resolve-Path target\comparison).Path
$env:CARGO_TARGET_DIR = $comparisonRoot + '\build\zipp-native-v005-7cb72106c959'
Remove-Item Env:RUSTFLAGS,Env:CARGO_ENCODED_RUSTFLAGS,Env:RUSTC_WRAPPER,`
  Env:CARGO_BUILD_RUSTFLAGS,Env:CARGO_PROFILE_RELEASE_LTO,`
  Env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS,Env:CARGO_PROFILE_RELEASE_OPT_LEVEL `
  -ErrorAction SilentlyContinue
Push-Location target\comparison\sources\zipp-v005-7cb72106c959
cargo +1.92.0 build --locked --release -p zipp-cli
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
| Zipp v0.0.5 release-default | 7,932,416 | `b36850e162f7a9d2221ac33e888c8e5ea3d7ee85e3833e6f2e5ad9a73c0c23be` |
| QuickJS-NG v0.16.2 official x86-64 | 2,149,746 | `7b27412de844403545bd151fbe49191b4d5b91a9e15b5db7c863fea54639a82b` |
| Boa v0.22.0 local official-profile build | 35,422,720 | `1b41611e2c0bafcb5c73736f04896e3d82bf1903a455755b2e1f6876e1629d24` |

Executable size is provenance, not an apples-to-apples product-size score:
the CLIs bundle different standard libraries, locale data, allocators, and host
facilities.

## Native corpus and protocol

The primary interpreter/QuickJS-NG comparison runs the complete frozen
`bench/real` 13-case suite.  Boa v0.22.0 takes tens of seconds on several of
those inputs, so Boa and Zipp's default JIT are compared separately on the
bounded `micro5` suite.  `micro5` derives interpreter-sized inputs from five
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

The final files use six repetitions.  Engine and case order are deterministically
counterbalanced; on `real13` each of the three engines occupies every engine
position twice, while on `micro5` each of the six engines occupies every engine
position once.  The 95% intervals are descriptive percentile bootstraps with
10,000 draws.  Each draw uses one shared repetition-index sample across every
case in that engine comparison, preserving pairing and common machine noise.
A ratio below `1.0` means Zipp was faster.

Run only after all builds and other CPU-heavy work have stopped:

```powershell
Get-Process cargo,rustc,cl,lld-link,wasm-opt -ErrorAction SilentlyContinue

python bench\comparison\run_native.py `
  --zipp target\comparison\build\zipp-native-v005-7cb72106c959\release\zipp.exe `
  --quickjs-ng target\comparison\bin\quickjs-ng-v0.16.2\qjs-windows-x86_64.exe `
  --boa target\comparison\build\boa-native\release\boa.exe `
  --suite real13 `
  --engines node,zipp-interp,quickjs-ng `
  --reps 6 `
  --bootstrap-samples 10000 `
  --zipp-revision 7cb72106c9591613b170ba057d3c07e1cee01379 `
  --quickjs-ng-revision 1ab8676f4b6d6d669baeb5f21790fb9734636a20 `
  --boa-revision 337a3668a0dc86dd401ea20906e782249a64a228 `
  --json target\comparison\results\native-real13-v005-qjsng-clean-6.json

python bench\comparison\run_native.py `
  --zipp target\comparison\build\zipp-native-v005-7cb72106c959\release\zipp.exe `
  --quickjs-ng target\comparison\bin\quickjs-ng-v0.16.2\qjs-windows-x86_64.exe `
  --boa target\comparison\build\boa-native\release\boa.exe `
  --suite micro5 `
  --reps 6 `
  --bootstrap-samples 10000 `
  --zipp-revision 7cb72106c9591613b170ba057d3c07e1cee01379 `
  --quickjs-ng-revision 1ab8676f4b6d6d669baeb5f21790fb9734636a20 `
  --boa-revision 337a3668a0dc86dd401ea20906e782249a64a228 `
  --json target\comparison\results\native-micro5-v005-clean-6.json
```

The captured Zipp binary is the clean v0.0.5 release-default build at
`7cb72106c9591613b170ba057d3c07e1cee01379`.  This deliberately avoids giving
only Zipp a PGO build while QuickJS-NG and Boa use their ordinary release build
policies.  Each result records Zipp's complete `--version --json` response, not
just the supplied revision label.  It also hashes and re-probes every binary
before and after measurement, so a concurrent rebuild fails the run.

The result belongs under ignored `target/comparison/results/`.  Promote only a
short reviewed summary here; do not commit a machine-specific raw transcript.

## Native results

Both clean-six runs used an AMD Ryzen 9 9950X3D under Windows 11.  The `real13`
file contains 234 measured launch pairs (six repetitions × 13 cases × three
engines) and 39/39 successful validation executions.  The `micro5` file contains
180 launch pairs and 30/30 successful validations.  Both report
`all_correct:true`, with no timed-out or non-zero launch in their observations.

The following ratios are **Zipp time / comparison-engine time** over case
medians.  Lower is better for Zipp.  Startup-adjusted time subtracts the same
engine's immediately preceding empty launch; cold time directly includes
process startup, parsing, and execution.

### Interpreter versus QuickJS-NG: real13

| Measure | Geomean ratio (descriptive 95% interval) | Point wins |
|---|---:|---:|
| startup-adjusted | `0.6413113054` (`0.6386244852`–`0.6452190331`) | 12/13 |
| cold fresh process | `0.6438535279` (`0.6414182174`–`0.6453438366`) | 12/13 |

On this run Zipp's interpreter used about 64% of QuickJS-NG's aggregate time.
It did **not** win every case: the only adjusted loss was `sparse-array` at
`1.0099332`.  The two closest adjusted wins were `polymorphic-objects` at
`0.9892256` and `typedarray-math` at `0.9782214`.  This is the honest boundary
of the measured claim.

### Interpreter, JIT, QuickJS-NG, and Boa: micro5

| Zipp mode | Compared with | Startup-adjusted ratio (descriptive 95% interval) | Point wins |
|---|---|---:|---:|
| interpreter only | QuickJS-NG | `0.8556219192` (`0.8404661751`–`0.8761366621`) | 5/5 |
| interpreter only | Boa | `0.2539293733` (`0.2501193080`–`0.2590054186`) | 5/5 |
| interpreter only | Boa `--optimize` | `0.2521990087` (`0.2492920521`–`0.2592836182`) | 5/5 |
| default JIT | QuickJS-NG | `0.0756904348` (`0.0729660478`–`0.0793315851`) | 5/5 |
| default JIT | Boa | `0.0224632215` (`0.0216944924`–`0.0234999480`) | 5/5 |

`micro5` is deliberately downscaled and is the only native evidence here for
Boa.  Its five wins do not establish the result on the larger `real13` corpus.
Likewise, six repetitions make the bootstrap intervals descriptive rather than
an assurance about other machines, engine versions, programs, or feature sets.

## WebAssembly artifact scope

The size comparison counts the final `.wasm` module only, excluding generated
JavaScript/TypeScript glue.  `gzip-9` and Brotli quality 11 are computed over
the exact raw module with Node v24.12.0, zlib `1.3.1-470d3a2`, and Brotli 1.1.0.
These settings make compression reproducible, but the rows are still **not
equivalent products**. The current v0.0.6 production row uses:

```powershell
node bench\comparison\measure_wasm.mjs `
  landing\public\wasm\zipp_wasm_bg.wasm `
  target\comparison\bin\boa-wasm\unpacked\package\boa_wasm_bg.wasm `
  target\comparison\bin\quickjs-ng-v0.16.2\qjs-wasi.wasm `
  target\comparison\bin\quickjs-ng-v0.16.2\qjs-wasi-reactor.wasm
```

The historical v0.0.5 and development artifacts remain identified by their
recorded hashes. They are not the file currently at the landing path.

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
  smaller reactor module.  The reactor is used for the runtime diagnostic below
  because it supports repeated evaluation in one live instance; it is not the
  same lifecycle/export surface as the CLI module.

| Module | Provenance | Raw bytes | gzip-9 | Brotli-11 | SHA-256 |
|---|---|---:|---:|---:|---|
| **Zipp Wasm v0.0.6** | committed production artifact, release CGU4, name/producers/target-features sections stripped | **5,480,311** | **1,825,812** | **1,233,575** | `318fc5cf7ee5d55751d829419d4de5af1ab2643b8f7fd30df2e3779c16ad1691` |
| Zipp Wasm, speed-kernel experiment | uncommitted post-v0.0.5 working tree, same production profile | 5,480,576 | 1,826,113 | 1,233,843 | `caf26214ffca1407fba46f3bb304e4bb78ebb01b12898bf4132fc4e7a21f05f3` |
| Zipp Wasm, pre-kernel experiment | uncommitted post-v0.0.5 working tree, same production profile | 5,462,006 | 1,818,299 | 1,230,296 | `8cf6e8207d1852cfa31c6e38d3b8a60bdec6e4894a65c1d08f39b244c849903b` |
| Zipp Wasm v0.0.5 | commit `7cb72106c959`, release CGU4, sections stripped | 5,595,833 | 1,859,668 | 1,254,075 | `f3d67856f5853c235c12ee62a1cc86032492012e3942c032a08d8d22df85ff0b` |
| QuickJS-NG WASI CLI | official v0.16.2 `qjs-wasi.wasm` | 1,566,956 | 548,057 | 434,855 | `d2939e98c808e8b9f4164cd0d7b0398cbc0121ddf52862bcd92157d923e461cc` |
| QuickJS-NG WASI reactor | official v0.16.2 `qjs-wasi-reactor.wasm` | 1,528,293 | 527,761 | 417,087 | `fc638ef0bad35edb860ca93fe5c0ea288a6ad137888b34afa8ca2c2513727cf0` |
| Boa Wasm | official `@boa-dev/boa_wasm` 0.22.0 | 21,296,176 | 7,737,026 | 5,484,164 | `03a3e4c1c0e71514cb28d2158ea52566dbbfbefe16fee795480a751e9b6b5f31` |

The current Zipp row uses Rust 1.92.0, wasm-bindgen 0.2.126, the locked graph,
`opt-level=3`, four codegen units, a 256 MiB linked memory maximum, and a 1 MiB
linked stack. It selects the isolated `safe-sandbox`, `meter-only`,
`wasm-no-fs-loader`, and `wasm-single-agent` zipp-vm features. After
`wasm-bindgen --target web` removes the name and producers sections, the
validated `strip-target-features.cjs` pass removes only the optional
`target_features` custom section. There is deliberately no `wasm-opt` pass.
The v0.0.6 module is 115,522 bytes smaller raw and 20,500 bytes smaller at
Brotli-11 than v0.0.5.

### Historical Wasm optimization context

The four-codegen-unit policy came from an isolated, like-for-like screen on the
older `d71168a` source snapshot.  In that experiment CGU4 reduced the stripped
module from 5,671,347 to 5,568,906 raw bytes and from 1,262,145 to 1,245,797
Brotli bytes, while its observed 11-row steady-time geomean was `0.99053×` the
old CGU16 baseline.  CGU1 was smaller but measured 2.01% slower.  Separately,
`wasm-opt -Oz` reduced raw size but increased Brotli size and measured 2.04%
slower.  Those historical experiments explain the shipped build policy; their
bytes are not current v0.0.6 artifact results and must not be compared as if the
source were unchanged.  The full bounded screen and its limitations remain in
[`crates/zipp-wasm/README.md`](../../crates/zipp-wasm/README.md).

## Exact frozen-suite WebAssembly diagnostic — v0.0.6

`run_wasm_suites.mjs` attempts all 13 normal rows and all 17 hostile rows from
the current v0.0.6 sources used by
`target/bench-results/real13-v006-6650647a718c-pgo-15.json` and
`target/bench-results/hostile17-v006-6650647a718c-pgo-15.json`. Those inputs are
newer than the retained public Node/Bun/Deno capture. The runner does no
rewriting, downscaling, or completion suppression. It validates the exact guest
bytes against a Node stdout oracle, uses a separate validation module, then
times fresh guest contexts inside one persistent module instance per engine.
Work/control order, engine order, and case position are balanced.

The clean six-round command was:

```powershell
node --no-warnings bench\comparison\run_wasm_suites.mjs `
  --suite all `
  --reps 6 `
  --compile-reps 6 `
  --startup-reps 6 `
  --node "C:\Program Files\nodejs\node.exe" `
  --zipp-wasm landing\public\wasm\zipp_wasm_bg.wasm `
  --zipp-glue landing\public\wasm\zipp_wasm.js `
  --quickjs-wasm target\comparison\bin\quickjs-ng-v0.16.2\qjs-wasi-reactor.wasm `
  --output target\comparison\results\wasm-suites-v006-c77829269703-final-6.json
```

The result records clean Git commit `c77829269703`, Node 24.12.0 / V8
13.6.233.17, Windows x64, the AMD Ryzen 9 9950X3D host, seed `1511464998`, and
unchanged source, artifact, harness, Git, and Node-oracle identities. The
production Zipp module is the 5,480,311-byte `318fc5cf…1691` artifact in the
size table; QuickJS-NG is the official v0.16.2 reactor.

This is an honest **failed full-comparison attempt**, not a publishable score:

- The frozen inventory contains 28 script rows and two module rows. Production
  Zipp WASM uses `wasm-no-fs-loader`, so `module-hot-graph` and `npm-nanoid`
  cannot be loaded through `Engine.initScript`.
- The official QuickJS-NG reactor evaluates and returns without
  `js_std_loop`/`JS_ExecutePendingJob` and exports no drain function. Its three
  async rows are therefore explicit cross-engine exclusions.
- Node validated all 28 scripts and QuickJS-NG validated 25. Zipp validated 7:
  fourteen rows stopped at the fixed 50-million instruction budget, three at
  the fixed 128 MiB approximate heap budget, and four at other engine errors
  (the real13 async trap, two invalid-string-length errors, and one
  typed-array-length error).
- The async trap poisoned the first validation instance. The harness recorded
  that event, abandoned the instance, validated an empty control in a fresh
  replacement, and continued. Timed instances are never reset.
- Zipp's `Engine.takeOutput()` merges VM stdout and errput into one ordered
  array. The runner compares that merged output with Node stdout, but cannot
  assert an independent empty Zipp stderr channel; these inputs emit no stderr.
- The wasm-bindgen API does not export typed resource-limit status. The fourteen
  instruction and three heap classifications are therefore advisory exact
  message matches; the raw embedding errors remain in the result.
- Synchronous in-process WASM evaluation cannot be interrupted by this runner;
  manifest timeouts are provenance only. Run it on a dedicated host.

The result consequently records `publishable:false`, `capture_usable:false`,
`evidence_usable:false`, and `comparison_publishable:false`. There is no
comparable `real13` row and therefore no normal-suite ratio. Only five hostile
rows produced six complete samples for both engines:

| Hostile row | Zipp persistent ms | QuickJS-NG persistent ms | persistent ratio | adjusted ratio |
|---|---:|---:|---:|---:|
| shapes-stable | 225.5261 | 175.3651 | `1.2860375×` | `1.2811210×` |
| allocation-ephemeral | 389.8556 | 371.0349 | `1.0507250×` | `1.0481586×` |
| allocation-survival | 274.1641 | 254.3970 | `1.0777016×` | `1.0731532×` |
| reactish-reconcile | 195.2943 | 166.1415 | `1.1754698×` | `1.1694467×` |
| warm-router | 291.2464 | 610.2150 | `0.4772848×` | `0.4755476×` |

The available five-row geomeans are `0.9603865×` persistent and `0.9566887×`
adjusted, with one Zipp point win. The word **available** is essential: these
are neither complete hostile geomeans nor combined-suite geomeans, and the
result's own status rejects publication. They do not establish that Zipp WASM
is faster than QuickJS-NG WASM. This six-repetition diagnostic reports raw
medians and available-row geomeans only; it computes no confidence intervals.

| Cold phase median | Zipp | QuickJS-NG reactor | Zipp / QuickJS-NG |
|---|---:|---:|---:|
| compile | 5.07995 ms | 1.79525 ms | `2.8297×` |
| compiled-module instantiation/start | 0.39700 ms | 1.67570 ms | `0.2369×` |
| sum of separately sampled phase medians | 5.47695 ms | 3.47095 ms | `1.5779×` |

The last row is arithmetic only. Compile and startup were sampled in separate
fresh-process sets, so it is not an observed end-to-end module-ready median.

Boa was not included in this exact-suite runner or capture. No current
same-source normal-13-plus-hostile-17 WASM comparison against Boa exists.

The harness itself is covered by:

```powershell
node --no-warnings --test bench\comparison\test_run_wasm_suites.mjs
```

## WebAssembly micro5 adapter diagnostic

The official interfaces cannot be made product-equivalent:

- Zipp exposes a persistent sandbox-profile `Engine` and host bridge.
- QuickJS-NG's official reactor enters evaluation through `qjs_init_argv` and
  WASI plumbing rather than a public `JS_Eval` export.
- Boa's official `evaluate(source)` creates a fresh `Context::default()` per
  call and includes a materially different bundled feature set.

`run_wasm.mjs` therefore reports an explicitly adapter-inclusive diagnostic.
It validates exact guest results, uses one live module instance per engine,
creates a fresh guest context/evaluation for each sample, and pairs every work
source with a byte-matched control that differs by one source byte.  The five
workload families are bounded specifically for Wasm; they are not the native
`real13` run.  Adjusted execution is work time minus its paired control, so it
cancels much fixed adapter cost without making the interfaces equivalent.

### v0.0.5 release capture

The exact clean-six command was:

```powershell
node --no-warnings bench\comparison\run_wasm.mjs `
  --reps 6 `
  --compile-reps 6 `
  --startup-reps 6 `
  --output target\comparison\results\wasm-v005-clean-6.json `
  --zipp-wasm landing\public\wasm\zipp_wasm_bg.wasm `
  --zipp-glue landing\public\wasm\zipp_wasm.js `
  --quickjs-wasm target\comparison\bin\quickjs-ng-v0.16.2\qjs-wasi-reactor.wasm `
  --boa-wasm target\comparison\bin\boa-wasm\unpacked\package\boa_wasm_bg.wasm `
  --boa-glue target\comparison\bin\boa-wasm\unpacked\package\boa_wasm_bg.js
```

The Boa glue filename is intentionally `boa_wasm_bg.js`, matching the official
package and its `boa_wasm_bg.wasm` module.

Cold compilation uses a fresh Node/V8 process with file reading excluded.
Instantiation uses a precompiled `WebAssembly.Module`; glue parsing and module
compilation are excluded, while initialization and Wasm start are included.

| Phase median | Zipp | QuickJS-NG reactor | Boa |
|---|---:|---:|---:|
| cold compile | 4.91325 ms | 1.54545 ms | 9.50440 ms |
| compiled-module instantiation/start | 0.45725 ms | 1.57375 ms | 2.61325 ms |

| Adjusted execution comparison | Zipp / comparison geomean | Reading |
|---|---:|---|
| QuickJS-NG reactor | `2.10743805` | Zipp Wasm was slower |
| Boa | `0.22738657` | Zipp Wasm was faster |

These are six-sample point estimates with no aggregate confidence interval.
They include the projects' different context creation, imports, source/result
marshalling, and teardown paths.  They do not support a universal Wasm speed or
size claim, nor do they erase the modules' different APIs and feature bundles.

### Historical speed-kernel development capture

The validated development module was measured with 48 execution repetitions
and 12 fresh-process samples for each cold phase. The exact command was:

```powershell
node --no-warnings bench\comparison\run_wasm.mjs `
  --reps 48 `
  --compile-reps 12 `
  --startup-reps 12 `
  --seed 1511464998 `
  --output target\comparison\results\wasm-kernels-final-48.json `
  --zipp-wasm target\comparison\candidates\wasm-kernels-final-commuted-20260901-web\zipp_wasm_bg.wasm `
  --zipp-glue target\comparison\candidates\wasm-kernels-final-commuted-20260901-web\zipp_wasm.js `
  --quickjs-wasm target\comparison\bin\quickjs-ng-v0.16.2\qjs-wasi-reactor.wasm `
  --boa-wasm target\comparison\bin\boa-wasm\unpacked\package\boa_wasm_bg.wasm `
  --boa-glue target\comparison\bin\boa-wasm\unpacked\package\boa_wasm_bg.js
```

The Zipp module supplied to that command is exactly the 5,480,576-byte,
`caf26214…05f3` artifact in the size table. The result records Node 24.12.0,
V8 13.6.233.17-node.37, Windows x64, and the AMD Ryzen 9 9950X3D host.

| Phase median | Zipp | QuickJS-NG reactor | Boa |
|---|---:|---:|---:|
| cold compile | 4.70735 ms | 1.51085 ms | 8.91050 ms |
| compiled-module instantiation/start | 0.41760 ms | 1.58345 ms | 2.57755 ms |
| sum of separately sampled phase medians | 5.12495 ms | 3.09430 ms | 11.48805 ms |

| Persistent execution comparison | Geomean | Point wins |
|---|---:|---:|
| Zipp / QuickJS-NG reactor | `0.0954663913` | 5/5 |
| Zipp / Boa | `0.0105336875` | 5/5 |

The persistent work medians make the selected QuickJS-NG comparison visible
row by row:

| Workload | Zipp | QuickJS-NG | Zipp / QuickJS-NG | Zipp positive adjusted samples |
|---|---:|---:|---:|---:|
| fib-recursive | 1.1180 ms | 58.1861 ms | `0.0192142110` | 25/48 |
| loop-arithmetic | 1.1003 ms | 27.6866 ms | `0.0397412467` | 24/48 |
| array-hof | 5.37885 ms | 9.39055 ms | `0.5727939258` | 48/48 |
| object-properties | 1.05105 ms | 16.83035 ms | `0.0624496817` | 24/48 |
| sort-comparator | 5.3476 ms | 18.4204 ms | `0.2903085709` | 48/48 |

Array HOF and comparator sort remain cleanly above the paired-control
subtraction floor; their adjusted Zipp / QuickJS-NG ratios are
`0.4681596596` and `0.2316111384`. Fib, loop, and object-property work is too
short after specialization for subtraction to be stable: their Zipp positive
counts are only 25/48, 24/48, and 24/48, and the object adjusted median is
negative. The harness records a `0.0245653793` adjusted geomean, but it is not a
robust performance headline because three of its five inputs are at or below
measurement resolution.

The six engine permutations repeat eight times, and each engine runs work first
and control first equally often. These are still point estimates without an
aggregate confidence interval. The result supports a 5/5 persistent-workload
win for this bounded corpus, including each project's public adapter and context
lifecycle. It does not establish a universal interpreter-core or all-JavaScript
ranking. QuickJS-NG also retains the transfer-size lead: this Zipp module
is `3.586×` as large raw and `2.958×` as large at Brotli-11. Boa's official
module remains larger than Zipp's on both measures.

Because the development source was uncommitted when measured, the artifact hash
and raw result file are its reproducibility anchors; no source-commit claim is
made.

The critical attribution control is
`target/comparison/results/wasm-v006-integrity-noexact-48.json`, captured with
the same 48 / 12 / 12 schedule but with the exact speed-kernel lanes disabled.
It measured Zipp / QuickJS-NG at `1.8150248200×` persistent and
`1.7712523177×` adjusted, with QuickJS-NG ahead on all five rows. Against Boa,
Zipp measured `0.1992205735×` persistent and `0.1922208356×` adjusted, with five
of five point wins—about five times faster on these five kernels.

This control is a dirty-tree diagnostic candidate, not release evidence: it
records HEAD `6650647a718c`, `M crates/zipp-vm/src/heap.rs`, and candidate module
SHA-256 `09c0772f10a36b58bca1ffe58bc5ad59ecc078fd08bd743fba5a0fa4745ea6bc`.
It shows that the `0.0954663913×` QuickJS-NG result above is dominated by exact
artifact specialization. Its Boa result is likewise bounded evidence for those
five adapter-inclusive kernels, not a general interpreter or full-suite claim.

### Prior development baseline (preserved)

The immediately preceding development module was measured with the same 48 / 12
/ 12 schedule, seed, host, and pinned comparator artifacts:

```powershell
node --no-warnings bench\comparison\run_wasm.mjs `
  --reps 48 `
  --compile-reps 12 `
  --startup-reps 12 `
  --seed 1511464998 `
  --output target\comparison\results\wasm-meter-countdown-final-48.json `
  --zipp-wasm target\comparison\candidates\meter-countdown-final-exact-web\zipp_wasm_bg.wasm `
  --zipp-glue target\comparison\candidates\meter-countdown-final-exact-web\zipp_wasm.js `
  --quickjs-wasm target\comparison\bin\quickjs-ng-v0.16.2\qjs-wasi-reactor.wasm `
  --boa-wasm target\comparison\bin\boa-wasm\unpacked\package\boa_wasm_bg.wasm `
  --boa-glue target\comparison\bin\boa-wasm\unpacked\package\boa_wasm_bg.js
```

That Zipp module is the 5,462,006-byte, `8cf6e820…9903b` artifact in the size
table. It measured 4.68905 ms cold compile and 0.41745 ms instantiation/start,
versus 1.44655 / 1.58390 ms for QuickJS-NG and 8.76340 / 2.61825 ms for Boa.

| Prior execution comparison | Persistent-total geomean | Adjusted work-minus-control geomean | Adjusted point wins |
|---|---:|---:|---:|
| Zipp / QuickJS-NG reactor | `1.8115847493` | `1.7725499353` | 0/5 |
| Zipp / Boa | `0.1898384829` | `0.1834251840` | 5/5 |

Every adjusted sample in the prior capture was positive. Its QuickJS-NG
adjusted ratios were fib `1.7121001051`, loop `1.5539940842`, array
`1.5029063394`, object properties `2.4622275963`, and sort `1.7772714631`.
The speed kernels added 18,570 raw, 7,814 gzip, and 3,547 Brotli bytes to
this preserved baseline.
