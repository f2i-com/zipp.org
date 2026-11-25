# Application benchmark

Three npm packages, used exactly as published, each doing the job it was
written for on one 22 KB article page. This is the suite to read for "how
fast does ZIPP run an application": the rows in `bench/real` and
`bench/hostile` are single programs of a few dozen functions, and an engine
can be ahead on every one of them and several times behind here.

| Workload | Package | What one iteration does |
|---|---|---|
| `linkedom` | linkedom 0.18.15 | parses the page into a DOM, then runs two selector queries |
| `defuddle` | defuddle 0.19.6 (over linkedom) | finds the article in the page and writes it as Markdown |
| `webforai` | webforai 5.1.0 | converts the page to Markdown through its own parser |

## Running it

```sh
cd bench/apps
npm ci                      # the three packages, at the versions the lockfile pins
cd ../..
python bench/apps/bench.py --zipp target/release/zipp.exe
```

`run.mjs` is the workload: it builds the page, runs the chosen workload once
cold and then 15 times warm, checks that every run wrote the same text, and
prints one JSON line. `bench.py` runs it as a fresh process under Node and
under each ZIPP binary given, interleaved, seven times, and prints medians
and ratios to Node. Compare two builds with two `--zipp` flags
(`--zipp old=a.exe --zipp new=b.exe`), or one build under a switch with
`--zipp-env nojit:ZIPP_NOJIT=1`.

Three figures per row:

- **first**: the cold run of the workload, modules already loaded.
- **warm**: the median of the warm iterations, which is where a long-running
  server spends its time.
- **wall**: the whole process, start to exit, 16 runs of the workload.

## Rules

- The packages are not patched, bundled or reordered. A workload that needs a
  change to run is a bug in the engine, to be fixed there.
- Output is compared with Node's (length, first and last 80 characters). A
  row whose output differs is flagged and is not a result.
- Node keeps its helper threads: every process is held to one 8-core die
  (`--mask`, default `0xFFFF`), not to one core. On one core Node's first run
  takes three times as long, which flatters every other engine.
- ZIPP's code cache is off (`ZIPP_CACHE_DIR` empty), so a first run is one.
- These are diagnostics, like every routine run: a plain release build on a
  quiet machine. Nothing here is a stamped capture.

## Where it stood

HEAD `ed5be457` (0.0.41), plain release build, 26 November 2026, Ryzen 9
9950X3D, Windows 11, Node 24.19.0; ratios are ZIPP / Node:

| Workload | first | warm | wall |
|---|---:|---:|---:|
| `linkedom` | 1.88x | 4.06x | 1.19x |
| `defuddle` | 2.17x | 5.05x | 2.91x |
| `webforai` | 2.29x | 4.89x | 2.62x |

With the JIT off (`ZIPP_NOJIT=1`) the warm ratios are 10.6x, 10.2x and 17.2x.
A profile of the warm loops puts 29 to 32% of the time in compiled code and
most of the rest in the runtime helpers it calls: allocation, property reads
that miss their cache, calls between compiled functions, and collection.
