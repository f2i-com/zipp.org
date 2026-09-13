# Validation and reproduction

The runtime is integrated into the Python-enabled Zipp WASM playground. The
original standalone report is preserved in [the historical archive](validation-archive-20260913.md),
along with its raw reports. Those old environment restrictions do not describe
current source integration.

Current checks are separated by what they establish:

| Check | Coverage |
|---|---|
| `npm test` in gpu-lab | JS/WASM numerical behavior, validators, adapter contracts, WebGPU lifecycle mocks, WebGL texture-budget bookkeeping |
| `python tests/test_python.py` | Native Python graph construction/export |
| `node ../tests/node/python-frontend.cjs` | Actual Python-enabled WASM ABI, projects, VFS mutations and dictionary conversion |
| `node ../tests/node/python-gpu.cjs` | Actual Python-to-host graph requests and JS/WASM evaluation |
| `landing/scripts/smoke-browser.py`, `REQUIRE_GPU=1` | Served playground, folder loading, examples, real hardware WebGL2/WebGPU and animation |
| `tests/browser-cases.mjs` in the diagnostic demo | 15 numerical cases per selected browser backend |

The Python CI lane in `.github/workflows/ci.yml` builds the Python feature explicitly,
checks the artifact profile before running its boundary tests, and runs the native
VM/CLI Python tests. Existing workflow triggers remain manual/reusable while the
repository's automatic CI pause is in effect.

GPU correctness has been checked locally with RTX 5090 hardware; the browser
reports WebGL2 through ANGLE/D3D11 and WebGPU through the Blackwell adapter.
Portable mocks do not compile shaders. Unavailable GPU checks must be reported
as unavailable, never silently replaced by a CPU pass.

For repeatable browser checks, build the landing page, start its preview, and run
`python landing/scripts/smoke-browser.py http://127.0.0.1:4173` from the repository
root with `REQUIRE_GPU=1`. This requires Chrome and Python Playwright.
The diagnostic demo provides **Check all backends** for the full numerical matrix.

Timings and GIFs are functional demonstrations, not performance benchmarks.
Shader compilation, transfers, driver memory and thermal/load differences affect
results. `maxLogicalBytes` and WebGL's explicit texture-byte ceiling are different
budgets; neither measures the entire browser's memory usage.

## Local acceptance — 13 September 2026

Windows / Chrome 152 / NVIDIA GeForce RTX 5090:

- 60 Node GPU contract tests and 12 native Python graph tests passed.
- Rebuilt default Python WASM: 57 frontend and 27 GPU boundary checks passed,
  including compiled Torch models and functional operations with CPU constants.
- 15 numerical cases each passed on actual WebGL2 and WebGPU hardware.
- Production landing build, 9 landing tests, folder/reload/arguments, browser VFS
  mutations, embedded Python, mobile layout, animation controls, Life and Torch
  inference passed. Torch predictions matched eager inference on WASM/WebGL2/WebGPU.
- 26 native Python VM tests passed; CLI tests and the new project regressions passed.
- Opt-in language interoperability passed 2 native tests and the WASM instance
  checks. The ordinary Python WASM build correctly rejects that optional module.
- All edited Rust files pass rustfmt. Repository-wide `cargo fmt --all -- --check`
  still reports pre-existing formatting differences in unrelated engine files.

These are local results. The updated manual/reusable CI lane has not been run on GitHub.
