# Call-order speed recovery and engine size, tenth pass - 2026-10-09

Continues `codex/engine-size-reduction` from `ec0839ce` to `c48fc4d2`: 18
commits, local only, CI skipped at the user's request. The work covers native
JavaScript throughput, the Python frontend and the size of the four
WebAssembly modules.

- **Native JavaScript.** The 13 real-workload rows take 10.95% less time in
  geomean than at `ec0839ce` (95% interval 10.32 to 11.65, same build mode on
  both sides). The ten headline rows are at 0.982x of Node 24 [0.974, 0.992].
  These are ordinary release builds, not the profile-guided build the
  published table uses; no canonical number is claimed.
- **Process start.** The default Windows CLI starts in 6.3 ms instead of 8.9.
- **Python.** In-script time moves from 1.82x to 1.80x of CPython 3.13, and
  whole-process time falls 5.5%. Parity with CPython is not reached.
- **WebAssembly.** The four modules are 0.66% to 1.43% smaller raw and 0.44%
  to 0.83% smaller after Brotli, and 1% to 3% faster in geomean. One row of
  the speed screen, promise-resolve, is 4% to 9% slower in the Lite and
  Python-base modules, which exceeds the ninth pass's rejection rule (see
  [the screen](#webassembly-speed-screen)), so the modules are reported as
  measured, not as accepted packages.
- **Gates on `c48fc4d2`.** Test262 passes with the expected-failure manifest
  (95,671 of 95,680 executions, nothing unexpected, stale or skipped). The
  full `zipp-vm` suite passes: 2,175 tests in 307 binaries. The Node boundary
  suite passes 35/35 on three modules and 33/35 on Python-base, whose two
  failures are the existing Torch-dependent checks.

## Where the native time had gone

A same-mode comparison of `ec0839ce` with `8229b3fc`, the commit of the
canonical table and the last before B280 changed native call lowering, showed
14.92% more time in geomean [14.42, 16.41]. The size branch was not the
cause: `09862c66`, its base, to `ec0839ce` is -0.14% [-0.76, +0.32]. The
README names B280's strict default as the unmeasured suspect for the slower
`14770703` capture; this measures it.

The cause was the strict member-call order that became the default in B280.
`recv.m(args)` whose arguments are not order-transparent now lowers to
`GetProp callee; <args>; CallWithThis` instead of one fused `CallMethod`, and
the native tiers had fast paths only for the fused form. On one binary,
`ZIPP_RELAXED_CALL_ORDER=1` against the default gave typedarray-math -45.8%,
sparse-array -26.8%, map-set-heavy -22.1%, regex-log-scan -7.0% and
parse-large-js -6.9%. The two shortest rows had a second cause, 2.5 ms of
process start, described below.

## Native changes

| Commit | Change |
| --- | --- |
| `739603bc` | A captured `Map`/`Set` `get`/`has`/`set`/`add`/`delete`, string `indexOf`/`substring`/`slice` or `hasOwn.call` callee runs the fused form's intrinsic helper when it is exactly the boot intrinsic a Get of that name resolves to; otherwise it declines before any effect. |
| `270f263e` | The nested DataView reduction accepts the captured getter pair. |
| `5c046753` | A `Map` or `Set` method read fills a way in the native inline cache. |
| `628e8a4a` | The interpreter's captured-intrinsic lane stays inlined in its call arm. |
| `d91ae358`, `98d3b010` | The interpreter answers `.length` of a string, an Array or a TypedArray from the internal slot ahead of the property cache; the TypedArray getter proof is memoized per kind. |
| `18614d3d` | The DOUBLE register tier reads `ta.length` from the pin snapshot. |
| `436116b6` | The nested leaf splice takes a wrapper over a closure that captures `Math.imul`, the shape of `ri(n) { return (rnd() * n) \| 0 }` over mulberry32. |
| `131aa66c` | The GPU runtime's `dxgi`, `user32`, `setupapi` and `oleaut32` imports are delay-loaded. |

Every native lane is guarded by the identity of the live intrinsic and has a
same-binary off switch (`ZIPP_NO_CAPTURED_METHOD_LANE`,
`ZIPP_NO_COLL_METHOD_WAY`, `ZIPP_NO_DOUBLE_TA_LENGTH`,
`ZIPP_NO_NESTED_CAPTURED_IMUL`); the interpreter's `.length` shortcut has
none. Each change has a test file whose expected values come from Node 24 and
that runs in the interpreter and in the native tiers, and with its switch off
where it has one: `captured_method_lane`, `dv_nested_reduce`, `length_fast`,
`double_ta_length`, `nested_captured_imul`.

## Native measurements

`tools/bench.py`, cold metric, exact output required on every row. Default
features, fat LTO, not profile-guided. AMD Ryzen 9 9950X3D, Windows 11.
`ec0839ce` against `c48fc4d2` uses 12 paired repetitions; the Node 24.19.0
table and the relaxed-order comparison use 10.

| Row | `ec0839ce` | Final | Paired change [95% interval] | Final / Node 24 | Relaxed order on final |
| --- | ---: | ---: | ---: | ---: | ---: |
| async-promise-chain | 477 ms | 482 ms | +1.1% [+0.3, +1.7] | 1.41x [1.40, 1.46] | -3.5% [-4.7, -2.9] |
| class-prototype-hot | 231 ms | 228 ms | -1.0% [-2.0, +0.4] | 0.76x [0.75, 0.77] | +1.4% [-0.9, +1.9] |
| json-large | 306 ms | 307 ms | -0.9% [-1.9, +1.3] | 1.16x [1.13, 1.21] | +0.2% [-2.4, +2.5] |
| map-set-heavy | 914 ms | 757 ms | -16.8% [-20.5, -12.6] | 1.04x [0.98, 1.10] | -6.3% [-8.0, -5.1] |
| markdown-render | 234 ms | 227 ms | -1.6% [-4.1, -0.8] | 0.83x [0.80, 0.85] | -3.1% [-5.7, -0.9] |
| parse-large-js | 256 ms | 254 ms | -1.3% [-2.2, +0.9] | 0.92x [0.91, 0.95] | -7.0% [-8.8, -6.5] |
| polymorphic-objects | 414 ms | 413 ms | -0.2% [-1.2, +1.1] | 1.26x [1.24, 1.27] | +0.3% [-0.3, +1.7] |
| polymorphic-objects-v2 | 30 ms | 28 ms | -5.8% [-8.5, -4.9] | 0.36x [0.35, 0.37] | +0.3% [-3.7, +5.0] |
| property-ic-shapes | 12 ms | 10 ms | -18.4% [-22.2, -14.4] | 0.04x [0.04, 0.04] | +1.4% [-8.3, +7.6] |
| regex-log-scan | 496 ms | 459 ms | -7.7% [-10.9, -5.8] | 0.98x [0.95, 1.01] | +1.3% [-2.3, +3.0] |
| sparse-array | 103 ms | 76 ms | -25.5% [-26.4, -24.9] | 0.97x [0.95, 0.99] | -5.0% [-5.7, -4.0] |
| sparse-array-v2 | 109 ms | 106 ms | -2.4% [-3.1, -0.8] | 0.63x [0.62, 0.63] | +0.8% [-2.5, +1.7] |
| typedarray-math | 271 ms | 145 ms | -46.4% [-46.9, -45.4] | 0.70x [0.68, 0.73] | -0.2% [-3.1, +0.7] |
| Geomean, 13 rows | | | -10.95% [-11.65, -10.32] | 0.687x [0.683, 0.693] | -1.55% [-2.59, -1.02] |
| Geomean, 10 headline rows | | | -11.49% [-12.2, -10.8] | 0.982x [0.974, 0.992] | -2.25% [-3.2, -1.8] |

The 13-row ratio to Node is dominated by the three diagnostic rows, two of
which are mostly process start; the headline ten is the figure to read. Against Node the engine is
behind on four rows: async-promise-chain, polymorphic-objects, json-large and
map-set-heavy, whose interval includes parity.

The strict order still costs 1.55% on the final binary. The largest remaining
gaps are parse-large-js, map-set-heavy and sparse-array. In parse-large-js the
scanner's integer regions engage six GPR homes under the strict order and nine
under the relaxed one; that is the next place to look.

async-promise-chain is 1.1% slower than at `ec0839ce`. Against the same base
the saved builds of `628e8a4a`, `18614d3d`, `66217a9b`, `0fb24b41`,
`f08a9b5d`, `436116b6` and `c48fc4d2` read +1.7%, +0.2%, +3.9%, +1.7%, +2.8%,
+1.8% and +0.8% (10 repetitions each). The readings rise and fall from build
to build instead of stepping at one commit, so the row is reported, not
attributed.

### Process start

The `gpu` feature is on by default, and its runtime imported four system DLLs
that the loader initialised before `main` on every launch. Minimum of 200
interleaved launches, thin-LTO builds of one source: `zipp --version` 7.59 ms
to 5.14 ms, the same as a build without the feature; `zipp js` on an empty
file 8.48 ms to 5.98 ms. The benchmark harness's paired launches read 8.9 ms
and 6.3 ms for the release binaries. `tests/windows_startup_imports.rs` reads
the linked executable's import and delay-import tables and fails with the
option removed. Both the Vulkan and the Direct3D 12 backend start through the
delay-loaded DLLs on a hardware adapter, with `link.exe` and with `rust-lld`
in `lld-link` flavour. GNU-flavour Windows targets keep the ordinary imports.

## Python

`66217a9b` makes the native `str` methods read their operands where they
live instead of copying them. `tools/python_bench.py`, 7 repetitions,
`ec0839ce` against `c48fc4d2`, CPython 3.13.14:

| Measure | `ec0839ce` | `c48fc4d2` |
| --- | ---: | ---: |
| In-script time / CPython, 36 programs | 1.82x | 1.80x |
| In-script time, final / base, 44 programs | | 0.99x |
| Whole-process time, final / base, 44 programs | | 0.945x |
| Process overhead (wall minus in-script), median | 24.7 ms | 22.0 ms |

Rows that moved by more than 5%: fstring 0.76x, json_roundtrip 0.91x,
sort_heapq 0.91x and torch_dataloader 0.92x faster; generator_iter 1.12x,
math_sqrt 1.10x, super_call 1.06x and property_read 1.05x slower. One engine
commit earlier, at `436116b6`, the slower four read 1.07x, 1.01x, 1.04x and
1.01x, so three of them move between builds.

generator_iter reads 0.99x, 0.99x, 1.00x and 0.99x on the builds of
`0fb24b41`, `10a033df`, `f08a9b5d` and `131aa66c`, 1.07x at `436116b6` and
1.11x at `c48fc4d2`. The behaviour `436116b6` adds is not the cause: with
`ZIPP_NO_NESTED_CAPTURED_IMUL=1` the final binary takes the same 65 ms, and
both builds log the same single compiled Python region. With `ZIPP_PY_JIT=0`
the build before the step and the final build both take 65 ms; with the JIT
on the earlier one takes 59 ms.

This note first called that a regression located at `436116b6`. It is not one
of the source. [The eleventh pass](2026-10-09-engine-size-round11.md#link-layout)
linked `c48fc4d2` a second time and that binary runs the row in 59.8 ms where
the one measured here took 67.7 ms; the same second link runs dict_get in
24.0 ms where this one took 16.9 ms. Whether a compiled Python loop beats the
interpreter depends on where the linker put the code, and the steps above
are that.

The largest remaining ratios to CPython are str_methods 6.3x, try_raise_catch
5.0x, instance_new 4.6x, generator_iter 4.5x and fstring 4.3x. CPython is
launched through `py -3.13`, so the whole-process comparison with it (0.67x)
includes the launcher and is not a claim.

## WebAssembly size

| Commit | Change |
| --- | --- |
| `4fbed039` | Two unreachable messages in the regex backtracker no longer format an instruction with `{:?}`, which was the only use of its derived `Debug`: 95 functions, 22.6 KB. |
| `f1b0ce0c`, `b18ef0b6`, `30ad2545` | Cold call sites share one sort through a `dyn` comparison instead of instantiating the standard library's sort per closure and element type. Only allocating stable sorts were converted in the end: the RegExp-program heap audit and the event loop's fired-timer batch, both allocation-free unstable sorts, have their own instantiations again. |
| `0fb24b41` | `--remap-path-prefix` removes the builder's checkout, registry and toolchain paths (157 paths, 11 KB in the JavaScript module). The Python library and runtime sources and the embedding's preamble are kept LF, so a Windows checkout embeds the same text as a Linux one (60 KB of carriage returns in the complete module). `wasm-opt -O1` runs twice. |

Rust 1.92.0, wasm-bindgen 0.2.126, Binaryen 125 `-O1 -O1`, Node 24.19.0,
Windows x64, built from `c48fc4d2`. Module bytes exclude JS glue; Brotli is
quality 11. "Previous" is the accepted ninth-pass package.

| Module | Previous raw | Selected raw | Change | Previous Brotli | Selected Brotli | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Lite | 2,827,053 | 2,808,464 | -18,589 (-0.66%) | 753,477 | 750,138 | -3,339 (-0.44%) |
| JavaScript | 5,220,442 | 5,145,953 | -74,489 (-1.43%) | 1,290,210 | 1,279,443 | -10,767 (-0.83%) |
| Python-base | 6,896,451 | 6,808,551 | -87,900 (-1.27%) | 1,663,170 | 1,651,669 | -11,501 (-0.69%) |
| Python + Torch | 8,851,582 | 8,733,543 | -118,039 (-1.33%) | 2,008,520 | 1,995,301 | -13,219 (-0.66%) |

The previous Python modules were built from a CRLF checkout (13,750 and
43,675 carriage-return pairs), so that part of the saving applies to Windows
builds only. No module contains an absolute path. Two builds of one tree from
two checkout directories still differ by four bytes of code, so the modules
are not bit-reproducible across locations.

Post-processing candidates on the JavaScript module, raw / Brotli, from
5,384,089 / 1,294,137 before optimisation:

| Pipeline | Raw | Brotli-11 | Decision |
| --- | ---: | ---: | --- |
| `-O1` | 5,225,008 | 1,291,816 | previous |
| `-O1 -O1` | 5,221,923 | 1,289,944 | selected: smaller on both for every variant |
| `-O1 -O1 -O1` | 5,221,889 | 1,290,355 | rejected, Brotli |
| `-O1 --converge` | 5,221,776 | 1,290,739 | rejected, Brotli |
| `-O1 -O1 --code-folding` | 5,167,285 | 1,294,029 | rejected, Brotli |
| `-O1 -O1 --dae-optimizing` | 5,207,583 | 1,295,886 | rejected, Brotli |
| `-O1 -O1 --reorder-functions` | 5,181,837 | 1,297,494 | rejected, Brotli |
| `-O1 -O1 --local-cse` | 5,215,918 | 1,302,410 | rejected, Brotli |
| `-O1 -O2` | 5,160,860 | 1,312,724 | rejected, Brotli |
| `-O1 -O1 --const-hoisting` | 5,076,475 | 1,316,742 | rejected, Brotli |
| `-Os` | 5,052,688 | 1,309,469 | rejected, Brotli |

The QuickJS size target remains unmet: Lite is 1,670,075 raw bytes larger
than the audited 1,138,389-byte reference.

## WebAssembly speed screen

Each case runs in its own Node process (`--no-liftoff`, one pinned logical
processor), with ten warmup pairs and 31 alternating paired samples, in four
configurations: baseline first and candidate first on two cores. A sample is
discarded if a compiler or linker is running. The figures are candidate time
over the accepted ninth-pass module's.

| Module | Core 1, baseline first | Core 1, candidate first | Core 2, baseline first | Core 2, candidate first | More than 2.5% slower in three of four | More than 2.5% faster in three of four |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| Lite | 0.979 | 0.988 | 0.980 | 0.985 | promise-resolve, string-build | array-build, regex |
| JavaScript | 0.989 | 0.973 | 0.990 | 0.977 | property-delete | array-build, regex |
| Python-base | 0.984 | 0.991 | 0.982 | 0.982 | promise-resolve, prop-mono | array-build, regex, python-list-e2e, python-dictionary-e2e |
| Python + Torch | 0.973 | 0.982 | 0.984 | 0.981 | none | array-build, regex, error-range, python-list-e2e, python-dictionary-e2e |

array-build is 0.57 to 0.75 in every module (the `.length` change) and regex
0.91 to 0.97.

The flagged rows were measured again. string-build (0.966 / 0.968 / 0.930 /
1.061) and prop-mono (1.026 / 1.022 / 0.986 / 1.022) did not repeat.
property-delete stayed at the threshold in JavaScript (1.041 / 0.992 / 1.025
/ 1.097). promise-resolve repeated in Lite (1.044 / 1.064 / 1.048 / 1.048)
and Python-base (1.196 / 1.061 / 1.072 / 1.087). By the ninth pass's rule,
more than 2.5% slower in three of four comparisons, Lite and Python-base fail
on promise-resolve and JavaScript is at the limit on property-delete.

That row could not be tied to a source change. It is a loop of
`Promise.resolve(i).then(f)`, which schedules no timer and compiles no
regular expression, yet it moved with edits to code it never runs:

| JavaScript build | promise-resolve, four configurations |
| --- | --- |
| Byte-identical copy of the baseline | 0.999 / 1.000 / 1.003 / 1.002 |
| `0fb24b41` | 0.987 / 0.991 / 0.978 / 0.990 |
| `f08a9b5d` | 1.066 / 1.047 / 1.087 / 1.043 |
| Final source, timer batch through the shared sort | 0.941 / 0.930 / 0.943 / 0.941 |
| Final source, timer sort out of line | 1.039 / 1.035 / 1.035 / 1.060 |
| Final source (`c48fc4d2`) | 1.018 / 1.005 / 1.019 / 1.029 |
| Final source, RegExp audit sort out of line | 1.064 / 1.044 / 1.130 / 1.096 |

The last variant improved Lite (1.028 / 1.017 / 1.001 / 1.061) and
Python-base (1.017 / 1.061 / 1.006 / 1.044) while making JavaScript worse, so
it was not adopted. The final source was chosen on other grounds: the timer
batch is sorted after a fallible reserve so that a host allocation failure
cannot abort the sandbox, and the shared sort allocates.

Two builds of the final code from different checkouts, differing only in
comments, a dead constant and line numbers, agree with each other (geomean
0.992 to 0.999, no row flagged), so the movement comes with changes to the
code, not from rebuilding.

The screen did find two real regressions, both fixed before these builds:

- RegExp compilation was 1.58x to 2.73x slower in all four runs while the
  shared sort served the periodic heap audit of compiled programs. With the
  audit's own sort restored the row is 0.98 to 1.02.
- Moving the `.length` shortcut behind the property cache (`10a033df`) was
  tried to protect cached reads, compared directly, and reverted
  (`f08a9b5d`): prop-mono was never faster (1.015 / 1.050 / 1.322 / 1.000) and
  a length read cost about twice as much.

An earlier screen that ran every case in one process gave promise-resolve
0.93 to 0.95 for a JavaScript build that reads 1.04 to 1.11 in isolation, and
1.18 to 1.25 for a complete build that reads 1.00 in isolation. Only the
isolated method, the ninth pass's, is reported above.

## Correctness

All on `c48fc4d2` with a clean tree, run one after another with nothing else
building:

- Test262, the full corpus with `tools/test262-expected-failures.txt`: 95,671
  of 95,680 executions pass, the nine failures are the manifest's, and there
  is no unexpected failure, stale expectation or skip. The local corpus is an
  exported tree under `target/`, so the runner prints the engine repository's
  commit as the corpus revision; the manifest pins corpus `42496613`.
- `cargo test -p zipp-vm --no-fail-fast` with the `tools/ci_quarantine.py`
  skips: 2,175 passed, 0 failed, 22 ignored, in 307 test binaries.
- `cargo test -p zipp-cli`: 78 passed, 1 ignored, including the native GPU
  tests on a hardware adapter.
- `cargo test -p zipp-vm --features python --lib`: 614 passed, 5 ignored.
- `zipp-vm` checks with warnings denied in the default, `safe-sandbox`,
  `python` and `wasm-lite` profiles.
- The Node boundary suite on the exact modules of the size table: JavaScript
  35/35, Lite 35/35, Python + Torch 35/35, Python-base 33/35. The two
  Python-base failures are `audit-2026-09-15-ml-transport.cjs` and
  `python-gpu.cjs`, which need the optional Torch package, as in the ninth
  pass.

The three workflow steps that build the module directly now pass their flags
through `remap-paths.sh`. They were syntax-checked only; CI was not run.

## Not claimed

- No profile-guided build was made, so nothing here updates the published
  comparison with Node.
- No Linux, macOS or browser measurement.
- CPython parity and the QuickJS size target are both unmet.

Module hashes, every screen ratio, the native and Python tables and the gate
results are in [the evidence](2026-10-09-engine-size-round10-evidence.json).
