# Historical pre-integration report

This report describes the original standalone environment, before integration into Zipp.
Its unavailable checks are historical, not the current integration status.

# Validation record: 13 September 2026

## Executed successfully

| Check | Actual result | Evidence |
|---|---|---|
| Node test runner | 57 passed, 0 failed | node-tests.tap |
| Python unittest | 12 passed | python-tests.txt |
| Native Python -> host Node -> compiled WASM | Vector output `[14,44,94,164]`, sum `316` | native-python-wasm.txt |
| Native Python -> host JavaScript reference | Same expected output | native-python-js.txt |
| Chromium: actual WASM runtime | 15 numerical checks passed | browser-validation.json |
| Chromium: JavaScript reference | 15 numerical checks passed | browser-validation.json |
| JavaScript syntax checks | All `.mjs` / `.js` files passed `node --check` | command below |
| Demo visual inspection | Desktop layout inspected; no horizontal overflow at 390px | demo-layout.png and browser-validation.json |

Node was v22.16.0. Python was 3.13.5. Chromium was 144.0.7559.96.
The included 3,521-byte WASM module was built with Clang 17.0.0 and wasm-ld from
`wasm/kernels.c`, using `scripts/build_wasm.sh`. It has no WebAssembly imports.

**Historical.** Every figure above, including the recorded response sizes in
`http-validation.json`, describes the 13 September 2026 artifact. `955914f3`
then changed `wasm/kernels.c` and rebuilt the module: the committed binary is
now 4,217 bytes (SHA-256
`79ceef0c974f0e62ecb676e88f22c68d14037251dedf5002ccd9096a64cefa57`). This
archive is left as the record of that run and is not updated.

The 57 Node tests include seven WebGPU **API/lifecycle mock** tests. Those verify
host-side contracts such as bounds-guard generation, reduction scratch cleanup,
readback mapping failure cleanup, compilation rejection cleanup and device-loss
handling. They do not compile shaders, emulate shader execution, or establish GPU
numerical correctness. The remaining Node tests exercise real JS/WASM computation,
graph validation, and mock ZIPP callback/queue contracts.

The small MLP example's expected values were also independently checked with NumPy
in the development environment. NumPy is not a project dependency and is not bundled.

## Not verified

**WebGPU shader execution:** unavailable here. The local-source Chromium test had
no `navigator.gpu` on its about:blank execution origin. No real GPU adapter result,
hardware throughput, WGSL compilation pass, or WebGPU device test is claimed.

**WebGL2 shader execution:** the environment could not create a WebGL2 context,
including an attempted local software-renderer probe. No GLSL compilation pass or
WebGL2 numerical result is claimed. Its source backend needs the demo's checks on
an actual available context with floating-point render targets.

**Actual ZIPP WASM / private zipp-python integration:** not run. Private source and
an authenticated repository connection were unavailable, and no current upstream
runtime binary was obtained. The adapter tests use mock Engine/queue objects, not
an actual ZIPP Engine. The Python library was executed in native CPython only.

**Real-origin demo Worker/HTTP integration:** not run inside Chromium. Browser
navigation to the local server was blocked by the environment's managed policy.
The policy was not changed or bypassed. The local smoke harness executes bundled
project code directly in a page with `set_content` / `evaluate`, with the actual
included WASM bytes supplied in memory. This validates browser runtime maths but
not native ES-module loading, the served origin, or Worker startup.

The visual screenshot is a layout-only rendering of local HTML/CSS. It is not a
screenshot of a successful GPU run or a proof of browser UI interaction coverage.

## Reproduction

From the project directory:

```sh
python examples/build_examples.py
node --test tests/*.test.mjs
python -m unittest discover -s tests -p 'test_python.py'
python examples/run_native.py
python examples/run_native.py cpu-js
```

Optional local-source browser checks:

```sh
python scripts/browser_smoke.py
```

That command requires Playwright and an installed Chromium executable. It may mark
WebGPU unavailable because its test page has no secure HTTP origin. Do not use that
absence as proof that WebGPU is unavailable in an ordinary localhost/HTTPS browser.

For the missing acceptance step, serve the actual project:

```sh
python scripts/serve.py
```

Open `http://localhost:8765/demo/` in the target browser, select WebGPU explicitly,
run examples and **Check all backends**, then save the report. Repeat on WebGL2.
A supported backend must pass numerical checks; an unavailable backend is a skip,
not success. A shader/numerical failure must remain a failure and never fall back
silently to CPU to make the test green.

## Performance interpretation

The recorded wall times are smoke-test measurements, not benchmarks or speedup
claims. Browser-local tests ran in a shared environment. Shader compilation,
GPU timestamps, real hardware execution, CPU-to-GPU bandwidth, steady-state GPU
residency, and comparisons against optimized tensor libraries were not measured.
