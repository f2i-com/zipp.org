# Documented corrections to the pinned Test262 corpus

These five patches correct nine contradictory executions at upstream commit
`4249661388e5d3f92a85186213da140a6481490f`. They are a ZIPP-maintained validation
profile, **not an unmodified upstream Test262 conformance result**. The original
suite is always run and reported separately. No files, execution modes or
immutable-buffer cases are removed.

| Files | Executions | Correction |
| --- | ---: | --- |
| Three `staging/sm/Error` tests | 6 | Keep the existing base `Error` assertions; apply subclass assertions only to subclasses. The pinned `nativeErrors.js` harness now includes `Error` itself. |
| TypedArray `slice/speciesctor-return-same-buffer-with-offset.js` | 2 | Keep the mutable alias-copy assertions. For the harness's immutable-buffer factory, require `TypeError` and unchanged source elements. |
| Annex B `block-decl-func-skip-arguments.js` | 1 | Retain assertions before and inside each block; update the superseded ES2017 assertion after the declaration to require the function binding. |

The Error rules are specified in
[NativeError constructors](https://tc39.es/ecma262/multipage/fundamental-objects.html#sec-native-error-types-used-in-this-standard).
The immutable-buffer proposal requires a writable destination through
[TypedArraySpeciesCreate](https://tc39.es/proposal-immutable-arraybuffer/#sec-typedarrayspeciescreate).
The Annex B correction follows the current
[FunctionDeclarationInstantiation changes](https://tc39.es/ecma262/multipage/additional-ecmascript-features-for-web-browsers.html#sec-web-compat-functiondeclarationinstantiation).
Its old post-block assertion also fails in Node 24.19.0.

The two German `String/internalUsage.js` failures require a product fix and have
**no patch here**. DateTimeFormat now uses generated German CLDR 48 data, and
those original tests pass.

## Reproduce both results

Create the upstream checkout with persistent LF settings, including on Windows:

```sh
git clone --config core.autocrlf=false https://github.com/tc39/test262.git target/test262
git -C target/test262 checkout 4249661388e5d3f92a85186213da140a6481490f
cargo build --locked --release -p zipp-cli
python tools/run_test262_dual.py --t262 target/test262 \
  --corrected-tree target/test262-corrected --zipp target/release/zipp \
  --jobs 2 --timeout 120 --output-dir target/test262-evidence
```

Use `target/release/zipp.exe` on Windows. The corrected checkout must not already
exist unless `--reuse-corrected` is supplied; reuse verifies the same revision,
changed paths and file hashes again. The output directory must not contain
earlier JSON reports. Both
runs include staging and exclude the separate `intl402` suite. This does not
claim complete ECMA-402 support.

The tool creates a separate local clone. It verifies the pinned revision,
checkout line endings, the absence of untracked files, the exact changed paths and every before/after file
hash in [manifest.json](manifest.json). Git applies ordinary reviewable unified
patches. No upstream harness is globally changed. Revisit these corrections
when updating the upstream revision; they deliberately cannot silently apply
to a different corpus.

Outputs are `upstream.json`, `corrected.json`, two failure lists, and
`comparison.json`. The comparison records executable, manifest, patch and raw
report SHA-256 hashes. The raw reports retain the engine's actual build identity
and the corrected tree's dirty status. The gate requires matching execution
sets and engine builds, the exact original failure identities, no skips, and
**zero corrected failures with no expected-failure allowance**. Both runs are
attempted even when the first gate fails.

Git newline conversion can otherwise alter imported byte fixtures while
`git status` still reports a clean tree. The preflight rejects that mismatch;
the VM must read the original fixture bytes, not compensate for a modified
checkout.

## DateTimeFormat data provenance and scope

The DateTimeFormat modules `cldr_dtf_en.rs`, `cldr_de.rs`, `cldr_ja.rs`,
`cldr_zh.rs` and `cldr_ar_eg.rs` are generated from the official
[CLDR JSON 48.0.0 release](https://github.com/unicode-org/cldr-json/tree/48.0.0),
under the repository's `LICENSE-UNICODE`. Their headers record source hashes.
Download each locale's date/calendar/number/time-zone JSON inputs and supplemental
dayPeriods, timeData and likelySubtags listed by `tools/gen_cldr_en.py`.
Japanese and Chinese also require their `cldr-rbnf` locale `rbnf.json`.
For example, regenerate German with:

```sh
python tools/gen_cldr_en.py target/cldr-dtf-de-48 --version 48.0.0 \
  --locale de --date-time-only -o crates/zipp-vm/src/vm/cldr_de.rs
rustfmt --edition 2021 crates/zipp-vm/src/vm/cldr_de.rs
```

DateTimeFormat advertises `en`/`en-US`, `de`/`de-DE`, `ja`/`ja-JP`,
`zh`/`zh-CN` and `ar-EG`. Its month/weekday/era
names, styles, flexible day periods, interval patterns and UTC long name use
the selected locale. German defaults to a 24-hour clock; explicit hour-cycle
overrides are supported. Calendar-specific patterns preserve related years,
cyclic year names and leap-month names. Other Intl services retain their
English locale set and existing CLDR 47 data.
This adds neither broad locale coverage nor a claim of full Node/ICU parity.
